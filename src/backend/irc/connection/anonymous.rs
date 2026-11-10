//! Anonymous (logged-out) IRC mode.

use super::*;

/// Flag to force anonymous mode after auth failure
pub(super) static USE_ANONYMOUS_MODE: AtomicBool = AtomicBool::new(false);
/// Process environment is immutable for Echo's lifetime. Cache this once so
/// the message hot path never queries the environment.
pub(super) static FORCE_ANONYMOUS_MODE: LazyLock<bool> =
    LazyLock::new(|| echo_core::env::flag("FORCE_ANON_IRC"));

/// Reset anonymous mode flag (call after successful re-auth)
pub fn reset_anonymous_mode() {
    if USE_ANONYMOUS_MODE.swap(false, Ordering::Release) {
        crate::log_irc!(info, "Anonymous mode reset - authenticated mode restored");
        crate::backend::eventsub::notify_auth_readiness_changed();
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
}

/// Set runtime anonymous mode without changing persisted settings.
pub fn set_runtime_anonymous_mode(enabled: bool) {
    if enabled {
        if !USE_ANONYMOUS_MODE.swap(true, Ordering::Release) {
            crate::log_irc!(info, "Anonymous mode enabled for IRC runtime");
            crate::backend::eventsub::notify_auth_readiness_changed();
        }
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    } else {
        reset_anonymous_mode();
    }
}

/// Check if anonymous mode is currently active
pub fn is_anonymous_mode() -> bool {
    USE_ANONYMOUS_MODE.load(Ordering::Acquire)
        || crate::backend::settings::get_anonymous_mode()
        || *FORCE_ANONYMOUS_MODE
}
