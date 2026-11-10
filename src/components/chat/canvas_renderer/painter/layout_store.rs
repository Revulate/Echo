//! Local layout cache plus split-local layout runtime metadata.

use super::*;

pub(super) struct LayoutStore {
    pub(super) layouts: EvictionTrackingCache<LayoutCacheKey, Arc<CanvasMessageLayout>>,
    pub(super) runtime: LayoutRuntimeState,
}

impl LayoutStore {
    pub(super) fn initial(container_width: f32, layout_scale_factor: f32) -> Self {
        Self {
            layouts: EvictionTrackingCache::new(NonZeroUsize::new(LAYOUT_CACHE_CAPACITY).unwrap()),
            runtime: LayoutRuntimeState::initial(container_width, layout_scale_factor),
        }
    }
}
