//! Demo Mode - Testing feature for development
//!
//! Loads [`DEMO_CHANNEL`] and tests all major subsystems:
//! - IRC connection
//! - Emote loading (7TV, BTTV, FFZ, Twitch)
//! - Chat history
//! - Badges
//! - Stream info
//!
//! Usage: cargo run -- --demo

use crate::backend::{self, ConnectionStatus, bridge};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Demo channel for testing: Twitch's own developer channel, so the self-test
/// does not single out a streamer.
pub const DEMO_CHANNEL: &str = "twitchdev";

/// Whether demo mode is enabled (set via CLI arg)
static DEMO_MODE: AtomicBool = AtomicBool::new(false);

/// Check if demo mode is enabled
pub fn is_demo_mode() -> bool {
    DEMO_MODE.load(Ordering::Relaxed)
}

/// Enable demo mode
pub fn enable_demo_mode() {
    DEMO_MODE.store(true, Ordering::Relaxed);
    crate::log_ui!(
        info,
        "Demo mode enabled - will test with channel: {}",
        DEMO_CHANNEL
    );
}

/// Demo test results
#[derive(Debug, Default)]
pub struct DemoResults {
    pub channel_id: Option<String>,
    pub irc_connected: bool,
    pub emote_count_7tv: usize,
    pub emote_count_bttv: usize,
    pub emote_count_ffz: usize,
    pub emote_count_twitch: usize,
    pub history_message_count: usize,
    pub badge_count: usize,
    pub stream_live: Option<bool>,
    pub errors: Vec<String>,
    pub duration_ms: u64,
}

impl DemoResults {
    pub fn summary(&self) -> String {
        let mut lines = vec![
            format!("Demo Test Results for #{}", DEMO_CHANNEL),
            "─".repeat(40),
        ];

        if let Some(ref id) = self.channel_id {
            lines.push(format!("Channel ID: {}", id));
        }

        lines.push(format!(
            "IRC Connected: {}",
            if self.irc_connected { "Yes" } else { "No" }
        ));
        lines.push(format!(
            "Emotes: 7TV={}, BTTV={}, FFZ={}, Twitch={}",
            self.emote_count_7tv,
            self.emote_count_bttv,
            self.emote_count_ffz,
            self.emote_count_twitch
        ));
        lines.push(format!(
            "Total Emotes: {}",
            self.emote_count_7tv
                + self.emote_count_bttv
                + self.emote_count_ffz
                + self.emote_count_twitch
        ));
        lines.push(format!("History Messages: {}", self.history_message_count));
        lines.push(format!("Badges Loaded: {}", self.badge_count));

        if let Some(live) = self.stream_live {
            lines.push(format!("Stream Live: {}", if live { "Yes" } else { "No" }));
        }

        lines.push(format!("Duration: {}ms", self.duration_ms));

        if !self.errors.is_empty() {
            lines.push("─".repeat(40));
            lines.push(format!("Errors ({})", self.errors.len()));
            for err in &self.errors {
                lines.push(format!("  - {}", err));
            }
        }

        lines.join("\n")
    }

    pub fn total_emotes(&self) -> usize {
        self.emote_count_7tv
            + self.emote_count_bttv
            + self.emote_count_ffz
            + self.emote_count_twitch
    }

    pub fn is_success(&self) -> bool {
        self.errors.is_empty() && self.irc_connected && self.total_emotes() > 0
    }
}

/// Run all demo tests asynchronously
/// Returns results that can be displayed via toast
pub async fn run_demo_tests() -> DemoResults {
    let start = Instant::now();
    let mut results = DemoResults::default();

    crate::log_ui!(info, "Starting demo tests for #{}", DEMO_CHANNEL);

    crate::log_ui!(info, "Test 1: Fetching channel ID...");
    match test_channel_id().await {
        Ok(id) => {
            crate::log_ui!(info, "Channel ID: {}", id);
            results.channel_id = Some(id);
        }
        Err(e) => {
            crate::log_ui!(error, "Failed to get channel ID: {}", e);
            results.errors.push(format!("Channel ID: {}", e));
        }
    }

    crate::log_ui!(info, "Test 2: Connecting to IRC...");
    match test_irc_connection().await {
        Ok(()) => {
            crate::log_ui!(info, "IRC connection successful");
            results.irc_connected = true;
        }
        Err(e) => {
            crate::log_irc!(error, "IRC connection failed: {}", e);
            results.errors.push(format!("IRC: {}", e));
        }
    }

    crate::log_ui!(info, "Test 3: Loading emotes...");
    if let Some(ref channel_id) = results.channel_id {
        match test_emote_loading(channel_id).await {
            Ok((e7tv, bttv, ffz, twitch)) => {
                crate::log_emotes!(
                    info,
                    "Emotes loaded: 7TV={}, BTTV={}, FFZ={}, Twitch={}",
                    e7tv,
                    bttv,
                    ffz,
                    twitch
                );
                results.emote_count_7tv = e7tv;
                results.emote_count_bttv = bttv;
                results.emote_count_ffz = ffz;
                results.emote_count_twitch = twitch;
            }
            Err(e) => {
                crate::log_emotes!(error, "Emote loading failed: {}", e);
                results.errors.push(format!("Emotes: {}", e));
            }
        }
    }

    crate::log_ui!(info, "Test 4: Loading badges...");
    if let Some(ref channel_id) = results.channel_id {
        match test_badge_loading(channel_id).await {
            Ok(count) => {
                crate::log_cache!(info, "Badges loaded: {}", count);
                results.badge_count = count;
            }
            Err(e) => {
                crate::log_cache!(error, "Badge loading failed: {}", e);
                results.errors.push(format!("Badges: {}", e));
            }
        }
    }

    crate::log_ui!(info, "Test 5: Fetching chat history...");
    match test_chat_history().await {
        Ok(count) => {
            crate::log_irc!(info, "History messages: {}", count);
            results.history_message_count = count;
        }
        Err(e) => {
            crate::log_irc!(error, "History fetch failed: {}", e);
            results.errors.push(format!("History: {}", e));
        }
    }

    crate::log_ui!(info, "Test 6: Checking stream status...");
    match test_stream_info().await {
        Ok(live) => {
            crate::log_ui!(info, "Stream live: {}", live);
            results.stream_live = Some(live);
        }
        Err(e) => {
            crate::log_ui!(warn, "Stream info failed: {}", e);
        }
    }

    results.duration_ms = start.elapsed().as_millis() as u64;

    crate::log_ui!(info, "Demo tests complete in {}ms", results.duration_ms);
    crate::log_ui!(info, "Demo results:\n{}", results.summary());

    results
}

