//! Shared retained settings surface.
//!
//! One implementation of the settings experience — flat navigation, search,
//! content pane, focus model, and accessibility contract — hosted twice: by the
//! overlay modal and by the native popout window. The hosts own only chrome and
//! geometry; everything a user interacts with lives here, so the two surfaces
//! cannot drift.
//!
//! The surface is *retained state held by value inside each host entity*, not a
//! nested `Entity`. Rendering is generic over the host through
//! [`SettingsSurfaceHost`], which is what lets both hosts share every listener
//! without duplicating a single element. Two concrete reasons for the by-value
//! shape:
//!
//! - The no-send interaction harness drives the modal through `&mut self`-only
//!   entry points (`benchmark_snapshot`, `benchmark_set_search_query`, ...) with
//!   no `App` in scope; state behind an inner entity would be unreadable there.
//! - Every read from a nested entity needs a context, which would turn simple
//!   accessors into context-threading noise for no reactivity gain: the host
//!   entity already re-renders the whole surface on `cx.notify()`.
//!
//! Change gating is preserved: every mutator returns whether visible state
//! changed, and the only asynchronous work is a one-shot, revision-fenced task
//! that publishes the search-count announcement after it settles.

use crate::components::auth::LoginModalState;
use crate::components::settings::sections::{self, UploaderEditorInputs};
use crate::components::settings::settings_navigation::{
    SettingsLocation, SettingsNavigationIntent, SettingsSearchModel, SettingsSearchState,
    SettingsSectionId, SettingsSubsectionId, anchor_disclosures_for_query, canonical_location,
    canonical_section, coerce_visible_location, disclosure_anchor, navigation_prefs,
    persist_location, persisted_location, persistence_location, presentation_location,
    resolve_requested_section_slug, restore_location, section_descriptor, section_slug,
    setting_target_by_id,
};
use crate::components::settings::settings_page::{SettingsPageScroll, breadcrumb, content_header};
use crate::components::settings::widgets::{
    SettingsDisclosureState, SettingsMetrics, SettingsPageUi, focus_ring, is_activation_key,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::PressToClick;
use crate::components::ui::input::{
    Escape, Input, InputEvent, InputState, normalize_single_line_text,
};
use crate::config;
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels, Spacing},
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const TAB_INDEX_SEARCH: isize = 1;
const TAB_INDEX_POPOUT: isize = 2;
const TAB_INDEX_CLOSE: isize = 3;
const TAB_INDEX_TREE: isize = 4;
/// Content focus follows the search and navigation controls.
const TAB_INDEX_CONTENT: isize = 6;

/// Type-ahead buffer lifetime for tree navigation.
const TYPE_AHEAD_WINDOW: Duration = Duration::from_millis(700);

/// Delay before the search result count is published to the live region, so a
/// fast typist hears one settled announcement instead of one per keystroke.
const STATUS_SETTLE: Duration = Duration::from_millis(350);

/// Upper bound on the search query, matching the previous shell behaviour.
const SEARCH_QUERY_MAX_CHARS: usize = 80;

/// Search is a single-line field even when text arrives through UI Automation,
/// paste, or an open-request transfer. Keep its bound and whitespace policy in
/// one place so those routes cannot disagree with typed input.
fn normalize_search_query(query: &str) -> String {
    let query: String = normalize_single_line_text(query)
        .replace('\t', " ")
        .chars()
        .take(SEARCH_QUERY_MAX_CHARS)
        .collect();
    if query.trim().is_empty() {
        String::new()
    } else {
        query
    }
}

/// Wake a retained Settings host when a palette input changes outside that
/// host, or when emote display rules change from chat (a context menu or its
/// Undo) so the rule list stays current. The subscription is deliberately owned by the host entity rather
/// than the shared surface: the modal may stay allocated while hidden, while
/// a native popout is dropped with its window.
///
/// A lagged receiver also refreshes because it may have missed one of the
/// relevant palette events. `visible` prevents a retained but closed modal
/// from redrawing merely to observe a setting it will read on its next open.
pub(crate) fn watch_appearance_changes<T: 'static>(
    cx: &mut Context<T>,
    visible: fn(&T) -> bool,
) -> Task<()> {
    let settings_events = crate::backend::settings::subscribe_settings();
    cx.spawn(async move |this, cx| {
        let Some(mut settings_events) = settings_events else {
            return;
        };

        loop {
            let refresh = match settings_events.recv().await {
                Ok(
                    crate::backend::settings::SettingsEvent::ThemeChanged
                    | crate::backend::settings::SettingsEvent::AccentChanged
                    | crate::backend::settings::SettingsEvent::EmoteDisplayRulesChanged,
                )
                | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                Ok(_) => false,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if !refresh {
                continue;
            }
            if this
                .update(cx, |host, cx| {
                    if visible(host) {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        }
    })
}

/// Resolve the width of the settings surface being rendered. A modal is fitted
/// inside the application viewport; a popout owns its entire native viewport.
/// Keeping this calculation here lets both hosts use the same compact-layout
/// breakpoint rather than drifting into separate narrow-window designs.
fn settings_surface_width(chrome: SettingsChrome, window: &Window) -> f32 {
    match chrome {
        SettingsChrome::Modal => {
            crate::utils::fit_overlay_to_viewport(
                window.viewport_size(),
                config::SETTINGS_MODAL_WIDTH_PX,
                config::SETTINGS_MODAL_HEIGHT_PX,
                8.0,
            )
            .width
        }
        SettingsChrome::Popout => window.viewport_size().width.into(),
    }
}

/// Keep the full tree when room permits it, then reclaim space for the content
/// pane at the settings surface's narrow safety sizes. Labels already elide in
/// the tree, so hierarchy and keyboard navigation remain unchanged.
fn settings_sidebar_width(chrome: SettingsChrome, window: &Window) -> f32 {
    if settings_surface_width(chrome, window) <= config::SETTINGS_COMPACT_LAYOUT_MAX_WIDTH_PX {
        config::SETTINGS_COMPACT_SIDEBAR_WIDTH_PX
    } else {
        config::SETTINGS_SIDEBAR_WIDTH_PX
    }
}

/// Width available to the rows after the navigation column, divider, and
/// content insets. Compact decisions belong to this column rather than the
/// outer window: the sidebar intentionally changes width near its breakpoint.
fn settings_content_width(chrome: SettingsChrome, window: &Window) -> f32 {
    available_settings_width(
        settings_surface_width(chrome, window),
        settings_sidebar_width(chrome, window),
        SettingsMetrics::CONTENT_PADDING,
    )
}

/// What a host should show when it opens.
///
/// Replaces the bare pending section id: navigation now addresses a page *and*
/// an anchor, and the modal hands its live query to the popout so "Open in
/// window" does not silently drop the user's search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsOpenRequest {
    /// Explicit destination, or `None` to restore the remembered location.
    pub location: Option<SettingsLocation>,
    /// Query transferred from another surface. Never persisted to disk.
    pub query: Option<String>,
    /// Exact row identity transferred between hosts.
    pub setting: Option<String>,
    /// Search-only visibility; never serialized or written to developer_mode.
    pub developer_override: Option<bool>,
    /// Manual expansion transferred between hosts, never persisted.
    pub manual_disclosures: Option<Vec<(SettingsSectionId, String)>>,
}

impl SettingsOpenRequest {
    /// Restore the remembered location with an empty query.
    pub fn restore() -> Self {
        Self::default()
    }

    pub fn at(location: SettingsLocation) -> Self {
        Self {
            location: Some(location),
            query: None,
            setting: None,
            developer_override: None,
            manual_disclosures: None,
        }
    }

    pub fn page(section: SettingsSectionId) -> Self {
        Self::at(SettingsLocation::page(section))
    }

    /// Bridge for callers that still speak in bare section ids (hotkeys, the
    /// top bar, update toasts). `None` restores the remembered location.
    pub fn from_section(section: Option<SettingsSectionId>) -> Self {
        match section {
            Some(section) => Self::page(section),
            None => Self::restore(),
        }
    }

    /// Bridge for callers that address settings by *slug* — deep links, saved
    /// shortcuts, and anything persisted before the IA restructure.
    ///
    /// Current and legacy spellings both resolve here, so a request for the
    /// removed `logging` page lands on Developer Tools › Chat logging rather
    /// than falling back to Account. An unrecognised slug restores the
    /// remembered location instead of guessing.
    pub fn from_slug(slug: &str) -> Self {
        match resolve_requested_section_slug(slug) {
            Some(location) => Self::at(location),
            None => Self::restore(),
        }
    }

    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        let query: String = query.into();
        self.query = (!query.trim().is_empty()).then_some(query);
        self
    }

    pub fn with_setting(mut self, id: impl Into<String>) -> Self {
        self.setting = Some(id.into());
        self
    }

    /// Resolve to a currently visible destination, applying the persisted
    /// fallbacks (unknown anchor -> page default, unknown/hidden page -> last
    /// standard location -> Account).
    pub fn resolve(&self, developer_visible: bool) -> SettingsLocation {
        let location = self
            .setting
            .as_deref()
            .and_then(setting_target_by_id)
            .map(|target| target.location)
            .or(self.location);
        match location {
            Some(location) => {
                coerce_visible_location(location, &navigation_prefs(), developer_visible)
            }
            None => persisted_location(developer_visible),
        }
        .with_default_subsection()
    }
}

/// Which shell is hosting the surface. Only window actions differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsChrome {
    /// In-window overlay: owns Open-in-window and Close, traps focus.
    Modal,
    /// Native window: the OS owns the window actions.
    Popout,
}

impl SettingsChrome {
    /// Whether the shell paints its own window actions.
    pub const fn shows_window_actions(self) -> bool {
        matches!(self, Self::Modal)
    }
}

/// A shell that hosts the shared surface.
///
/// Hosts contribute chrome and lifecycle only. Everything the surface renders is
/// driven through these accessors, so the modal and the popout run the same code
/// path for navigation, search, keyboard, and accessibility.
pub trait SettingsSurfaceHost: Sized + 'static {
    fn surface(&self) -> &SettingsSurface;
    fn surface_mut(&mut self) -> &mut SettingsSurface;

    /// Dismiss the surface (modal: close the overlay and restore the opener;
    /// popout: close the native window).
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>);

    /// Hand the current location and query to a native popout window. The
    /// popout itself has nowhere to pop out to and leaves this unimplemented.
    fn request_popout(
        &mut self,
        _request: SettingsOpenRequest,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}

