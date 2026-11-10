//! Polling, EventSub, embeds, and metrics
//!
//! Handles event polling, Twitch EventSub subscriptions, embed metadata,
//! link info, performance metrics, and window focus.

use super::{ConnectionStatus, PredictionOutcomeSnapshot, RUNTIME, TwitchEvent, TwitchEventType};
use crate::backend::event_text::{
    format_twitch_event_text as format_twitch_event_text_core, streamer_mode_twitch_event_privacy,
    twitch_event_title as twitch_event_title_core,
};
use crate::backend::filters::{FILTER_STORE, FilterAction, TestContext};
use crate::backend::lock_utils::MutexExt;
use crate::backend::models::HighlightType;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// Flag indicating if a version update task is already scheduled
static VERSION_UPDATE_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Pending version increment count (batched)
static PENDING_VERSION_INCREMENTS: AtomicU32 = AtomicU32::new(0);

static EMBED_CAPACITY_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);

static EMBED_METADATA_UPDATES: LazyLock<
    tokio::sync::broadcast::Sender<crate::backend::link_types::ResourceKey>,
> = LazyLock::new(|| tokio::sync::broadcast::channel(64).0);

pub fn subscribe_embed_metadata_updates()
-> tokio::sync::broadcast::Receiver<crate::backend::link_types::ResourceKey> {
    EMBED_METADATA_UPDATES.subscribe()
}

pub fn hover_link_metadata_allowed(url: &str) -> bool {
    if !crate::backend::settings::get_embed_remote_fetch_enabled()
        || matches!(
            crate::backend::settings::get_embed_display_mode(),
            crate::backend::settings::EmbedDisplayMode::None
        )
        || crate::backend::streamer_mode::should_hide_link_urls()
        || crate::backend::upload_history::is_known_upload_deletion_url(url)
        || !crate::backend::embeds::fetchers::is_public_fetch_url(url)
    {
        return false;
    }
    let classified = crate::backend::link_types::classify_link(url);
    crate::backend::settings::is_embed_classification_enabled(
        classified.provider,
        classified.content_kind,
    )
}

pub fn request_hover_link_metadata(
    url: Arc<str>,
    occurrence: crate::backend::embeds::state::PreviewOccurrence,
) {
    if hover_link_metadata_allowed(&url) {
        request_embed_metadata(
            url,
            occurrence,
            crate::backend::embeds::cache::FlightDemand::Always,
        );
    }
}

/// Schedule a batched version update (debounced to 100ms)
/// PERF: Prevents layout thrashing by batching rapid embed completions
fn schedule_version_update() {
    PENDING_VERSION_INCREMENTS.fetch_add(1, Ordering::Relaxed);

    if VERSION_UPDATE_SCHEDULED
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        RUNTIME.spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            VERSION_UPDATE_SCHEDULED.store(false, Ordering::Release);
            let pending = PENDING_VERSION_INCREMENTS.swap(0, Ordering::Relaxed);

            if pending > 0 {
                crate::state::VersionRegistry::bump_embed_metadata_version();
                let new_version = crate::state::VersionRegistry::embed_metadata_version();
                crate::log_ui!(
                    info,
                    "Batched version update: {} fetches completed, version now {}",
                    pending,
                    new_version
                );
            }

            if PENDING_VERSION_INCREMENTS.load(Ordering::Acquire) > 0 {
                schedule_version_update();
            }
        });
    }
}

/// Request metadata for an occurrence. Only a started flight spawns physical
/// work; joins and deferred work retain occurrence-local subscribers.
pub fn fetch_embed_metadata_async(
    url: impl Into<Arc<str>>,
    message_id: impl Into<Arc<str>>,
    embed_id: u32,
) -> crate::backend::embeds::cache::FlightAdmissionKind {
    let url = url.into();
    let occurrence = crate::backend::embeds::state::PreviewOccurrence {
        message_id: message_id.into().to_string(),
        embed_id,
        resource_key: crate::backend::link_types::ResourceKey::for_url(url.as_ref()),
    };
    request_embed_metadata(
        url,
        occurrence,
        crate::backend::embeds::cache::FlightDemand::Always,
    )
}

pub fn request_embed_metadata(
    url: impl Into<Arc<str>>,
    occurrence: crate::backend::embeds::state::PreviewOccurrence,
    demand: crate::backend::embeds::cache::FlightDemand,
) -> crate::backend::embeds::cache::FlightAdmissionKind {
    let url = url.into();
    if occurrence.embed_id == crate::backend::upload_history::UPLOAD_LINK_NO_PREVIEW
        || crate::backend::upload_history::is_known_upload_deletion_url(url.as_ref())
    {
        return crate::backend::embeds::cache::FlightAdmissionKind::RejectedCapacity;
    }
    let classified = crate::backend::link_types::classify_link(url.as_ref());
    if occurrence.message_id.is_empty()
        || !crate::backend::settings::is_embed_classification_enabled(
            classified.provider,
            classified.content_kind,
        )
    {
        return crate::backend::embeds::cache::FlightAdmissionKind::RejectedCapacity;
    }
    if crate::backend::embeds::cache::EMBED_CACHE
        .get(url.as_ref())
        .is_some()
    {
        return crate::backend::embeds::cache::FlightAdmissionKind::Joined;
    }
    let admission = crate::backend::embeds::cache::RESOURCE_FLIGHTS.request(
        occurrence.resource_key.clone(),
        url.to_string(),
        occurrence,
        demand,
    );
    match admission.kind {
        crate::backend::embeds::cache::FlightAdmissionKind::Started => {
            EMBED_CAPACITY_WARNING_EMITTED.store(false, Ordering::Release);
            if let Some(work) = admission.work {
                spawn_embed_flight(work);
            }
        }
        crate::backend::embeds::cache::FlightAdmissionKind::Deferred
        | crate::backend::embeds::cache::FlightAdmissionKind::Joined => {}
        crate::backend::embeds::cache::FlightAdmissionKind::RejectedCapacity
            if demand == crate::backend::embeds::cache::FlightDemand::Manual
                && !EMBED_CAPACITY_WARNING_EMITTED.swap(true, Ordering::AcqRel) =>
        {
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "Link preview queue is full".to_string(),
                detail: Some("Try Show Preview again after pending previews finish.".to_string()),
            });
        }
        crate::backend::embeds::cache::FlightAdmissionKind::RejectedCapacity => {}
    }
    admission.kind
}