/// Test getting channel ID from Twitch API
async fn test_channel_id() -> Result<String, String> {
    if let Some(id) = bridge::get_channel_id_cached(DEMO_CHANNEL) {
        return Ok(id);
    }

    bridge::get_channel_id(DEMO_CHANNEL).ok_or_else(|| "Failed to resolve channel ID".to_string())
}

/// Test IRC connection
async fn test_irc_connection() -> Result<(), String> {
    bridge::connect_channel(DEMO_CHANNEL);

    std::thread::sleep(Duration::from_secs(2));

    let status = bridge::poll_connection_status();
    match status {
        ConnectionStatus::Connected => Ok(()),
        ConnectionStatus::Reconnecting => {
            std::thread::sleep(Duration::from_secs(3));
            let status = bridge::poll_connection_status();
            match status {
                ConnectionStatus::Connected => Ok(()),
                _ => Err(format!("Still reconnecting after 5s: {:?}", status)),
            }
        }
        ConnectionStatus::Disconnected => Err("Disconnected".to_string()),
    }
}

/// Test emote loading from all providers
async fn test_emote_loading(channel_id: &str) -> Result<(usize, usize, usize, usize), String> {
    bridge::load_channel_emotes(channel_id, DEMO_CHANNEL);

    bridge::load_channel_assets(DEMO_CHANNEL);

    std::thread::sleep(Duration::from_secs(3));

    let emotes = backend::emotes::get_channel_emotes_fast(DEMO_CHANNEL);

    let mut count_7tv = 0;
    let mut count_bttv = 0;
    let mut count_ffz = 0;
    let mut count_twitch = 0;

    for emote in &emotes {
        match emote.provider {
            backend::emotes::EmoteProvider::SevenTV => count_7tv += 1,
            backend::emotes::EmoteProvider::BTTV => count_bttv += 1,
            backend::emotes::EmoteProvider::FFZ => count_ffz += 1,
            backend::emotes::EmoteProvider::Twitch => count_twitch += 1,
            backend::emotes::EmoteProvider::Emoji => {}
        }
    }

    Ok((count_7tv, count_bttv, count_ffz, count_twitch))
}

/// Test badge loading
async fn test_badge_loading(channel_id: &str) -> Result<usize, String> {
    bridge::load_channel_badges(channel_id, DEMO_CHANNEL);

    std::thread::sleep(Duration::from_secs(2));

    let test_badges = ["broadcaster:1", "subscriber:0", "moderator:1", "vip:1"];
    let mut loaded_count = 0;
    for badge_key in &test_badges {
        if bridge::get_badge_url(badge_key, channel_id).is_some() {
            loaded_count += 1;
        }
    }
    Ok(loaded_count)
}

/// Test chat history fetching
async fn test_chat_history() -> Result<usize, String> {
    let result = bridge::get_runtime()
        .block_on(async { backend::history::fetch_recent_messages(DEMO_CHANNEL).await });
    match result {
        Ok(messages) => Ok(messages.len()),
        Err(e) => Err(e),
    }
}

/// Test stream info fetching
async fn test_stream_info() -> Result<bool, String> {
    std::thread::sleep(Duration::from_millis(500));

    bridge::get_stream_info(DEMO_CHANNEL)
        .map(|info| info.is_live)
        .ok_or_else(|| "No stream info available".to_string())
}

/// Initialize demo mode from command line args
pub fn init_from_args() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--demo" || a == "-d") {
        enable_demo_mode();
    }
}