use super::settings_results::{
    SETTINGS_STACKED_CONTENT_MAX_WIDTH_PX, available_settings_width, node_for_location,
    visible_settings_rows,
};
pub use super::settings_results::{SettingsTreeNode, SettingsTreeRow};

pub(crate) fn notify_uploader_confirmation(
    origin: AnyWeakEntity,
    origin_window: AnyWindowHandle,
    target: Option<&'static str>,
    cx: &mut App,
) {
    let Some(origin) = origin.upgrade() else {
        return;
    };
    let _ = origin_window.update(cx, move |_, window, cx| {
        if let Ok(modal) = origin.clone().downcast::<super::SettingsModalState>() {
            if modal.read(cx).is_open {
                refresh_uploader_confirmation_host(modal, target, window, cx);
            }
        } else if let Ok(popout) = origin.downcast::<super::SettingsPopout>() {
            refresh_uploader_confirmation_host(popout, target, window, cx);
        }
    });
}

fn refresh_uploader_confirmation_host<T: SettingsSurfaceHost>(
    host: Entity<T>,
    target: Option<&'static str>,
    window: &mut Window,
    cx: &mut App,
) {
    host.update(cx, |host, cx| {
        if let Some(target) = target {
            host.surface_mut().set_query("");
            host.surface_mut().navigate_setting(target);
            host.surface_mut()
                .disclosure_state(
                    SettingsSectionId::Connections,
                    "uploads-custom-provider",
                    cx,
                )
                .set_expanded(true, window, cx);
        }
        cx.notify();
    });
    if target.is_some() {
        window.activate_window();
    }
    window.refresh();
}

/// Retained state for one settings surface.
pub struct SettingsSurface {
    chrome: SettingsChrome,
    host_id: EntityId,
    location: SettingsLocation,
    /// Keyboard focus inside the tree composite, tracked separately from the
    /// content location so a category can be focused without navigating.
    focused: SettingsTreeNode,
    query: String,
    /// Whether the search field owns typed input.
    search_active: bool,
    search: SettingsSearchState,
    search_focus: FocusHandle,
    /// The real editable search field retained by this host. Keeping it here
    /// gives keyboard, IME, paste, and accessibility clients one InputState.
    search_input: Option<Entity<InputState>>,
    _search_input_subscription: Option<Subscription>,
    tree_focus: FocusHandle,
    content_focus: FocusHandle,
    type_ahead: String,
    type_ahead_at: Option<Instant>,
    page_scroll: HashMap<SettingsSectionId, SettingsPageScroll>,
    page_ui: HashMap<SettingsSectionId, SettingsPageUi>,
    selected_setting: Option<&'static str>,
    pending_setting: Option<&'static str>,
    setting_jump_revision: u64,
    search_developer_revealed: bool,
    /// Destination and exact scroll position before the current search began.
    /// Escape returns here without replacing remembered navigation.
    search_restore: Option<SearchRestore>,
    /// Anchor jump waiting for the next laid-out frame to resolve its index.
    pending_anchor: Option<SettingsSubsectionId>,
    anchor_reveal: Option<SettingsLocation>,
    pending_disclosure_handoff: Option<Vec<(SettingsSectionId, String)>>,
    uploader_inputs: Option<UploaderEditorInputs>,
    login_modal: Option<Entity<LoginModalState>>,
    /// Non-persisted disclosure override used only by the interaction harness.
    developer_override: Option<bool>,
    status: Option<SharedString>,
    status_revision: u64,
    _status_task: Option<Task<()>>,
    _diagnostics_refresh_task: Option<Task<()>>,
    last_rendered_section: Option<SettingsSectionId>,
    /// Focus the tree on the first frame after opening.
    focus_tree_on_open: bool,
    /// Focus target to restore when a modal host closes.
    focus_return: Option<WeakFocusHandle>,
}

#[derive(Clone, Copy)]
struct SearchRestore {
    location: SettingsLocation,
    offset: Point<Pixels>,
}

impl Drop for SettingsSurface {
    fn drop(&mut self) {
        sections::release_general_settings_inputs_for_host(self.host_id);
        sections::release_proxy_inputs_for_host(self.host_id);
        sections::forget_logging_viewer_state_for_host(self.host_id);
        sections::release_command_editor_inputs_for_host(self.host_id);
        sections::forget_hotkey_editor_host(self.host_id);
        sections::clear_uploader_status(self.host_id.as_u64());
    }
}

impl SettingsSurface {
    pub fn new<T: 'static>(chrome: SettingsChrome, cx: &mut Context<T>) -> Self {
        let developer_visible = crate::backend::settings::get_developer_mode();
        let location = persisted_location(developer_visible).with_default_subsection();
        let mut search = SettingsSearchState::new();
        search.sync("", developer_visible);

