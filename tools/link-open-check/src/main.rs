//! Contract: external-URL validation (accepted schemes, trimming, rejected inputs)
//! and per-browser incognito arguments in `src/utils/open_url.rs`.

pub(crate) use check_support::category_log as log_ui;
use check_support::{FIXTURE_HEADER, assert_fixture};

mod log {
    pub fn redact_url(raw: &str) -> String {
        raw.to_string()
    }
}

mod backend {
    #[allow(dead_code)] // the toast payload is only built, never shown
    pub enum UiAction {
        ShowWarningToast {
            message: String,
            detail: Option<String>,
        },
    }

    pub fn push_ui_action(_action: UiAction) {}

    pub mod lock_utils {
        pub trait MutexExt<T> {
            fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T>;
        }

        impl<T> MutexExt<T> for std::sync::Mutex<T> {
            fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T> {
                self.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
        }
    }

    pub mod settings {
        pub fn get_open_links_incognito() -> bool {
            false
        }

        pub fn get_browser_path() -> Option<String> {
            None
        }
    }
}

mod components {
    pub mod chat {
        pub mod canvas_renderer {
            pub mod global_layout_cache {
                pub fn clear_layouts_linking_to(_url: &str) {}
            }
        }
    }
}

mod state {
    pub mod ui_invalidation {
        pub enum UiInvalidation {
            SettingsLayout,
        }

        pub fn request(_invalidation: UiInvalidation) {}
    }
}

mod managers {
    pub mod frame_rate_controller {
        pub fn record_activity() {}

        pub fn wakeup() {}
    }
}

#[path = "../../../src/utils/open_url.rs"]
#[allow(dead_code)] // the launch and opened-URL history paths spawn real processes
mod open_url;

use open_url::{incognito_args, validate_external_url};

fn main() {
    println!("{FIXTURE_HEADER}");

    assert_fixture(
        "link_accept_http",
        validate_external_url(" http://example.com/path ").expect("http URL"),
        "http://example.com/path",
    );
    assert_fixture(
        "link_accept_https",
        validate_external_url("https://example.com/path?q=1").expect("https URL"),
        "https://example.com/path?q=1",
    );
    assert_fixture(
        "link_reject_empty",
        validate_external_url("   ").expect_err("empty URL"),
        "URL is empty",
    );
    assert_fixture(
        "link_reject_javascript",
        validate_external_url("javascript:alert(1)").expect_err("javascript URL"),
        "scheme 'javascript' is not allowed for external links",
    );
    assert_fixture(
        "link_reject_file",
        validate_external_url("file:///tmp/secret").expect_err("file URL"),
        "scheme 'file' is not allowed for external links",
    );
    assert_fixture(
        "link_reject_relative",
        validate_external_url("example.com/path").expect_err("relative URL"),
        "invalid URL 'example.com/path': relative URL without a base",
    );

    assert_fixture(
        "incognito_args_firefox",
        incognito_arg_summary("firefox"),
        "--private-window",
    );
    assert_fixture(
        "incognito_args_edge",
        incognito_arg_summary("msedge"),
        "--inprivate",
    );
    assert_fixture(
        "incognito_args_chrome",
        incognito_arg_summary("google-chrome"),
        "--incognito",
    );
    assert_fixture(
        "incognito_args_brave",
        incognito_arg_summary("brave-browser"),
        "--incognito",
    );
}

fn incognito_arg_summary(browser: &str) -> String {
    incognito_args(browser).join(" ")
}
