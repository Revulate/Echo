//! Settings popout window host.
//!
//! A standalone OS window around the same retained surface the modal hosts, so
//! navigation, search, page hierarchy, density, and accessibility are identical
//! and only the window chrome differs. The popout additionally owns a local
//! login surface, which is what makes account management work here instead of
//! degrading to read-only.

use crate::backend::lock_utils::MutexExt;
use crate::components::auth::{LoginModalState, render_login_modal};
use crate::components::settings::settings_surface::{
    SettingsChrome, SettingsOpenRequest, SettingsSurface, SettingsSurfaceHost,
    render_settings_surface, watch_appearance_changes,
};
use crate::config;
use crate::theme::colors;
use gpui::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Flag indicating if settings popout is currently open
static SETTINGS_POPOUT_OPEN: AtomicBool = AtomicBool::new(false);
static SETTINGS_POPOUT_HANDLE: LazyLock<Mutex<Option<AnyWindowHandle>>> =
    LazyLock::new(|| Mutex::new(None));

/// Pending popout request: the destination plus any query being transferred
/// from the modal.
static PENDING_SETTINGS_POPOUT: LazyLock<Mutex<Option<SettingsOpenRequest>>> =
    LazyLock::new(|| Mutex::new(None));

/// Request a settings popout (called from the modal, processed by the polling
/// coordinator).
pub fn request_settings_popout(request: SettingsOpenRequest) {
    *PENDING_SETTINGS_POPOUT.lock_or_recover() = Some(request);
}

/// Drain pending popout request
pub fn drain_settings_popout_request() -> Option<SettingsOpenRequest> {
    PENDING_SETTINGS_POPOUT.lock_or_recover().take()
}

/// Check if settings popout is currently open
pub fn is_settings_popout_open() -> bool {
    SETTINGS_POPOUT_OPEN.load(Ordering::SeqCst)
}

fn reset_settings_popout_state() {
    SETTINGS_POPOUT_OPEN.store(false, Ordering::SeqCst);
    *SETTINGS_POPOUT_HANDLE.lock_or_recover() = None;
}

/// Size to open the settings popout at.
///
/// A window still sitting on exactly the pre-density-pass default has never
/// been deliberately resized, so it adopts the roomier default once; any other
/// persisted size is the user's and is used verbatim.
fn preferred_popout_size() -> (f32, f32) {
    let persisted = crate::backend::settings::get_settings_popout_window_size();
    if persisted == config::SETTINGS_POPOUT_LEGACY_DEFAULT_SIZE_PX {
        return (
            config::SETTINGS_POPOUT_DEFAULT_WIDTH_PX,
            config::SETTINGS_POPOUT_DEFAULT_HEIGHT_PX,
        );
    }
    persisted
}

/// Open the settings popout window
pub fn open_settings_popout(request: SettingsOpenRequest, cx: &mut App) {
    if is_settings_popout_open() {
        let existing = *SETTINGS_POPOUT_HANDLE.lock_or_recover();
        let activated = existing.is_some_and(|handle| {
            handle
                .update(cx, |_, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    window.refresh();
                })
                .is_ok()
        });
        if activated {
            *PENDING_SETTINGS_POPOUT.lock_or_recover() = Some(request);
            return;
        }

        reset_settings_popout_state();
    }

    SETTINGS_POPOUT_OPEN.store(true, Ordering::SeqCst);

    let (width, height) = preferred_popout_size();
    let (bounds, display_id) = crate::utils::popup_window_placement(
        crate::backend::settings::get_settings_popout_window_position(),
        size(px(width), px(height)),
        cx,
    );

    if let Err(error) = crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Echo Settings".into()),
                appears_transparent: false,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(
                px(config::SETTINGS_POPOUT_MIN_WIDTH_PX),
                px(config::SETTINGS_POPOUT_MIN_HEIGHT_PX),
            )),
            ..Default::default()
        },
        move |window, cx| {
            *SETTINGS_POPOUT_HANDLE.lock_or_recover() = Some(window.window_handle());
            cx.new(|cx| SettingsPopout::new(request, window, cx))
        },
    ) {
        reset_settings_popout_state();
        crate::log_ui!(warn, "Failed to open settings popout: {}", error);
    }
}

/// State for the settings popout
pub struct SettingsPopout {
    surface: SettingsSurface,
    /// Login surface local to this window. The modal's shared entity belongs to
    /// the main window and cannot render here.
    login_modal: Entity<LoginModalState>,
    last_persisted_position: Option<(f32, f32)>,
    last_persisted_size: (f32, f32),
    _update_state_task: Task<()>,
    /// Keeps the native popout in sync with palette changes made from another
    /// Settings host.
    _appearance_changes_task: Task<()>,
    /// Refresh loop scoped to an open device-code dialog. Auth has no watch
    /// channel to subscribe to, and this window is not reached by the main
    /// window's notify, so the dialog would otherwise never show its code.
    _login_refresh_task: Option<Task<()>>,
    login_refresh_active: bool,
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
}