        Self {
            chrome,
            host_id: cx.entity_id(),
            location,
            focused: node_for_location(location),
            query: String::new(),
            search_active: false,
            search,
            search_focus: cx.focus_handle().tab_index(TAB_INDEX_SEARCH).tab_stop(true),
            search_input: None,
            _search_input_subscription: None,
            tree_focus: cx.focus_handle().tab_index(TAB_INDEX_TREE).tab_stop(true),
            content_focus: cx.focus_handle().tab_index(0).tab_stop(false),
            type_ahead: String::new(),
            type_ahead_at: None,
            page_scroll: HashMap::new(),
            page_ui: HashMap::new(),
            selected_setting: None,
            pending_setting: None,
            setting_jump_revision: 0,
            search_developer_revealed: false,
            search_restore: None,
            pending_anchor: None,
            anchor_reveal: None,
            pending_disclosure_handoff: None,
            uploader_inputs: None,
            login_modal: None,
            developer_override: None,
            status: None,
            status_revision: 0,
            _status_task: None,
            _diagnostics_refresh_task: None,
            last_rendered_section: None,
            focus_tree_on_open: true,
            focus_return: None,
        }
    }

    pub fn chrome(&self) -> SettingsChrome {
        self.chrome
    }

    pub fn location(&self) -> SettingsLocation {
        self.location
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn search_active(&self) -> bool {
        self.search_active
    }

    pub fn model(&self) -> &SettingsSearchModel {
        self.search.model()
    }

    /// Page-scoped disclosure state, shared by render callbacks until close.
    pub fn disclosure_state(
        &mut self,
        page: SettingsSectionId,
        id: impl Into<SharedString>,
        cx: &mut App,
    ) -> SettingsDisclosureState {
        self.page_ui
            .entry(canonical_section(page))
            .or_default()
            .disclosure(id, cx)
    }

    /// Attach the shared login surface. Idempotent: hosts call this once.
    pub fn attach_login_modal(&mut self, login_modal: &Entity<LoginModalState>) {
        if self.login_modal.is_none() {
            self.login_modal = Some(login_modal.clone());
        }
    }

    pub fn needs_login_modal(&self) -> bool {
        self.login_modal.is_none()
    }

    pub fn login_modal(&self) -> Option<&Entity<LoginModalState>> {
        self.login_modal.as_ref()
    }

    pub fn developer_visible(&self) -> bool {
        self.developer_override
            .unwrap_or_else(crate::backend::settings::get_developer_mode)
    }

    /// Snapshot of this surface for handing to another host.
    pub fn open_request(&self) -> SettingsOpenRequest {
        let mut request = SettingsOpenRequest::at(self.location);
        request.manual_disclosures = Some(
            self.page_ui
                .iter()
                .flat_map(|(page, ui)| {
                    ui.disclosures
                        .iter()
                        .filter(|(_, state)| state.is_manually_expanded())
                        .map(move |(id, _)| (*page, id.to_string()))
                })
                .collect(),
        );
        request.setting = self.selected_setting.map(str::to_string);
        request.developer_override = if self.search_developer_revealed {
            self.developer_override
        } else {
            None
        };
        if self.query.trim().is_empty() {
            request
        } else {
            request.with_query(self.query.clone())
        }
    }

    /// Move the content location.
    ///
    /// `persist` marks *explicit* navigation: it writes the location and the
    /// ancestor expansion it implies. Search-driven and reconciliation moves
    /// pass `false`, so a query never rewrites the user's remembered position.
    fn go_to(&mut self, location: SettingsLocation, persist: bool) -> bool {
        let reveal = disclosure_anchor(location, SettingsNavigationIntent::Browse, &self.query);
        let mut changed = self.anchor_reveal != reveal;
        self.anchor_reveal = reveal;
        self.selected_setting = None;
        self.pending_setting = None;
        self.setting_jump_revision = self.setting_jump_revision.wrapping_add(1);
        let location = self.resolve_target(location);

        if self.location != location {
            self.location = location;
            changed = true;
            if let Some(subsection) = location.subsection {
                self.pending_anchor = Some(subsection);
            }
        }

        let node = node_for_location(location);
        if self.focused != node {
            self.focused = node;
            changed = true;
        }

        if persist {
            self.persist_current_location();
        }

        changed
    }

    /// [`Self::go_to`] that always re-issues the anchor scroll, for clicks and
    /// Enter on a row that is already selected.
    fn jump_to(&mut self, location: SettingsLocation, persist: bool) -> bool {
        let changed = self.go_to(location, persist);
        if let Some(subsection) = self.location.subsection {
            self.pending_anchor = Some(subsection);
        }
        changed
    }

    /// Fill in the anchor a bare page selection should land on: the anchor the
    /// user last read on that page, else the page default.
    fn resolve_target(&self, location: SettingsLocation) -> SettingsLocation {
        let location = canonical_location(location);
        if location.subsection.is_some() {
            return location;
        }
        match self.remembered_anchor(location.section) {
            Some(subsection) => SettingsLocation::anchor(location.section, subsection),
            None => location.with_default_subsection(),
        }
    }

    /// Anchor implied by a page's retained scroll position.
    fn remembered_anchor(&self, section: SettingsSectionId) -> Option<SettingsSubsectionId> {
        let active = self.page_scroll.get(&section)?.active();
        section_descriptor(section)?
            .subsections
            .get(active)
            .map(|descriptor| descriptor.id)
    }

    /// Public navigation entry point for hosts and automation.
    pub fn navigate(&mut self, location: SettingsLocation) -> bool {
        self.jump_to(location, true)
    }

    pub fn navigate_setting(&mut self, id: &str) -> bool {
        let Some(target) = setting_target_by_id(id) else {
            return false;
        };
        if target.is_developer_only() && !self.developer_visible() {
            return false;
        }
        self.reset_search_disclosure_dismissals();
        let changed = self.go_to(target.location, false);
        self.selected_setting = Some(target.row_id());
        self.pending_setting = self.selected_setting;
        if !self.is_searching() {
            self.anchor_reveal = Some(target.location);
        }
        self.pending_anchor = None;
        self.page_scroll_mut(target.location.section)
            .cancel_pending();
        self.focused = if self.is_searching() {
            SettingsTreeNode::Setting(
                target.location.section,
                target.location.subsection,
                target.row_id(),
            )
        } else {
            SettingsTreeNode::Page(target.location.section)
        };
        changed || self.pending_setting.is_some()
    }

    /// Adopt a location without persisting it (state mirroring, harness writes).
    pub fn adopt_location(&mut self, location: SettingsLocation) -> bool {
        let changed = self.jump_to(location, false);
        if canonical_location(location) != location {
            self.anchor_reveal =
                disclosure_anchor(location, SettingsNavigationIntent::OpenRequest, &self.query);
        }
        changed
    }

    /// Apply a host open request: destination, transferred query, initial focus.
    pub fn apply_open_request(&mut self, request: &SettingsOpenRequest) -> bool {
        if let Some(visible) = request.developer_override
            && request
                .query
                .as_ref()
                .is_some_and(|query| !query.trim().is_empty())
        {
            self.developer_override = Some(visible);
            self.search_developer_revealed = true;
        }
        let developer_visible = self.developer_visible();
        let location = request.resolve(developer_visible);
        let query = request.query.clone().unwrap_or_default();
        // Adopt the handoff destination before the query begins. If the
        // transferred query is later dismissed with Escape, its restoration
        // belongs to the requested page rather than this host's old default.
        let mut changed = self.jump_to(location, false);
        changed |= self.set_query(&query);
        self.pending_disclosure_handoff = request.manual_disclosures.clone();
        self.anchor_reveal = request
            .location
            .and_then(|location| {
                disclosure_anchor(location, SettingsNavigationIntent::OpenRequest, &self.query)
            })
            .filter(|_| request.manual_disclosures.is_none());
        if let Some(id) = request.setting.as_deref() {
            changed |= self.navigate_setting(id);
        } else if self.is_searching() {
            if let Some(requested) = request.location {
                let scope = SettingsLocation {
                    section: location.section,
                    subsection: if requested.section == location.section {
                        requested.subsection
                    } else {
                        location.subsection
                    },
                };
                if let Some(target) = self.model().preferred_target(scope, None, true) {
                    changed |= self.navigate_setting(target.row_id());
                }
            } else {
                changed |= self.resync_selection();
            }
        }
        self.focus_tree_on_open = true;
        changed
    }

    /// Session teardown on close. Scroll positions stay: they are cheap, and a
    /// reopened surface should land where the user left it.
    pub fn reset_session(&mut self) -> bool {
        sections::release_general_settings_inputs_for_host(self.host_id);
        sections::release_proxy_inputs_for_host(self.host_id);
        sections::forget_logging_viewer_state_for_host(self.host_id);
        self._diagnostics_refresh_task = None;
        self.last_rendered_section = None;
        sections::release_command_editor_inputs_for_host(self.host_id);
        sections::forget_hotkey_editor_host(self.host_id);
        self.uploader_inputs = None;
        sections::clear_uploader_status(self.host_id.as_u64());
        self.anchor_reveal = None;
        self.pending_disclosure_handoff = None;
        let mut changed = self.set_query("");
        changed |= self.search_active || self.status.is_some();
        changed |= !self.page_ui.is_empty();
        self.page_ui.clear();
        self.selected_setting = None;
        self.pending_setting = None;
        self.setting_jump_revision = self.setting_jump_revision.wrapping_add(1);
        self.search_developer_revealed = false;
        self.search_active = false;
        self.developer_override = None;
        self.type_ahead.clear();
        self.type_ahead_at = None;
        self.pending_anchor = None;
        self.status = None;
        self.focus_tree_on_open = true;
        changed
    }

    /// Persist the current destination, e.g. when a host closes.
    pub fn persist_current_location(&self) -> bool {
        persist_location(persistence_location(
            self.location,
            crate::backend::settings::get_developer_mode(),
        ))
    }

    pub fn is_searching(&self) -> bool {
        self.search.model().is_searching()
    }

    pub fn set_query(&mut self, query: &str) -> bool {
        let query = normalize_search_query(query);
        if self.query == query {
            return false;
        }
        let was_searching = self.is_searching();
        let is_searching = !query.trim().is_empty();
        if !was_searching && is_searching {
            let offset = self
                .page_scroll
                .get(&self.location.section)
                .map(|scroll| scroll.handle().offset())
                .unwrap_or_else(|| point(px(0.0), px(0.0)));
            self.search_restore = Some(SearchRestore {
                location: self.location,
                offset,
            });
        }
        self.query = query;
        self.after_query_change();
        if !is_searching {
            // Clear, backspace, and accessibility SetValue are ordinary query
            // edits. They intentionally keep the current result context; only
            // Escape returns to the page and offset from before searching.
            self.search_restore = None;
        }
        true
    }

    fn clear_query(&mut self) -> bool {
        self.set_query("")
    }

    fn escape_clear_query_and_restore(&mut self) -> bool {
        if self.query.is_empty() {
            return false;
        }
        let restore = self.search_restore.take();
        self.query.clear();
        self.after_query_change();
        if let Some(restore) = restore {
            self.restore_search_location(restore);
        }
        true
    }

    fn restore_search_location(&mut self, restore: SearchRestore) {
        self.location = restore.location;
        self.focused = node_for_location(restore.location);
        self.pending_anchor = None;
        self.anchor_reveal = None;
        if let Some(scroll) = self.page_scroll.get(&restore.location.section) {
            scroll.handle().set_offset(restore.offset);
        }
    }

    fn after_query_change(&mut self) {
        self.setting_jump_revision = self.setting_jump_revision.wrapping_add(1);
        self.reset_search_disclosure_dismissals();
        // An explicit jump is a temporary reveal. A changed query establishes
        // a new search context, so it must not keep an unrelated disclosure
        // expanded beneath the results.
        self.anchor_reveal = None;
        if self.query.trim().is_empty() {
            self.selected_setting = None;
            self.pending_setting = None;
            if self.search_developer_revealed {
                self.developer_override = None;
                self.search_developer_revealed = false;
            }
        }
        let developer_visible = self.developer_visible();
        self.search.sync(&self.query, developer_visible);
        self.resync_selection();
        if !self.is_searching() {
            let rows = self.visible_rows();
            self.focused = rows
                .iter()
                .find(|row| row.selected)
                .or_else(|| rows.iter().rev().find(|row| row.on_path))
                .map_or_else(|| node_for_location(self.location), |row| row.node);
        }
    }

    fn reset_search_disclosure_dismissals(&self) {
        for ui in self.page_ui.values() {
            for state in ui.disclosures.values() {
                state.reset_search_dismissal();
            }
        }
    }

    /// Keep the selection inside the result set. Uses the *ranked* order, so an
    /// exact page-name hit wins over a looser keyword hit in an earlier
    /// category.
    fn resync_selection(&mut self) -> bool {
        if !self.is_searching() {
            return false;
        }
        let target = self
            .model()
            .preferred_target(self.location, self.selected_setting, false);
        if let Some(target) = target {
            return self.navigate_setting(target.row_id());
        }
        self.selected_setting = None;
        self.pending_setting = None;
        self.focused = node_for_location(self.location);
        if self.search.model().contains_location(self.location) {
            return false;
        }
        match first_ranked_location(self.search.model()) {
            Some(location) => self.go_to(location, false),
            None => false,
        }
    }

    /// Change diagnostics visibility without restoring or persisting a location.
    pub fn set_developer_visible(&mut self, visible: bool) -> bool {
        let leaving_diagnostics = !visible && self.location.is_developer_only();
        crate::backend::settings::set_developer_mode(visible);
        self.developer_override = None;
        self.search_developer_revealed = false;
        self.search.sync(&self.query, visible);
        self.resync_selection();
        if leaving_diagnostics {
            self.go_to(
                SettingsLocation::anchor(
                    SettingsSectionId::Advanced,
                    SettingsSubsectionId::DeveloperMode,
                ),
                false,
            );
        }
        true
    }

    /// Reveal the developer disclosure and land on the first hidden match, so
    /// the query the user already typed immediately answers their question.
    ///
    /// Search reveal is session-only and never changes developer_mode.
    fn reveal_hidden_developer(&mut self, _persist: bool) -> bool {
        let target = self.search.model().hidden_developer_location();
        self.developer_override = Some(true);
        self.search_developer_revealed = true;
        self.search.sync(&self.query, true);
        self.resync_selection();
        if self.selected_setting.is_some() {
            return true;
        }
        let location = target.or_else(|| first_ranked_location(self.search.model()));
        if let Some(location) = location {
            self.jump_to(location, false);
        }
        true
    }

    fn page_scroll_mut(&mut self, section: SettingsSectionId) -> &mut SettingsPageScroll {
        self.page_scroll.entry(section).or_default()
    }

    /// Adopt an anchor reported by scroll-spy. Updates the breadcrumb and the
    /// tree selection without stealing keyboard focus or announcing anything.
    fn adopt_scrolled_anchor(&mut self, subsection: SettingsSubsectionId) -> bool {
        if self.location.subsection == Some(subsection) {
            return false;
        }
        self.location.subsection = Some(subsection);
        true
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.search_active = true;
        if let Some(input) = self.search_input.as_ref() {
            let focus = input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        } else {
            // The first F6/Ctrl+F may arrive before the first render creates
            // the retained input. Render transfers this placeholder focus.
            window.focus(&self.search_focus, cx);
        }
        true
    }

    fn search_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window))
            || self.search_focus.is_focused(window)
    }

    pub fn focus_tree(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.search_active = false;
        window.focus(&self.tree_focus, cx);
        true
    }

    pub fn focus_content(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.search_active = false;
        window.focus(&self.content_focus, cx);
        true
    }

    /// Restore the focus target captured when a modal host opened.
    pub fn restore_focus(&mut self, window: &mut Window, cx: &mut App) {
        match self.focus_return.take().and_then(|handle| handle.upgrade()) {
            Some(handle) => window.focus(&handle, cx),
            None => window.blur(cx),
        }
    }

    /// F6 cycles Search -> Tree -> Content; Shift+F6 reverses.
    fn cycle_focus(&mut self, backwards: bool, window: &mut Window, cx: &mut App) -> bool {
        let on_search = self.search_is_focused(window, cx);
        let on_tree = self.tree_focus.is_focused(window);
        if backwards {
            if on_search {
                self.focus_content(window, cx)
            } else if on_tree {
                self.focus_search(window, cx)
            } else {
                self.focus_tree(window, cx)
            }
        } else if on_search {
            self.focus_tree(window, cx)
        } else if on_tree {
            self.focus_content(window, cx)
        } else {
            self.focus_search(window, cx)
        }
    }

    /// Visible rows for the current frame, in traversal order.
    pub fn visible_rows(&self) -> Vec<SettingsTreeRow> {
        visible_settings_rows(self.search.model(), self.location, self.selected_setting)
    }

    /// Focus a node, applying selection-follows-focus for content nodes.
    fn focus_node(&mut self, node: SettingsTreeNode) -> bool {
        let mut changed = self.focused != node;
        if let SettingsTreeNode::Setting(_, _, id) = node {
            changed |= self.navigate_setting(id);
        } else if let Some(location) = node.location() {
            changed |= self.go_to(location, true);
        }
        self.focused = node;
        changed
    }

    /// Enter/Space and pointer activation.
    fn activate_node(&mut self, node: SettingsTreeNode) -> bool {
        let mut changed = self.focused != node;
        match node {
            SettingsTreeNode::Category(_) => self.focused = node,
            SettingsTreeNode::Page(section) => {
                self.focused = node;
                changed |= self.jump_to(SettingsLocation::page(section), true);
            }
            SettingsTreeNode::Anchor(section, subsection) => {
                self.focused = node;
                let location = SettingsLocation::anchor(section, subsection);
                changed |= self.jump_to(location, true);
                let intent = if self.is_searching() {
                    SettingsNavigationIntent::Search
                } else {
                    SettingsNavigationIntent::Browse
                };
                let reveal = disclosure_anchor(location, intent, &self.query);
                changed |= self.anchor_reveal != reveal;
                self.anchor_reveal = reveal;
            }
            SettingsTreeNode::Setting(_, _, id) => changed |= self.navigate_setting(id),
        }
        self.focused = node;
        changed
    }

    /// Flat-list traversal, grouped search movement, activation and type-ahead.
    pub fn handle_tree_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut App,
    ) -> KeyOutcome {
        let modifiers = event.keystroke.modifiers;
        let shortcut = modifiers.control || modifiers.platform;
        let rows = self.visible_rows();
        if rows.is_empty() {
            return KeyOutcome::default();
        }
        let current = rows
            .iter()
            .position(|row| row.node == self.focused)
            .unwrap_or(0);

        let key = event.keystroke.key.as_str();
        match key {
            "down" if !shortcut => {
                let next = (current + 1).min(rows.len() - 1);
                KeyOutcome::handled(self.focus_node(rows[next].node))
            }
            "up" if !shortcut => {
                let previous = current.saturating_sub(1);
                KeyOutcome::handled(self.focus_node(rows[previous].node))
            }
            "home" if !shortcut => KeyOutcome::handled(self.focus_node(rows[0].node)),
            "end" if !shortcut => {
                let last = rows.len() - 1;
                KeyOutcome::handled(self.focus_node(rows[last].node))
            }
            "right" if !shortcut => {
                match rows
                    .get(current + 1)
                    .filter(|next| next.level > rows[current].level)
                {
                    Some(child) => KeyOutcome::handled(self.focus_node(child.node)),
                    None => KeyOutcome::consumed(),
                }
            }
            "left" if !shortcut => {
                match rows[..current]
                    .iter()
                    .rev()
                    .find(|candidate| candidate.level < rows[current].level)
                {
                    Some(parent) => KeyOutcome::handled(self.focus_node(parent.node)),
                    None => KeyOutcome::consumed(),
                }
            }
            "/" if !shortcut && !modifiers.alt => {
                KeyOutcome::handled(self.focus_search(window, cx))
            }
            _ if is_activation_key(event) => {
                KeyOutcome::handled(self.activate_node(rows[current].node))
            }
            _ if !shortcut && !modifiers.alt => {
                let Some(text) = event.keystroke.key_char.as_deref() else {
                    return KeyOutcome::default();
                };
                if text.trim().is_empty() {
                    return KeyOutcome::default();
                }
                KeyOutcome::handled(self.type_ahead(text, &rows, current))
            }
            _ => KeyOutcome::default(),
        }
    }

    fn type_ahead(&mut self, text: &str, rows: &[SettingsTreeRow], current: usize) -> bool {
        let now = Instant::now();
        let expired = self
            .type_ahead_at
            .is_none_or(|at| now.duration_since(at) > TYPE_AHEAD_WINDOW);
        if expired {
            self.type_ahead.clear();
        }
        self.type_ahead.push_str(&text.to_lowercase());
        self.type_ahead_at = Some(now);

        let needle = self.type_ahead.clone();
        let start = if needle.chars().count() <= 1 {
            current + 1
        } else {
            current
        };
        let count = rows.len();
        let hit = (0..count)
            .map(|offset| (start + offset) % count)
            .find(|index| {
                rows[*index]
                    .label
                    .to_lowercase()
                    .starts_with(needle.as_str())
            });

        match hit {
            Some(index) => self.focus_node(rows[index].node),
            None => false,
        }
    }

    #[doc(hidden)]
    pub fn benchmark_focus_search(&mut self) {
        self.search_active = true;
        self.developer_override = Some(false);
        self.search.sync(&self.query, false);
        if self.location.is_developer_only() {
            let location = restore_location(&navigation_prefs(), false);
            self.go_to(location, false);
        }
        self.resync_selection();
    }

    #[doc(hidden)]
    pub fn benchmark_focus_region(&self, window: &Window, cx: &App) -> &'static str {
        if self.search_is_focused(window, cx) {
            "search"
        } else if self.tree_focus.is_focused(window) {
            "navigation"
        } else if self.content_focus.contains_focused(window, cx) {
            "content"
        } else {
            "outside"
        }
    }

    #[doc(hidden)]
    pub fn benchmark_content_bounds(&self) -> Option<Bounds<Pixels>> {
        self.page_scroll
            .get(&self.location.section)
            .map(|scroll| scroll.handle().bounds())
    }

    #[doc(hidden)]
    pub fn benchmark_set_query(&mut self, query: &str) {
        self.set_query(query);
    }

    /// Run the production hidden-developer recovery without writing the user's
    /// developer-mode preference.
    #[doc(hidden)]
    pub fn benchmark_reveal_advanced(&mut self) {
        self.reveal_hidden_developer(false);
    }

    #[doc(hidden)]
    pub fn benchmark_developer_revealed(&self) -> bool {
        self.developer_override == Some(true)
    }

    #[doc(hidden)]
    pub fn benchmark_result_count(&self) -> usize {
        self.search.model().ordered_ids().len()
    }

    /// Press a key on the tree through the *production* key handler.
    ///
    /// The harness synthesizes the keystroke rather than the outcome, so the
    /// acceptance run exercises the same `handle_tree_key` the `on_key_down`
    /// listener calls — a regression in the key model fails the run instead of
    /// being papered over by a parallel test-only navigation path.
    #[doc(hidden)]
    pub fn benchmark_press_tree_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let event = KeyDownEvent {
            keystroke: Keystroke {
                modifiers: Modifiers::default(),
                key: key.to_string(),
                key_char: (key.chars().count() == 1).then(|| key.to_string()),
            },
            is_held: false,
            prefer_character_input: false,
        };
        self.handle_tree_key(&event, window, cx).handled
    }

    /// Current destination as the slug pair that is persisted and logged.
    #[doc(hidden)]
    pub fn benchmark_location_slugs(&self) -> (&'static str, Option<&'static str>) {
        (
            section_slug(self.location.section),
            self.location
                .subsection
                .and_then(|id| {
                    crate::components::settings::settings_navigation::subsection_descriptor(
                        self.location.section,
                        id,
                    )
                })
                .map(|descriptor| descriptor.slug),
        )
    }

    /// Visible tree rows flattened to `level:label` for log assertions.
    #[doc(hidden)]
    pub fn benchmark_tree_labels(&self) -> Vec<String> {
        self.visible_rows()
            .iter()
            .map(|row| format!("{}:{}", row.level, row.label))
            .collect()
    }

    /// Label of the row the tree keyboard model currently focuses.
    #[doc(hidden)]
    pub fn benchmark_focused_label(&self) -> Option<&'static str> {
        let focused = self.focused;
        self.visible_rows()
            .into_iter()
            .find(|row| row.node == focused)
            .map(|row| row.label)
    }

    /// Search-only secondary line on the row for `location`, e.g.
    /// `"Streamlink path · +3"`.
    #[doc(hidden)]
    pub fn benchmark_secondary_for(&self, location: SettingsLocation) -> Option<String> {
        let target = node_for_location(location);
        self.visible_rows()
            .into_iter()
            .find(|row| row.node == target)
            .and_then(|row| row.secondary)
    }

    /// Live-region text the status node would settle on, without waiting out the
    /// announcement delay.
    #[doc(hidden)]
    pub fn benchmark_status_announcement(&self) -> Option<String> {
        self.search.model().status_announcement()
    }

    /// `(settings, subsections, pages)` matching only behind the disclosure.
    #[doc(hidden)]
    pub fn benchmark_hidden_developer_counts(&self) -> (usize, usize, usize) {
        let counts = self.search.model().hidden_developer_counts();
        (counts.settings, counts.subsections, counts.pages)
    }

    /// Block index the current page's scroll-spy reports as active.
    ///
    /// This is the value a laid-out frame settles on, so comparing it with
    /// [`Self::benchmark_target_block`] after a render proves the anchor jump
    /// reached the block it addressed rather than merely updating the label.
    #[doc(hidden)]
    pub fn benchmark_active_block(&self) -> usize {
        self.page_scroll
            .get(&self.location.section)
            .map_or(0, SettingsPageScroll::active)
    }

    /// Block index the current destination *should* occupy, taken from the
    /// descriptor the tree and breadcrumb index against.
    #[doc(hidden)]
    pub fn benchmark_target_block(&self) -> usize {
        let Some(subsection) = self.location.subsection else {
            return 0;
        };
        section_descriptor(self.location.section)
            .and_then(|section| {
                section
                    .subsections
                    .iter()
                    .position(|descriptor| descriptor.id == subsection)
            })
            .unwrap_or(usize::MAX)
    }

    /// Whether an anchor jump is still waiting for a laid-out frame.
    #[doc(hidden)]
    pub fn benchmark_anchor_pending(&self) -> bool {
        self.pending_anchor.is_some()
    }

    #[doc(hidden)]
    pub fn benchmark_disclosure_expanded(&self, id: &str) -> bool {
        self.page_ui
            .get(&self.location.section)
            .and_then(|ui| ui.disclosures.get(id))
            .is_some_and(SettingsDisclosureState::is_expanded)
    }
}

