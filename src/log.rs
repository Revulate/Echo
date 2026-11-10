//! Category-based logging for Echo
//!
//! Provides simple environment variable toggles for log categories,
//! replacing complex RUST_LOG patterns with intuitive category controls.
//!
//! Warn and error events always pass the category gate; trace, debug and
//! info need their category enabled. With no logging env vars, a release
//! build keeps warn and error in a small rotating `echo.log` (see
//! [`crate::diagnostics::profiling`]).
//!
//! ## Usage
//!
//! ```bash
//! # Enable single category
//! ECHO_LOG_IRC=1 cargo run
//! ECHO_LOG_EMOTES=1 cargo run
//!
//! # Enable multiple categories
//! ECHO_LOG=irc,emotes,cache cargo run
//!
//! # Traditional tracing still works
//! RUST_LOG=debug cargo run
//! ```
//!
//! ## Categories
//!
//! | Category | Env Var | Description |
//! |----------|---------|-------------|
//! | irc | `ECHO_LOG_IRC` | IRC connection, watchdog, lifecycle |
//! | emotes | `ECHO_LOG_EMOTES` | 7TV, BTTV, FFZ, Twitch emotes |
//! | auth | `ECHO_LOG_AUTH` | Authentication, tokens |
//! | cache | `ECHO_LOG_CACHE` | Image cache, badges, disk |
//! | perf | `ECHO_LOG_PERF` | Performance timing (umbrella for perf_* subcategories) |
//! | perf_render | `ECHO_LOG_PERF_RENDER` | Render/layout/frame pacing perf logs |
//! | perf_cache | `ECHO_LOG_PERF_CACHE` | Cache/preload perf logs |
//! | perf_emotes | `ECHO_LOG_PERF_EMOTES` | Emote pipeline perf logs |
//! | ui | `ECHO_LOG_UI` | UI events, overlays |
//! | net | `ECHO_LOG_NET` | Network requests, API calls |
//! | app | `ECHO_LOG_APP` | Lifecycle, startup, shutdown, crash markers |
//! | persist | `ECHO_LOG_PERSIST` | Settings, tabs, history and other file I/O |
//! | update | `ECHO_LOG_UPDATE` | Update check, download, verify, install |
//!
//! Extra high-volume diagnostics:
//! - `ECHO_EMOTE_TRACE=1` enables per-token/per-emote parse and resolve logs.
//! - `ECHO_LAYOUT_TRACE=1` / `ECHO_RENDER_TRACE=1` enable slow/re-entrant layout/render logs.
//! - `ECHO_LAYOUT_TRACE_VERBOSE=1` / `ECHO_RENDER_TRACE_VERBOSE=1` log every span.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// One log category: its `ECHO_LOG` name, tracing target, and toggle env var.
pub(crate) struct Category {
    pub name: &'static str,
    #[cfg_attr(feature = "profiling", allow(dead_code))]
    pub target: &'static str,
    pub env: &'static str,
}

const fn category(name: &'static str, target: &'static str, env: &'static str) -> Category {
    Category { name, target, env }
}

/// The single source of truth for Echo's log categories.
///
/// The `log_*!` macros spell the same targets as literals because `tracing`
/// requires a constant target at the call site.
pub(crate) const CATEGORY_TABLE: &[Category] = &[
    category("irc", "echo::irc", "ECHO_LOG_IRC"),
    category("emotes", "echo::emotes", "ECHO_LOG_EMOTES"),
    category("auth", "echo::auth", "ECHO_LOG_AUTH"),
    category("cache", "echo::cache", "ECHO_LOG_CACHE"),
    category("perf", "echo::perf", "ECHO_LOG_PERF"),
    category("perf_render", "echo::perf_render", "ECHO_LOG_PERF_RENDER"),
    category("perf_cache", "echo::perf_cache", "ECHO_LOG_PERF_CACHE"),
    category("perf_emotes", "echo::perf_emotes", "ECHO_LOG_PERF_EMOTES"),
    category("ui", "echo::ui", "ECHO_LOG_UI"),
    category("net", "echo::net", "ECHO_LOG_NET"),
    category("app", "echo::app", "ECHO_LOG_APP"),
    category("persist", "echo::persist", "ECHO_LOG_PERSIST"),
    category("update", "echo::update", "ECHO_LOG_UPDATE"),
];

/// Env vars that turn on high-volume emote tracing (and with it the `emotes` category).
const EMOTE_TRACE_ENV_VARS: &[&str] = &["ECHO_EMOTE_TRACE", "ECHO_LOG_EMOTE_TRACE"];

/// Env vars that turn on layout/render tracing (and with it the `perf_render` category).
const RENDER_TRACE_ENV_VARS: &[&str] = &[
    "ECHO_LAYOUT_TRACE",
    "ECHO_LAYOUT_TRACE_VERBOSE",
    "ECHO_RENDER_TRACE",
    "ECHO_RENDER_TRACE_VERBOSE",
];

