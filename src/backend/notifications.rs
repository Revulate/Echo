//! Desktop notification service for Echo.
//!
//! Provides native desktop notifications for mentions, whispers, and other events.
//! Only shows notifications when the window is not focused to avoid interruption.
//! Called from irc.rs when mentions/whispers are received.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use super::components::event_notification::{TwitchEvent, TwitchEventType};
use super::event_text::{
    append_event_detail, event_subject, format_twitch_event_text as format_twitch_event_text_core,
    streamer_mode_twitch_event_privacy, twitch_event_title as twitch_event_title_core,
};
use super::lock_utils::MutexExt;
use super::sound_events::sound_for_twitch_event_type;
use super::sounds::SOUND_STORE;
use super::sounds::SoundEventType;
use echo_core::notification_text::truncate_message;

/// Track if the window is currently focused
static WINDOW_FOCUSED: AtomicBool = AtomicBool::new(true);

/// Track if notifications have been initialized
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Track notification permission state
static PERMISSION_GRANTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Default)]
struct NotificationDeliveryState {
    last_backend: Option<String>,
    last_issue: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DesktopNotificationStatus {
    pub supported: bool,
    pub detail: String,
}

#[derive(Debug)]
struct NotificationRequest {
    channel: Option<String>,
    title: String,
    body: String,
}

static NOTIFICATION_TX: OnceLock<mpsc::Sender<NotificationRequest>> = OnceLock::new();
static DELIVERY_STATE: OnceLock<Mutex<NotificationDeliveryState>> = OnceLock::new();

fn delivery_state() -> &'static Mutex<NotificationDeliveryState> {
    DELIVERY_STATE.get_or_init(|| Mutex::new(NotificationDeliveryState::default()))
}

fn notification_tx() -> &'static mpsc::Sender<NotificationRequest> {
    NOTIFICATION_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<NotificationRequest>();
        std::thread::spawn(move || {
            while let Ok(req) = rx.recv() {
                if req
                    .channel
                    .as_deref()
                    .is_some_and(|channel| SOUND_STORE.is_channel_desktop_muted(channel))
                {
                    continue;
                }
                let _ = show_notification_blocking(&req.title, &req.body);
            }
        });
        tx
    })
}

/// Initialize the notification system
pub fn init_notifications() -> bool {
    if INITIALIZED.load(Ordering::Relaxed) {
        return PERMISSION_GRANTED.load(Ordering::Relaxed);
    }

    PERMISSION_GRANTED.store(platform_notifications_supported(), Ordering::Relaxed);
    INITIALIZED.store(true, Ordering::Relaxed);

    crate::log_ui!(
        info,
        "Desktop notifications initialized (provider hint: {})",
        platform_notification_provider_label()
    );
    PERMISSION_GRANTED.load(Ordering::Relaxed)
}

/// Check if window is currently focused
pub fn is_window_focused() -> bool {
    crate::utils::echo_window_is_foreground()
        .unwrap_or_else(|| WINDOW_FOCUSED.load(Ordering::Relaxed))
}

/// Flash the taskbar for a mention or an alerting highlight while Echo is in
/// the background.
pub fn flash_taskbar_for_attention() {
    if crate::backend::streamer_mode::should_suppress_notifications() || is_window_focused() {
        return;
    }
    crate::utils::flash_main_window_taskbar();
}

/// Mention notification data
#[derive(Debug, Clone)]
pub struct MentionNotification {
    pub channel: String,
    pub author: String,
    pub message: String,
}

/// Custom highlight desktop notification data.
#[derive(Debug, Clone)]
pub struct HighlightNotification {
    pub channel: String,
    pub author: String,
    pub message: String,
}

pub(crate) fn desktop_notifications_enabled_for_event(event_type: Option<SoundEventType>) -> bool {
    let settings = SOUND_STORE.settings();
    if !settings.desktop_notifications_enabled {
        return false;
    }

    match event_type {
        Some(event_type) => settings
            .events
            .get(&event_type)
            .map(|event| event.enabled)
            .unwrap_or_else(|| event_type.default_settings().enabled),
        None => true,
    }
}