/// Result of a key handler: whether it consumed the key, and whether visible
/// state changed (which is the only thing that justifies a notify).
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyOutcome {
    pub handled: bool,
    pub changed: bool,
}

impl KeyOutcome {
    fn handled(changed: bool) -> Self {
        Self {
            handled: true,
            changed,
        }
    }

    /// Consumed the key without changing anything (e.g. Left at the root).
    fn consumed() -> Self {
        Self {
            handled: true,
            changed: false,
        }
    }
}

/// First result in *rank* order, resolved to its first matching anchor.
///
/// [`SettingsSearchModel::first_location`] walks the tree in declaration order,
/// which would prefer an early category's loose keyword hit over an exact page
/// name match further down.
pub fn first_ranked_location(model: &SettingsSearchModel) -> Option<SettingsLocation> {
    let id = *model.ordered_ids().first()?;
    model
        .tree()
        .iter()
        .flat_map(|category| category.sections.iter())
        .find(|section| section.descriptor.id == id)
        .map(|section| section.location())
        .or_else(|| Some(SettingsLocation::page(id)))
}

/// Render the whole surface for a host.
///
/// Order matters: the content pane runs scroll-spy while it builds, so the tree
/// and the breadcrumb are constructed afterwards and read the settled location
/// in the same frame instead of trailing it by one.
pub fn render_settings_surface<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    prepare_frame(host, window, cx);

    let content = render_content(host, window, cx);
    let tree = render_tree(host, window, cx);
    let top_bar = render_top_bar(host, window, cx);

    div()
        .id("settings-surface")
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .capture_action(
            cx.listener(|host, _: &crate::components::ui::FocusNext, window, cx| {
                capture_settings_tab(host, false, window, cx);
            }),
        )
        .capture_action(
            cx.listener(|host, _: &crate::components::ui::FocusPrev, window, cx| {
                capture_settings_tab(host, true, window, cx);
            }),
        )
        .capture_key_down(
            cx.listener(|host, event: &KeyDownEvent, window, cx: &mut Context<T>| {
                crate::components::ui::accessibility::clear_accessible_focus();
                // NativeWindowRoot has no EchoRoot Tab binding. Capture raw
                // Tab before a menu's bubble handler can consume it.
                if let Some(shift) = settings_tab_direction(event) {
                    capture_settings_tab(host, shift, window, cx);
                    return;
                }
                if !sections::settings_hotkey_capture_owned_by(host.surface().host_id) {
                    return;
                }
                if crate::components::ui::input::is_input_focused(window) {
                    sections::cancel_settings_hotkey_capture(host.surface().host_id);
                    cx.notify();
                    return;
                }
                if let Some(binding) = crate::backend::hotkeys::binding_from_event(event) {
                    if event.keystroke.key == "escape" {
                        sections::clear_capture_rejection();
                        sections::cancel_settings_hotkey_capture(host.surface().host_id);
                    } else if !sections::reject_plain_typing_capture(
                        &binding,
                        crate::event_handlers::is_typed_text(event),
                    ) {
                        sections::complete_settings_hotkey_capture(binding);
                    }
                }
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_key_down(
            cx.listener(|host, event: &KeyDownEvent, window, cx: &mut Context<T>| {
                handle_shell_key(host, event, window, cx);
            }),
        )
        .child(top_bar)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .overflow_hidden()
                .child(tree)
                .child(div().w(px(1.0)).h_full().bg(colors::border()))
                .child(content),
        )
        .into_any_element()
}

fn settings_tab_direction(event: &KeyDownEvent) -> Option<bool> {
    let modifiers = event.keystroke.modifiers;
    (event.keystroke.key == "tab"
        && !(modifiers.control || modifiers.alt || modifiers.platform || modifiers.function))
        .then_some(modifiers.shift)
}

fn capture_settings_tab<T: SettingsSurfaceHost>(
    host: &mut T,
    shift: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    crate::components::ui::accessibility::clear_accessible_focus();
    close_settings_selects(host.surface(), window, cx);
    if sections::settings_hotkey_capture_owned_by(host.surface().host_id)
        && crate::components::ui::input::is_input_focused(window)
    {
        sections::cancel_settings_hotkey_capture(host.surface().host_id);
    } else if sections::settings_hotkey_capture_owned_by(host.surface().host_id) {
        sections::clear_capture_rejection();
        let binding = crate::backend::hotkeys::HotkeyBinding {
            key: "tab".to_string(),
            ctrl: false,
            alt: false,
            shift,
        };
        if !sections::reject_plain_typing_capture(&binding, false) {
            sections::complete_settings_hotkey_capture(binding);
        }
        cx.stop_propagation();
        cx.notify();
        return;
    }

    if shift {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
    cx.stop_propagation();
    cx.notify();
}

fn close_settings_selects(surface: &SettingsSurface, window: &mut Window, cx: &mut App) {
    if let Some(inputs) = &surface.uploader_inputs {
        inputs.close_provider_menu(window);
    }
    for ui in surface.page_ui.values() {
        ui.close_selects(window, cx);
    }
}

/// Per-frame bookkeeping: search revision, disclosure coercion, initial focus,
/// and the revision-fenced status announcement.
fn prepare_frame<T: SettingsSurfaceHost>(host: &mut T, window: &mut Window, cx: &mut Context<T>) {
    if let Some(expanded) = host.surface_mut().pending_disclosure_handoff.take() {
        for ui in host.surface().page_ui.values() {
            for state in ui.disclosures.values() {
                state.reveal_for_search(false, window, cx);
                state.set_expanded(false, window, cx);
            }
        }
        for (page, id) in expanded {
            host.surface_mut()
                .disclosure_state(page, id, cx)
                .set_expanded(true, window, cx);
        }
    }
    let developer_visible = host.surface().developer_visible();
    let query = host.surface().query.clone();
    if host.surface_mut().search.sync(&query, developer_visible) {
        host.surface_mut().resync_selection();
    }

    if !host.surface().location.is_visible(developer_visible) {
        let location = coerce_visible_location(
            host.surface().location,
            &navigation_prefs(),
            developer_visible,
        );
        host.surface_mut().go_to(location, false);
    }

    let selected = host
        .surface()
        .selected_setting
        .and_then(setting_target_by_id);
    let required = selected
        .map(|target| (target.location.section, target.setting.disclosures))
        .or_else(|| {
            host.surface().anchor_reveal.map(|location| {
                (
                    canonical_location(location).section,
                    anchor_disclosures_for_query(location, host.surface().query()),
                )
            })
        });
    let temporary_reveal = host.surface().is_searching() || host.surface().anchor_reveal.is_some();
    if host.surface().location.section == SettingsSectionId::Theme {
        let theme = crate::backend::settings::get_theme();
        let has_details = theme.as_deref().is_some_and(|theme| {
            ["Custom", "System", "Theme File"]
                .iter()
                .any(|name| theme.eq_ignore_ascii_case(name))
        });
        let searched = host.surface().is_searching()
            && required.is_some_and(|(page, chain)| {
                page == SettingsSectionId::Theme && chain.contains(&"appearance-theme-details")
            });
        if !has_details && !searched {
            let focused = host
                .surface()
                .page_ui
                .get(&SettingsSectionId::Theme)
                .and_then(|ui| ui.disclosures.get("appearance-theme-details"))
                .is_some_and(|state| state.contains_focus(window, cx));
            if focused && let Some(target) = setting_target_by_id("appearance.color-theme") {
                host.surface_mut()
                    .page_ui
                    .entry(SettingsSectionId::Theme)
                    .or_default()
                    .select(target.setting.control_id, cx)
                    .focus(window, cx);
            }
        }
    }
    for (page, ui) in &host.surface().page_ui {
        for (id, state) in &ui.disclosures {
            let reveal = temporary_reveal
                && required.is_some_and(|(section, chain)| {
                    section == *page && chain.contains(&id.as_ref())
                });
            state.reveal_for_search(reveal, window, cx);
        }
    }
    if temporary_reveal && let Some((page, chain)) = required {
        for id in chain {
            let state = host.surface_mut().disclosure_state(page, *id, cx);
            state.reveal_for_search(true, window, cx);
        }
    }
    if host.surface().focus_tree_on_open {
        host.surface_mut().focus_tree_on_open = false;
        let tree_focus = host.surface().tree_focus.clone();
        host.surface_mut().focus_return = window
            .focused(cx)
            .filter(|focused| focused != &tree_focus)
            .map(|focused| focused.downgrade());
        window.focus(&tree_focus, cx);
    }

    let revision = host.surface().search.revision();
    if host.surface().status_revision != revision {
        host.surface_mut().status_revision = revision;
        host.surface_mut().status = None;
        let announcement = host.surface().search.model().status_announcement();
        let task = cx.spawn(async move |this: WeakEntity<T>, cx| {
            cx.background_executor().timer(STATUS_SETTLE).await;
            let _ = this.update(cx, |host, cx| {
                let surface = host.surface_mut();
                if surface.search.revision() != revision {
                    return;
                }
                let next = announcement.map(SharedString::from);
                if surface.status != next {
                    surface.status = next;
                    cx.notify();
                }
            });
        });
        host.surface_mut()._status_task = Some(task);
    }
}

pub(crate) fn handle_shell_key<T: SettingsSurfaceHost>(
    host: &mut T,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let modifiers = event.keystroke.modifiers;
    let shortcut = (modifiers.control || modifiers.platform) && !modifiers.alt;

    match event.keystroke.key.as_str() {
        "f6" => {
            host.surface_mut().cycle_focus(modifiers.shift, window, cx);
            cx.notify();
            cx.stop_propagation();
        }
        "f" if shortcut => {
            host.surface_mut().focus_search(window, cx);
            cx.notify();
            cx.stop_propagation();
        }
        "tab" => {
            close_settings_selects(host.surface(), window, cx);
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
        }
        "escape" => {
            if host.surface().query().trim().is_empty() {
                host.request_close(window, cx);
            } else {
                host.surface_mut().escape_clear_query_and_restore();
                cx.notify();
            }
            cx.stop_propagation();
        }
        _ => {}
    }
}

fn render_top_bar<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let chrome = host.surface().chrome;
    let status = host.surface().status.clone();
    let title_width =
        settings_sidebar_width(chrome, window) - SettingsMetrics::CONTENT_PADDING * 2.0;

    div()
        .w_full()
        .flex_shrink_0()
        .h(px(SettingsMetrics::TOP_BAR_HEIGHT))
        .px(px(SettingsMetrics::CONTENT_PADDING))
        .border_b_1()
        .border_color(colors::border())
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(title_width))
                .flex_shrink_0()
                .text_size(px(SettingsMetrics::TITLE_TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child("Settings"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(render_search(host, window, cx)),
        )
        .when_some(status, |bar, status| {
            bar.child(
                div()
                    .id("settings-search-status")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .flex_shrink_0()
                    .max_w(px(220.0))
                    .text_size(px(SettingsMetrics::HELPER_TEXT))
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(colors::text_secondary())
                    .child(status),
            )
        })
        .when(chrome.shows_window_actions(), |bar| {
            bar.child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .child(render_popout_button(cx))
                    .child(render_close_button(cx)),
            )
        })
        .into_any_element()
}

fn render_popout_button<T: SettingsSurfaceHost>(cx: &mut Context<T>) -> AnyElement {
    let surface = colors::bg_elevated();
    div()
        .id("popout-settings")
        .role(Role::Button)
        .map(|element| {
            crate::components::ui::accessibility::accessible_focus(
                element,
                super::widgets::control_focus("popout-settings", cx).tab_index(TAB_INDEX_POPOUT),
            )
        })
        .aria_label("Open settings in a window")
        .tab_index(TAB_INDEX_POPOUT)
        .h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
        .px_2()
        .rounded(Radii::sm())
        .cursor_pointer()
        .hover(|style| style.bg(colors::bg_hover()))
        .focus_visible(move |style| style.bg(surface).shadow(focus_ring(surface)))
        .flex()
        .items_center()
        .justify_center()
        .on_key_down(cx.listener(|host, event: &KeyDownEvent, window, cx| {
            if !is_activation_key(event) {
                return;
            }
            let request = host.surface().open_request();
            host.request_popout(request, window, cx);
            cx.stop_propagation();
        }))
        .on_press_click(cx.listener(|host, _, window, cx| {
            let request = host.surface().open_request();
            host.request_popout(request, window, cx);
            cx.stop_propagation();
        }))
        .on_a11y_action(accesskit::Action::Click, {
            let entity = cx.entity();
            move |_, window, cx| {
                entity.update(cx, |host, cx| {
                    let request = host.surface().open_request();
                    host.request_popout(request, window, cx);
                })
            }
        })
        .child(
            div()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child("Open in window"),
        )
        .into_any_element()
}

fn render_close_button<T: SettingsSurfaceHost>(cx: &mut Context<T>) -> AnyElement {
    let surface = colors::bg_elevated();
    div()
        .id("close-settings")
        .role(Role::Button)
        .map(|element| {
            crate::components::ui::accessibility::accessible_focus(
                element,
                super::widgets::control_focus("close-settings", cx).tab_index(TAB_INDEX_CLOSE),
            )
        })
        .aria_label("Close settings")
        .tab_index(TAB_INDEX_CLOSE)
        .size(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
        .rounded(Radii::sm())
        .cursor_pointer()
        .hover(|style| style.bg(colors::bg_hover()))
        .focus_visible(move |style| style.bg(surface).shadow(focus_ring(surface)))
        .flex()
        .items_center()
        .justify_center()
        .on_key_down(cx.listener(|host, event: &KeyDownEvent, window, cx| {
            if !is_activation_key(event) {
                return;
            }
            host.request_close(window, cx);
            cx.stop_propagation();
        }))
        .on_press_click(cx.listener(|host, _, window, cx| {
            host.request_close(window, cx);
            cx.stop_propagation();
        }))
        .on_a11y_action(accesskit::Action::Click, {
            let entity = cx.entity();
            move |_, window, cx| entity.update(cx, |host, cx| host.request_close(window, cx))
        })
        .child(
            div()
                .text_size(px(SettingsMetrics::LABEL_TEXT))
                .text_color(colors::text_primary())
                .child("\u{2715}"),
        )
        .into_any_element()
}

/// Shared search field.
///
/// Retain one real shared input per host. This keeps keyboard, IME, paste, and
/// UI Automation edits on InputState's normalized change path instead of
/// maintaining a second painted-field implementation for search.
fn ensure_search_input<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<InputState> {
    if let Some(input) = host.surface().search_input.clone() {
        let query = host.surface().query.clone();
        if input.read(cx).value() != query.as_str() {
            input.update(cx, |input, cx| input.set_value(query, window, cx));
        }
        return input;
    }

    let query = host.surface().query.clone();
    let transfer_placeholder_focus = host.surface().search_focus.is_focused(window);
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .with_placeholder("Search every setting")
            .with_value(query)
    });
    input
        .read(cx)
        .focus_handle()
        .clone()
        .tab_index(TAB_INDEX_SEARCH)
        .tab_stop(true);
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |host, input, event: &InputEvent, window, cx| match event {
            InputEvent::Focus => {
                host.surface_mut().search_active = true;
                cx.notify();
            }
            InputEvent::Blur => {
                host.surface_mut().search_active = false;
                cx.notify();
            }
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                let changed = host.surface_mut().set_query(&value);
                let normalized = host.surface().query.clone();
                if value != normalized {
                    input.update(cx, |input, cx| input.set_value(normalized, window, cx));
                }
                if changed {
                    cx.notify();
                }
            }
            InputEvent::PressEnter => {
                let selected = host.surface().selected_setting;
                let changed = match selected {
                    Some(id) => host.surface_mut().navigate_setting(id),
                    None => {
                        let location = host.surface().location;
                        host.surface_mut().jump_to(location, false)
                    }
                };
                let focused = host.surface_mut().focus_tree(window, cx);
                if changed || focused {
                    cx.notify();
                }
            }
        },
    );
    host.surface_mut().search_input = Some(input.clone());
    host.surface_mut()._search_input_subscription = Some(subscription);
    if transfer_placeholder_focus {
        let focus = input.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    }
    input
}

