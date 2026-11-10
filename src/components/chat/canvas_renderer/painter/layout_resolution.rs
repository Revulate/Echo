//! Layout height resolution shared by append, pending, and rebuild passes.

use super::*;
use crate::components::chat::canvas_renderer::{async_layout, layout};

#[derive(Clone, Copy)]
pub(super) enum LayoutHeightMode {
    Incremental,
    FullRebuild {
        force_layout_refresh: bool,
        should_prime_layout: bool,
    },
}

impl LayoutHeightMode {
    #[inline]
    fn force_layout_refresh(self) -> bool {
        matches!(
            self,
            Self::FullRebuild {
                force_layout_refresh: true,
                ..
            }
        )
    }

    #[inline]
    fn should_prime_layout(self) -> bool {
        match self {
            Self::Incremental => true,
            Self::FullRebuild {
                should_prime_layout,
                ..
            } => should_prime_layout,
        }
    }
}

impl CanvasMessageList {
    pub(super) fn resolve_layout_height<F>(
        &mut self,
        msg: &Arc<ChatMessage>,
        cache_key: LayoutCacheKey,
        mode: LayoutHeightMode,
        effective_layout_width: f32,
        current_emote_version: u64,
        current_channel_emote_version: u64,
        window: &mut Window,
        allow_sync_layout: &mut F,
    ) -> f32
    where
        F: FnMut() -> bool,
    {
        if !mode.force_layout_refresh()
            && let Some(layout) = self
                .get_cached_layout(&cache_key)
                .filter(|layout| layout.content_fingerprint == message_content_fingerprint(msg))
        {
            self.render_perf_frame_stats.layout_cache_hits += 1;
            crate::log_perf_render!(
                trace,
                channel_id = %self.channel_id,
                msg_id = %msg.id,
                cache_quantized = cache_key.quantized_width,
                "CACHE HIT - using cached layout"
            );
            return layout.height;
        }

        self.render_perf_frame_stats.layout_cache_misses += 1;
        crate::log_perf_render!(
            trace,
            channel_id = %self.channel_id,
            msg_id = %msg.id,
            force_layout_refresh = mode.force_layout_refresh(),
            "CACHE MISS - checking async status"
        );

        use async_layout::LayoutStatus;
        match self.get_async_layout_status(msg) {
            LayoutStatus::Ready(layout) => self.resolve_ready_layout_height(
                msg,
                cache_key,
                layout,
                mode,
                current_emote_version,
                current_channel_emote_version,
            ),
            LayoutStatus::Pending | LayoutStatus::Computing => self.resolve_pending_layout_height(
                msg,
                cache_key,
                mode,
                effective_layout_width,
                current_emote_version,
                window,
                allow_sync_layout,
            ),
            LayoutStatus::Failed(_) => self.resolve_failed_layout_height(
                msg,
                cache_key,
                mode,
                effective_layout_width,
                current_emote_version,
                window,
                allow_sync_layout,
            ),
        }
    }

    fn resolve_ready_layout_height(
        &mut self,
        msg: &Arc<ChatMessage>,
        cache_key: LayoutCacheKey,
        layout: Arc<CanvasMessageLayout>,
        mode: LayoutHeightMode,
        current_emote_version: u64,
        current_channel_emote_version: u64,
    ) -> f32 {
        let layout_quantized = self
            .async_layout_manager
            .get_layout_width(
                &msg.id,
                self.show_timestamp,
                message_content_fingerprint(msg),
                current_emote_version,
                current_channel_emote_version,
            )
            .unwrap_or_else(|| layout::quantize_width(self.container_width));

        if layout_quantized == cache_key.quantized_width {
            self.render_perf_frame_stats.async_ready_valid_width += 1;
            self.render_perf_frame_stats.layout_hit_async_ready += 1;
            crate::log_perf_render!(
                trace,
                channel_id = %self.channel_id,
                msg_id = %msg.id,
                layout_quantized,
                cache_quantized = cache_key.quantized_width,
                "Async layout ready (width valid), moving to cache"
            );
            let height = layout.height;
            self.cache_layout_arc(cache_key, layout);
            height
        } else {
            self.render_perf_frame_stats.async_ready_stale_width += 1;
            crate::log_perf_render!(
                trace,
                channel_id = %self.channel_id,
                msg_id = %msg.id,
                layout_quantized,
                cache_quantized = cache_key.quantized_width,
                "Async layout ready but width stale, using temporarily"
            );
            if mode.should_prime_layout() {
                self.render_perf_frame_stats.async_layout_requests += 1;
                self.request_async_layout(msg);
            }
            layout.height
        }
    }