fn spawn_embed_flight(work: crate::backend::embeds::cache::FlightWork) {
    RUNTIME.spawn(async move {
        let mut cleanup = EmbedFlightCleanup::new(work.clone());
        if !work.is_current() {
            return;
        }
        let fetched = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            crate::backend::embeds::fetch_embed_metadata(&work.url),
        )
        .await
        .unwrap_or_else(|_| crate::backend::embeds::fetchers::failed_embed_metadata(&work.url));

        if !work.is_current() {
            return;
        }

        let thumbnail = fetched
            .thumbnail_url
            .as_ref()
            .filter(|url| {
                !crate::backend::streamer_mode::should_hide_link_thumbnails()
                    && crate::backend::embed_thumbnails::should_render_thumbnail(
                        crate::backend::settings::get_embed_thumbnail_mode(),
                        Some(url),
                    )
                    && crate::backend::embeds::fetchers::is_public_fetch_url(url)
                    && !crate::cache::loader::has_cached_render_image(url)
            })
            .cloned();
        let expected_metadata = fetched.clone();
        let proxy_generation = crate::backend::http::proxy_generation();
        let completion = cleanup.complete(fetched);
        for next in completion.next {
            spawn_embed_flight(next);
        }
        if completion.stale {
            return;
        }
        for occurrence in &completion.subscribers {
            queue_embed_layout_invalidation(occurrence.message_id.clone());
        }
        let _ = completion.overflowed;
        schedule_version_update();
        let _ = EMBED_METADATA_UPDATES.send(work.resource_key.clone());

        if let Some(thumbnail) = thumbnail {
            RUNTIME.spawn(async move {
                if !crate::cache::loader::ensure_render_image_cached(
                    thumbnail.clone(),
                    crate::cache::loader::PreloadSource::EmbedThumbnail,
                )
                .await
                {
                    return;
                }
                // The metadata flight is complete; fence the repaint against current
                // cache policy and disclosure instead of its former active-flight id.
                if crate::backend::http::proxy_generation() != proxy_generation
                    || crate::backend::streamer_mode::should_hide_link_urls()
                    || crate::backend::streamer_mode::should_hide_link_thumbnails()
                    || !crate::backend::embed_thumbnails::should_render_thumbnail(
                        crate::backend::settings::get_embed_thumbnail_mode(),
                        Some(&thumbnail),
                    )
                    || get_embed_metadata(&work.url).as_ref() != Some(&expected_metadata)
                {
                    return;
                }
                let mut changed = false;
                for occurrence in completion.subscribers {
                    if occurrence.resource_key != work.resource_key {
                        continue;
                    }
                    let disclosed = match crate::backend::settings::get_embed_display_mode() {
                        crate::backend::settings::EmbedDisplayMode::None => false,
                        crate::backend::settings::EmbedDisplayMode::Click => {
                            crate::backend::embeds::state::is_embed_expanded(&occurrence)
                        }
                        crate::backend::settings::EmbedDisplayMode::Always => true,
                    };
                    if disclosed {
                        queue_embed_layout_invalidation(occurrence.message_id);
                        changed = true;
                    }
                }
                if changed {
                    schedule_version_update();
                }
                let _ = EMBED_METADATA_UPDATES.send(work.resource_key);
            });
        }
    });
}

/// Releases a registry slot on every future exit path, including Tokio task
/// cancellation/panic. Normal publication consumes it explicitly to obtain
/// the subscribers that must be invalidated.
struct EmbedFlightCleanup {
    work: Option<crate::backend::embeds::cache::FlightWork>,
}

impl EmbedFlightCleanup {
    fn new(work: crate::backend::embeds::cache::FlightWork) -> Self {
        Self { work: Some(work) }
    }

    fn complete(
        &mut self,
        metadata: crate::backend::link_types::EmbedMetadata,
    ) -> crate::backend::embeds::cache::FlightCompletion {
        let work = self.work.take().expect("flight cleanup used once");
        crate::backend::embeds::cache::RESOURCE_FLIGHTS.complete(&work, metadata)
    }
}

