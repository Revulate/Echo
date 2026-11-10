//! Sound store for event-based audio notifications.
//!
//! Provides sound playback settings for different event types with global and
//! per-event volume controls. Uses rodio for audio playback in Rust.
//! Triggered from irc.rs on mentions, whispers, subs, and raids.

use super::lock_utils::RwLockExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// Embedded default mention sound (49KB OGG file)
/// This ensures sounds work even without external files
static DEFAULT_MENTION_SOUND: &[u8] = include_bytes!("assets/sounds/mention.ogg");

/// Playback request sent to the dedicated sound worker.
enum SoundPlaybackRequest {
    File { path: PathBuf, volume: f32 },
    Embedded { bytes: &'static [u8], volume: f32 },
}

/// Dedicated playback worker channel.
static SOUND_PLAYBACK_TX: OnceLock<mpsc::Sender<SoundPlaybackRequest>> = OnceLock::new();

const SOUND_DEVICE_RETRY_MAX: Duration = Duration::from_secs(30);
const SOUND_DEVICE_STABLE_WINDOW: Duration = Duration::from_secs(2);

struct SoundPlaybackOutput {
    stream: rodio::MixerDeviceSink,
    current: Option<rodio::Player>,
    stream_failed: Arc<AtomicBool>,
    recovery_check_at: Option<Instant>,
}

impl SoundPlaybackOutput {
    fn open() -> Result<Self, String> {
        use rodio::DeviceSinkBuilder;

        let builder = match DeviceSinkBuilder::from_default_device() {
            Ok(builder) => builder,
            Err(error) => {
                return Err(format!("could not get the default output device: {error}"));
            }
        };
        let stream_failed = Arc::new(AtomicBool::new(false));
        let callback_failed = Arc::clone(&stream_failed);
        let mut stream = match builder
            .with_error_callback(move |error| {
                if !callback_failed.swap(true, Ordering::AcqRel) {
                    crate::log_ui!(warn, "Audio output stream failed: {error}");
                }
            })
            .open_stream()
        {
            Ok(stream) => stream,
            Err(error) => {
                return Err(format!("could not open the output stream: {error}"));
            }
        };
        stream.log_on_drop(false);
        Ok(Self {
            stream,
            current: None,
            stream_failed,
            recovery_check_at: Some(Instant::now() + SOUND_DEVICE_STABLE_WINDOW),
        })
    }

    fn has_failed(&self) -> bool {
        self.stream_failed.load(Ordering::Acquire)
    }

    fn replace(&mut self, request: SoundPlaybackRequest) {
        use rodio::{Decoder, Player};

        self.current.take();

        let player = Player::connect_new(self.stream.mixer());
        match request {
            SoundPlaybackRequest::File { path, volume } => {
                let file = match std::fs::File::open(&path) {
                    Ok(file) => file,
                    Err(error) => {
                        crate::log_ui!(
                            warn,
                            "Custom sound file could not open {:?}: {}",
                            path,
                            error
                        );
                        return;
                    }
                };
                let source = match Decoder::new(BufReader::new(file)) {
                    Ok(source) => source,
                    Err(error) => {
                        crate::log_ui!(
                            warn,
                            "Custom sound file could not decode {:?}: {}",
                            path,
                            error
                        );
                        return;
                    }
                };
                player.set_volume(volume.clamp(0.0, 1.0));
                player.append(source);
                crate::log_ui!(debug, "Playing sound: {:?} at volume {}", path, volume);
            }
            SoundPlaybackRequest::Embedded { bytes, volume } => {
                let source = match Decoder::new(Cursor::new(bytes)) {
                    Ok(source) => source,
                    Err(error) => {
                        crate::log_ui!(warn, "Embedded default sound could not decode: {}", error);
                        return;
                    }
                };
                player.set_volume(volume.clamp(0.0, 1.0));
                player.append(source);
                crate::log_ui!(debug, "Playing embedded sound at volume {}", volume);
            }
        }
        self.current = Some(player);
    }
}

struct SoundPlaybackHealth {
    retry_after: Option<Instant>,
    consecutive_device_failures: u32,
    device_warning_shown: bool,
}

impl SoundPlaybackHealth {
    fn can_retry(&self, now: Instant) -> bool {
        self.retry_after
            .is_none_or(|retry_after| now >= retry_after)
    }

