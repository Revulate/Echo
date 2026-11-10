//! Layout cache lookup, insertion, and secondary index maintenance.

use super::*;

use crate::components::chat::canvas_renderer::layout;

impl CanvasMessageList {
    pub(in crate::components::chat::canvas_renderer) fn layout_quantized_width_for_cache(
        &self,
    ) -> u32 {
        let active = self.layout_store.runtime.active_layout_quantized_width;
        if active != 0 {
            active
        } else {
            quantize_width(self.container_width)
        }
    }

    #[inline]
    pub(super) fn layout_effective_width_for_builds(&self) -> f32 {
        let q = self.layout_quantized_width_for_cache();
        if q == 0 {
            self.container_width
        } else {
            width_from_quantized(q)
        }
    }

    /// Build and look up layouts for this canvas's window scale and moderation
    /// mode until the returned guards drop.
    #[inline]
    pub(in crate::components::chat::canvas_renderer) fn enter_layout_context(
        &self,
    ) -> (
        crate::components::chat::canvas_renderer::layout_scale::ScaleScope,
        crate::components::chat::canvas_renderer::layout_mode::ModeScope,
    ) {
        (
            crate::components::chat::canvas_renderer::layout_scale::enter(self.layout_scale_factor),
            crate::components::chat::canvas_renderer::layout_mode::enter(self.mod_buttons_active),
        )
    }

    #[inline]
    pub(super) fn layout_cache_key_from_shared(&self, message_id: SharedString) -> LayoutCacheKey {
        LayoutCacheKey::from_shared_quantized(
            message_id,
            self.layout_quantized_width_for_cache(),
            self.show_timestamp,
        )
    }

    pub(super) fn find_any_layout_for_message(
        &self,
        shared_id: &SharedString,
    ) -> Option<Arc<layout::CanvasMessageLayout>> {
        if let Some(widths) = self.layout_store.runtime.cached_width_index.get(shared_id) {
            for &(quantized, show_timestamp) in widths.iter() {
                if show_timestamp != self.show_timestamp {
                    continue;
                }
                let cache_key = layout::LayoutCacheKey {
                    message_id: shared_id.clone(),
                    quantized_width: quantized,
                    show_timestamp,
                };
                if let Some(layout) = self.peek_cached_layout(&cache_key) {
                    return Some(layout);
                }
            }
        }
        None
    }

    #[inline]
    pub(super) fn fallback_layout_height(&self, shared_id: &SharedString) -> Option<f32> {
        self.find_any_layout_for_message(shared_id)
            .map(|layout| layout.height)
    }

    #[inline]
    pub(super) fn request_primed_async_layout(&mut self, msg: &Arc<ChatMessage>) {
        self.render_perf_frame_stats.async_layout_requests += 1;
        crate::log_perf_render!(
            trace,
            channel_id = %self.channel_id,
            msg_id = %msg.id,
            container_width = self.container_width,
            "Requesting async layout"
        );
        self.request_async_layout(msg);
    }

    /// Update a row height while preserving the current top-edge anchor.
    ///
    /// Deferred history/async layout can resolve rows above the viewport after
    /// they were inserted with estimated heights. Without compensating the
    /// scroll offsets by the height delta, resolving those offscreen rows shifts
    /// the visible content in the split that is scrolling older messages.
    pub(super) fn cache_layout_impl(
        layouts: &mut EvictionTrackingCache<LayoutCacheKey, Arc<layout::CanvasMessageLayout>>,
        cached_width_index: &mut FxHashMap<SharedString, SmallVec<[(u32, bool); 4]>>,
        key: LayoutCacheKey,
        layout: Arc<layout::CanvasMessageLayout>,
    ) {
        let entry = (key.quantized_width, key.show_timestamp);
        let widths = cached_width_index
            .entry(key.message_id.clone())
            .or_default();
        if !widths.contains(&entry) {
            widths.push(entry);
        }
        layouts.put(key, layout);
    }