/// Presentation follows *real* input focus, so a control in the content pane
/// taking focus stops the search field claiming keyboard ownership.
fn render_search<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let input = ensure_search_input(host, window, cx);
    let focus = input.read(cx).focus_handle().clone();
    let is_active = focus.is_focused(window);
    let surface = colors::bg_surface();
    let query = host.surface().query.clone();

    div()
        .id("settings-search")
        .w_full()
        .h(px(SettingsMetrics::SEARCH_FIELD_HEIGHT))
        .px_2()
        .rounded(Radii::md())
        .bg(surface)
        .border_1()
        .border_color(if is_active {
            colors::accent()
        } else {
            colors::border()
        })
        .focus_visible(move |style| style.shadow(focus_ring(surface)))
        .cursor_text()
        .flex()
        .items_center()
        .gap_1()
        // Search owns Escape only while its shared InputState is focused.
        // Other editor inputs keep their own draft-revert capture paths.
        .capture_action(cx.listener(|host, _: &Escape, window, cx| {
            if host.surface().query().trim().is_empty() {
                host.request_close(window, cx);
            } else {
                host.surface_mut().escape_clear_query_and_restore();
                cx.notify();
            }
            cx.stop_propagation();
        }))
        .on_key_down(cx.listener(|host, event: &KeyDownEvent, window, cx| {
            let modifiers = event.keystroke.modifiers;
            if event.keystroke.key == "down"
                && !modifiers.control
                && !modifiers.platform
                && !modifiers.alt
                && !modifiers.shift
            {
                host.surface_mut().focus_tree(window, cx);
                cx.notify();
                cx.stop_propagation();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|host, _, window, cx| {
                host.surface_mut().focus_search(window, cx);
                cx.notify();
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_secondary())
                .child("SEARCH"),
        )
        .child(
            div().min_w_0().flex_1().child(
                Input::new(&input)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .accessibility_search(true)
                    .accessibility_label("Search settings"),
            ),
        )
        .when(!query.is_empty(), |field| {
            field.child(
                div()
                    .id("settings-search-clear")
                    .role(Role::Button)
                    .map(|control| super::widgets::settings_keyboard_control(control, cx))
                    .aria_label("Clear search")
                    .h(px(22.0))
                    .px_2()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(SettingsMetrics::HELPER_TEXT))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::interactive_foreground())
                    .hover(|style| {
                        style
                            .bg(colors::bg_hover())
                            .text_color(colors::text_primary())
                    })
                    .on_accessible_activate(cx.listener(|host, _, window, cx| {
                        host.surface_mut().clear_query();
                        host.surface_mut().focus_search(window, cx);
                        cx.notify();
                        cx.stop_propagation();
                    }))
                    .focus_visible(move |style| style.bg(surface).shadow(focus_ring(surface)))
                    .child("Clear"),
            )
        })
        .into_any_element()
}