impl Drop for EmbedFlightCleanup {
    fn drop(&mut self) {
        let Some(work) = self.work.take() else {
            return;
        };
        let completion = crate::backend::embeds::cache::RESOURCE_FLIGHTS.finish(&work);
        for next in completion.next {
            spawn_embed_flight(next);
        }
    }
}

/// Explicitly disclose one preview occurrence. Click mode reaches this only
/// from the link context menu; ordinary left-click keeps its external-link
/// confirmation behavior.
pub fn show_embed_preview(url: impl Into<Arc<str>>, message_id: impl Into<String>, embed_id: u32) {
    let url = url.into();
    let message_id = message_id.into();
    if message_id.is_empty() || crate::backend::streamer_mode::should_hide_link_urls() {
        return;
    }
    let classified = crate::backend::link_types::classify_link(url.as_ref());
    if !crate::backend::settings::is_embed_classification_enabled(
        classified.provider,
        classified.content_kind,
    ) {
        return;
    }
    let occurrence = crate::backend::embeds::state::PreviewOccurrence {
        message_id: message_id.clone(),
        embed_id,
        resource_key: classified.resource_key,
    };
    show_embed_preview_occurrence(url, occurrence);
}

/// Same disclosure boundary as `show_embed_preview`, with a resource identity
/// supplied by the occurrence-aware context-menu payload.
pub fn show_embed_preview_occurrence(
    url: impl Into<Arc<str>>,
    occurrence: crate::backend::embeds::state::PreviewOccurrence,
) {
    let url = url.into();
    if occurrence.message_id.is_empty()
        || matches!(
            crate::backend::settings::get_embed_display_mode(),
            crate::backend::settings::EmbedDisplayMode::None
        )
        || crate::backend::streamer_mode::should_hide_link_urls()
        || occurrence.embed_id == crate::backend::upload_history::UPLOAD_LINK_NO_PREVIEW
        || crate::backend::upload_history::is_known_upload_deletion_url(url.as_ref())
    {
        return;
    }
    let classified = crate::backend::link_types::classify_link(url.as_ref());
    if !crate::backend::settings::is_embed_classification_enabled(
        classified.provider,
        classified.content_kind,
    ) {
        return;
    }
    let admission = request_embed_metadata(
        url,
        occurrence.clone(),
        crate::backend::embeds::cache::FlightDemand::Manual,
    );
    if admission == crate::backend::embeds::cache::FlightAdmissionKind::RejectedCapacity {
        crate::backend::embeds::state::set_embed_expanded(occurrence.clone(), false);
        invalidate_embed_occurrence(&occurrence.message_id);
        return;
    }
    crate::backend::embeds::state::set_embed_expanded(occurrence.clone(), true);
    invalidate_embed_occurrence(&occurrence.message_id);
}

/// Hide exactly one disclosed occurrence and evict that message's cached
/// layouts, including layouts made before the preview existed.
pub fn hide_embed_preview(url: &str, message_id: &str, embed_id: u32) {
    if message_id.is_empty() {
        return;
    }
    let occurrence = crate::backend::embeds::state::PreviewOccurrence {
        message_id: message_id.to_string(),
        embed_id,
        resource_key: crate::backend::link_types::ResourceKey::for_url(url),
    };
    hide_embed_preview_occurrence(occurrence);
}

pub fn hide_embed_preview_occurrence(occurrence: crate::backend::embeds::state::PreviewOccurrence) {
    if occurrence.message_id.is_empty() {
        return;
    }
    crate::backend::embeds::state::set_embed_expanded(occurrence.clone(), false);
    for next in crate::backend::embeds::cache::RESOURCE_FLIGHTS.cancel_occurrence(&occurrence) {
        spawn_embed_flight(next);
    }
    invalidate_embed_occurrence(&occurrence.message_id);
}

fn invalidate_embed_occurrence(message_id: &str) {
    let message_ids = std::collections::HashSet::from([message_id]);
    crate::components::chat::canvas_renderer::global_layout_cache::GLOBAL_LAYOUT_CACHE
        .clear_layouts_for_messages(&message_ids);
    crate::state::VersionRegistry::bump_embed_metadata_version();
}

/// Message ids whose embed metadata completed after their layout was built.
/// Drained by the app's embed poll, which evicts those cached layouts so the
/// embed preview appears without waiting for an unrelated relayout.
static PENDING_EMBED_LAYOUT_INVALIDATIONS: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

fn queue_embed_layout_invalidation(message_id: String) {
    if message_id.is_empty() {
        return;
    }
    PENDING_EMBED_LAYOUT_INVALIDATIONS
        .lock_or_recover()
        .push(message_id);
}

/// Drain message ids that need their cached layouts evicted because embed
/// metadata arrived after layout.
pub fn take_pending_embed_layout_invalidations() -> Vec<String> {
    std::mem::take(&mut *PENDING_EMBED_LAYOUT_INVALIDATIONS.lock_or_recover())
}

/// Get current embed metadata version (for UI polling).
/// Delegates to VersionRegistry for centralized version management.
/// Note: Returns u32 for backward compatibility; internally uses u64.
pub fn get_embed_metadata_version() -> u32 {
    crate::state::VersionRegistry::embed_metadata_version() as u32
}

/// Get cached embed metadata for a URL
pub fn get_embed_metadata(url: &str) -> Option<crate::backend::link_types::EmbedMetadata> {
    crate::backend::embeds::cache::EMBED_CACHE.get(url)
}

