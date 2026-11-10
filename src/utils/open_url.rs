//! URL opening utilities for Echo GPUI
//! Used by ContextMenu for opening links and profiles

use std::collections::{HashSet, VecDeque};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};

use crate::backend::lock_utils::MutexExt;

const MAX_SESSION_OPENED_URLS: usize = 2_048;

static SESSION_OPENED_URLS: LazyLock<Mutex<OpenedUrlHistory>> =
    LazyLock::new(|| Mutex::new(OpenedUrlHistory::default()));

#[derive(Default)]
struct OpenedUrlHistory {
    ordered_urls: VecDeque<String>,
    urls: HashSet<String>,
}

impl OpenedUrlHistory {
    /// `None` when `url` was already recorded; otherwise the URL evicted to
    /// stay within the bound, if any.
    fn insert(&mut self, url: &str) -> Option<Option<String>> {
        if !self.urls.insert(url.to_string()) {
            return None;
        }

        self.ordered_urls.push_back(url.to_string());
        if self.ordered_urls.len() > MAX_SESSION_OPENED_URLS
            && let Some(evicted) = self.ordered_urls.pop_front()
        {
            self.urls.remove(&evicted);
            return Some(Some(evicted));
        }
        Some(None)
    }

    fn contains(&self, url: &str) -> bool {
        self.urls.contains(url)
    }
}

/// Whether Echo successfully launched this external URL during the current session.
///
/// This state is intentionally process-local and retains at most
/// [`MAX_SESSION_OPENED_URLS`] distinct URLs. Renderers can use it to provide
/// a local "opened" link treatment without reading or persisting browser history.
pub fn is_url_opened(url: &str) -> bool {
    SESSION_OPENED_URLS.lock_or_recover().contains(url.trim())
}

/// URLs the launcher thread opened, waiting for [`apply_opened_urls`].
static PENDING_OPENED_URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Hand a launched URL to the UI thread, which marks it visited.
fn queue_successful_url_open(url: &str) {
    PENDING_OPENED_URLS.lock_or_recover().push(url.to_string());
    crate::managers::frame_rate_controller::record_activity();
    crate::managers::frame_rate_controller::wakeup();
}

/// Mark URLs opened since the last call as visited and evict layouts that link
/// to them. Call on the UI thread: layouts shaped there then cannot be cached
/// with the old link color after the eviction, and async layout jobs requested
/// before it are rejected by the eviction log.
pub fn apply_opened_urls() {
    let urls = std::mem::take(&mut *PENDING_OPENED_URLS.lock_or_recover());
    for url in urls {
        record_successful_url_open(&url);
    }
}

fn record_successful_url_open(url: &str) {
    let Some(evicted) = SESSION_OPENED_URLS.lock_or_recover().insert(url) else {
        return;
    };

    crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_linking_to(url);
    if let Some(evicted) = evicted {
        crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_linking_to(
            &evicted,
        );
    }
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::SettingsLayout,
    );
}

/// Open a URL using the configured link-opening mode.
pub fn open_url(url: &str) {
    let Some(target) = checked_external_url(url) else {
        return;
    };

    let incognito = crate::backend::settings::get_open_links_incognito();
    spawn_launch(url.trim().to_string(), target, incognito);
}

/// Open a URL in the normal/default browser session.
pub fn open_url_normal(url: &str) {
    let Some(target) = checked_external_url(url) else {
        return;
    };
    spawn_launch(url.trim().to_string(), target, false);
}

/// Open a URL in a private/incognito browser window when a supported browser is available.
///
/// Default-browser APIs generally do not expose private-window flags, so this
/// uses Echo's configured browser path first and then common browser commands
/// for the platform. If none work, it reports the failure without opening the URL.
pub fn open_url_incognito(url: &str) {
    let Some(target) = checked_external_url(url) else {
        return;
    };
    spawn_launch(url.trim().to_string(), target, true);
}

/// Open a local file or folder with the OS handler on its own thread.
///
/// The `open` crate launches through a helper process and waits for it, which
/// can take hundreds of milliseconds; `on_done` gets the result on that thread.
pub fn open_path(
    path: impl AsRef<std::ffi::OsStr> + Send + 'static,
    on_done: impl FnOnce(std::io::Result<()>) + Send + 'static,
) {
    let job = Arc::new(Mutex::new(Some((path, on_done))));
    let thread_job = Arc::clone(&job);
    let spawned = std::thread::Builder::new()
        .name("open-path".to_string())
        .spawn(move || run_open_path(&thread_job));
    if let Err(error) = spawned {
        crate::log_ui!(warn, "Could not start the file opener thread: {}", error);
        run_open_path(&job);
    }
}

type OpenPathJob<P, F> = Arc<Mutex<Option<(P, F)>>>;

fn run_open_path<P, F>(job: &OpenPathJob<P, F>)
where
    P: AsRef<std::ffi::OsStr>,
    F: FnOnce(std::io::Result<()>),
{
    let taken = job.lock_or_recover().take();
    if let Some((path, on_done)) = taken {
        on_done(open::that(path));
    }
}

/// Validation stays on the caller's thread so a blocked link toasts at once;
/// the OS launch (ShellExecute or a browser spawn) runs on its own thread so a
/// slow shell never stalls the UI.
fn spawn_launch(url: String, target: String, incognito: bool) {
    let (thread_url, thread_target) = (url.clone(), target.clone());
    let spawned = std::thread::Builder::new()
        .name("open-url".to_string())
        .spawn(move || launch(&thread_url, &thread_target, incognito));
    if let Err(error) = spawned {
        crate::log_ui!(warn, "Could not start the link opener thread: {}", error);
        launch(&url, &target, incognito);
    }
}