/// Whether `name` is one of the `perf_*` subcategories covered by the `perf` umbrella.
fn is_perf_subcategory(name: &str) -> bool {
    name.starts_with("perf_")
}

/// Log category flags, indexed like [`CATEGORY_TABLE`], for fast runtime checks.
struct LogCategories {
    flags: [AtomicBool; CATEGORY_TABLE.len()],
}

impl LogCategories {
    fn flag(&self, name: &str) -> Option<&AtomicBool> {
        CATEGORY_TABLE
            .iter()
            .position(|category| category.name == name)
            .map(|index| &self.flags[index])
    }
}

static CATEGORIES: OnceLock<LogCategories> = OnceLock::new();

/// Whether `ECHO_RENDER_TRACE_VERBOSE` is on. Evaluated once at first access.
///
/// Used by `app.rs`, `chat_pane.rs`, and `chat_view.rs` to gate verbose render logs.
/// The painter module (`canvas_renderer/painter/trace.rs`) has its own copy because
/// it lives inside a `pub(super)` boundary.
pub(crate) static RENDER_TRACE_VERBOSE: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| echo_core::env::flag("ECHO_RENDER_TRACE_VERBOSE"));

/// The categories named in the comma-separated `ECHO_LOG` list, trimmed.
fn listed_categories() -> Vec<String> {
    echo_core::env::value("ECHO_LOG")
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the environment switches `category` on.
///
/// This is the only place that rule lives: [`init`] stores its answers in the
/// runtime flags, and the tracing filter in [`crate::diagnostics::profiling`] is built from
/// those flags through [`enabled_categories`].
fn category_enabled_by_env(category: &Category, listed: &[String]) -> bool {
    let is_listed = |name: &str| listed.iter().any(|entry| entry == name);
    let perf_all = is_listed("perf") || echo_core::env::flag("ECHO_LOG_PERF");
    is_listed(category.name)
        || echo_core::env::flag(category.env)
        || (is_perf_subcategory(category.name) && perf_all)
        || (category.name == "emotes" && emote_trace_enabled())
        || (category.name == "perf_render" && render_trace_enabled())
}

fn render_trace_enabled() -> bool {
    RENDER_TRACE_ENV_VARS
        .iter()
        .any(|var| echo_core::env::flag(var))
}

/// Whether any `ECHO_LOG*` or trace switch is on, which makes a release build
/// keep its console-level logging instead of the quiet default.
#[cfg(all(not(debug_assertions), not(feature = "profiling")))]
pub(crate) fn any_category_switch_set() -> bool {
    !listed_categories().is_empty()
        || CATEGORY_TABLE
            .iter()
            .any(|category| echo_core::env::flag(category.env))
        || emote_trace_enabled()
        || render_trace_enabled()
}

/// Initialize log categories from environment variables.
///
/// Idempotent: the tracing filter built by [`crate::diagnostics::profiling::init`] already
/// reads the categories through [`enabled_categories`].
pub fn init() {
    CATEGORIES.get_or_init(|| {
        let listed = listed_categories();
        let cats = LogCategories {
            flags: std::array::from_fn(|index| {
                AtomicBool::new(category_enabled_by_env(&CATEGORY_TABLE[index], &listed))
            }),
        };

        #[cfg(debug_assertions)]
        {
            let mut active: Vec<&str> = CATEGORY_TABLE
                .iter()
                .zip(&cats.flags)
                .filter(|(_, flag)| flag.load(Ordering::Relaxed))
                .map(|(category, _)| category.name)
                .collect();
            if emote_trace_enabled() {
                active.push("emote_trace");
            }
            if !active.is_empty() {
                eprintln!("[36m[log][0m Categories enabled: {}", active.join(", "));
            }
        }

        cats
    });
    echo_core::install_log_category_enabled(enabled);
}

fn categories() -> &'static LogCategories {
    init();
    CATEGORIES
        .get()
        .expect("log categories are initialized by log::init")
}

pub fn enabled_categories() -> Vec<&'static str> {
    let categories = categories();
    CATEGORY_TABLE
        .iter()
        .zip(&categories.flags)
        .filter(|(_, flag)| flag.load(Ordering::Relaxed))
        .map(|(category, _)| category.name)
        .collect()
}

/// High-volume emote tracing for parser/resolver/layout internals.
///
/// This is intentionally separate from `ECHO_LOG=emotes`, which should remain
/// usable for lifecycle and pipeline diagnostics without producing megabytes of
/// per-token logs or perturbing render timing via the tracing writer lock.
#[inline]
pub fn emote_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        EMOTE_TRACE_ENV_VARS
            .iter()
            .any(|var| echo_core::env::flag(var))
    })
}