fn render_tree<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let rows = host.surface().visible_rows();
    let focused = host.surface().focused;
    let focus = host.surface().tree_focus.clone();
    let is_focused = focus.is_focused(window);
    let query = host.surface().query.clone();
    let searching = host.surface().is_searching();
    let sidebar_width = settings_sidebar_width(host.surface().chrome, window);
    let mut regular = Vec::new();
    let mut advanced = None;
    for row in &rows {
        let element = render_tree_row(
            row,
            is_focused && row.node == focused,
            row.node == focused,
            &query,
            cx,
        );
        if !searching && row.node == SettingsTreeNode::Page(SettingsSectionId::Advanced) {
            advanced = Some(element);
        } else {
            regular.push(element);
        }
    }
    div()
        .id("settings-sidebar")
        .w(px(sidebar_width))
        .flex_shrink_0()
        .h_full()
        .bg(colors::bg_surface())
        .child(
            div()
                .id("settings-tree")
                .role(if searching { Role::Tree } else { Role::ListBox })
                .aria_label("Settings navigation")
                .aria_orientation(Orientation::Vertical)
                .track_focus(&focus)
                .size_full()
                .flex()
                .flex_col()
                .on_key_down(cx.listener(|host, event: &KeyDownEvent, window, cx| {
                    let outcome = host.surface_mut().handle_tree_key(event, window, cx);
                    if outcome.changed {
                        cx.notify();
                    }
                    if outcome.handled {
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .id("settings-navigation-pages")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_1()
                        .py_1()
                        .when(regular.is_empty(), |list| {
                            list.child(
                                div()
                                    .px_2()
                                    .py_4()
                                    .text_size(px(SettingsMetrics::LABEL_TEXT))
                                    .text_color(colors::text_secondary())
                                    .child("No matching settings"),
                            )
                        })
                        .children(regular),
                )
                .when_some(advanced, |list, advanced| {
                    list.child(
                        div()
                            .px_1()
                            .py_2()
                            .border_t_1()
                            .border_color(colors::border())
                            .child(advanced),
                    )
                }),
        )
        .into_any_element()
}

fn render_tree_row<T: SettingsSurfaceHost>(
    row: &SettingsTreeRow,
    show_focus_ring: bool,
    is_active_descendant: bool,
    query: &str,
    cx: &mut Context<T>,
) -> AnyElement {
    let node = row.node;
    let is_selected = row.selected;
    let emphasized = row.selected || row.on_path;
    let surface = colors::bg_surface();
    let selected_fill = colors::settings_selection_fill(surface);
    let foreground = if is_selected {
        colors::settings_selection_foreground(surface)
    } else {
        colors::text_primary()
    };
    let rail = colors::settings_selection_rail(surface);
    let indent = 4.0 + row.level as f32 * SettingsMetrics::TREE_INDENT;
    let icon_color = if is_selected {
        foreground
    } else {
        colors::interactive_foreground()
    };
    let ring = focus_ring(surface);

    let mut label_column = div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(Spacing::HAIRLINE))
        .child(
            div()
                .min_w_0()
                .text_size(px(SettingsMetrics::LABEL_TEXT))
                .when(emphasized, |label| label.font_weight(FontWeight::MEDIUM))
                .text_color(foreground)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(highlighted_label(row.label, query)),
        );
    if let Some(secondary) = row.secondary.clone() {
        label_column = label_column.child(
            div()
                .min_w_0()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(colors::text_secondary())
                .child(secondary),
        );
    }

    div()
        .id(SharedString::from(format!(
            "settings-tree-{}",
            node.element_fragment()
        )))
        .role(if query.trim().is_empty() {
            Role::ListBoxOption
        } else {
            Role::TreeItem
        })
        .aria_label(SharedString::new_static(row.label))
        .when(!query.trim().is_empty(), |item| {
            item.aria_level(row.level + 1)
        })
        .aria_position_in_set(row.position)
        .aria_size_of_set(row.set_size)
        .aria_selected(row.selected)
        .when(is_active_descendant, |item| item.aria_active_descendant())
        .w_full()
        .min_h(px(SettingsMetrics::TREE_ROW_HEIGHT))
        .pr_1()
        .rounded(Radii::sm())
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(Spacing::XS))
        .border_l(px(2.0))
        .border_color(if is_selected {
            rail
        } else {
            transparent_black()
        })
        .when(is_selected, |item| item.bg(selected_fill))
        .when(show_focus_ring, |item| item.shadow(ring.clone()))
        .hover(move |style| {
            if is_selected {
                style.bg(selected_fill)
            } else {
                style.bg(colors::bg_hover())
            }
        })
        .on_completed_click(cx.listener(move |host, _, window, cx| {
            host.surface_mut().focus_tree(window, cx);
            if host.surface_mut().activate_node(node) {
                cx.notify();
            }
            cx.stop_propagation();
        }))
        .child(div().flex_shrink_0().w(px(indent)))
        .when_some(row.icon, |item, icon| {
            item.child(
                svg()
                    .path(icon)
                    .size(px(14.0))
                    .flex_shrink_0()
                    .text_color(icon_color),
            )
        })
        .child(label_column)
        .when_some(row.badge, |item, count| {
            item.child(
                div()
                    .flex_shrink_0()
                    .px(px(Spacing::XS))
                    .rounded(Radii::sm())
                    .bg(colors::bg_hover())
                    .text_size(px(SettingsMetrics::HELPER_TEXT))
                    .text_color(colors::text_secondary())
                    .child(count.to_string()),
            )
        })
        .into_any_element()
}