    /// Cache a layout and maintain the secondary width index (convenience wrapper)
    /// PERF: Wraps layout in Arc for O(1) cache insertion
    /// Also inserts into global cache for cross-split sharing (60-80% memory savings)
    pub(in crate::components::chat::canvas_renderer) fn cache_layout(
        &mut self,
        key: LayoutCacheKey,
        layout: layout::CanvasMessageLayout,
    ) {
        self.cache_layout_arc(key, Arc::new(layout));
    }

    /// Cache an already-shared layout without cloning its shaped lines, paint
    /// commands, hitboxes, or source text. Async workers publish layouts as
    /// `Arc`s, so their completion path can transfer that ownership directly.
    pub(in crate::components::chat::canvas_renderer) fn cache_layout_arc(
        &mut self,
        key: LayoutCacheKey,
        layout: Arc<layout::CanvasMessageLayout>,
    ) {
        if !layout.uses_window_text_metrics {
            let supersedes = |cached: &layout::CanvasMessageLayout| {
                cached.uses_window_text_metrics
                    && !cached.is_stale_for_message(layout.content_fingerprint)
            };
            if self
                .layout_store
                .layouts
                .peek(&key)
                .is_some_and(|cached| supersedes(cached))
            {
                return;
            }
            if global_layout_cache::get_layout(&key).is_some_and(|cached| supersedes(&cached)) {
                return;
            }
        }

        global_layout_cache::insert_layout(key.clone(), Arc::clone(&layout));

        Self::cache_layout_impl(
            &mut self.layout_store.layouts,
            &mut self.layout_store.runtime.cached_width_index,
            key,
            layout,
        );
        // Drain the eviction this insert may have caused. The other drains run
        // only on invalidation paths, so in steady chat the width index and the
        // eviction list otherwise grew by one entry per message.
        self.cleanup_width_index();
    }

    /// Get a layout from cache, checking local cache first then global cache.
    /// PERF: Global cache allows cross-split sharing (60-80% memory savings)
    /// PERF: Local cache provides fast access for recently used layouts
    /// Note: Updates LRU on local cache hit; increments per-source hit counters.
    pub(super) fn get_cached_layout(
        &mut self,
        key: &LayoutCacheKey,
    ) -> Option<Arc<layout::CanvasMessageLayout>> {
        let _layout_context = self.enter_layout_context();
        if let Some(layout) = self.layout_store.layouts.get(key) {
            self.render_perf_frame_stats.layout_hit_split_cache += 1;
            return Some(Arc::clone(layout));
        }

        if let Some(layout) = global_layout_cache::get_layout(key) {
            self.render_perf_frame_stats.layout_hit_global_cache += 1;
            return Some(layout);
        }

        None
    }

    /// Peek at a layout from cache without updating LRU
    /// PERF: Use this for read-only operations like selection
    pub(in crate::components::chat::canvas_renderer) fn peek_cached_layout(
        &self,
        key: &LayoutCacheKey,
    ) -> Option<Arc<layout::CanvasMessageLayout>> {
        let _layout_context = self.enter_layout_context();
        if let Some(layout) = self.layout_store.layouts.peek(key) {
            return Some(Arc::clone(layout));
        }

        if let Some(layout) = global_layout_cache::peek_layout(key) {
            return Some(layout);
        }

        None
    }

    /// PERF: Clean up orphaned entries in cached_width_index using eviction tracking
    /// O(k) where k = evictions since last cleanup (vs O(n*m) full scan)
    pub(super) fn cleanup_width_index(&mut self) {
        let evicted: SmallVec<[LayoutCacheKey; 4]> =
            self.layout_store.layouts.drain_evictions().collect();
        for evicted_key in evicted {
            // Popped and cleared keys are recorded as evictions too, and the
            // same key is often cached again right away (stale refresh, height
            // resolution). Keep the index entry for a layout that is cached.
            if self.layout_store.layouts.peek(&evicted_key).is_some() {
                continue;
            }
            if let Some(widths) = self
                .layout_store
                .runtime
                .cached_width_index
                .get_mut(&evicted_key.message_id)
            {
                widths.retain(|&mut (w, ts)| {
                    w != evicted_key.quantized_width || ts != evicted_key.show_timestamp
                });
                if widths.is_empty() {
                    self.layout_store
                        .runtime
                        .cached_width_index
                        .remove(&evicted_key.message_id);
                }
            }
        }
    }
}