/// Check if embed is expanded
pub fn is_embed_expanded_state(message_id: &str, embed_id: u32, url: &str) -> bool {
    crate::backend::embeds::state::is_embed_expanded(
        &crate::backend::embeds::state::PreviewOccurrence {
            message_id: message_id.to_string(),
            embed_id,
            resource_key: crate::backend::link_types::ResourceKey::for_url(url),
        },
    )
}

/// Poll connection status
pub fn poll_connection_status() -> ConnectionStatus {
    crate::backend::irc::get_connection_status().into()
}

/// Poll Twitch events (subs, raids, etc.)
pub fn poll_twitch_events() -> Vec<TwitchEvent> {
    crate::backend::components::event_notification::pop_events()
        .into_iter()
        .map(|e| e.into())
        .collect()
}

/// Return recent Twitch events for Activity Events.
pub fn get_recent_twitch_events(limit: usize) -> Vec<TwitchEvent> {
    crate::backend::components::event_notification::recent_events(limit)
        .into_iter()
        .map(|e| e.into())
        .collect()
}

/// Return only EventSub rows that still require attention.
pub fn get_unread_twitch_event_count() -> usize {
    crate::backend::components::event_notification::unread_recent_event_count()
}

/// Clear events observed by exact live-following chat channels.
pub fn mark_recent_twitch_events_read_for_channels(channels: &[String]) -> bool {
    let channels: HashSet<_> = channels
        .iter()
        .map(|channel| channel.trim().trim_start_matches('#').to_lowercase())
        .filter(|channel| !channel.is_empty())
        .collect();
    crate::backend::components::event_notification::mark_recent_events_read_for_channels(&channels)
}

/// Return the latest open prediction outcomes observed through EventSub for a channel.
pub fn get_active_prediction_outcomes(channel: &str) -> Vec<PredictionOutcomeSnapshot> {
    crate::backend::components::event_notification::active_prediction_outcomes(channel)
        .into_iter()
        .map(|outcome| outcome.into())
        .collect()
}

/// Clear the in-app Twitch event notification history.
pub fn clear_recent_twitch_events() {
    crate::backend::components::event_notification::clear_recent_events();
}

/// Clear AutoMod entries from the in-app Twitch event notification history.
pub fn clear_recent_automod_events() {
    crate::backend::components::event_notification::clear_recent_automod_events();
}

/// Remove one in-app Twitch event notification history entry.
pub fn remove_recent_twitch_event(timestamp: i64) -> bool {
    crate::backend::components::event_notification::remove_recent_event(timestamp)
}

/// Allow or deny a held AutoMod message without blocking the UI thread.
pub fn manage_held_automod_message_async(message_id: String, allow: bool, event_timestamp: i64) {
    if let Err(error) =
        crate::benchmark_profile::reject_live_outbound_user_action("AutoMod decision")
    {
        crate::log_irc!(warn, "{error}");
        crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
            message: "Live benchmark blocked AutoMod action".to_string(),
            detail: Some(error),
        });
        return;
    }
    let message_id = message_id.trim().to_string();
    if message_id.is_empty() {
        crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
            message: "AutoMod action failed".to_string(),
            detail: Some("The held message ID is missing.".to_string()),
        });
        return;
    }

    RUNTIME.spawn(async move {
        let action = if allow {
            crate::backend::commands::helix::AutomodMessageAction::Allow
        } else {
            crate::backend::commands::helix::AutomodMessageAction::Deny
        };

        match crate::backend::commands::helix::manage_held_automod_message(&message_id, action)
            .await
        {
            crate::backend::commands::CommandResult::Success(status) => {
                if event_timestamp > 0 {
                    crate::backend::components::event_notification::remove_recent_event(
                        event_timestamp,
                    );
                }
                crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                    message: status.unwrap_or_else(|| "AutoMod message updated".to_string()),
                    detail: None,
                });
            }
            crate::backend::commands::CommandResult::Error(error)
            | crate::backend::commands::CommandResult::DeliveryUnknown(error) => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "AutoMod action failed".to_string(),
                    detail: Some(error),
                });
            }
            crate::backend::commands::CommandResult::NotACommand => {}
        }
        crate::managers::frame_rate_controller::record_activity();
    });
}

/// Apply or clear suspicious-user treatment without blocking the UI thread.
pub fn manage_suspicious_user_status_async(
    channel: String,
    user_login: String,
    user_id: Option<String>,
    action: crate::backend::commands::helix::SuspiciousUserAction,
    event_timestamp: i64,
) {
    if let Err(error) =
        crate::benchmark_profile::reject_live_outbound_user_action("suspicious-user decision")
    {
        crate::log_irc!(warn, "{error}");
        super::chat::publish_command_error(&channel, "Suspicious-user action blocked", &error);
        return;
    }
    let channel = channel.trim().trim_start_matches('#').to_lowercase();
    let user_login = user_login.trim().trim_start_matches('@').to_lowercase();
    let user_id = user_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    if channel.is_empty() || (user_login.is_empty() && user_id.is_none()) {
        super::chat::publish_command_error(
            &channel,
            "Suspicious-user action failed",
            "The channel or target user is missing.",
        );
        return;
    }

    RUNTIME.spawn(async move {
        match crate::backend::commands::helix::manage_suspicious_user_status(
            &channel,
            &user_login,
            user_id.as_deref(),
            action,
        )
        .await
        {
            crate::backend::commands::CommandResult::Success(status) => {
                if event_timestamp > 0 {
                    crate::backend::components::event_notification::remove_recent_event(
                        event_timestamp,
                    );
                }
                crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                    message: status.unwrap_or_else(|| "Suspicious-user status updated".to_string()),
                    detail: None,
                });
            }
            crate::backend::commands::CommandResult::Error(error)
            | crate::backend::commands::CommandResult::DeliveryUnknown(error) => {
                super::chat::publish_command_error(
                    &channel,
                    "Suspicious-user action failed",
                    &error,
                );
            }
            crate::backend::commands::CommandResult::NotACommand => {}
        }
        crate::managers::frame_rate_controller::record_activity();
    });
}