/// Label with the matched substring emphasized. Plain text when not searching,
/// so ordinary browsing keeps the cheaper element.
fn highlighted_label(label: &'static str, query: &str) -> AnyElement {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return SharedString::new_static(label).into_any_element();
    }
    let Some(start) = label.to_lowercase().find(&needle) else {
        return SharedString::new_static(label).into_any_element();
    };
    let end = start + needle.len();

    StyledText::new(label.to_string())
        .with_highlights([(
            start..end,
            HighlightStyle {
                color: Some(colors::text_primary()),
                font_weight: Some(FontWeight::SEMIBOLD),
                ..Default::default()
            },
        )])
        .into_any_element()
}

fn render_content<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let searching = host.surface().search.model().is_searching();
    let has_matches = host.surface().search.model().has_matches();
    let hidden_developer = host.surface().search.model().has_hidden_developer_matches();
    let query = host.surface().query.clone();

    let (header, body) = if searching && !has_matches && hidden_developer {
        (
            content_header(no_results_title("Developer settings are hidden")).into_any_element(),
            render_hidden_developer_state(host, cx),
        )
    } else if searching && !has_matches {
        (
            content_header(no_results_title("No results")).into_any_element(),
            render_no_results(&query),
        )
    } else {
        let scroll = render_page(host, window, cx);
        (
            content_header(breadcrumb(host.surface().location)).into_any_element(),
            scroll,
        )
    };

    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|host, _, _window, cx| {
                if host.surface().search_active {
                    host.surface_mut().search_active = false;
                    cx.notify();
                }
            }),
        )
        .child(header)
        .child(body)
        .into_any_element()
}

fn render_diagnostics_gate<T: SettingsSurfaceHost>(
    stacked: bool,
    cx: &mut Context<T>,
) -> AnyElement {
    let descriptor = sections::ADVANCED_DEVELOPER_MODE_SETTINGS[0];
    let enabled = crate::backend::settings::get_developer_mode();
    let defaults = super::widgets::settings_defaults();
    let reset_host = cx.entity().downgrade();
    let toggle_host = reset_host.clone();
    let reset = (enabled != defaults.developer_mode).then(|| {
        super::widgets::setting_reset(
            "advanced-reset-developer-mode",
            descriptor.label,
            cx,
            move |cx| {
                let host = reset_host.clone();
                cx.defer(move |app| {
                    if let Some(host) = host.upgrade() {
                        host.update(app, |host, cx| {
                            host.surface_mut().set_developer_visible(
                                super::widgets::settings_defaults().developer_mode,
                            );
                            cx.notify();
                        });
                    }
                });
            },
        )
    });
    let control = super::widgets::labelled_toggle(
        descriptor.control_id,
        descriptor.label,
        enabled,
        cx,
        move |enabled, cx| {
            let host = toggle_host.clone();
            cx.defer(move |app| {
                if let Some(host) = host.upgrade() {
                    host.update(app, |host, cx| {
                        host.surface_mut().set_developer_visible(enabled);
                        cx.notify();
                    });
                }
            });
        },
    );
    super::widgets::settings_group("advanced-developer-access", "Developer mode", false,
        super::widgets::setting_target(descriptor, super::widgets::setting_row(descriptor.id, descriptor.label,
            Some("Show live diagnostics and metrics. Ordinary Advanced operations remain available."),
            control, enabled != defaults.developer_mode, reset, stacked))).into_any_element()
}

/// Build the current page, run the anchor jump / scroll-spy contract, and return
/// the scrolling element.
fn diagnostics_refresh_requested(surface: &SettingsSurface) -> bool {
    surface.location.section == SettingsSectionId::Advanced
        && surface.developer_visible()
        && surface
            .page_ui
            .get(&SettingsSectionId::Advanced)
            .and_then(|ui| ui.disclosures.get("advanced-diagnostics"))
            .is_some_and(SettingsDisclosureState::is_expanded)
}

fn sync_diagnostics_refresh<T: SettingsSurfaceHost>(host: &mut T, cx: &mut Context<T>) {
    if !diagnostics_refresh_requested(host.surface()) {
        host.surface_mut()._diagnostics_refresh_task = None;
    } else if host.surface()._diagnostics_refresh_task.is_none() {
        host.surface_mut()._diagnostics_refresh_task = Some(cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_refreshing = weak
                    .update(cx, |host, cx| {
                        let requested = diagnostics_refresh_requested(host.surface());
                        if requested {
                            cx.notify();
                        }
                        requested
                    })
                    .unwrap_or(false);
                if !keep_refreshing {
                    break;
                }
            }
        }));
    }
}

