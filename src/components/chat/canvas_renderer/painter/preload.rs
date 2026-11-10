//! Visible-range media preloading and visible animation/image readiness tracking.

use super::direct_paint::{animated_cache_state_tag, image_load_state_tag};
use super::media_preload::{
    VisibleMediaPlan, collect_message_visible_media, trigger_visible_animated_preload,
    trigger_visible_preload, trigger_visible_url_preload,
};
use super::*;

#[derive(Default)]
pub(super) struct VisibleImageMisses {
    pub(super) pending: bool,
    /// Earliest time the loader accepts a retry of a visible failed image.
    pub(super) next_failed_retry: Option<std::time::Instant>,
}

/// Visible animated images at their current decoded frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VisibleAnimationFrame {
    pub(super) signature: u64,
    /// Milliseconds until the earliest visible animation changes frame, or
    /// `None` when none of them ever does.
    pub(super) next_change_ms: Option<u64>,
}

#[inline]
fn earliest_change(current: Option<u64>, candidate: Option<u64>) -> Option<u64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (current, candidate) => current.or(candidate),
    }
}

/// Whether a visible animated image needs no further load wake: its animation
/// is showing, or a static stand-in is showing and the animation will not
/// arrive (failed, never requested, or animation is turned off, when no
/// refill would follow). While the animation is still loading
/// behind a static stand-in the split must stay subscribed: the render that
/// registers the decoded animation is what starts its animation waker.
#[inline]
fn animated_image_settled(url: &str) -> bool {
    image_cache::get_current_animated_render_image(url).is_some()
        || (image_cache::get_cached_render_image(url).is_some()
            && (!crate::emote_renderer::animation_enabled()
                || !matches!(
                    image_cache::get_animated_cache_state(url),
                    image_cache::AnimatedCacheState::Loading
                )))
}

impl VisibleImageMisses {
    fn note(&mut self, failed: bool, url: &str) {
        if !failed {
            self.pending = true;
        } else if let Some(deadline) = image_cache::failed_media_retry_deadline(url) {
            // A retry that is due now is claimed by the image-load waker or this
            // frame's paint; keep the waker running to see it land.
            if deadline <= std::time::Instant::now() {
                self.pending = true;
            }
            self.next_failed_retry = Some(
                self.next_failed_retry
                    .map_or(deadline, |current| current.min(deadline)),
            );
        }
    }
}

