//! Contract: streamer-mode privacy gates in `src/backend/streamer_mode.rs` for each
//! mode (always on, disabled, auto with nothing detected) and the per-gate
//! content toggles.

pub(crate) use check_support::category_log as log_ui;
use check_support::{FIXTURE_HEADER, assert_fixture};

mod lock_utils {
    pub use echo_core::sync::{MutexExt, RwLockExt};
}

mod state {
    pub mod ui_invalidation {
        #[derive(Clone, Copy, Debug)]
        pub enum UiInvalidation {
            TopBarState,
        }
        pub fn request(_: UiInvalidation) {}
    }
}

mod backend {
    pub mod bridge {
        pub struct Runtime;

        impl Runtime {
            pub fn spawn_blocking<F>(&self, _task: F)
            where
                F: FnOnce() + Send + 'static,
            {
            }
        }

        static RUNTIME: Runtime = Runtime;

        pub fn get_runtime() -> &'static Runtime {
            &RUNTIME
        }
    }
}

mod settings {
    use std::sync::{LazyLock, Mutex};

    #[derive(Default)]
    pub struct Settings {
        pub streamer_mode: Option<crate::streamer_mode::StreamerModeSettings>,
    }

    pub static SETTINGS: LazyLock<Mutex<Settings>> =
        LazyLock::new(|| Mutex::new(Settings::default()));

    pub fn save_settings() {}

    pub fn broadcast_streamer_mode_changed() {}
}

// The production detector consults the profile-aware external config root for
// OBS/Discord activity logs. This deterministic harness does not create or
// inspect those logs, but retains the same module surface for compilation.
mod storage_roots {
    use std::path::PathBuf;

    pub fn external_config_dir() -> Option<PathBuf> {
        dirs::config_dir()
    }
}

// Only the settings-driven gates are checked; process detection, cached status
// and settings updates run against live system state.
#[path = "../../../src/backend/streamer_mode.rs"]
#[allow(dead_code)]
mod streamer_mode;

use streamer_mode::{
    StreamerModeSettings, StreamerModeStatus, should_hide_account_names, should_hide_automod_terms,
    should_hide_avatars, should_hide_link_thumbnails, should_hide_link_urls,
    should_hide_mention_content, should_hide_mod_actions, should_hide_user_card_details,
    should_hide_viewer_count, should_hide_whisper_content, should_mute_mentions,
    should_suppress_notifications,
};

fn main() {
    println!("{FIXTURE_HEADER}");

    set_streamer_mode(StreamerModeSettings {
        enabled: StreamerModeStatus::AlwaysOn,
        ..Default::default()
    });
    assert_fixture(
        "streamer_always_on_all_default_gates",
        gate_summary(),
        "111111111111",
    );

    set_streamer_mode(StreamerModeSettings {
        enabled: StreamerModeStatus::Disabled,
        ..Default::default()
    });
    assert_fixture(
        "streamer_disabled_all_gates_off",
        gate_summary(),
        "000000000000",
    );

    set_streamer_mode(StreamerModeSettings {
        enabled: StreamerModeStatus::Auto,
        ..Default::default()
    });
    assert_fixture(
        "streamer_auto_no_detection_gates_off",
        gate_summary(),
        "000000000000",
    );

    set_streamer_mode(StreamerModeSettings {
        enabled: StreamerModeStatus::AlwaysOn,
        hide_link_urls: false,
        hide_mod_actions: false,
        mute_mentions: false,
        suppress_notifications: false,
        ..Default::default()
    });
    assert_fixture(
        "streamer_disabled_individual_toggles",
        selected_gate_summary(),
        "0000",
    );

    set_streamer_mode(StreamerModeSettings {
        enabled: StreamerModeStatus::AlwaysOn,
        hide_automod_terms: false,
        hide_whisper_content: false,
        ..Default::default()
    });
    assert_fixture(
        "streamer_content_toggle_overrides",
        content_gate_summary(),
        "001",
    );
}

fn set_streamer_mode(settings: StreamerModeSettings) {
    settings::SETTINGS
        .lock()
        .expect("settings lock")
        .streamer_mode = Some(settings);
}

fn gate_summary() -> String {
    [
        should_hide_viewer_count(),
        should_hide_link_thumbnails(),
        should_hide_link_urls(),
        should_hide_avatars(),
        should_hide_user_card_details(),
        should_hide_account_names(),
        should_hide_mod_actions(),
        should_hide_automod_terms(),
        should_hide_whisper_content(),
        should_hide_mention_content(),
        should_mute_mentions(),
        should_suppress_notifications(),
    ]
    .into_iter()
    .map(bool_digit)
    .collect()
}

fn selected_gate_summary() -> String {
    [
        should_hide_link_urls(),
        should_hide_mod_actions(),
        should_mute_mentions(),
        should_suppress_notifications(),
    ]
    .into_iter()
    .map(bool_digit)
    .collect()
}

fn content_gate_summary() -> String {
    [
        should_hide_automod_terms(),
        should_hide_whisper_content(),
        should_hide_mention_content(),
    ]
    .into_iter()
    .map(bool_digit)
    .collect()
}

fn bool_digit(value: bool) -> char {
    if value { '1' } else { '0' }
}
