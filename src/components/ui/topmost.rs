use gpui::{App, Window};

const MAX_APPLY_ATTEMPTS: u8 = 3;

/// Keeps a popout window's topmost state in step with the Always-on-top
/// setting. A non-topmost window can never sit above a topmost one, so without
/// this every popout would open behind the main window while it is pinned.
#[derive(Default)]
pub struct AlwaysOnTopFollower {
    applied: Option<bool>,
    failed_attempts: u8,
}

impl AlwaysOnTopFollower {
    pub fn sync(&mut self, window: &Window) {
        let enabled = crate::backend::settings::get_always_on_top();
        if self.applied == Some(enabled) {
            return;
        }
        if crate::app::apply_main_window_always_on_top(window, enabled) || !cfg!(windows) {
            self.applied = Some(enabled);
            self.failed_attempts = 0;
            return;
        }
        self.failed_attempts += 1;
        if self.failed_attempts >= MAX_APPLY_ATTEMPTS {
            crate::log_ui!(warn, enabled, "Giving up on syncing popout always-on-top");
            self.applied = Some(enabled);
            self.failed_attempts = 0;
        }
    }
}

/// Re-render every window after the Always-on-top setting changes so each
/// popout's [`AlwaysOnTopFollower`] picks up the new state.
pub fn refresh_windows_for_always_on_top(cx: &mut App) {
    cx.defer(|cx| {
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, _| window.refresh());
        }
    });
}