impl CanvasMessageList {
    pub(super) fn schedule_hover_link_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self.hovered_link_url().map(Arc::<str>::from) else {
            return;
        };
        if !crate::backend::bridge::hover_link_metadata_allowed(&url) {
            return;
        }
        let generation = self.wake_state.tooltip_delay_wake_generation;
        let Some(owner) = self.interaction_state.tooltip.owner_window else {
            return;
        };
        let mut updates = crate::backend::bridge::subscribe_embed_metadata_updates();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(150))
                .await;
            let occurrence = this
                .update(cx, |this, cx| {
                    if !this.hover_link_request_is_current(generation, &url)
                        || !owner
                            .update(cx, |_, window, _| window.is_window_hovered())
                            .unwrap_or(false)
                    {
                        return None;
                    }
                    let local = this.to_local_pos(this.interaction_state.tooltip.position);
                    let hitbox = this.interaction_state.hitbox_manager.hit_test(local)?;
                    let embed_id = match &hitbox.kind {
                        HitboxKind::Url { embed_id, .. }
                        | HitboxKind::EmbedPreview { embed_id, .. }
                        | HitboxKind::MediaInspector { embed_id, .. } => *embed_id,
                        _ => return None,
                    };
                    Some(crate::backend::embeds::state::PreviewOccurrence {
                        message_id: hitbox.message_id.clone(),
                        embed_id,
                        resource_key: crate::backend::link_types::ResourceKey::for_url(&url),
                    })
                })
                .ok()
                .flatten();
            let Some(occurrence) = occurrence else {
                return;
            };
            let resource_key = occurrence.resource_key.clone();
            crate::backend::bridge::request_hover_link_metadata(Arc::clone(&url), occurrence);
            let timeout = cx
                .background_executor()
                .timer(std::time::Duration::from_secs(90));
            futures_util::pin_mut!(timeout);
            loop {
                let changed = tokio::select! {
                    update = updates.recv() => match update {
                        Ok(key) => key == resource_key,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = timeout.as_mut() => break,
                };
                let keep_waiting = this
                    .update(cx, |this, cx| {
                        if !this.hover_link_request_is_current(generation, &url) {
                            return false;
                        }
                        if changed && crate::backend::bridge::get_embed_metadata(&url).is_some() {
                            this.sync_global_tooltip_overlay_from_state();
                            cx.notify();
                            return owner
                                .update(cx, |_, window, _| {
                                    if !window.is_window_hovered() {
                                        return false;
                                    }
                                    window.refresh();
                                    true
                                })
                                .unwrap_or(false);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_waiting {
                    break;
                }
            }
        })
        .detach();
    }

    fn hovered_link_url(&self) -> Option<&str> {
        match self.interaction_state.tooltip.hovered.as_ref()? {
            HitboxKind::Url { url, .. }
            | HitboxKind::EmbedPreview { url, .. }
            | HitboxKind::MediaInspector { url, .. } => Some(url.as_ref()),
            _ => None,
        }
    }

    fn hover_link_request_is_current(&self, generation: u64, url: &str) -> bool {
        self.wake_state.tooltip_delay_wake_generation == generation
            && self.is_tab_visible
            && !self.wake_state.window_hidden
            && self.scroll_controller.state.mouse_hovering_viewport
            && !self.interaction_state.selection.is_dragging
            && self.interaction_state.tooltip.should_show()
            && self.hovered_link_url() == Some(url)
            && crate::backend::bridge::hover_link_metadata_allowed(url)
    }

    pub(super) fn fetch_embeds_for_visible_range(&mut self, start_idx: usize, end_idx: usize) {
        use crate::backend::bridge::fetch_embed_metadata_async;

        if !matches!(
            crate::backend::settings::get_embed_display_mode(),
            crate::backend::settings::EmbedDisplayMode::Always
        ) || crate::backend::streamer_mode::should_hide_link_urls()
        {
            return;
        }

        let preload_start = start_idx.saturating_sub(8);
        let preload_end = (end_idx + 8).min(self.message_model.message_order.len());

        const EMBED_SCAN_INTERVAL_MS: u128 = 300;
        let current_range = (preload_start, preload_end);
        let range_changed = self.preload_state.last_embed_fetch_range != Some(current_range);
        let elapsed_ms = self
            .preload_state
            .last_embed_fetch_time
            .elapsed()
            .as_millis();
        if !range_changed && elapsed_ms < EMBED_SCAN_INTERVAL_MS {
            return;
        }
        if range_changed {
            self.preload_state.embed_fetch_scan_cursor = 0;
        }
        self.preload_state.last_embed_fetch_range = Some(current_range);
        self.preload_state.last_embed_fetch_time = std::time::Instant::now();

        let range_len = preload_end.saturating_sub(preload_start);
        if range_len == 0 {
            return;
        }
        let scan_count = range_len.min(EMBED_SCAN_MESSAGE_BUDGET);
        let start_offset = self.preload_state.embed_fetch_scan_cursor % range_len;
        self.preload_state.embed_fetch_scan_cursor = (start_offset + scan_count) % range_len;

        let mut media_plan = VisibleMediaPlan::with_capacity(0, scan_count, 0);

        for step in 0..scan_count {
            let idx = preload_start + ((start_offset + step) % range_len);

            let msg_id = &self.message_model.message_order[idx];
            let Some(message) = self.get_message(msg_id) else {
                continue;
            };
            if matches!(
                message.flags.system_type,
                Some(crate::backend::models::SystemMessageType::UploadSuccess { .. })
            ) {
                continue;
            }
            collect_message_visible_media(
                Some(&message.id),
                None,
                message.slots.as_ref(),
                false,
                true,
                None,
                &mut media_plan,
            );
        }

        for request in media_plan.embed_requests {
            let classified = crate::backend::link_types::classify_link(&request.url);
            if !crate::backend::settings::is_embed_classification_enabled(
                classified.provider,
                classified.content_kind,
            ) {
                continue;
            }
            let _ = fetch_embed_metadata_async(request.url, request.message_id, request.embed_id);
        }
    }

    /// Preload emote images for messages in visible range
    /// PERF: Triggers async emote loading for messages entering viewport during scroll
    /// Only triggers when viewport range actually changes to avoid spam
    pub(super) fn preload_emotes_for_visible_range(&mut self, start_idx: usize, end_idx: usize) {
        let _layout_context = self.enter_layout_context();
        let direction = self.detect_scroll_direction();
        let (margin_behind, margin_ahead) = self.get_adaptive_preload_margins(direction);

        let (preload_start, preload_end) = (
            start_idx.saturating_sub(margin_behind),
            (end_idx + margin_ahead).min(self.message_model.message_order.len()),
        );

        let current_range = (preload_start, preload_end);
        let range_changed = self.preload_state.last_emote_preload_range != Some(current_range);
        let elapsed_ms = self
            .preload_state
            .last_emote_preload_time
            .elapsed()
            .as_millis();
        if !range_changed && elapsed_ms < EMOTE_PRELOAD_INTERVAL_MS {
            return;
        }
        if range_changed {
            // Start at the viewport, not the margin behind it: new messages land
            // in the bottom rows, which a scan from `preload_start` misses once
            // the visible rows plus that margin exceed the per-pass budget.
            self.preload_state.emote_preload_scan_cursor = start_idx.saturating_sub(preload_start);
        }

        self.preload_state.last_emote_preload_range = Some(current_range);
        self.preload_state.last_emote_preload_time = std::time::Instant::now();

        let range_len = preload_end.saturating_sub(preload_start);
        if range_len == 0 {
            return;
        }
        let scan_count = range_len.min(EMOTE_PRELOAD_MESSAGE_BUDGET);
        let start_offset = self.preload_state.emote_preload_scan_cursor % range_len;
        self.preload_state.emote_preload_scan_cursor = (start_offset + scan_count) % range_len;

        let estimated_emotes = scan_count * 3;
        let mut media_plan = VisibleMediaPlan::with_capacity(estimated_emotes, 0, 0);
        let mut visible_users: Vec<(Arc<str>, Arc<str>, Arc<str>)> =
            Vec::with_capacity(preload_end.saturating_sub(preload_start).min(scan_count));
        let mut visible_user_ids: rustc_hash::FxHashSet<&str> = rustc_hash::FxHashSet::default();
        let mut resolver: Option<(Arc<str>, crate::backend::slots::resolver::SlotResolver)> = None;
        let display_rules = crate::backend::settings::emote_display_rules_snapshot();

        for step in 0..scan_count {
            let idx = preload_start + ((start_offset + step) % range_len);

            let msg_id = &self.message_model.message_order[idx];
            let Some(message) = self.get_message(msg_id) else {
                continue;
            };
            if resolver
                .as_ref()
                .is_none_or(|(channel, _)| channel.as_ref() != message.channel.as_ref())
            {
                resolver = Some((
                    Arc::clone(&message.channel),
                    crate::backend::slots::resolver::SlotResolver::new(Arc::clone(
                        &message.channel,
                    )),
                ));
            }
            let slot_resolver = &resolver.as_ref().expect("resolver initialized").1;

            if let Some(user_id) = message.user_id.as_ref()
                && visible_user_ids.insert(user_id.as_ref())
            {
                visible_users.push((
                    Arc::clone(user_id),
                    Arc::clone(&message.sender),
                    Arc::clone(&message.channel),
                ));
            }

            collect_message_visible_media(
                None,
                Some(slot_resolver),
                message.slots.as_ref(),
                true,
                false,
                display_rules
                    .as_deref()
                    .map(|rules| (rules, message.channel.as_ref())),
                &mut media_plan,
            );
        }

        trigger_visible_preload(
            &mut media_plan.emote_requests,
            "canvas.visible_preload.urls",
            image_cache::PreloadSource::VisibleChat,
        );
        trigger_visible_animated_preload(
            &mut media_plan.animated_emote_requests,
            "canvas.visible_preload.animated_urls",
            image_cache::PreloadSource::VisibleChat,
        );

        if !visible_users.is_empty() {
            if crate::backend::settings::get_show_live_chatter_indicator() {
                crate::backend::twitch_api::queue_visible_user_live_status_batch(&visible_users);
            }
            crate::backend::cosmetics::queue_visible_cosmetics_batch(visible_users);
        }
    }

    /// Prime async layouts for the current visible range (and any predicted landing zone)
    /// without forcing a synchronous render pass.
    pub fn prewarm_visible_layouts(&mut self) -> usize {
        let (start_idx, end_idx) = self.get_visible_range();
        self.preload_layouts_for_visible_range(start_idx, end_idx)
    }

    /// Preload the static image URLs that built layouts will draw through direct paint.
    ///
    /// This keeps image loading owned by the render/layout planning phase instead of
    /// letting `paint_single_image_direct` enqueue async work from the paint callback.
    pub(super) fn preload_static_images_for_visible_layouts(
        &mut self,
        start_idx: usize,
        end_idx: usize,
    ) -> usize {
        if self.viewport_state.visible_render_rows.is_empty() {
            return 0;
        }

        let current_range = (start_idx, end_idx);
        let range_changed =
            self.preload_state.last_layout_image_preload_range != Some(current_range);
        let elapsed_ms = self
            .preload_state
            .last_layout_image_preload_time
            .elapsed()
            .as_millis();
        if !range_changed && elapsed_ms < EMOTE_PRELOAD_INTERVAL_MS {
            return 0;
        }

        self.preload_state.last_layout_image_preload_range = Some(current_range);
        self.preload_state.last_layout_image_preload_time = std::time::Instant::now();

        let estimated_urls = self
            .viewport_state
            .visible_render_rows
            .iter()
            .map(|row| row.layout.image_paint_commands.len())
            .sum();
        let mut media_plan = VisibleMediaPlan::with_capacity(0, 0, estimated_urls);

        for row in &self.viewport_state.visible_render_rows {
            for cmd in &row.layout.image_paint_commands {
                if !cmd.is_animated {
                    media_plan.static_image_urls.push(cmd.url.clone());
                }
            }
        }

        trigger_visible_url_preload(
            &mut media_plan.static_image_urls,
            "canvas.visible_layout_preload.urls",
            image_cache::PreloadSource::VisibleLayout,
        )
    }

    /// Re-request visible images that are missing and that nothing is loading.
    ///
    /// Static images are unserved when they are neither resident, downloading
    /// nor failed; animated ones while their decode is not loaded. A failed
    /// image whose retry is due is claimed here too, so the retry does not
    /// depend on a paint of that exact image. The loader's enqueue cooldowns
    /// and retry budget bound repeats.
    pub(super) fn request_unserved_visible_images(&self) -> usize {
        let mut static_urls: Vec<SharedString> = Vec::new();
        let mut requested = 0usize;
        let mut request = |url: &str, is_animated: bool, static_urls: &mut Vec<SharedString>| {
            if image_cache::get_cached_render_image(url).is_some() {
                return;
            }
            if is_animated {
                if image_cache::get_current_animated_render_image(url).is_some() {
                    return;
                }
                match image_cache::get_animated_cache_state(url) {
                    image_cache::AnimatedCacheState::NotLoaded => {
                        image_cache::trigger_animated_decode_with_source(
                            url,
                            image_cache::PreloadSource::VisibleChat,
                        );
                        requested += 1;
                    }
                    image_cache::AnimatedCacheState::Failed => {
                        if image_cache::retry_failed_media(
                            url,
                            true,
                            image_cache::PreloadSource::VisibleChat,
                        ) {
                            requested += 1;
                        }
                    }
                    // Deduplicated in flight; requeues a decode whose static
                    // fallback was evicted.
                    image_cache::AnimatedCacheState::Loading => {
                        image_cache::trigger_animated_decode_with_source(
                            url,
                            image_cache::PreloadSource::VisibleChat,
                        );
                    }
                    image_cache::AnimatedCacheState::Ready => {}
                }
                return;
            }
            match image_cache::get_state_and_downloading(url) {
                (image_cache::ImageLoadState::Failed, _) => {
                    if image_cache::retry_failed_media(
                        url,
                        false,
                        image_cache::PreloadSource::VisibleChat,
                    ) {
                        requested += 1;
                    }
                }
                (_, true) => {}
                _ => static_urls.push(SharedString::from(url.to_string())),
            }
        };

        for row in &self.viewport_state.visible_render_rows {
            for cmd in &row.layout.image_paint_commands {
                request(cmd.url.as_ref(), cmd.is_animated, &mut static_urls);
            }
            for segment in &row.layout.segments {
                if let LayoutSegment::UploadPreview {
                    thumbnail_url: Some(url),
                    ..
                }
                | LayoutSegment::EmbedPreview {
                    thumbnail_url: Some(url),
                    ..
                } = segment
                {
                    request(url.as_ref(), false, &mut static_urls);
                }
            }
            for line in &row.layout.shaped_lines {
                if let Some(paint) = line.seventv_paint.as_deref() {
                    for (url, animated) in paint.image_layers() {
                        request(url, animated, &mut static_urls);
                    }
                }
            }
        }

        let static_count = trigger_visible_url_preload(
            &mut static_urls,
            "canvas.image_waker.rerequest",
            image_cache::PreloadSource::VisibleLayout,
        );
        if static_count + requested > 0 {
            crate::backend::metrics::record_emote_event("canvas.image_waker.rerequest");
        }
        static_count + requested
    }

    /// Register visible decoded-animation metadata for images painted by the unified canvas,
    /// and (optionally) compute the current visible-frame signature in the same pass.
    ///
    /// The animation scheduler runs before GPUI invokes canvas paint callbacks, so direct-paint
    /// animated emotes must publish visibility during render rather than from paint.
    ///
    /// PERF: registration and the frame signature share a single pass over the visible rows,
    /// with one `get_animation_metadata` DashMap probe per animated emote. Registration always
    /// happens for every metadata-resolved emote even when the signature is unavailable (a
    /// current frame missing or metadata not yet decoded), so the waker keeps repainting until
    /// frames are ready.
    ///
    /// Returns `(registered_count, frame)`. `frame` mirrors
    /// [`Self::visible_animation_frame_signature`]: `None` while any current frame is missing,
    /// while any emote lacks decoded metadata, or when no animated emote is visible.
    pub(super) fn register_and_signature_visible_direct_paint_animations(
        &self,
        want_signature: bool,
    ) -> (usize, Option<VisibleAnimationFrame>) {
        use std::hash::{Hash, Hasher};

        let mut registered = 0usize;
        let time_ms = crate::cache::frame_layer::current_animation_time_ms();
        let mut hasher = ahash::AHasher::default();
        let mut signature_count = 0usize;
        let mut signature_valid =
            want_signature && !self.viewport_state.visible_render_rows.is_empty();
        let mut next_change_ms: Option<u64> = None;

        for row in &self.viewport_state.visible_render_rows {
            let has_animated_name_paint = row.layout.shaped_lines.iter().any(|line| {
                line.seventv_paint
                    .as_deref()
                    .is_some_and(|paint| paint.is_animated())
            });
            if row.layout.animated_emote_count == 0 && !has_animated_name_paint {
                continue;
            }

            if signature_valid {
                row.message_index.hash(&mut hasher);
                row.layout.message_id_shared.as_ref().hash(&mut hasher);
            }

            for (command_index, cmd) in row.layout.image_paint_commands.iter().enumerate() {
                if !cmd.is_animated {
                    continue;
                }

                let Some(metadata) = image_cache::get_animation_metadata(cmd.url.as_ref()) else {
                    signature_valid = false;
                    continue;
                };

                if signature_valid {
                    let frame_idx = metadata.frame_index_at(time_ms);
                    if image_cache::has_animated_frame(cmd.url.as_ref(), frame_idx) {
                        command_index.hash(&mut hasher);
                        cmd.url.as_ref().hash(&mut hasher);
                        frame_idx.hash(&mut hasher);
                        signature_count += 1;
                        next_change_ms =
                            earliest_change(next_change_ms, metadata.ms_until_next_frame(time_ms));
                    } else {
                        signature_valid = false;
                    }
                }

                emote_renderer::register_visible_metadata(metadata);
                registered += 1;
            }

            for (line_index, line) in row.layout.shaped_lines.iter().enumerate() {
                let Some(paint) = line.seventv_paint.as_deref() else {
                    continue;
                };
                for (url, animated) in paint.image_layers() {
                    if !animated {
                        continue;
                    }
                    let Some(metadata) = image_cache::get_animation_metadata(url) else {
                        signature_valid = false;
                        continue;
                    };

                    if signature_valid {
                        let frame_idx = metadata.frame_index_at(time_ms);
                        if image_cache::has_animated_frame(url, frame_idx) {
                            1u8.hash(&mut hasher);
                            line_index.hash(&mut hasher);
                            url.hash(&mut hasher);
                            frame_idx.hash(&mut hasher);
                            signature_count += 1;
                            next_change_ms = earliest_change(
                                next_change_ms,
                                metadata.ms_until_next_frame(time_ms),
                            );
                        } else {
                            signature_valid = false;
                        }
                    }

                    emote_renderer::register_visible_metadata(metadata);
                    registered += 1;
                }
            }
        }

        let frame = if signature_valid && signature_count > 0 {
            signature_count.hash(&mut hasher);
            Some(VisibleAnimationFrame {
                signature: hasher.finish(),
                next_change_ms,
            })
        } else {
            None
        };

        (registered, frame)
    }

    /// Signature for visible animated images at their currently decoded frame.
    ///
    /// Returns `None` while any current frame is missing so the waker keeps
    /// repainting and direct paint can display newly-ready decoded frames.
    ///
    /// This is the standalone signature used by the animation waker between renders, where the
    /// registration side effect of
    /// [`Self::register_and_signature_visible_direct_paint_animations`] must NOT run (the waker
    /// does not own a render-frame `begin_split_frame` boundary). The render path computes
    /// the same value as part of the merged register pass.
    pub(super) fn visible_animation_frame_signature(&self) -> Option<VisibleAnimationFrame> {
        if self.viewport_state.visible_render_rows.is_empty() {
            return None;
        }

        use std::hash::{Hash, Hasher};

        let time_ms = crate::cache::frame_layer::current_animation_time_ms();
        let mut hasher = ahash::AHasher::default();
        let mut count = 0usize;
        let mut next_change_ms: Option<u64> = None;

        for row in &self.viewport_state.visible_render_rows {
            let has_animated_name_paint = row.layout.shaped_lines.iter().any(|line| {
                line.seventv_paint
                    .as_deref()
                    .is_some_and(|paint| paint.is_animated())
            });
            if row.layout.animated_emote_count == 0 && !has_animated_name_paint {
                continue;
            }

            row.message_index.hash(&mut hasher);
            row.layout.message_id_shared.as_ref().hash(&mut hasher);

            for (command_index, cmd) in row.layout.image_paint_commands.iter().enumerate() {
                if !cmd.is_animated {
                    continue;
                }

                let metadata = image_cache::get_animation_metadata(cmd.url.as_ref())?;
                let frame_idx = metadata.frame_index_at(time_ms);
                if !image_cache::has_animated_frame(cmd.url.as_ref(), frame_idx) {
                    return None;
                }

                command_index.hash(&mut hasher);
                cmd.url.as_ref().hash(&mut hasher);
                frame_idx.hash(&mut hasher);
                count += 1;
                next_change_ms =
                    earliest_change(next_change_ms, metadata.ms_until_next_frame(time_ms));
            }

            for (line_index, line) in row.layout.shaped_lines.iter().enumerate() {
                let Some(paint) = line.seventv_paint.as_deref() else {
                    continue;
                };
                for (url, animated) in paint.image_layers() {
                    if !animated {
                        continue;
                    }
                    let metadata = image_cache::get_animation_metadata(url)?;
                    let frame_idx = metadata.frame_index_at(time_ms);
                    if !image_cache::has_animated_frame(url, frame_idx) {
                        return None;
                    }

                    1u8.hash(&mut hasher);
                    line_index.hash(&mut hasher);
                    url.hash(&mut hasher);
                    frame_idx.hash(&mut hasher);
                    count += 1;
                    next_change_ms =
                        earliest_change(next_change_ms, metadata.ms_until_next_frame(time_ms));
                }
            }
        }

        if count == 0 {
            return None;
        }

        count.hash(&mut hasher);
        Some(VisibleAnimationFrame {
            signature: hasher.finish(),
            next_change_ms,
        })
    }

    /// Whether this split's current visible direct-paint rows are waiting on
    /// image cache fills, and whether a visible failed image is cooling down
    /// before its next retry.
    ///
    /// Keep this scoped to `visible_render_rows`: offscreen image loads must not wake this split,
    /// and image loads in another split must not force a layout/repaint here.
    pub(super) fn visible_image_load_misses(&self) -> VisibleImageMisses {
        let mut misses = VisibleImageMisses::default();
        for row in &self.viewport_state.visible_render_rows {
            for cmd in &row.layout.image_paint_commands {
                let url = cmd.url.as_ref();
                if cmd.is_animated {
                    if animated_image_settled(url) {
                        continue;
                    }
                    misses.note(
                        matches!(
                            image_cache::get_animated_cache_state(url),
                            image_cache::AnimatedCacheState::Failed
                        ),
                        url,
                    );
                } else if image_cache::get_cached_render_image(url).is_none() {
                    misses.note(
                        matches!(
                            image_cache::get_state(url),
                            image_cache::ImageLoadState::Failed
                        ),
                        url,
                    );
                }
            }

            for segment in &row.layout.segments {
                let url = match segment {
                    LayoutSegment::UploadPreview {
                        thumbnail_url: Some(url),
                        ..
                    }
                    | LayoutSegment::EmbedPreview {
                        thumbnail_url: Some(url),
                        ..
                    } => url.as_ref(),
                    _ => continue,
                };
                if image_cache::get_cached_render_image(url).is_some() {
                    continue;
                }
                misses.note(
                    matches!(
                        image_cache::get_state(url),
                        image_cache::ImageLoadState::Failed
                    ),
                    url,
                );
            }

            for line in &row.layout.shaped_lines {
                let Some(paint) = line.seventv_paint.as_deref() else {
                    continue;
                };
                for (url, animated) in paint.image_layers() {
                    if animated {
                        if animated_image_settled(url) {
                            continue;
                        }
                        misses.note(
                            matches!(
                                image_cache::get_animated_cache_state(url),
                                image_cache::AnimatedCacheState::Failed
                            ),
                            url,
                        );
                    } else if image_cache::get_cached_render_image(url).is_none() {
                        misses.note(
                            matches!(
                                image_cache::get_state(url),
                                image_cache::ImageLoadState::Failed
                            ),
                            url,
                        );
                    }
                }
            }
        }

        misses
    }

    /// Hash unresolved visible image state for this split only.
    ///
    /// A global image-ready generation is too coarse: when one split scrolls into
    /// uncached emotes, unrelated splits with any pending image can repaint. This
    /// signature changes only when one of this split's visible unresolved image
    /// URLs changes readiness/retry state.
    pub(super) fn visible_image_load_signature(&self) -> Option<u64> {
        if self.viewport_state.visible_render_rows.is_empty() {
            return None;
        }

        use std::hash::{Hash, Hasher};

        let mut hasher = ahash::AHasher::default();
        let mut pending_count = 0usize;

        for row in &self.viewport_state.visible_render_rows {
            row.message_index.hash(&mut hasher);
            row.layout.message_id_shared.as_ref().hash(&mut hasher);

            for (command_index, cmd) in row.layout.image_paint_commands.iter().enumerate() {
                if cmd.is_animated {
                    if animated_image_settled(cmd.url.as_ref()) {
                        continue;
                    }

                    let state = image_cache::get_animated_cache_state(cmd.url.as_ref());
                    let can_retry = image_cache::should_retry(cmd.url.as_ref());
                    if matches!(state, image_cache::AnimatedCacheState::Failed) && !can_retry {
                        continue;
                    }

                    row.message_index.hash(&mut hasher);
                    command_index.hash(&mut hasher);
                    cmd.url.as_ref().hash(&mut hasher);
                    1u8.hash(&mut hasher);
                    animated_cache_state_tag(&state).hash(&mut hasher);
                    can_retry.hash(&mut hasher);
                    pending_count += 1;
                } else {
                    if image_cache::get_cached_render_image(cmd.url.as_ref()).is_some() {
                        continue;
                    }

                    let (state, downloading) =
                        image_cache::get_state_and_downloading(cmd.url.as_ref());
                    let can_retry = image_cache::should_retry(cmd.url.as_ref());
                    if matches!(state, image_cache::ImageLoadState::Failed) && !can_retry {
                        continue;
                    }

                    row.message_index.hash(&mut hasher);
                    command_index.hash(&mut hasher);
                    cmd.url.as_ref().hash(&mut hasher);
                    0u8.hash(&mut hasher);
                    image_load_state_tag(&state).hash(&mut hasher);
                    downloading.hash(&mut hasher);
                    can_retry.hash(&mut hasher);
                    pending_count += 1;
                }
            }

            for (segment_index, segment) in row.layout.segments.iter().enumerate() {
                let url = match segment {
                    LayoutSegment::UploadPreview {
                        thumbnail_url: Some(url),
                        ..
                    }
                    | LayoutSegment::EmbedPreview {
                        thumbnail_url: Some(url),
                        ..
                    } => url,
                    _ => continue,
                };
                if image_cache::get_cached_render_image(url.as_ref()).is_some() {
                    continue;
                }

                let (state, downloading) = image_cache::get_state_and_downloading(url.as_ref());
                let can_retry = image_cache::should_retry(url.as_ref());
                if matches!(state, image_cache::ImageLoadState::Failed) && !can_retry {
                    continue;
                }

                row.message_index.hash(&mut hasher);
                segment_index.hash(&mut hasher);
                url.as_ref().hash(&mut hasher);
                4u8.hash(&mut hasher);
                image_load_state_tag(&state).hash(&mut hasher);
                downloading.hash(&mut hasher);
                can_retry.hash(&mut hasher);
                pending_count += 1;
            }

            for (line_index, line) in row.layout.shaped_lines.iter().enumerate() {
                let Some(paint) = line.seventv_paint.as_deref() else {
                    continue;
                };
                for (url, animated) in paint.image_layers() {
                    if animated {
                        if animated_image_settled(url) {
                            continue;
                        }
                        let state = image_cache::get_animated_cache_state(url);
                        let can_retry = image_cache::should_retry(url);
                        if matches!(state, image_cache::AnimatedCacheState::Failed) && !can_retry {
                            continue;
                        }

                        line_index.hash(&mut hasher);
                        url.hash(&mut hasher);
                        3u8.hash(&mut hasher);
                        animated_cache_state_tag(&state).hash(&mut hasher);
                        can_retry.hash(&mut hasher);
                        pending_count += 1;
                    } else {
                        if image_cache::get_cached_render_image(url).is_some() {
                            continue;
                        }
                        let (state, downloading) = image_cache::get_state_and_downloading(url);
                        let can_retry = image_cache::should_retry(url);
                        if matches!(state, image_cache::ImageLoadState::Failed) && !can_retry {
                            continue;
                        }

                        line_index.hash(&mut hasher);
                        url.hash(&mut hasher);
                        2u8.hash(&mut hasher);
                        image_load_state_tag(&state).hash(&mut hasher);
                        downloading.hash(&mut hasher);
                        can_retry.hash(&mut hasher);
                        pending_count += 1;
                    }
                }
            }
        }

        if pending_count == 0 {
            return None;
        }

        pending_count.hash(&mut hasher);
        Some(hasher.finish())
    }
}