/// Human-readable title for a Twitch event notification.
pub fn twitch_event_title(event_type: &TwitchEventType) -> &'static str {
    twitch_event_title_core(event_type)
}

/// Human-readable event body shared by chat system messages and the dropdown.
pub fn format_twitch_event_text(event: &TwitchEvent) -> String {
    format_twitch_event_text_core(event, streamer_mode_twitch_event_privacy())
}

/// Insert a system message for a Twitch event (sub, raid, etc.)
pub fn insert_event_message(event: &TwitchEvent) {
    let channel = &event.channel;
    let text = format_twitch_event_text(event);
    let mut highlight_type = twitch_event_highlight_type(event);

    let Some((filter_highlight_match, filter_highlighted)) =
        event_filter_highlight_pattern(event, &text, highlight_type.as_ref())
    else {
        return;
    };
    if highlight_type.is_none() {
        highlight_type = filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage));
    }

    let flag_overrides = event_message_flag_overrides(event);
    let held_id = event
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());

    if event.event_type == TwitchEventType::AutomodMessageUpdate
        && let Some(held_id) = held_id
        && crate::backend::irc::resolve_automod_message(channel, held_id, &text, flag_overrides)
    {
        crate::backend::components::event_notification::mark_recent_event_chat_visible(
            event.timestamp,
        );
        return;
    }

    let explicit_id = if event.event_type == TwitchEventType::AutomodMessageHold {
        held_id.map(crate::backend::irc::automod_row_id)
    } else {
        None
    };

    crate::backend::irc::add_system_message_with_highlight_flags_id(
        channel,
        &text,
        crate::backend::models::SystemMessageType::Info,
        highlight_type,
        flag_overrides,
        explicit_id,
    );
    crate::backend::components::event_notification::mark_recent_event_chat_visible(event.timestamp);
}

fn event_message_flag_overrides(
    event: &TwitchEvent,
) -> crate::backend::irc::SystemMessageFlagOverrides {
    let is_automod = matches!(
        event.event_type,
        TwitchEventType::AutomodMessageHold | TwitchEventType::AutomodMessageUpdate
    );
    let is_suspicious_user = matches!(
        event.event_type,
        TwitchEventType::SuspiciousUserMessage | TwitchEventType::SuspiciousUserUpdate
    );
    let status = event.status.as_deref().unwrap_or_default();

    crate::backend::irc::SystemMessageFlagOverrides {
        is_automod,
        is_automod_pending: event.event_type == TwitchEventType::AutomodMessageHold,
        is_suspicious_user,
        is_restricted: status.eq_ignore_ascii_case("restricted"),
        is_monitored: status.eq_ignore_ascii_case("active_monitoring")
            || status.eq_ignore_ascii_case("monitored"),
    }
}

fn twitch_event_highlight_type(event: &TwitchEvent) -> Option<HighlightType> {
    match event.event_type {
        TwitchEventType::Subscribe | TwitchEventType::Resubscribe | TwitchEventType::GiftSub => {
            Some(HighlightType::Subscription)
        }
        TwitchEventType::Raid => Some(HighlightType::Raid),
        TwitchEventType::ChannelPointRedemption => Some(HighlightType::ChannelPointRedeem {
            reward_id: event.reward_id.clone().unwrap_or_default(),
            reward_title: event
                .reward_title
                .clone()
                .unwrap_or_else(|| "Channel reward".to_string()),
            reward_cost: event.reward_cost.unwrap_or_default(),
        }),
        TwitchEventType::StreamOnline
        | TwitchEventType::StreamOffline
        | TwitchEventType::Ban
        | TwitchEventType::Unban
        | TwitchEventType::AutomodMessageHold
        | TwitchEventType::AutomodMessageUpdate
        | TwitchEventType::SuspiciousUserMessage
        | TwitchEventType::SuspiciousUserUpdate
        | TwitchEventType::PollBegin
        | TwitchEventType::PollProgress
        | TwitchEventType::PollEnd
        | TwitchEventType::PredictionBegin
        | TwitchEventType::PredictionProgress
        | TwitchEventType::PredictionLock
        | TwitchEventType::PredictionEnd
        | TwitchEventType::HypeTrainBegin
        | TwitchEventType::HypeTrainProgress
        | TwitchEventType::HypeTrainEnd
        | TwitchEventType::VipAdd
        | TwitchEventType::VipRemove
        | TwitchEventType::ModeratorAdd
        | TwitchEventType::ModeratorRemove
        | TwitchEventType::WarningAcknowledge
        | TwitchEventType::WarningSend
        | TwitchEventType::ShieldModeBegin
        | TwitchEventType::ShieldModeEnd => None,
    }
}