pub fn desktop_notification_status() -> DesktopNotificationStatus {
    let supported = platform_notifications_supported();
    let initialized = INITIALIZED.load(Ordering::Relaxed);
    let enabled = SOUND_STORE.settings().desktop_notifications_enabled;
    let provider_label = platform_notification_provider_label();
    let last_state = delivery_state().lock_or_recover().clone();

    let detail = if !supported {
        "Desktop notifications are unavailable on this platform.".to_string()
    } else if !enabled {
        format!(
            "Disabled. When enabled, Echo uses {provider_label} and only shows notifications while the window is unfocused."
        )
    } else if let Some(issue) = last_state.last_issue {
        format!(
            "Enabled via {provider_label}. Last delivery issue: {issue}. Notifications only appear while the window is unfocused."
        )
    } else if let Some(backend) = last_state.last_backend {
        format!("Ready via {backend}. Notifications only appear while the window is unfocused.")
    } else if initialized {
        format!("Enabled. Echo will use {provider_label} when the window is unfocused.")
    } else {
        format!("Enabled. Echo will initialize {provider_label} on the first desktop notification.")
    };

    DesktopNotificationStatus { supported, detail }
}

pub fn test_desktop_notification() {
    if !INITIALIZED.load(Ordering::Relaxed) {
        init_notifications();
    }

    send_notification(
        "Echo desktop notifications",
        "Desktop notifications are configured and can be delivered while Echo is unfocused.",
    );
}

/// Send a desktop notification for a mention
pub fn notify_mention(mention: MentionNotification) {
    if !desktop_notifications_enabled_for_event(Some(SoundEventType::Mention)) {
        return;
    }

    if crate::backend::streamer_mode::should_suppress_notifications() {
        return;
    }

    if is_window_focused() {
        return;
    }

    if !INITIALIZED.load(Ordering::Relaxed) {
        init_notifications();
    }

    if !PERMISSION_GRANTED.load(Ordering::Relaxed) {
        return;
    }

    let (title, body) = if crate::backend::streamer_mode::should_hide_mention_content() {
        (
            "Mention received".to_string(),
            "Mention hidden in streamer mode.".to_string(),
        )
    } else {
        (
            format!("Mention in #{}", mention.channel),
            format!(
                "{}: {}",
                mention.author,
                truncate_message(&mention.message, 100)
            ),
        )
    };

    send_channel_notification(&mention.channel, &title, &body);
}

/// Send a desktop notification for a custom highlight rule.
pub fn notify_highlight(highlight: HighlightNotification, event_type: SoundEventType) {
    if !desktop_notifications_enabled_for_event(Some(event_type)) {
        return;
    }

    if crate::backend::streamer_mode::should_suppress_notifications() {
        return;
    }

    if is_window_focused() {
        return;
    }

    if !INITIALIZED.load(Ordering::Relaxed) {
        init_notifications();
    }

    if !PERMISSION_GRANTED.load(Ordering::Relaxed) {
        return;
    }

    let (title, body) = if crate::backend::streamer_mode::should_hide_mention_content() {
        (
            "Highlight received".to_string(),
            "Highlight hidden in streamer mode.".to_string(),
        )
    } else {
        (
            format!("Highlight in #{}", highlight.channel),
            format!(
                "{}: {}",
                highlight.author,
                truncate_message(&highlight.message, 100)
            ),
        )
    };

    send_channel_notification(&highlight.channel, &title, &body);
}

/// Send a desktop notification for a whisper
pub fn notify_whisper(from: &str, message: &str) {
    if !desktop_notifications_enabled_for_event(Some(SoundEventType::Whisper)) {
        return;
    }

    if crate::backend::streamer_mode::should_suppress_notifications() {
        return;
    }

    if is_window_focused() {
        return;
    }

    if !INITIALIZED.load(Ordering::Relaxed) {
        init_notifications();
    }

    if !PERMISSION_GRANTED.load(Ordering::Relaxed) {
        return;
    }

    let (title, body) = if crate::backend::streamer_mode::should_hide_whisper_content() {
        (
            "Whisper received".to_string(),
            "Whisper hidden in streamer mode.".to_string(),
        )
    } else {
        (
            format!("Whisper from {}", from),
            truncate_message(message, 100),
        )
    };

    send_notification(&title, &body);
}

/// Send a desktop notification for a surfaced Twitch EventSub event.
pub fn notify_twitch_event(event: &TwitchEvent) {
    if !desktop_notifications_enabled_for_event(sound_for_twitch_event_type(&event.event_type)) {
        return;
    }

    if crate::backend::streamer_mode::should_suppress_notifications() {
        return;
    }

    if is_window_focused() {
        return;
    }

    if !INITIALIZED.load(Ordering::Relaxed) {
        init_notifications();
    }

    if !PERMISSION_GRANTED.load(Ordering::Relaxed) {
        return;
    }

    let title = format!(
        "{} in #{}",
        twitch_event_title_core(&event.event_type),
        event.channel
    );
    let body = truncate_message(&twitch_event_notification_body(event), 100);

    send_channel_notification(&event.channel, &title, &body);
}