impl SettingsPopout {
    pub fn new(request: SettingsOpenRequest, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let last_persisted_size = preferred_popout_size();
        let last_persisted_position =
            crate::backend::settings::get_settings_popout_window_position();

        let mut update_state_changes = crate::backend::updates::subscribe_state_changes();
        let update_state_task = cx.spawn(async move |this, cx| {
            while update_state_changes.changed().await.is_ok() {
                if this.update(cx, |_this, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let appearance_changes_task = watch_appearance_changes(cx, |_| true);

        let login_modal = cx.new(|_| LoginModalState::default());
        let mut surface = SettingsSurface::new(SettingsChrome::Popout, cx);
        surface.attach_login_modal(&login_modal);
        surface.apply_open_request(&request);

        Self {
            surface,
            login_modal,
            always_on_top: Default::default(),
            last_persisted_position,
            last_persisted_size,
            _update_state_task: update_state_task,
            _appearance_changes_task: appearance_changes_task,
            _login_refresh_task: None,
            login_refresh_active: false,
        }
    }

    /// Adopt a request that arrived while the window was already open.
    fn drain_pending_request(&mut self, cx: &mut Context<Self>) {
        let Some(request) = drain_settings_popout_request() else {
            return;
        };
        if self.surface.apply_open_request(&request) {
            cx.notify();
        }
    }

    /// Persist window geometry the moment the user changes it.
    fn persist_window_geometry(&mut self, window: &Window) {
        let bounds = window.window_bounds().get_bounds();
        let window_x: f32 = bounds.origin.x.into();
        let window_y: f32 = bounds.origin.y.into();
        let window_width: f32 = bounds.size.width.into();
        let window_height: f32 = bounds.size.height.into();

        let position_changed = self
            .last_persisted_position
            .map(|(x, y)| (x - window_x).abs() >= 1.0 || (y - window_y).abs() >= 1.0)
            .unwrap_or(true);
        if position_changed {
            crate::backend::settings::set_settings_popout_window_position(window_x, window_y);
            self.last_persisted_position = Some((window_x, window_y));
        }

        let size_changed = (self.last_persisted_size.0 - window_width).abs() >= 1.0
            || (self.last_persisted_size.1 - window_height).abs() >= 1.0;
        if size_changed {
            crate::backend::settings::set_settings_popout_window_size(window_width, window_height);
            self.last_persisted_size = (window_width, window_height);
        }
    }

    /// Start or stop the device-code refresh loop with the dialog it serves.
    fn sync_login_refresh(&mut self, cx: &mut Context<Self>) {
        let is_open = self.login_modal.read(cx).is_open;
        if is_open == self.login_refresh_active {
            return;
        }
        self.login_refresh_active = is_open;
        if !is_open {
            self._login_refresh_task = None;
            return;
        }

        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let still_open = this.update(cx, |this: &mut Self, cx| {
                    let is_open = this.login_modal.read(cx).is_open;
                    if is_open {
                        cx.notify();
                    } else {
                        this.login_refresh_active = false;
                    }
                    is_open
                });
                if !matches!(still_open, Ok(true)) {
                    break;
                }
            }
        });
        self._login_refresh_task = Some(task);
    }
}

impl SettingsSurfaceHost for SettingsPopout {
    fn surface(&self) -> &SettingsSurface {
        &self.surface
    }

    fn surface_mut(&mut self) -> &mut SettingsSurface {
        &mut self.surface
    }

    fn request_close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.surface.persist_current_location();
        window.remove_window();
    }
}

impl Drop for SettingsPopout {
    fn drop(&mut self) {
        self.surface.persist_current_location();
        reset_settings_popout_state();
    }
}

impl Render for SettingsPopout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        colors::begin_secondary_window_frame();
        self.always_on_top.sync(window);
        self.drain_pending_request(cx);
        self.persist_window_geometry(window);
        self.sync_login_refresh(cx);

        let surface = render_settings_surface(self, window, cx);
        let login_overlay =
            render_login_modal(self.login_modal.clone(), window.viewport_size(), cx);

        div()
            .id("settings-popout")
            .role(Role::Window)
            .aria_label("Echo settings")
            .relative()
            .size_full()
            .bg(colors::bg_elevated())
            .flex()
            .flex_col()
            .on_scroll(cx.listener(|_this, _event: &ScrollEvent, _window, cx| {
                cx.stop_propagation();
            }))
            .child(surface)
            .child(login_overlay)
            .into_any_element()
    }
}