    fn note_device_failure(&mut self, detail: String) {
        self.consecutive_device_failures = self.consecutive_device_failures.saturating_add(1);
        let exponent = self.consecutive_device_failures.saturating_sub(1).min(5);
        let retry_delay = Duration::from_secs(1_u64 << exponent).min(SOUND_DEVICE_RETRY_MAX);
        self.retry_after = Some(Instant::now() + retry_delay);

        crate::log_ui!(warn, "Audio output unavailable: {detail}");
        if !self.device_warning_shown {
            self.device_warning_shown = true;
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "Sound notifications are unavailable".to_string(),
                detail: Some(
                    "Echo will retry your audio output when the next sound notification arrives."
                        .to_string(),
                ),
            });
        }
    }

    fn note_active_stream_failure(&mut self) {
        let first_failure = self.consecutive_device_failures == 0;
        self.note_device_failure("the active output stream stopped".to_string());
        // Retry a previously healthy output immediately, but keep backoff if
        // a replacement stream fails before it has proved stable.
        if first_failure {
            self.retry_after = None;
        }
    }

    fn note_device_recovered(&mut self) {
        self.retry_after = None;
        self.consecutive_device_failures = 0;
        self.device_warning_shown = false;
    }
}

fn sound_playback_tx() -> &'static mpsc::Sender<SoundPlaybackRequest> {
    SOUND_PLAYBACK_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<SoundPlaybackRequest>();

        std::thread::spawn(move || {
            let mut output: Option<SoundPlaybackOutput> = None;
            let mut health = SoundPlaybackHealth {
                retry_after: None,
                consecutive_device_failures: 0,
                device_warning_shown: false,
            };
            loop {
                let request = match output.as_ref().and_then(|output| output.recovery_check_at) {
                    Some(deadline) => {
                        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            Ok(request) => Some(request),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    None => match rx.recv() {
                        Ok(request) => Some(request),
                        Err(_) => break,
                    },
                };

                if output.as_ref().is_some_and(SoundPlaybackOutput::has_failed) {
                    output = None;
                    health.note_active_stream_failure();
                } else if let Some(output) = output.as_mut()
                    && output
                        .recovery_check_at
                        .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    output.recovery_check_at = None;
                    health.note_device_recovered();
                }

                let Some(mut request) = request else {
                    continue;
                };
                while let Ok(newest) = rx.try_recv() {
                    request = newest;
                }

                if output.is_none() {
                    if !health.can_retry(Instant::now()) {
                        continue;
                    }
                    match SoundPlaybackOutput::open() {
                        Ok(new_output) => {
                            output = Some(new_output);
                        }
                        Err(error) => {
                            health.note_device_failure(error);
                            continue;
                        }
                    }
                }
                let Some(output) = output.as_mut() else {
                    continue;
                };

                while let Ok(newest) = rx.try_recv() {
                    request = newest;
                }
                output.replace(request);
            }
        });

        tx
    })
}

/// Sound event types that can trigger audio
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundEventType {
    Mention,
    Whisper,
    HighlightedMessage,
    FirstMessage,
    ReturningChatter,
    Subscription,
    Raid,
    BitsBadge,
    Announcement,
    Phrase,
    ChannelPointRedeem,
    StreamOnline,
    StreamOffline,
}

impl SoundEventType {
    /// Get all sound event types
    pub fn all() -> &'static [SoundEventType] {
        &[
            SoundEventType::Mention,
            SoundEventType::Whisper,
            SoundEventType::HighlightedMessage,
            SoundEventType::FirstMessage,
            SoundEventType::ReturningChatter,
            SoundEventType::Subscription,
            SoundEventType::Raid,
            SoundEventType::BitsBadge,
            SoundEventType::Announcement,
            SoundEventType::Phrase,
            SoundEventType::ChannelPointRedeem,
            SoundEventType::StreamOnline,
            SoundEventType::StreamOffline,
        ]
    }

    /// Get display name for the event type
    pub fn display_name(&self) -> &'static str {
        match self {
            SoundEventType::Mention => "Mention",
            SoundEventType::Whisper => "Whisper",
            SoundEventType::HighlightedMessage => "Highlighted Message",
            SoundEventType::FirstMessage => "First Message",
            SoundEventType::ReturningChatter => "Returning Chatter",
            SoundEventType::Subscription => "Subscription",
            SoundEventType::Raid => "Raid",
            SoundEventType::BitsBadge => "Bits Badge",
            SoundEventType::Announcement => "Announcement",
            SoundEventType::Phrase => "Phrase Match",
            SoundEventType::ChannelPointRedeem => "Channel Point Redeem",
            SoundEventType::StreamOnline => "Stream Online",
            SoundEventType::StreamOffline => "Stream Offline",
        }
    }

    pub fn default_settings(&self) -> SoundEventSettings {
        SoundEventSettings {
            enabled: matches!(self, SoundEventType::Mention),
            volume: match self {
                SoundEventType::FirstMessage
                | SoundEventType::ReturningChatter
                | SoundEventType::BitsBadge
                | SoundEventType::StreamOffline => 0.5,
                SoundEventType::ChannelPointRedeem => 0.6,
                SoundEventType::Announcement | SoundEventType::StreamOnline => 0.7,
                SoundEventType::Subscription | SoundEventType::Raid => 0.8,
                SoundEventType::Mention
                | SoundEventType::Whisper
                | SoundEventType::HighlightedMessage
                | SoundEventType::Phrase => 1.0,
            },
            custom_sound_path: None,
        }
    }
}

