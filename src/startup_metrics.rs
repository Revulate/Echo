//! One-shot startup lifecycle markers shared by the binary and renderer.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static CLOCK: OnceLock<Instant> = OnceLock::new();
static PROCESS_CREATED: AtomicBool = AtomicBool::new(false);
static PREWARM_JOINED: AtomicBool = AtomicBool::new(false);
static PREWARM_DETACHED: AtomicBool = AtomicBool::new(false);
static GPUI_HTTP_CLIENT_READY: AtomicBool = AtomicBool::new(false);
static USER_COLORS_LOADED: AtomicBool = AtomicBool::new(false);
static SETTINGS_LOADED: AtomicBool = AtomicBool::new(false);
static BACKEND_INITIALIZED: AtomicBool = AtomicBool::new(false);
static CACHES_INITIALIZED: AtomicBool = AtomicBool::new(false);
static THEME_INITIALIZED: AtomicBool = AtomicBool::new(false);
static MAIN_WINDOW_OPENING: AtomicBool = AtomicBool::new(false);
static MAIN_WINDOW_CREATED: AtomicBool = AtomicBool::new(false);
static FIRST_ROOT_RENDER: AtomicBool = AtomicBool::new(false);
static LAYOUT_RESTORED: AtomicBool = AtomicBool::new(false);
static CHAT_VIEW_USABLE: AtomicBool = AtomicBool::new(false);
static TEXT_PAINT: AtomicBool = AtomicBool::new(false);
static VISIBLE_EMOTE_PAINT: AtomicBool = AtomicBool::new(false);
static VISIBLE_EMOTES_COMPLETE: AtomicBool = AtomicBool::new(false);
static NETWORK_FALLBACK_COMPLETE: AtomicBool = AtomicBool::new(false);
static INITIAL_CATALOGS_READY: AtomicBool = AtomicBool::new(false);
static CACHED_HISTORY_COMMITTED: AtomicBool = AtomicBool::new(false);

/// Initialize the monotonic startup clock at process entry.
pub fn initialize_clock() {
    CLOCK.get_or_init(Instant::now);
}

/// Emit a one-shot startup boundary owned by production.
pub fn record(marker: &'static str) {
    let slot = match marker {
        "process_created" => &PROCESS_CREATED,
        "prewarm_joined" => &PREWARM_JOINED,
        "prewarm_detached" => &PREWARM_DETACHED,
        "gpui_http_client_ready" => &GPUI_HTTP_CLIENT_READY,
        "user_colors_loaded" => &USER_COLORS_LOADED,
        "settings_loaded" => &SETTINGS_LOADED,
        "backend_initialized" => &BACKEND_INITIALIZED,
        "caches_initialized" => &CACHES_INITIALIZED,
        "theme_initialized" => &THEME_INITIALIZED,
        "main_window_opening" => &MAIN_WINDOW_OPENING,
        "main_window_created" => &MAIN_WINDOW_CREATED,
        "first_root_render" => &FIRST_ROOT_RENDER,
        "layout_restored" => &LAYOUT_RESTORED,
        "first_chat_view_usable" => &CHAT_VIEW_USABLE,
        "first_text_paint" => &TEXT_PAINT,
        "first_visible_emote_paint" => &VISIBLE_EMOTE_PAINT,
        "visible_emotes_complete" => &VISIBLE_EMOTES_COMPLETE,
        "startup_network_fallback_complete" => &NETWORK_FALLBACK_COMPLETE,
        "initial_catalogs_ready" => &INITIAL_CATALOGS_READY,
        "cached_history_committed" => &CACHED_HISTORY_COMMITTED,
        _ => return,
    };
    if slot.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = CLOCK.get_or_init(Instant::now);
    tracing::info!(
        target: "echo.startup",
        marker,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "startup_marker"
    );
}
