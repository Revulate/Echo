//! Settings modal host.
//!
//! The modal owns chrome and lifecycle only — backdrop, dialog geometry, focus
//! trap, and the escape-stack contract. Navigation, search, the content pane,
//! and the whole keyboard/accessibility model live in
//! [`settings_surface`][crate::components::settings::settings_surface], which the
//! native popout hosts identically.

mod benchmark_hooks;

use crate::components::auth::LoginModalState;
use crate::components::overlays::traits::OverlayPointerIsolation;
use crate::config;
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels},
};
use gpui::*;

use super::settings_surface::{
    SettingsChrome, SettingsOpenRequest, SettingsSurface, SettingsSurfaceHost,
    render_settings_surface, watch_appearance_changes,
};

pub use super::settings_navigation::{SettingsLocation, SettingsSectionId, section_label};

/// Retained state for the in-window settings overlay.
pub struct SettingsModalState {
    pub is_open: bool,
    /// Mirror of the surface's current page.
    ///
    /// The no-send interaction harness navigates by writing this field directly,
    /// so it is a real field rather than an accessor: writes are reconciled into
    /// the surface at the next mutation or render, and surface-driven navigation
    /// is mirrored back out.
    pub selected_section: SettingsSectionId,
    /// Last value both sides agreed on, which is what makes an external write
    /// distinguishable from surface-driven navigation.
    mirrored_section: SettingsSectionId,
    surface: SettingsSurface,
    /// Keeps an open modal synchronized with palette changes made elsewhere.
    /// The retained modal can be closed without being dropped, so the shared
    /// watcher deliberately checks `is_open` before requesting a redraw.
    _appearance_changes_task: Task<()>,
}

/// Read-only state exposed to the no-send interaction harness. Keeping this
/// contract beside the production controller ensures automation observes the
/// same filtering and disclosure rules as the rendered settings surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsAutomationSnapshot {
    pub is_open: bool,
    pub search_focused: bool,
    pub query: String,
    pub selected_destination: &'static str,
    pub visible_result_count: usize,
    pub no_results: bool,
    pub advanced_revealed: bool,
    pub developer_visible: bool,
}

/// Navigation-tree state exposed to the acceptance driver.
///
/// Kept separate from [`SettingsAutomationSnapshot`] so the existing settings
/// checkpoints keep their exact shape while the tree/anchor/search checkpoints
/// get the identities they actually need to assert on: slugs, not labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsNavigationSnapshot {
    /// Current page slug (`"developer-tools"`, ...).
    pub page: &'static str,
    /// Current anchor slug, or `None` on a page leaf.
    pub anchor: Option<&'static str>,
    /// Label of the row the tree keyboard model focuses.
    pub focused_row: Option<&'static str>,
    /// Visible rows as `level:label`, in traversal order.
    pub tree_rows: Vec<String>,
    /// Text the `Role::Status` live region settles on.
    pub status: Option<String>,
    pub hidden_settings: usize,
    pub hidden_subsections: usize,
    pub hidden_pages: usize,
    /// Query a modal -> popout transfer would carry.
    pub transfer_query: Option<String>,
    /// Whether the transfer payload points at the destination on screen.
    pub transfer_matches_location: bool,
    /// Block index the page's scroll-spy currently reports.
    pub active_block: usize,
    /// Block index the current destination should occupy.
    pub target_block: usize,
    /// Whether an anchor jump is still waiting for a laid-out frame.
    pub anchor_pending: bool,
}

impl SettingsModalState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let surface = SettingsSurface::new(SettingsChrome::Modal, cx);
        let appearance_changes_task = watch_appearance_changes(cx, |host| host.is_open);
        let selected_section = surface.location().section;
        Self {
            is_open: false,
            selected_section,
            mirrored_section: selected_section,
            surface,
            _appearance_changes_task: appearance_changes_task,
        }
    }

    /// Attach the shared login surface. Idempotent: the host calls this once,
    /// the first time it mounts the shell.
    pub fn attach_login_modal(&mut self, login_modal: &Entity<LoginModalState>) {
        self.surface.attach_login_modal(login_modal);
    }

    /// Whether the login surface still needs to be attached by the host.
    pub fn needs_login_modal(&self) -> bool {
        self.surface.needs_login_modal()
    }

    /// Open on the remembered location.
    pub fn open(&mut self) {
        self.open_at(&SettingsOpenRequest::restore());
    }

    /// Open on an explicit destination (hotkey, toast, top-bar target).
    pub fn open_at(&mut self, request: &SettingsOpenRequest) {
        self.surface.apply_open_request(request);
        self.is_open = true;
        self.pull_selected_section();
    }

    /// Close and remember the destination the user ended on.
    pub fn close(&mut self) {
        self.surface.persist_current_location();
        self.close_silent();
    }

    /// Close without touching persisted preferences.
    fn close_silent(&mut self) {
        self.is_open = false;
        self.surface.reset_session();
        self.pull_selected_section();
    }

    pub fn surface(&self) -> &SettingsSurface {
        &self.surface
    }

    pub fn developer_visible(&self) -> bool {
        self.surface.developer_visible()
    }

    /// Reconcile the automation mirror with the surface.
    ///
    /// An external write to `selected_section` wins (the harness is expressing
    /// intent); otherwise the surface's own navigation is mirrored back out.
    fn reconcile_selected_section(&mut self) -> bool {
        if self.selected_section != self.mirrored_section {
            let location = SettingsLocation::page(self.selected_section);
            let changed = self.surface.adopt_location(location);
            return self.pull_selected_section() || changed;
        }
        self.pull_selected_section()
    }

    fn pull_selected_section(&mut self) -> bool {
        let section = self.surface.location().section;
        if self.selected_section == section && self.mirrored_section == section {
            return false;
        }
        self.selected_section = section;
        self.mirrored_section = section;
        true
    }

    fn selected_label(&self) -> &'static str {
        section_label(self.selected_section)
    }
}

impl SettingsSurfaceHost for SettingsModalState {
    fn surface(&self) -> &SettingsSurface {
        &self.surface
    }

    fn surface_mut(&mut self) -> &mut SettingsSurface {
        &mut self.surface
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close();
        self.surface.restore_focus(window, cx);
        cx.notify();
    }

    fn request_popout(
        &mut self,
        request: SettingsOpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close();
        crate::components::settings::request_settings_popout(request);
        self.surface.restore_focus(window, cx);
        cx.notify();
    }
}

impl Render for SettingsModalState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open {
            return div().into_any_element();
        }

        self.reconcile_selected_section();

        let modal_fit = crate::utils::fit_overlay_to_viewport(
            window.viewport_size(),
            config::SETTINGS_MODAL_WIDTH_PX,
            config::SETTINGS_MODAL_HEIGHT_PX,
            8.0,
        );
        let surface = render_settings_surface(self, window, cx);

        div()
            .id("settings-modal")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors::backdrop())
            .block_overlay_pointer_passthrough()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    SettingsSurfaceHost::request_close(this, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("settings-dialog")
                    .role(Role::Dialog)
                    .aria_label("Echo settings")
                    .w(px(modal_fit.width))
                    .h(px(modal_fit.height))
                    .bg(colors::bg_elevated())
                    .rounded(Radii::lg())
                    .border_1()
                    .border_color(colors::border())
                    .shadow_lg()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .block_overlay_pointer_passthrough()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _event, _window, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|_this, _event, _window, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(|_this, _event, _window, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .on_scroll(cx.listener(|_this, _event: &ScrollEvent, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child(surface),
            )
            .into_any_element()
    }
}