/// Check if a log category is enabled. Fast path using atomic load.
#[inline]
pub fn enabled(cat: &str) -> bool {
    CATEGORIES
        .get()
        .and_then(|categories| categories.flag(cat))
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// Shared body of the `log_*!` macros: trace/debug/info are gated on the
/// category flag, warn/error always pass through to the tracing filter.
#[doc(hidden)]
#[macro_export]
macro_rules! __echo_log {
    ($cat:literal, $target:literal, warn, $($arg:tt)*) => {{
        tracing::warn!(target: $target, $($arg)*)
    }};
    ($cat:literal, $target:literal, error, $($arg:tt)*) => {{
        tracing::error!(target: $target, $($arg)*)
    }};
    ($cat:literal, $target:literal, $level:ident, $($arg:tt)*) => {{
        if $crate::log::enabled($cat) {
            tracing::$level!(target: $target, $($arg)*)
        }
    }};
}

/// IRC logging (connection, watchdog, lifecycle, supervisor)
#[macro_export]
macro_rules! log_irc {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("irc", "echo::irc", $level, $($arg)*)
    };
}

/// Emotes logging (7TV, BTTV, FFZ, Twitch emotes)
#[macro_export]
macro_rules! log_emotes {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("emotes", "echo::emotes", $level, $($arg)*)
    };
}

/// Auth logging (authentication, tokens)
#[macro_export]
macro_rules! log_auth {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("auth", "echo::auth", $level, $($arg)*)
    };
}

/// Cache logging (image cache, badges, disk cache)
#[macro_export]
macro_rules! log_cache {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("cache", "echo::cache", $level, $($arg)*)
    };
}

/// Performance logging (timing, profiling)
#[macro_export]
macro_rules! log_perf {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("perf", "echo::perf", $level, $($arg)*)
    };
}

/// Render/layout performance logging
#[macro_export]
macro_rules! log_perf_render {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("perf_render", "echo::perf_render", $level, $($arg)*)
    };
}

/// Cache/preload performance logging
#[macro_export]
macro_rules! log_perf_cache {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("perf_cache", "echo::perf_cache", $level, $($arg)*)
    };
}

/// Emote pipeline performance logging
#[macro_export]
macro_rules! log_perf_emotes {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("perf_emotes", "echo::perf_emotes", $level, $($arg)*)
    };
}

/// UI logging (app events, overlays, input)
#[macro_export]
macro_rules! log_ui {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("ui", "echo::ui", $level, $($arg)*)
    };
}

/// Network logging (API requests, history fetches)
#[macro_export]
macro_rules! log_net {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("net", "echo::net", $level, $($arg)*)
    };
}

/// Lifecycle logging (startup, shutdown, windows, crash markers)
#[macro_export]
macro_rules! log_app {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("app", "echo::app", $level, $($arg)*)
    };
}

/// Persistence logging (settings, tabs, history, notes, file operations)
#[macro_export]
macro_rules! log_persist {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("persist", "echo::persist", $level, $($arg)*)
    };
}

/// Updater logging (check, download, verify, install)
#[macro_export]
macro_rules! log_update {
    ($level:ident, $($arg:tt)*) => {
        $crate::__echo_log!("update", "echo::update", $level, $($arg)*)
    };
}

/// A URL that is safe to log: userinfo (`user:pass@`), query and fragment are
/// removed, since they can carry proxy credentials or tokens. Input that does
/// not parse as a hierarchical URL (such as a scheme-less `user:pass@host`)
/// is replaced by a placeholder.
pub fn redact_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "<unparseable url>".to_string();
    };
    if url.cannot_be_a_base() {
        return "<unparseable url>".to_string();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// A value that must never reach a log: `Debug` and `Display` print `<redacted>`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret<T>(pub T);

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl<T> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::{Secret, redact_url};
    use core::prelude::v1::test;

    #[test]
    fn redact_url_strips_userinfo_query_and_fragment() {
        assert_eq!(
            redact_url("https://user:pass@example.com/path?token=abc#frag"),
            "https://example.com/path"
        );
        assert_eq!(
            redact_url("http://user:pw@proxy.local:8080"),
            "http://proxy.local:8080/"
        );
        assert_eq!(
            redact_url("socks5://user:pw@proxy.local:1080"),
            "socks5://proxy.local:1080"
        );
    }

    #[test]
    fn redact_url_keeps_plain_urls() {
        assert_eq!(
            redact_url("https://api.twitch.tv/helix/users"),
            "https://api.twitch.tv/helix/users"
        );
    }

    #[test]
    fn redact_url_hides_unparseable_input() {
        assert_eq!(redact_url("user:pass@ not a url"), "<unparseable url>");
        assert_eq!(redact_url("user:secret@proxy:8080"), "<unparseable url>");
        assert_eq!(redact_url(""), "<unparseable url>");
    }

    #[test]
    fn secret_never_prints_its_value() {
        let secret = Secret("oauth:abc123".to_string());
        assert_eq!(format!("{secret}"), "<redacted>");
        assert_eq!(format!("{secret:?}"), "<redacted>");
    }
}