    fn resolve_pending_layout_height<F>(
        &mut self,
        msg: &Arc<ChatMessage>,
        cache_key: LayoutCacheKey,
        mode: LayoutHeightMode,
        effective_layout_width: f32,
        current_emote_version: u64,
        window: &mut Window,
        allow_sync_layout: &mut F,
    ) -> f32
    where
        F: FnMut() -> bool,
    {
        match mode {
            LayoutHeightMode::Incremental => {
                if allow_sync_layout() {
                    self.build_sync_layout_height(
                        msg,
                        cache_key,
                        effective_layout_width,
                        current_emote_version,
                        window,
                    )
                } else {
                    self.render_perf_frame_stats.async_layout_requests += 1;
                    self.request_async_layout(msg);
                    self.fallback_layout_height_or_estimated(&cache_key.message_id)
                }
            }
            LayoutHeightMode::FullRebuild {
                force_layout_refresh: true,
                should_prime_layout,
            } => {
                if let Some(height) = self.fallback_layout_height(&cache_key.message_id) {
                    if should_prime_layout {
                        self.request_primed_async_layout(msg);
                    }
                    height
                } else if should_prime_layout && allow_sync_layout() {
                    self.build_sync_layout_height(
                        msg,
                        cache_key,
                        effective_layout_width,
                        current_emote_version,
                        window,
                    )
                } else {
                    if should_prime_layout {
                        self.request_primed_async_layout(msg);
                    }
                    self.message_model.estimated_row_height()
                }
            }
            LayoutHeightMode::FullRebuild {
                force_layout_refresh: false,
                should_prime_layout,
            } => {
                let fallback_layout = self.find_any_layout_for_message(&cache_key.message_id);
                if should_prime_layout && allow_sync_layout() {
                    self.build_sync_layout_height(
                        msg,
                        cache_key,
                        effective_layout_width,
                        current_emote_version,
                        window,
                    )
                } else if let Some(layout_arc) = fallback_layout {
                    if should_prime_layout {
                        self.request_primed_async_layout(msg);
                    }
                    layout_arc.height
                } else {
                    if should_prime_layout {
                        self.request_primed_async_layout(msg);
                    }
                    self.message_model.estimated_row_height()
                }
            }
        }
    }

    fn resolve_failed_layout_height<F>(
        &mut self,
        msg: &Arc<ChatMessage>,
        cache_key: LayoutCacheKey,
        mode: LayoutHeightMode,
        effective_layout_width: f32,
        current_emote_version: u64,
        window: &mut Window,
        allow_sync_layout: &mut F,
    ) -> f32
    where
        F: FnMut() -> bool,
    {
        crate::log_perf_render!(
            info,
            channel_id = %self.channel_id,
            msg_id = %msg.id,
            "Async failed, building synchronously"
        );

        match mode {
            LayoutHeightMode::Incremental => {
                if allow_sync_layout() {
                    self.build_sync_layout_height(
                        msg,
                        cache_key,
                        effective_layout_width,
                        current_emote_version,
                        window,
                    )
                } else {
                    self.render_perf_frame_stats.async_layout_requests += 1;
                    self.request_async_layout(msg);
                    self.fallback_layout_height_or_estimated(&cache_key.message_id)
                }
            }
            LayoutHeightMode::FullRebuild {
                should_prime_layout,
                ..
            } => {
                if should_prime_layout && allow_sync_layout() {
                    self.build_sync_layout_height(
                        msg,
                        cache_key,
                        effective_layout_width,
                        current_emote_version,
                        window,
                    )
                } else {
                    if should_prime_layout {
                        self.render_perf_frame_stats.async_layout_requests += 1;
                        self.request_async_layout(msg);
                    }
                    self.fallback_layout_height_or_estimated(&cache_key.message_id)
                }
            }
        }
    }

    fn build_sync_layout_height(
        &mut self,
        msg: &Arc<ChatMessage>,
        cache_key: LayoutCacheKey,
        effective_layout_width: f32,
        current_emote_version: u64,
        window: &mut Window,
    ) -> f32 {
        let _layout_context = self.enter_layout_context();
        self.render_perf_frame_stats.layout_miss_sync_shape += 1;
        let mut layout = build_message_layout(
            msg,
            &self.channel_name,
            effective_layout_width,
            self.show_timestamp,
            Some(window),
        );
        layout.mark_emote_versions_current(current_emote_version);
        let height = layout.height;
        self.cache_layout(cache_key, layout);
        height
    }

    #[inline]
    fn fallback_layout_height_or_estimated(&self, message_id: &SharedString) -> f32 {
        self.fallback_layout_height(message_id)
            .unwrap_or_else(|| self.message_model.estimated_row_height())
    }
}