fn render_page<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let section = host.surface().location.section;
    let previous_section = host.surface_mut().last_rendered_section.replace(section);
    if previous_section == Some(SettingsSectionId::Advanced)
        && section != SettingsSectionId::Advanced
    {
        let host_id = host.surface().host_id;
        sections::release_proxy_inputs_for_host(host_id);
        sections::release_logging_inputs_for_host(host_id);
    }
    if section != SettingsSectionId::Hotkeys {
        let host_id = host.surface().host_id;
        sections::release_command_editor_inputs_for_host(host_id);
        sections::release_hotkey_editor_inputs_for_host(host_id);
    }
    let login_modal = host.surface().login_modal.clone();
    let uploader_inputs = if section == SettingsSectionId::Connections {
        Some(ensure_uploader_inputs(host, window, cx))
    } else {
        let host_id = host.surface().host_id;
        host.surface_mut().uploader_inputs = None;
        sections::release_general_settings_inputs_for_host(host_id);
        None
    };
    let handle = host.surface_mut().page_scroll_mut(section).handle().clone();
    let pending_setting = host.surface().pending_setting;
    let selected_setting = host.surface().selected_setting;
    let mut matched: Vec<_> = host
        .surface()
        .model()
        .targets()
        .iter()
        .filter(|target| target.location.section == section)
        .map(|target| target.row_id())
        .collect();
    if let Some(id) = selected_setting
        && !matched.contains(&id)
    {
        matched.push(id);
    }
    let capture = (host.surface().is_searching() || selected_setting.is_some()).then(|| {
        super::widgets::SettingTargetCapture::new(handle.clone(), matched.clone(), selected_setting)
    });
    super::widgets::set_search_match_labels(search_match_labels(host.surface(), section));
    let developer_visible = host.surface().developer_visible();
    let compact = settings_content_width(host.surface().chrome(), window)
        <= SETTINGS_STACKED_CONTENT_MAX_WIDTH_PX;
    let searching = host.surface().is_searching();
    let ui = host.surface_mut().page_ui.entry(section).or_default();
    ui.stacked = compact;
    ui.searching = searching;
    ui.selected_setting = selected_setting;
    ui.matched_settings = if searching { matched } else { Vec::new() };
    let last_control_focus = ui.last_control_focus.clone();
    let mut page = sections::build_settings_page(
        section,
        login_modal,
        uploader_inputs.as_ref(),
        developer_visible,
        ui,
        window,
        cx,
    );
    if section == SettingsSectionId::Advanced {
        page.replace_subsection_body(
            SettingsSubsectionId::DeveloperMode,
            render_diagnostics_gate(compact, cx),
        );
    }
    sync_diagnostics_refresh(host, cx);
    super::widgets::set_search_match_labels(Vec::new());
    page.debug_assert_matches_descriptor();

    if (host.surface().is_searching() || selected_setting.is_some())
        && let Some(id) = selected_setting
        && let Some(capture) = &capture
        && capture.anchor(id).is_none()
        && let Some(target) = setting_target_by_id(id)
    {
        page.append_search_notice(
            target.location,
            super::widgets::setting_target(
                target.setting,
                div()
                    .id("unavailable-setting")
                    .role(Role::Status)
                    .p(px(Spacing::MD))
                    .text_color(colors::text_secondary())
                    .child(format!(
                        "{} is unavailable with your current settings.",
                        target.setting.label
                    )),
            ),
        );
    }

    if let Some(id) = pending_setting
        && let Some(anchor) = capture.as_ref().and_then(|capture| capture.anchor(id))
    {
        host.surface_mut().pending_setting = None;
        let revision = host.surface().setting_jump_revision;
        let entity = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = entity.update(cx, |host, cx| {
                let surface = host.surface();
                if surface.setting_jump_revision == revision
                    && surface.selected_setting == Some(id)
                    && surface.location.section == section
                    && anchor.scroll_to(window)
                {
                    cx.notify();
                }
            });
        });
    }

    let block_count = page.block_count();
    let pending = host.surface().pending_anchor;
    let target = pending.and_then(|anchor| {
        presentation_location(SettingsLocation::anchor(section, anchor))
            .subsection
            .and_then(|anchor| page.index_of(anchor))
    });
    if pending.is_some() {
        host.surface_mut().pending_anchor = None;
    }

    let scroll = host.surface_mut().page_scroll_mut(section);
    let spy_changed = match target {
        Some(index) => {
            scroll.scroll_to(index);
            false
        }
        None => scroll.observe(block_count),
    };
    let active = scroll.active();
    let handle = scroll.handle().clone();

    if spy_changed && let Some(subsection) = page.subsection_at(active) {
        host.surface_mut().adopt_scrolled_anchor(subsection);
    }

    let content_focus = host.surface().content_focus.clone();
    let content_is_focused = content_focus.is_focused(window) && window.last_input_was_keyboard();
    let content_surface = colors::bg_surface();

    let body = div()
        .id("settings-content")
        .capture_any_mouse_down(|_, _, _| {
            crate::components::ui::accessibility::clear_accessible_focus();
        })
        .role(Role::Group)
        .aria_label("Settings content")
        .track_focus(&content_focus)
        .tab_index(TAB_INDEX_CONTENT)
        .tab_group()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .p(px(SettingsMetrics::CONTENT_PADDING))
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .overflow_y_scroll()
        .track_scroll(&handle)
        // A constant inset border preserves layout and cannot be clipped by
        // the shell's overflow boundary, unlike an outward focus shadow.
        .border_2()
        .border_color(if content_is_focused {
            colors::settings_focus_ring(content_surface)
        } else {
            gpui::transparent_black()
        })
        .children(page.into_blocks());

    super::widgets::settings_focus_scroll(body, handle, last_control_focus, window, cx)
        .into_any_element()
}

/// Labels of the settings on `section` that the active search matched.
fn search_match_labels(surface: &SettingsSurface, section: SettingsSectionId) -> Vec<&'static str> {
    if !surface.is_searching() {
        return Vec::new();
    }
    surface
        .search
        .model()
        .tree()
        .iter()
        .flat_map(|category| category.sections.iter())
        .filter(|found| found.id() == section)
        .flat_map(|found| {
            found
                .settings
                .iter()
                .chain(found.subsections.iter().flat_map(|sub| sub.settings.iter()))
                .map(|setting| setting.label)
        })
        .collect()
}

fn ensure_uploader_inputs<T: SettingsSurfaceHost>(
    host: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> UploaderEditorInputs {
    if let Some(inputs) = host.surface().uploader_inputs.clone() {
        return inputs;
    }
    let inputs = UploaderEditorInputs::new(window, cx);
    host.surface_mut().uploader_inputs = Some(inputs.clone());
    inputs
}

fn no_results_title(title: &'static str) -> AnyElement {
    div()
        .text_size(px(SettingsMetrics::TITLE_TEXT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(colors::text_primary())
        .child(title)
        .into_any_element()
}

/// Empty state for a search that matches no visible destination.
fn render_no_results(query: &str) -> AnyElement {
    div()
        .id("settings-no-results")
        .role(Role::Status)
        .aria_label("No settings found")
        .flex_1()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .max_w(px(360.0))
                .px_5()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(SettingsMetrics::SECTION_TITLE_TEXT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::text_primary())
                        .child("No settings found"),
                )
                .child(
                    div()
                        .text_size(px(SettingsMetrics::LABEL_TEXT))
                        .text_center()
                        .text_color(colors::text_secondary())
                        .child(format!(
                            "Nothing matches \u{201c}{}\u{201d}. Try a broader search or clear the query.",
                            query.trim()
                        )),
                ),
        )
        .into_any_element()
}

/// Disclosure state for a query that only matches hidden developer settings —
/// a truthful alternative to a "no results" the user could act on but cannot see.
fn render_hidden_developer_state<T: SettingsSurfaceHost>(
    host: &mut T,
    cx: &mut Context<T>,
) -> AnyElement {
    let counts = host.surface().search.model().hidden_developer_counts();
    let detail = if counts.settings > 0 {
        format!(
            "{} developer {} match this search.",
            counts.settings,
            if counts.settings == 1 {
                "setting"
            } else {
                "settings"
            }
        )
    } else {
        "Developer settings match this search.".to_string()
    };
    let surface = colors::bg_elevated();

    div()
        .id("settings-hidden-developer")
        .role(Role::Status)
        .aria_label("Developer settings are hidden")
        .flex_1()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .max_w(px(360.0))
                .px_5()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(SettingsMetrics::SECTION_TITLE_TEXT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::text_primary())
                        .child("Developer settings are hidden"),
                )
                .child(
                    div()
                        .text_size(px(SettingsMetrics::LABEL_TEXT))
                        .text_center()
                        .text_color(colors::text_secondary())
                        .child(detail),
                )
                .child(
                    div()
                        .id("settings-show-developer")
                        .role(Role::Button)
                        .map(|element| {
                            crate::components::ui::accessibility::accessible_focus(
                                element,
                                super::widgets::control_focus("settings-show-developer", cx)
                                    .tab_index(TAB_INDEX_CONTENT),
                            )
                        })
                        .aria_label("Show developer settings")
                        .tab_index(TAB_INDEX_CONTENT)
                        .mt_1()
                        .h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
                        .px(px(SettingsMetrics::ACTION_BUTTON_PADDING_X))
                        .rounded(Radii::sm())
                        .bg(colors::accent())
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .focus_visible(move |style| style.shadow(focus_ring(surface)))
                        .on_key_down(cx.listener(|host, event: &KeyDownEvent, _window, cx| {
                            if !is_activation_key(event) {
                                return;
                            }
                            reveal_hidden_developer_matches(host);
                            cx.notify();
                            cx.stop_propagation();
                        }))
                        .on_press_click(cx.listener(|host, _, _window, cx| {
                            reveal_hidden_developer_matches(host);
                            cx.notify();
                            cx.stop_propagation();
                        }))
                        .on_a11y_action(accesskit::Action::Click, {
                            let entity = cx.entity();
                            move |_, _, cx| {
                                entity.update(cx, |host, cx| {
                                    reveal_hidden_developer_matches(host);
                                    cx.notify();
                                })
                            }
                        })
                        .child(
                            div()
                                .text_size(px(SettingsMetrics::LABEL_TEXT))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_on_accent_contrast())
                                .child("Show developer settings"),
                        ),
                ),
        )
        .into_any_element()
}

/// Reveal the developer disclosure and land on the first hidden match, so the
/// same query immediately answers the question the user asked.
fn reveal_hidden_developer_matches<T: SettingsSurfaceHost>(host: &mut T) {
    host.surface_mut().reveal_hidden_developer(true);
}

#[cfg(test)]
mod tests {
    use super::{SEARCH_QUERY_MAX_CHARS, normalize_search_query};
    use core::prelude::v1::test;

    #[test]
    fn search_query_normalization_matches_the_shared_single_line_input_contract() {
        assert_eq!(
            normalize_search_query("links\r\nmedia\tsettings"),
            "links media settings"
        );
        assert_eq!(
            normalize_search_query(&"x".repeat(SEARCH_QUERY_MAX_CHARS + 1)),
            "x".repeat(SEARCH_QUERY_MAX_CHARS)
        );
    }
}