fn launch(url: &str, target: &str, incognito: bool) {
    if incognito {
        launch_incognito(url, target);
    } else {
        launch_normal(url, target);
    }
}

fn launch_normal(url: &str, target: &str) {
    crate::log_ui!(info, "Attempting to open: {}", crate::log::redact_url(url));
    match open::that(target) {
        Ok(()) => {
            crate::log_ui!(info, "Successfully opened: {}", crate::log::redact_url(url));
            queue_successful_url_open(url);
        }
        Err(e) => crate::log_ui!(
            error,
            "Failed to open {}: {}",
            crate::log::redact_url(url),
            e
        ),
    }
}

fn launch_incognito(url: &str, target: &str) {
    crate::log_ui!(
        info,
        "Attempting to open incognito: {}",
        crate::log::redact_url(url)
    );

    if let Some(browser_path) = crate::backend::settings::get_browser_path()
        && spawn_browser_incognito(&browser_path, target).is_ok()
    {
        crate::log_ui!(info, "Successfully opened incognito via configured browser");
        queue_successful_url_open(url);
        return;
    }

    for candidate in platform_incognito_candidates() {
        if spawn_browser_incognito(candidate, target).is_ok() {
            crate::log_ui!(
                info,
                "Successfully opened incognito via browser candidate: {}",
                candidate
            );
            queue_successful_url_open(url);
            return;
        }
    }

    crate::log_ui!(warn, "Could not find an incognito-capable browser command");
    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
        message: "Could not open a private browser window".to_string(),
        detail: Some(
            "Set a supported browser path in Settings and try again, or choose Open with → Normal window."
                .to_string(),
        ),
    });
}

/// The validated URL in its serialized form. That form, not the raw input, is
/// handed to the browser so the launched string is exactly the one checked;
/// the raw trimmed input stays the key for the opened-link history.
fn checked_external_url(url: &str) -> Option<String> {
    match parse_external_url(url) {
        Ok(parsed) => Some(parsed.into()),
        Err(reason) => {
            let raw = url.trim();
            let logged_reason = if raw.is_empty() {
                reason.clone()
            } else {
                reason.replace(raw, &crate::log::redact_url(raw))
            };
            crate::log_ui!(warn, "Blocked unsafe URL open: {}", logged_reason);
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "Blocked unsafe link".to_string(),
                detail: Some(reason),
            });
            None
        }
    }
}

pub fn validate_external_url(url: &str) -> Result<&str, String> {
    parse_external_url(url).map(|_| url.trim())
}

fn parse_external_url(url: &str) -> Result<url::Url, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("URL is empty".to_string());
    }

    let parsed =
        url::Url::parse(trimmed).map_err(|error| format!("invalid URL '{trimmed}': {error}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        scheme => Err(format!(
            "scheme '{scheme}' is not allowed for external links"
        )),
    }
}

fn spawn_browser_incognito(browser: &str, url: &str) -> std::io::Result<()> {
    let mut command = Command::new(browser);
    for arg in incognito_args(browser) {
        command.arg(arg);
    }
    command.arg(url);
    command.spawn().map(|_| ())
}

pub fn incognito_args(browser: &str) -> &'static [&'static str] {
    let lower = browser.to_ascii_lowercase();
    if lower.contains("firefox") {
        &["--private-window"]
    } else if lower.contains("edge") || lower.contains("msedge") {
        &["--inprivate"]
    } else {
        &["--incognito"]
    }
}

#[cfg(target_os = "windows")]
fn platform_incognito_candidates() -> &'static [&'static str] {
    &["msedge", "chrome", "brave", "firefox"]
}

#[cfg(target_os = "macos")]
fn platform_incognito_candidates() -> &'static [&'static str] {
    &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Firefox.app/Contents/MacOS/firefox",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn opened_url_history_keeps_distinct_urls_bounded() {
        let mut history = OpenedUrlHistory::default();
        for index in 0..MAX_SESSION_OPENED_URLS {
            let url = format!("https://example.test/{index}");
            assert_eq!(history.insert(&url), Some(None));
        }
        assert_eq!(
            history.insert(&format!("https://example.test/{MAX_SESSION_OPENED_URLS}")),
            Some(Some("https://example.test/0".to_string()))
        );

        assert_eq!(history.ordered_urls.len(), MAX_SESSION_OPENED_URLS);
        assert!(!history.contains("https://example.test/0"));
        assert!(history.contains(&format!("https://example.test/{MAX_SESSION_OPENED_URLS}")));
    }

    #[test]
    fn external_urls_are_launched_in_serialized_form() {
        assert_eq!(
            parse_external_url(" https://example.test/a b ")
                .unwrap()
                .as_str(),
            "https://example.test/a%20b"
        );
        assert!(parse_external_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn opened_url_history_does_not_duplicate_entries() {
        let mut history = OpenedUrlHistory::default();

        assert_eq!(history.insert("https://example.test/link"), Some(None));
        assert_eq!(history.insert("https://example.test/link"), None);
        assert_eq!(history.ordered_urls.len(), 1);
    }
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn platform_incognito_candidates() -> &'static [&'static str] {
    &[
        "google-chrome",
        "chromium",
        "chromium-browser",
        "brave-browser",
        "microsoft-edge",
        "firefox",
    ]
}