fn twitch_event_notification_body(event: &TwitchEvent) -> String {
    let user = event.user_name.as_deref().unwrap_or("Someone");

    match event.event_type {
        TwitchEventType::Subscribe => {
            let tier = crate::backend::event_text::format_tier(event.tier.as_deref());
            format!("{user} subscribed{tier}!")
        }
        TwitchEventType::Resubscribe => {
            let tier = crate::backend::event_text::format_tier(event.tier.as_deref());
            let months = event
                .cumulative_months
                .map(|m| format!(" for {m} months"))
                .unwrap_or_default();
            format!("{user} resubscribed{tier}{months}!")
        }
        TwitchEventType::GiftSub => {
            let giver = if event.is_anonymous {
                "An anonymous user"
            } else {
                user
            };
            let tier = crate::backend::event_text::format_tier(event.tier.as_deref());
            if let Some(total) = event.total {
                format!("{giver} gifted {total} subs{tier}!")
            } else {
                format!("{giver} gifted a sub{tier}!")
            }
        }
        TwitchEventType::Raid => {
            let from = event.from_channel.as_deref().unwrap_or(user);
            let viewers = event.viewers.unwrap_or(0);
            format!("{from} raided with {viewers} viewers!")
        }
        TwitchEventType::ChannelPointRedemption => {
            let reward = event.reward_title.as_deref().unwrap_or("a reward");
            let cost = event
                .reward_cost
                .map(|c| format!(" ({c} points)"))
                .unwrap_or_default();
            let input = event
                .user_input
                .as_deref()
                .map(str::trim)
                .filter(|input| !input.is_empty())
                .map(|input| format!(": {input}"))
                .unwrap_or_default();
            format!("{user} redeemed {reward}{cost}{input}")
        }
        TwitchEventType::StreamOnline => "Stream is now live.".to_string(),
        TwitchEventType::StreamOffline => "Stream has ended.".to_string(),
        TwitchEventType::Ban
        | TwitchEventType::Unban
        | TwitchEventType::AutomodMessageHold
        | TwitchEventType::AutomodMessageUpdate
        | TwitchEventType::SuspiciousUserMessage
        | TwitchEventType::SuspiciousUserUpdate
        | TwitchEventType::VipAdd
        | TwitchEventType::VipRemove
        | TwitchEventType::ModeratorAdd
        | TwitchEventType::ModeratorRemove
        | TwitchEventType::WarningAcknowledge
        | TwitchEventType::WarningSend
        | TwitchEventType::ShieldModeBegin
        | TwitchEventType::ShieldModeEnd => shared_privacy_event_text(event),
        TwitchEventType::PollBegin => append_event_detail(
            format!(
                "Poll started: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled poll")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PollProgress => append_event_detail(
            format!(
                "Poll update: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled poll")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PollEnd => {
            let status = event.status.as_deref().unwrap_or("ended");
            append_event_detail(
                format!(
                    "Poll ended ({}): \"{}\"",
                    status,
                    event_subject(event.event_title.as_deref(), "Untitled poll")
                ),
                event.event_detail.as_deref(),
            )
        }
        TwitchEventType::PredictionBegin => append_event_detail(
            format!(
                "Prediction started: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled prediction")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PredictionProgress => append_event_detail(
            format!(
                "Prediction update: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled prediction")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PredictionLock => append_event_detail(
            format!(
                "Prediction locked: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled prediction")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PredictionEnd => {
            let status = event.status.as_deref().unwrap_or("ended");
            append_event_detail(
                format!(
                    "Prediction ended ({}): \"{}\"",
                    status,
                    event_subject(event.event_title.as_deref(), "Untitled prediction")
                ),
                event.event_detail.as_deref(),
            )
        }
        TwitchEventType::HypeTrainBegin => append_event_detail(
            "Hype Train started".to_string(),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::HypeTrainProgress => append_event_detail(
            "Hype Train progress".to_string(),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::HypeTrainEnd => append_event_detail(
            "Hype Train ended".to_string(),
            event.event_detail.as_deref(),
        ),
    }
}

fn shared_privacy_event_text(event: &TwitchEvent) -> String {
    format_twitch_event_text_core(event, streamer_mode_twitch_event_privacy())
}

/// Internal function to send a notification
fn send_notification(title: &str, body: &str) {
    queue_notification(None, title, body);
}

fn send_channel_notification(channel: &str, title: &str, body: &str) {
    if !SOUND_STORE.is_channel_desktop_muted(channel) {
        queue_notification(Some(channel.to_string()), title, body);
    }
}

fn queue_notification(channel: Option<String>, title: &str, body: &str) {
    if crate::backend::streamer_mode::should_suppress_notifications() {
        return;
    }

    let _ = notification_tx().send(NotificationRequest {
        channel,
        title: title.to_string(),
        body: body.to_string(),
    });
}

fn show_notification_blocking(title: &str, body: &str) -> bool {
    if crate::backend::streamer_mode::should_suppress_notifications() {
        return false;
    }

    deliver_notification(title, body)
}

#[cfg(target_os = "windows")]
fn deliver_notification(title: &str, body: &str) -> bool {
    use notify_rust::Notification;
    match Notification::new()
        .summary(title)
        .body(body)
        .appname("Echo")
        .show()
    {
        Ok(_) => {
            update_delivery_state(Some("Windows toast notifications"), None);
            true
        }
        Err(e) => {
            let issue = format!("Windows toast delivery failed: {e}");
            update_delivery_state(None, Some(issue.clone()));
            crate::log_ui!(warn, "{}", issue);
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn deliver_notification(title: &str, body: &str) -> bool {
    match std::process::Command::new("osascript")
        .args([
            "-e",
            &format!(
                "display notification \"{}\" with title \"{}\"",
                body.replace("\"", "\\\""),
                title.replace("\"", "\\\"")
            ),
        ])
        .spawn()
    {
        Ok(_) => {
            update_delivery_state(Some("macOS Notification Center"), None);
            true
        }
        Err(e) => {
            let issue = format!("osascript delivery failed: {e}");
            update_delivery_state(None, Some(issue.clone()));
            crate::log_ui!(warn, "{}", issue);
            false
        }
    }
}

#[cfg(target_os = "linux")]
fn deliver_notification(title: &str, body: &str) -> bool {
    use notify_rust::{Hint, Notification};
    // The summary is plain text, but the body is markup. The desktop-entry
    // hint files the notification under Echo's launcher entry, which gives
    // Plasma its icon and per-app notification settings.
    let body = if server_parses_body_markup() {
        echo_core::notification_text::escape_notification_markup(body)
    } else {
        std::borrow::Cow::Borrowed(body)
    };
    match Notification::new()
        .summary(title)
        .body(&body)
        .appname("Echo")
        .hint(Hint::DesktopEntry(crate::identity::APP_ID.to_string()))
        .show()
    {
        Ok(_) => {
            update_delivery_state(Some("notify-rust"), None);
            true
        }
        Err(e) => {
            let notify_rust_error = format!("notify-rust delivery failed: {e}");
            match std::process::Command::new("notify-send")
                .arg("--app-name=Echo")
                .arg(format!(
                    "--hint=string:desktop-entry:{}",
                    crate::identity::APP_ID
                ))
                .arg("--")
                .args([title, body.as_ref()])
                .spawn()
            {
                Ok(_) => {
                    update_delivery_state(
                        Some("notify-send"),
                        Some(format!("{notify_rust_error}; fell back to notify-send")),
                    );
                    crate::log_ui!(debug, "{}", notify_rust_error);
                    true
                }
                Err(fallback_error) => {
                    let issue = format!(
                        "{notify_rust_error}; notify-send fallback failed: {fallback_error}"
                    );
                    update_delivery_state(None, Some(issue.clone()));
                    crate::log_ui!(warn, "{}", issue);
                    false
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn deliver_notification(_title: &str, _body: &str) -> bool {
    update_delivery_state(
        None,
        Some("Unsupported desktop notification platform".to_string()),
    );
    false
}

/// Whether the notification server renders bodies as markup (Plasma, GNOME,
/// dunst, and mako do); one that does not would show escapes literally.
#[cfg(target_os = "linux")]
fn server_parses_body_markup() -> bool {
    static BODY_MARKUP: OnceLock<bool> = OnceLock::new();
    *BODY_MARKUP.get_or_init(|| {
        notify_rust::get_capabilities().map_or(true, |capabilities| {
            capabilities
                .iter()
                .any(|capability| capability == "body-markup")
        })
    })
}

fn update_delivery_state(backend: Option<&str>, issue: Option<String>) {
    let mut state = delivery_state().lock_or_recover();
    state.last_backend = backend.map(str::to_string);
    state.last_issue = issue;
}

fn platform_notifications_supported() -> bool {
    cfg!(target_os = "windows") || cfg!(target_os = "macos") || cfg!(target_os = "linux")
}

fn platform_notification_provider_label() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Windows toast notifications"
    }

    #[cfg(target_os = "macos")]
    {
        "macOS Notification Center"
    }

    #[cfg(target_os = "linux")]
    {
        "notify-rust with notify-send fallback"
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "an unsupported desktop notification provider"
    }
}