fn event_filter_highlight_pattern(
    event: &TwitchEvent,
    text: &str,
    highlight_type: Option<&HighlightType>,
) -> Option<(Option<String>, bool)> {
    let filter_ctx = event_filter_context(event, text, highlight_type);
    let mut should_hide = false;
    let mut filter_highlight_match: Option<String> = None;
    let mut filter_highlighted = false;

    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }

        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Highlight => {
                filter_highlighted = true;
                if filter_highlight_match.is_none() {
                    filter_highlight_match =
                        result.matched_text.filter(|text| !text.trim().is_empty());
                }
            }
            FilterAction::Allow => {
                should_hide = false;
                filter_highlighted = false;
                filter_highlight_match = None;
                break;
            }
            FilterAction::Replace => {}
        }
    }

    if should_hide {
        crate::log_irc!(
            debug,
            "Filter: Hiding EventSub {:?} row in {}",
            event.event_type,
            event.channel
        );
        None
    } else {
        Some((filter_highlight_match, filter_highlighted))
    }
}

fn event_filter_context(
    event: &TwitchEvent,
    text: &str,
    highlight_type: Option<&HighlightType>,
) -> TestContext {
    let target_name = event.user_name.clone().unwrap_or_default();
    let target_login = event.user_login.clone().unwrap_or_default();
    let target_user_id = event.user_id.clone().unwrap_or_default();
    let moderator_name = event.moderator_name.clone().unwrap_or_default();
    let moderator_login = moderator_name.to_lowercase();
    let author = event
        .user_name
        .as_deref()
        .or(event.from_channel.as_deref())
        .or(event.moderator_name.as_deref())
        .unwrap_or("System")
        .to_string();
    let is_sub_message = matches!(
        event.event_type,
        TwitchEventType::Subscribe | TwitchEventType::Resubscribe | TwitchEventType::GiftSub
    );
    let is_reward_message = event.event_type == TwitchEventType::ChannelPointRedemption;
    let is_automod = matches!(
        event.event_type,
        TwitchEventType::AutomodMessageHold | TwitchEventType::AutomodMessageUpdate
    );
    let status = event.status.as_deref().unwrap_or_default();
    let event_source = if is_automod {
        "eventsub:automod".to_string()
    } else {
        "eventsub".to_string()
    };

    let channel_live = if event.event_type == TwitchEventType::StreamOnline {
        true
    } else if event.event_type == TwitchEventType::StreamOffline {
        false
    } else {
        crate::backend::bridge::get_cached_channel_live_status(&event.channel)
    };

    TestContext {
        content: text.to_string(),
        author: author.clone(),
        author_login: event
            .user_login
            .as_deref()
            .unwrap_or(author.as_str())
            .to_lowercase(),
        moderator_name,
        moderator_login,
        target_name,
        target_login,
        target_user_id,
        channel_name: event.channel.clone(),
        reward_title: event.reward_title.clone().unwrap_or_default(),
        reward_cost: event.reward_cost.unwrap_or_default(),
        reward_id: event.reward_id.clone().unwrap_or_default(),
        event_action: event_type_filter_label(&event.event_type).to_string(),
        event_reason: event.reason.clone().unwrap_or_default(),
        event_source,
        event_status: event.status.clone().unwrap_or_default(),
        event_title: event.event_title.clone().unwrap_or_default(),
        event_detail: event.event_detail.clone().unwrap_or_default(),
        is_sub: is_sub_message,
        is_system: true,
        is_highlighted: highlight_type.is_some(),
        is_points_redeemed: is_reward_message,
        is_reward_message,
        is_automod,
        is_restricted: status.eq_ignore_ascii_case("restricted"),
        is_monitored: status.eq_ignore_ascii_case("active_monitoring")
            || status.eq_ignore_ascii_case("monitored"),
        is_sub_message,
        channel_watching: true,
        channel_live,
        ..Default::default()
    }
}

fn event_type_filter_label(event_type: &TwitchEventType) -> &'static str {
    match event_type {
        TwitchEventType::Subscribe => "subscribe",
        TwitchEventType::Resubscribe => "resubscribe",
        TwitchEventType::GiftSub => "gift_sub",
        TwitchEventType::Raid => "raid",
        TwitchEventType::ChannelPointRedemption => "channel_point_redemption",
        TwitchEventType::StreamOnline => "stream_online",
        TwitchEventType::StreamOffline => "stream_offline",
        TwitchEventType::Ban => "ban",
        TwitchEventType::Unban => "unban",
        TwitchEventType::AutomodMessageHold => "automod_message_hold",
        TwitchEventType::AutomodMessageUpdate => "automod_message_update",
        TwitchEventType::SuspiciousUserMessage => "suspicious_user_message",
        TwitchEventType::SuspiciousUserUpdate => "suspicious_user_update",
        TwitchEventType::PollBegin => "poll_begin",
        TwitchEventType::PollProgress => "poll_progress",
        TwitchEventType::PollEnd => "poll_end",
        TwitchEventType::PredictionBegin => "prediction_begin",
        TwitchEventType::PredictionProgress => "prediction_progress",
        TwitchEventType::PredictionLock => "prediction_lock",
        TwitchEventType::PredictionEnd => "prediction_end",
        TwitchEventType::HypeTrainBegin => "hype_train_begin",
        TwitchEventType::HypeTrainProgress => "hype_train_progress",
        TwitchEventType::HypeTrainEnd => "hype_train_end",
        TwitchEventType::VipAdd => "vip_add",
        TwitchEventType::VipRemove => "vip_remove",
        TwitchEventType::ModeratorAdd => "moderator_add",
        TwitchEventType::ModeratorRemove => "moderator_remove",
        TwitchEventType::WarningAcknowledge => "warning_acknowledge",
        TwitchEventType::WarningSend => "warning_send",
        TwitchEventType::ShieldModeBegin => "shield_mode_begin",
        TwitchEventType::ShieldModeEnd => "shield_mode_end",
    }
}

