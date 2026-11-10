#![allow(dead_code, unused_variables)]

#[macro_export]
macro_rules! log_ui {
    ($($arg:tt)*) => {{}};
}

mod log {
    pub fn redact_url(raw: &str) -> String {
        raw.to_string()
    }
}

mod backend {
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
mod open_url;

use open_url::{incognito_args, validate_external_url};

fn main() {
    println!("kind,name,value,checksum");

    print_fixture(
        "link_accept_http",
        validate_external_url(" http://example.com/path ").expect("http URL"),
        "http://example.com/path",
    );
    print_fixture(
        "link_accept_https",
        validate_external_url("https://example.com/path?q=1").expect("https URL"),
        "https://example.com/path?q=1",
    );
    print_fixture(
        "link_reject_empty",
        &validate_external_url("   ").expect_err("empty URL"),
        "URL is empty",
    );
    print_fixture(
        "link_reject_javascript",
        &validate_external_url("javascript:alert(1)").expect_err("javascript URL"),
        "scheme 'javascript' is not allowed for external links",
    );
    print_fixture(
        "link_reject_file",
        &validate_external_url("file:///tmp/secret").expect_err("file URL"),
        "scheme 'file' is not allowed for external links",
    );
    print_fixture(
        "link_reject_relative",
        &validate_external_url("example.com/path").expect_err("relative URL"),
        "invalid URL 'example.com/path': relative URL without a base",
    );

    print_fixture(
        "incognito_args_firefox",
        &incognito_arg_summary("firefox"),
        "--private-window",
    );
    print_fixture(
        "incognito_args_edge",
        &incognito_arg_summary("msedge"),
        "--inprivate",
    );
    print_fixture(
        "incognito_args_chrome",
        &incognito_arg_summary("google-chrome"),
        "--incognito",
    );
    print_fixture(
        "incognito_args_brave",
        &incognito_arg_summary("brave-browser"),
        "--incognito",
    );
}

fn incognito_arg_summary(browser: &str) -> String {
    incognito_args(browser).join(" ")
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
