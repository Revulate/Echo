//! Single app-facing shutdown coordinator.

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn shutdown_once(reason: &'static str) {
    if SHUTDOWN_REQUESTED.swap(true, Ordering::SeqCst) {
        crate::log_ui!(debug, reason, "Shutdown already requested");
        return;
    }

    crate::state::ui_invalidation::request(crate::state::ui_invalidation::UiInvalidation::Shutdown);
    crate::log_ui!(info, reason, "Shutdown requested");
    crate::backend::bridge::shutdown();
}

pub fn shutdown() {
    shutdown_once("backend_shutdown");
}

pub fn is_shutdown_started() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}