/// Re-export UI action types
pub use crate::backend::ui_actions::{UiAction, push_action as push_ui_action};

/// Re-export pending mention insertion functions
pub use crate::backend::ui_actions::{
    drain_pending_mentions, drain_pending_message_composer_inserts, drain_pending_quick_switches,
    drain_pending_replies, drain_pending_search_jumps, drain_pending_split_message_jumps,
    drain_pending_thread_views, drain_pending_uploaded_links, drain_pending_whisper_composes,
    push_pending_mention, push_pending_message_composer_insert, push_pending_open_new_tab,
    push_pending_open_whispers_split, push_pending_quick_switch, push_pending_reply,
    push_pending_search_jump, push_pending_split_message_jump, push_pending_thread_view,
    push_pending_uploaded_link, push_pending_whisper_compose, take_pending_open_new_tab,
    take_pending_open_whispers_split,
};

/// Poll UI actions (pane operations, reply, etc.)
pub fn poll_ui_actions() -> Vec<UiAction> {
    crate::backend::ui_actions::pop_actions()
}

/// Get performance metrics as JSON
pub fn get_metrics_json() -> String {
    let metrics_enabled = crate::backend::metrics::is_enabled();
    let fps_profiler_enabled = crate::fps_profiler::is_enabled();
    let summary = crate::backend::metrics::get_summary();
    let memory = crate::backend::metrics::get_memory_stats();
    let cache = crate::cache::loader::stats().diagnostic_snapshot();
    let top_splits = crate::backend::metrics::top_split_metrics(5);
    let top_notify_sources = crate::backend::metrics::top_notify_sources(8);
    let top_emote_events = crate::backend::metrics::top_emote_events(24);
    let top_emote_stages = crate::backend::metrics::top_emote_stage_metrics(12);
    let anim_cap_hz = crate::managers::dynamic_tick_scheduler::animation_cap_fps_hz();
    let anim_cap_source = crate::managers::dynamic_tick_scheduler::animation_cap_source();
    let anim_min_tick_ms =
        crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms();
    let frame_diag = crate::managers::frame_rate_controller::diagnostics();
    let eventapi_diag = crate::backend::emotes::seventv_eventapi::diagnostics();
    let eventapi_ownership = crate::backend::emotes::seventv_eventapi::ownership_snapshot();
    let layout_worker = crate::components::chat::canvas_renderer::layout_worker_diagnostics();
    serde_json::json!({
        "messages_received": summary.messages_received,
        "messages_rendered": summary.messages_rendered,
        "messages_per_second": summary.messages_per_second,
        "avg_batch_time_ms": summary.avg_batch_time_ms,
        "emote_loads": summary.emote_loads,
        "emote_failures": summary.emote_failures,
        "emote_success_rate": summary.emote_success_rate,
        "ui_notify_count": summary.ui_notify_count,
        "app_render_count": summary.app_render_count,
        "app_render_from_notify_count": summary.app_render_from_notify_count,
        "app_render_external_count": summary.app_render_external_count,
        "layout": {
            "cache_hits": summary.layout.cache_hits,
            "cache_misses": summary.layout.cache_misses,
            "async_requests": summary.layout.async_requests,
            "async_ready_valid_width": summary.layout.async_ready_valid_width,
            "async_ready_stale_width": summary.layout.async_ready_stale_width,
            "full_rebuilds": summary.layout.full_rebuilds,
            "message_order_resets": summary.layout.message_order_resets,
            "rebuild_reason_order": summary.layout.rebuild_reason_order,
            "rebuild_reason_height": summary.layout.rebuild_reason_height,
            "rebuild_reason_stale": summary.layout.rebuild_reason_stale,
            "rebuild_reason_force": summary.layout.rebuild_reason_force,
            "async_worker": {
                "scope": "process",
                "queue_depth": layout_worker.queue_depth,
                "queue_capacity": layout_worker.queue_capacity,
                "queue_peak_depth": layout_worker.queue_peak_depth,
                "queue_wait_max_us": layout_worker.queue_wait_max_us,
                "enqueued_total": layout_worker.enqueued_total,
                "queue_full_total": layout_worker.queue_full_total,
                "queue_disconnected_total": layout_worker.queue_disconnected_total,
                "worker_panic_total": layout_worker.worker_panic_total,
                "shaped_total": layout_worker.shaped_total,
                "published_total": layout_worker.published_total,
                "obsolete_before_shape_total": layout_worker.obsolete_before_shape_total,
                "obsolete_after_shape_total": layout_worker.obsolete_after_shape_total,
            },
        },
        "monitoring": {
            "perf_metrics_enabled": metrics_enabled,
            "fps_profiler_enabled": fps_profiler_enabled,
        },
        "top_splits": top_splits.iter().map(|row| serde_json::json!({
            "split": row.split,
            "renders": row.renders,
            "updates": row.updates,
            "messages_delta": row.messages_delta,
        })).collect::<Vec<_>>(),
        "top_notify_sources": top_notify_sources.iter().map(|row| serde_json::json!({
            "source": row.source,
            "count": row.count,
        })).collect::<Vec<_>>(),
        "top_emote_events": top_emote_events.iter().map(|row| serde_json::json!({
            "event": row.source,
            "count": row.count,
        })).collect::<Vec<_>>(),
        "top_emote_stages": top_emote_stages.iter().map(|row| serde_json::json!({
            "stage": row.stage,
            "count": row.count,
            "total_ms": row.total_ms,
            "avg_ms": row.avg_ms,
            "max_ms": row.max_ms,
            "last_ms": row.last_ms,
            "total_items": row.total_items,
            "avg_items": row.avg_items,
            "last_items": row.last_items,
        })).collect::<Vec<_>>(),
        "animation_cap": {
            "hz": anim_cap_hz,
            "source": anim_cap_source,
            "min_tick_ms": anim_min_tick_ms,
        },
        "frame_liveness": {
            "mode": format!("{:?}", frame_diag.mode),
            "focused": frame_diag.focused,
            "visible_animations": frame_diag.visible_animations,
            "skipping_frames": frame_diag.skipping_frames,
            "last_interaction_ms_ago": frame_diag.last_interaction_ms_ago,
            "last_activity_ms_ago": frame_diag.last_activity_ms_ago,
            "last_irc_message_ms_ago": frame_diag.last_irc_message_ms_ago,
            "last_observer_drain_ms_ago": frame_diag.last_observer_drain_ms_ago,
            "last_image_ready_ms_ago": frame_diag.last_image_ready_ms_ago,
        },
        "seventv_eventapi": {
            "connected": eventapi_diag.connected,
            "desired_subscriptions": eventapi_diag.desired_subscriptions,
            "sent_subscriptions": eventapi_diag.active_subscriptions,
            "queued_subscription_commands": eventapi_diag.queued_subscription_commands,
            "pending_refreshes": eventapi_diag.pending_refreshes,
            "active_set_refreshes": eventapi_diag.active_set_refreshes,
            "owned_channels": eventapi_diag.owned_channels,
            "owned_sets": eventapi_diag.owned_sets,
            "owned_users": eventapi_diag.owned_users,
            "reconnects": eventapi_diag.reconnects,
            "last_heartbeat_ms_ago": eventapi_diag.last_heartbeat_ms_ago,
            "last_error": eventapi_diag.last_error,
            "channels": eventapi_ownership.channels.iter().map(|row| serde_json::json!({
                "channel": row.channel.as_str(),
                "emote_set_id": row.emote_set_id.as_str(),
                "user_id": row.user_id.as_deref(),
                "connection_index": row.connection_index,
            })).collect::<Vec<_>>(),
            "sets": eventapi_ownership.sets.iter().map(|row| serde_json::json!({
                "emote_set_id": row.emote_set_id.as_str(),
                "channels": &row.channels,
            })).collect::<Vec<_>>(),
            "users": eventapi_ownership.users.iter().map(|row| serde_json::json!({
                "user_id": row.user_id.as_str(),
                "channels": &row.channels,
            })).collect::<Vec<_>>(),
            "active_set_refreshes": eventapi_ownership.active_set_refreshes.iter().map(|(channel, set_id)| serde_json::json!({
                "channel": channel,
                "emote_set_id": set_id,
            })).collect::<Vec<_>>(),
        },
        "memory": {
            "message_buffer_bytes": memory.message_buffer_bytes,
            "stored_messages": memory.stored_messages,
            "active_channels": memory.active_channels,
            "bytes_per_message": memory.bytes_per_message,
        },
        "cache": {
            "static_images": cache.static_images,
            "animated_images": cache.animated_images,
            "memory_used": cache.memory_used,
            "hot_frames": cache.hot_frames,
            "animation_metadata": cache.animation_metadata,
            "frame_memory": cache.frame_memory,
            "disk_entries": cache.disk_entries,
            "disk_bytes": cache.disk_bytes,
            "disk_evictions": cache.disk_evictions,
            "total_downloaded": cache.total_downloaded,
            "total_bytes_downloaded": cache.total_bytes_downloaded,
            "cache_hits": cache.cache_hits,
            "cache_misses": cache.cache_misses,
            "active_downloads": cache.active_downloads,
            "disk_hydrations": cache.disk_hydrations,
            "disk_hydration_time_us": cache.disk_hydration_time_us,
            "evictions": cache.evictions,
            "bytes_evicted": cache.bytes_evicted,
            "texture_reuses": cache.texture_reuses,
            "texture_bytes_saved": cache.texture_bytes_saved,
            "freshness_blocked_evictions": cache.freshness_blocked_evictions,
        }
    })
    .to_string()
}

/// Reset metrics
pub fn reset_metrics() {
    crate::backend::metrics::reset_metrics();
}
