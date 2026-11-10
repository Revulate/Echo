//! Visible media discovery and preload dispatch helpers.

use super::*;

pub(super) struct VisibleEmbedRequest {
    pub(super) url: Arc<str>,
    pub(super) message_id: Arc<str>,
    pub(super) embed_id: u32,
}

/// Cheap render-scan ownership retained until repeated visible URLs have been
/// collapsed. The loader-facing `MediaRequest` owns URL and source-identity
/// strings, so constructing it here would pay those copies for every repeated
/// emote occurrence before the existing deduplication boundary.
pub(super) struct VisibleEmoteRequest {
    url: Arc<str>,
    emote: Arc<crate::backend::emotes::Emote>,
}

/// Media work discovered from the visible/preload window.
///
/// Slot-derived emotes and embeds share one render-unit scan so viewport demand
/// follows the exact same grouping semantics used by layout. Static layout
/// images are appended after layouts are built because those commands are the
/// authoritative direct-paint surface.
#[derive(Default)]
pub(super) struct VisibleMediaPlan {
    pub(super) emote_requests: Vec<VisibleEmoteRequest>,
    pub(super) animated_emote_requests: Vec<VisibleEmoteRequest>,
    pub(super) embed_requests: Vec<VisibleEmbedRequest>,
    /// PERF: carried as `SharedString` (cheap clones from the layout's `ImagePaintCommand.url`).
    /// Owned `String`s are only materialized at the loader dispatch boundary, after
    /// dedup collapses repeated emote URLs across visible rows.
    pub(super) static_image_urls: Vec<SharedString>,
}

impl VisibleMediaPlan {
    pub(super) fn with_capacity(
        emote_urls: usize,
        embed_requests: usize,
        static_image_urls: usize,
    ) -> Self {
        Self {
            emote_requests: Vec::with_capacity(emote_urls),
            animated_emote_requests: Vec::new(),
            embed_requests: Vec::with_capacity(embed_requests),
            static_image_urls: Vec::with_capacity(static_image_urls),
        }
    }
}

/// `display_rules` carries the active emote display rules and the message's
/// channel, so emotes shown as text request nothing and still emotes request
/// only their static first frame, matching layout.
pub(super) fn collect_message_visible_media(
    message_id: Option<&Arc<str>>,
    resolver: Option<&crate::backend::slots::resolver::SlotResolver>,
    slots: &[crate::backend::slots::MessageSlot],
    include_emotes: bool,
    include_embeds: bool,
    display_rules: Option<(&crate::backend::settings::CompiledEmoteDisplayRules, &str)>,
    plan: &mut VisibleMediaPlan,
) {
    use crate::backend::slots::MessageSlot;
    use crate::backend::slots::resolver::ResolvedSlot;

    if !include_emotes && !include_embeds {
        return;
    }

    for slot in slots {
        if include_embeds
            && let MessageSlot::Url { url, embed_id } = slot
            && let Some(message_id) = message_id
        {
            plan.embed_requests.push(VisibleEmbedRequest {
                url: Arc::clone(url),
                message_id: Arc::clone(message_id),
                embed_id: *embed_id,
            });
        }

        if !include_emotes
            || !matches!(
                slot,
                MessageSlot::Emote(_) | MessageSlot::TwitchEmote { .. }
            )
        {
            continue;
        }

        let resolver = resolver.expect("emote discovery requires a slot resolver");
        if let ResolvedSlot::Emote(emote) = resolver.resolve(slot) {
            let flags = display_rules
                .map(|(rules, channel)| rules.flags_for(emote.provider, &emote.id, channel))
                .unwrap_or_default();
            if flags.show_as_text {
                continue;
            }
            let url = crate::backend::emotes::chat_display_url_shared_for_scale(
                &emote,
                crate::components::chat::canvas_renderer::layout_scale::current(),
            );
            let is_animated = !flags.still_image && (emote.is_animated || is_animated_url(&url));
            let request = VisibleEmoteRequest { url, emote };
            if is_animated {
                plan.animated_emote_requests.push(request);
            } else {
                plan.emote_requests.push(request);
            }
        }
    }
}

pub(super) fn trigger_visible_preload(
    requests: &mut Vec<VisibleEmoteRequest>,
    metric_stage: &'static str,
    source: image_cache::PreloadSource,
) -> usize {
    if requests.is_empty() {
        return 0;
    }

    requests.sort_unstable_by(|a, b| a.url.cmp(&b.url));
    requests.dedup_by(|a, b| a.url == b.url);
    let url_count = requests.len();
    let owned_requests: Vec<_> = requests
        .drain(..)
        .map(|request| {
            crate::backend::emotes::media_request_for_url(&request.emote, request.url.as_ref())
        })
        .collect();
    image_cache::record_displayed_media_requests(&owned_requests);
    crate::backend::metrics::record_emote_stage(metric_stage, std::time::Duration::ZERO, url_count);
    image_cache::trigger_preload_media_with_source(owned_requests, source);
    url_count
}

pub(super) fn trigger_visible_url_preload(
    urls: &mut Vec<SharedString>,
    metric_stage: &'static str,
    source: image_cache::PreloadSource,
) -> usize {
    if urls.is_empty() {
        return 0;
    }

    urls.sort_unstable();
    urls.dedup();
    let url_count = urls.len();
    crate::backend::metrics::record_emote_stage(metric_stage, std::time::Duration::ZERO, url_count);
    image_cache::trigger_preload_with_source(std::mem::take(urls), source);
    url_count
}

pub(super) fn trigger_visible_animated_preload(
    requests: &mut Vec<VisibleEmoteRequest>,
    metric_stage: &'static str,
    source: image_cache::PreloadSource,
) -> usize {
    if requests.is_empty() {
        return 0;
    }

    requests.sort_unstable_by(|a, b| a.url.cmp(&b.url));
    requests.dedup_by(|a, b| a.url == b.url);
    let url_count = requests.len();
    let owned_requests: Vec<_> = requests
        .drain(..)
        .map(|request| {
            crate::backend::emotes::media_request_for_url(&request.emote, request.url.as_ref())
        })
        .collect();
    image_cache::record_displayed_media_requests(&owned_requests);
    crate::backend::metrics::record_emote_stage(metric_stage, std::time::Duration::ZERO, url_count);
    for request in owned_requests {
        image_cache::trigger_animated_decode_request_with_source(request, source);
    }
    crate::log_cache!(
        trace,
        count = url_count,
        "Queued visible animated emote decode"
    );
    url_count
}