/// Settings for a single sound event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundEventSettings {
    pub enabled: bool,
    pub volume: f32,
    pub custom_sound_path: Option<String>,
}

impl Default for SoundEventSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            volume: 1.0,
            custom_sound_path: None,
        }
    }
}

/// All sound settings
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundSettings {
    pub global_enabled: bool,
    pub global_volume: f32,
    pub desktop_notifications_enabled: bool,
    #[serde(default)]
    pub muted_channels: Vec<String>,
    #[serde(default)]
    pub desktop_muted_channels: Vec<String>,
    pub events: HashMap<SoundEventType, SoundEventSettings>,
}

impl Default for SoundSettings {
    fn default() -> Self {
        let mut events = HashMap::new();

        events.insert(
            SoundEventType::Mention,
            SoundEventSettings {
                enabled: true,
                volume: 1.0,
                custom_sound_path: None,
            },
        );

        events.insert(
            SoundEventType::Whisper,
            SoundEventSettings {
                enabled: false,
                volume: 1.0,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::HighlightedMessage,
            SoundEventSettings {
                enabled: false,
                volume: 1.0,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::FirstMessage,
            SoundEventSettings {
                enabled: false,
                volume: 0.5,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::ReturningChatter,
            SoundEventSettings {
                enabled: false,
                volume: 0.5,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::Subscription,
            SoundEventSettings {
                enabled: false,
                volume: 0.8,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::Raid,
            SoundEventSettings {
                enabled: false,
                volume: 0.8,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::BitsBadge,
            SoundEventSettings {
                enabled: false,
                volume: 0.5,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::Announcement,
            SoundEventSettings {
                enabled: false,
                volume: 0.7,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::Phrase,
            SoundEventSettings {
                enabled: false,
                volume: 1.0,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::ChannelPointRedeem,
            SoundEventSettings {
                enabled: false,
                volume: 0.6,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::StreamOnline,
            SoundEventSettings {
                enabled: false,
                volume: 0.7,
                custom_sound_path: None,
            },
        );
        events.insert(
            SoundEventType::StreamOffline,
            SoundEventSettings {
                enabled: false,
                volume: 0.5,
                custom_sound_path: None,
            },
        );

        Self {
            global_enabled: true,
            global_volume: 0.7,
            desktop_notifications_enabled: false,
            muted_channels: Vec::new(),
            desktop_muted_channels: Vec::new(),
            events,
        }
    }
}

fn normalize_sound_channel(channel: &str) -> Option<String> {
    let channel = channel.trim().trim_start_matches('#').to_lowercase();
    (!channel.is_empty()).then_some(channel)
}

fn normalize_sound_settings(mut settings: SoundSettings) -> SoundSettings {
    settings.global_volume = settings.global_volume.clamp(0.0, 1.0);
    let mut seen = HashSet::new();
    settings.muted_channels = settings
        .muted_channels
        .into_iter()
        .filter_map(|channel| normalize_sound_channel(&channel))
        .filter(|channel| seen.insert(channel.clone()))
        .collect();
    settings.muted_channels.sort();
    let mut seen = HashSet::new();
    settings.desktop_muted_channels = settings
        .desktop_muted_channels
        .into_iter()
        .filter_map(|channel| normalize_sound_channel(&channel))
        .filter(|channel| settings.muted_channels.contains(channel) && seen.insert(channel.clone()))
        .collect();
    settings.desktop_muted_channels.sort();

    for event_type in SoundEventType::all() {
        let defaults = event_type.default_settings();
        let event = settings
            .events
            .entry(*event_type)
            .or_insert_with(|| defaults.clone());
        event.volume = event.volume.clamp(0.0, 1.0);
        event.custom_sound_path = event
            .custom_sound_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
    }

    settings
}

/// Copy a pre-config-root `sounds.json` from the working directory into the
/// config root once. The legacy file is left in place.
fn migrate_legacy_sound_settings(file_path: &std::path::Path) {
    let Some(legacy_path) = crate::benchmark_profile::legacy_cwd_file("sounds.json") else {
        return;
    };
    if file_path.exists() || !legacy_path.is_file() {
        return;
    }
    let copied = file_path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::copy(&legacy_path, file_path));
    match copied {
        Ok(_) => crate::log_persist!(
            info,
            "Migrated sound settings from '{}' to '{}'",
            legacy_path.display(),
            file_path.display()
        ),
        Err(error) => crate::log_persist!(
            error,
            "Could not migrate sound settings from '{}' to '{}': {error}",
            legacy_path.display(),
            file_path.display()
        ),
    }
}

/// Sound store manager
pub struct SoundStore {
    settings: RwLock<SoundSettings>,
    file_path: PathBuf,
    /// The file on disk failed to load and could not be copied aside, so it
    /// must not be overwritten yet.
    overwrite_blocked: AtomicBool,
}

impl SoundStore {
    fn new() -> Self {
        let file_path = crate::benchmark_profile::sound_settings_path();
        migrate_legacy_sound_settings(&file_path);
        let loaded = match fs::read_to_string(&file_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(settings) => Ok(settings),
                Err(error) => Err(crate::utils::json_error_summary(&error)),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(SoundSettings::default())
            }
            Err(error) => Err(error.to_string()),
        };
        let (settings, overwrite_blocked) = match loaded {
            Ok(settings) => (settings, false),
            Err(error) => {
                crate::log_persist!(
                    error,
                    "Sound settings '{}' are unloadable ({}); using defaults",
                    file_path.display(),
                    error
                );
                let preserved =
                    crate::utils::preserve_unloadable_file(&file_path, "sound settings");
                (SoundSettings::default(), !preserved)
            }
        };
        let settings = normalize_sound_settings(settings);

        Self {
            settings: RwLock::new(settings),
            file_path,
            overwrite_blocked: AtomicBool::new(overwrite_blocked),
        }
    }

    /// Get current settings (clone)
    pub fn settings(&self) -> SoundSettings {
        self.settings.read_or_recover().clone()
    }

    /// Save settings to disk
    fn save(&self) {
        if crate::benchmark_ui_fixture::should_suppress_persistence() {
            return;
        }
        if self.overwrite_blocked.load(Ordering::Acquire) {
            if !crate::utils::preserve_unloadable_file(&self.file_path, "sound settings") {
                return;
            }
            self.overwrite_blocked.store(false, Ordering::Release);
        }
        let settings = self.settings.read_or_recover().clone();
        match serde_json::to_string_pretty(&settings) {
            Ok(content) => {
                if let Err(e) = crate::config::atomic_write_json(&self.file_path, &content) {
                    tracing::error!("Failed to save sound settings: {e}");
                }
            }
            Err(e) => {
                tracing::error!("Failed to serialize sound settings: {e}");
            }
        }
    }

    /// Restore the exact model captured by the disposable feedback fixture.
    pub fn restore_settings_for_benchmark(&self, settings: SoundSettings) -> bool {
        if !crate::benchmark_ui_fixture::feedback_ui_enabled() {
            return false;
        }
        let mut current = self.settings.write_or_recover();
        *current = settings;
        true
    }

    pub fn file_bytes_for_benchmark(&self) -> Option<Vec<u8>> {
        crate::benchmark_ui_fixture::feedback_ui_enabled()
            .then(|| fs::read(&self.file_path).ok())
            .flatten()
    }

    pub fn restore_file_bytes_for_benchmark(&self, bytes: Option<&[u8]>) -> bool {
        if !crate::benchmark_ui_fixture::feedback_ui_enabled() {
            return false;
        }
        match bytes {
            Some(bytes) => fs::write(&self.file_path, bytes).is_ok(),
            None if self.file_path.exists() => fs::remove_file(&self.file_path).is_ok(),
            None => true,
        }
    }

    /// Play sound for a specific event type
    pub fn play_sound(&self, event_type: SoundEventType) {
        if event_type == SoundEventType::Mention
            && crate::backend::streamer_mode::should_mute_mentions()
        {
            return;
        }

        let settings = self.settings.read_or_recover().clone();

        if !settings.global_enabled {
            return;
        }

        let event_settings = settings
            .events
            .get(&event_type)
            .cloned()
            .unwrap_or_else(|| event_type.default_settings());

        if !event_settings.enabled {
            return;
        }

        let final_volume = settings.global_volume * event_settings.volume;

        if let Some(custom_path) = local_custom_sound_path(&event_settings) {
            let path = PathBuf::from(custom_path);
            if path.exists() {
                let _ = sound_playback_tx().send(SoundPlaybackRequest::File {
                    path,
                    volume: final_volume,
                });
                return;
            }
        }

        let _ = sound_playback_tx().send(SoundPlaybackRequest::Embedded {
            bytes: DEFAULT_MENTION_SOUND,
            volume: final_volume,
        });
    }

    /// Play a sound unless highlight sounds are muted for this channel.
    pub fn play_sound_for_channel(&self, event_type: SoundEventType, channel: &str) {
        if self.is_channel_muted(channel) {
            return;
        }
        self.play_sound(event_type);
    }

    /// Play a per-rule custom sound path, falling back to the event sound if the file is missing.
    pub fn play_custom_sound_for_channel(
        &self,
        custom_path: &str,
        fallback_event_type: SoundEventType,
        channel: &str,
    ) {
        if fallback_event_type == SoundEventType::Mention
            && crate::backend::streamer_mode::should_mute_mentions()
        {
            return;
        }

        if self.is_channel_muted(channel) {
            return;
        }

        let settings = self.settings.read_or_recover().clone();

        if !settings.global_enabled {
            return;
        }

        let event_settings = settings
            .events
            .get(&fallback_event_type)
            .cloned()
            .unwrap_or_else(|| fallback_event_type.default_settings());

        if !event_settings.enabled {
            return;
        }

        let final_volume = settings.global_volume * event_settings.volume;
        let path = PathBuf::from(custom_path.trim().trim_matches('"'));
        if path.exists() {
            let _ = sound_playback_tx().send(SoundPlaybackRequest::File {
                path,
                volume: final_volume,
            });
            return;
        }

        self.play_sound(fallback_event_type);
    }

    pub fn is_channel_muted(&self, channel: &str) -> bool {
        let Some(channel) = normalize_sound_channel(channel) else {
            return false;
        };
        self.settings
            .read_or_recover()
            .muted_channels
            .iter()
            .any(|muted| muted == &channel)
    }

    pub fn set_channel_muted(&self, channel: &str, muted: bool) -> bool {
        let Some(channel) = normalize_sound_channel(channel) else {
            return false;
        };

        {
            let mut settings = self.settings.write_or_recover();
            let settings = &mut *settings;
            for channels in [
                &mut settings.muted_channels,
                &mut settings.desktop_muted_channels,
            ] {
                if muted {
                    if !channels.contains(&channel) {
                        channels.push(channel.clone());
                        channels.sort();
                    }
                } else {
                    channels.retain(|existing| existing != &channel);
                }
            }
        }
        self.save();
        muted
    }

    pub fn is_channel_desktop_muted(&self, channel: &str) -> bool {
        let Some(channel) = normalize_sound_channel(channel) else {
            return false;
        };
        self.settings
            .read_or_recover()
            .desktop_muted_channels
            .contains(&channel)
    }

    pub fn toggle_channel_muted(&self, channel: &str) -> bool {
        let muted = !self.is_channel_muted(channel);
        self.set_channel_muted(channel, muted)
    }

    pub fn clear_muted_channels(&self) {
        {
            let mut settings = self.settings.write_or_recover();
            settings.muted_channels.clear();
            settings.desktop_muted_channels.clear();
        }
        self.save();
    }

    /// Test play a sound (ignores enabled state, respects volume)
    pub fn test_sound(&self, event_type: SoundEventType) {
        let settings = self.settings.read_or_recover().clone();

        let event_settings = settings
            .events
            .get(&event_type)
            .cloned()
            .unwrap_or_else(|| event_type.default_settings());
        let final_volume = settings.global_volume * event_settings.volume;

        if let Some(custom_path) = local_custom_sound_path(&event_settings) {
            let path = PathBuf::from(custom_path);
            if path.exists() {
                let _ = sound_playback_tx().send(SoundPlaybackRequest::File {
                    path,
                    volume: final_volume,
                });
                return;
            }
        }

        let _ = sound_playback_tx().send(SoundPlaybackRequest::Embedded {
            bytes: DEFAULT_MENTION_SOUND,
            volume: final_volume,
        });
    }

    /// Test play a highlight rule's sound file at the fallback event's volume,
    /// ignoring enabled state like [`Self::test_sound`]. A missing file plays
    /// the fallback event's sound, as live highlights do.
    pub fn test_custom_sound(&self, custom_path: &str, fallback_event_type: SoundEventType) {
        let custom_path = custom_path.trim().trim_matches('"');
        if !custom_path.is_empty()
            && !crate::backend::settings::is_remote_or_device_path(custom_path)
        {
            let path = PathBuf::from(custom_path);
            if path.exists() {
                let settings = self.settings.read_or_recover().clone();
                let event_settings = settings
                    .events
                    .get(&fallback_event_type)
                    .cloned()
                    .unwrap_or_else(|| fallback_event_type.default_settings());
                let _ = sound_playback_tx().send(SoundPlaybackRequest::File {
                    path,
                    volume: settings.global_volume * event_settings.volume,
                });
                return;
            }
        }
        self.test_sound(fallback_event_type);
    }

    /// Set global enabled state
    pub fn set_global_enabled(&self, enabled: bool) {
        self.settings.write_or_recover().global_enabled = enabled;
        self.save();
    }

    /// Set global volume
    pub fn set_global_volume(&self, volume: f32) {
        self.settings.write_or_recover().global_volume = volume.clamp(0.0, 1.0);
        self.save();
    }

    /// Set desktop notifications enabled
    pub fn set_desktop_notifications_enabled(&self, enabled: bool) {
        self.settings
            .write_or_recover()
            .desktop_notifications_enabled = enabled;
        self.save();
    }

    /// Set event enabled state
    pub fn set_event_enabled(&self, event_type: SoundEventType, enabled: bool) {
        self.settings
            .write_or_recover()
            .events
            .entry(event_type)
            .or_insert_with(|| event_type.default_settings())
            .enabled = enabled;
        self.save();
    }

    /// Set event volume
    pub fn set_event_volume(&self, event_type: SoundEventType, volume: f32) {
        self.settings
            .write_or_recover()
            .events
            .entry(event_type)
            .or_insert_with(|| event_type.default_settings())
            .volume = volume.clamp(0.0, 1.0);
        self.save();
    }
}

/// The event's custom sound file, unless it names a network share or device:
/// probing one makes Windows authenticate to that host. `sounds.json` has no
/// validating setter, so the check happens where the path is opened.
fn local_custom_sound_path(settings: &SoundEventSettings) -> Option<&str> {
    settings
        .custom_sound_path
        .as_deref()
        .filter(|path| !crate::backend::settings::is_remote_or_device_path(path))
}

/// Global sound store instance
pub static SOUND_STORE: LazyLock<SoundStore> = LazyLock::new(SoundStore::new);

#[cfg(test)]
mod tests {
    use super::{SoundSettings, normalize_sound_settings};
    use core::prelude::v1::test;

    #[test]
    fn legacy_sound_mutes_do_not_silently_mute_desktop_notifications() {
        let mut saved = serde_json::to_value(SoundSettings::default()).unwrap();
        saved
            .as_object_mut()
            .unwrap()
            .remove("desktop_muted_channels");
        saved["muted_channels"] = serde_json::json!(["#SomeChannel", "somechannel"]);
        let mut settings = normalize_sound_settings(serde_json::from_value(saved).unwrap());
        assert_eq!(settings.muted_channels, ["somechannel"]);
        assert!(settings.desktop_muted_channels.is_empty());

        settings.desktop_muted_channels = vec!["#SOMECHANNEL".into(), "unknown".into()];
        let settings = normalize_sound_settings(settings);
        assert_eq!(settings.desktop_muted_channels, ["somechannel"]);
    }
}
