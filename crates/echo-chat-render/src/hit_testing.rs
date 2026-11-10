//! Hitbox management for interactive elements
//!
//! Provides efficient hit testing for hover and click detection
//! on emotes, badges, usernames, URLs, and mentions.

use gpui::{Bounds, Pixels, Point, point, px};

use super::layout::{HitboxKind, HitboxRegion};

/// Check if a point is within bounds
fn point_in_bounds(point: Point<Pixels>, bounds: &Bounds<Pixels>) -> bool {
    point.x >= bounds.origin.x
        && point.x < bounds.origin.x + bounds.size.width
        && point.y >= bounds.origin.y
        && point.y < bounds.origin.y + bounds.size.height
}

// ============================================================================
// Spatial Grid for O(n/k) Hitbox Lookups
// ============================================================================

/// Minimum grid cell height (prevents too many cells on small viewports)
const MIN_GRID_CELL_HEIGHT: f32 = 30.0;

/// Maximum grid cell height (prevents too few cells on large viewports)
const MAX_GRID_CELL_HEIGHT: f32 = 100.0;

/// Target number of cells for typical viewport (balance memory vs lookup speed)
const TARGET_CELL_COUNT: usize = 20;

/// Spatial grid for efficient hitbox lookups
/// Partitions the viewport into horizontal bands for O(n/k) search instead of O(n)
/// PERF: Uses adaptive cell sizing based on viewport height
#[derive(Debug)]
pub struct HitboxGrid {
    /// Grid cells containing indices into hitboxes vec
    cells: Vec<Vec<usize>>,
    /// Total viewport height
    viewport_height: f32,
    /// Computed cell height for this viewport
    cell_height: f32,
}

impl HitboxGrid {
    /// Create a new grid with adaptive cell sizing based on viewport height
    pub fn new(viewport_height: f32) -> Self {
        // Compute optimal cell height: target ~20 cells, clamped to reasonable range
        let cell_height = (viewport_height / TARGET_CELL_COUNT as f32)
            .clamp(MIN_GRID_CELL_HEIGHT, MAX_GRID_CELL_HEIGHT);
        let cell_count = ((viewport_height / cell_height) as usize).max(1) + 1;

        Self {
            cells: vec![Vec::with_capacity(8); cell_count],
            viewport_height,
            cell_height,
        }
    }

    /// Insert a hitbox index into all overlapping cells
    pub fn insert(&mut self, idx: usize, y_min: f32, y_max: f32) {
        let start_cell = (y_min.max(0.0) / self.cell_height) as usize;
        let end_cell = (y_max / self.cell_height) as usize;

        for cell in start_cell..=end_cell.min(self.cells.len().saturating_sub(1)) {
            self.cells[cell].push(idx);
        }
    }

    /// Get candidate hitbox indices for the given Y coordinate
    #[inline]
    pub fn candidates(&self, y: f32) -> &[usize] {
        let cell = (y.max(0.0) / self.cell_height) as usize;
        self.cells.get(cell).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Clear all cells
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            cell.clear();
        }
    }
}

impl Default for HitboxGrid {
    fn default() -> Self {
        Self::new(600.0) // Default viewport height
    }
}

// ============================================================================
// Hitbox Manager
// ============================================================================

/// Manager for tracking hitboxes during paint
/// PERF: Caches hitboxes between frames when visible range hasn't changed
#[derive(Debug, Default)]
pub struct HitboxManager {
    /// Active hitboxes from current frame
    pub hitboxes: Vec<ManagedHitbox>,
    /// Spatial grid for efficient lookups
    grid: HitboxGrid,
    /// Cached visible range for invalidation check
    cached_visible_range: Option<(usize, usize)>,
    /// Cached scroll offset for invalidation check
    cached_scroll_offset: f32,
}

/// A managed hitbox with GPUI handle
#[derive(Debug)]
pub struct ManagedHitbox {
    pub bounds: Bounds<Pixels>,
    pub kind: HitboxKind,
    pub message_id: String,
}

impl HitboxManager {
    /// Create new hitbox manager
    pub fn new() -> Self {
        Self::default()
    }

    /// PERF: Check if cache is still valid for current view state
    /// Returns true if hitboxes don't need to be rebuilt
    /// Note: Scroll threshold removed - hitboxes store viewport-relative Y positions
    /// so they must be rebuilt on ANY scroll change to stay accurate for hit testing
    #[inline]
    pub fn is_cache_valid(&self, start: usize, end: usize, scroll: f32) -> bool {
        self.cached_visible_range == Some((start, end))
            && (self.cached_scroll_offset - scroll).abs() < 0.1 // Essentially any scroll change
    }

    /// Returns true when the cached hitboxes cover the same visible range and can be
    /// cheaply translated for a pure-scroll update instead of rebuilt from layouts.
    #[inline]
    pub fn can_translate_for_scroll(&self, start: usize, end: usize, scroll: f32) -> bool {
        self.cached_visible_range == Some((start, end))
            && (self.cached_scroll_offset - scroll).abs() >= 0.1
            && !self.hitboxes.is_empty()
    }

    /// PERF: Mark cache as valid for current state (call after rebuilding hitboxes)
    #[inline]
    pub fn validate_cache(&mut self, start: usize, end: usize, scroll: f32) {
        self.cached_visible_range = Some((start, end));
        self.cached_scroll_offset = scroll;
    }

    /// Clear all hitboxes (call before each paint)
    pub fn clear(&mut self) {
        self.hitboxes.clear();
        self.grid.clear();
        // Note: Don't clear cache here - we check it before calling clear()
    }

    /// Invalidate the cache (call when messages change)
    pub fn invalidate_cache(&mut self) {
        self.cached_visible_range = None;
    }

    /// Resize grid for new viewport height
    pub fn set_viewport_height(&mut self, viewport_height: f32) {
        // Rebuild grid if viewport changed significantly (more than one cell height)
        if (viewport_height - self.grid.viewport_height).abs() > self.grid.cell_height {
            self.grid = HitboxGrid::new(viewport_height);
            self.cached_visible_range = None; // Invalidate cache on resize
        }
    }

    /// Translate all cached hitboxes by a viewport-space Y delta and rebuild only the
    /// spatial grid. This avoids re-walking layouts when the visible range is unchanged.
    pub fn translate_for_scroll(&mut self, delta_y: f32, new_scroll: f32) {
        if delta_y.abs() < 0.1 {
            self.cached_scroll_offset = new_scroll;
            return;
        }

        self.grid.clear();
        for (idx, hitbox) in self.hitboxes.iter_mut().enumerate() {
            hitbox.bounds.origin.y += px(delta_y);
            let y_min: f32 = hitbox.bounds.origin.y.into();
            let height: f32 = hitbox.bounds.size.height.into();
            let y_max = y_min + height;
            self.grid.insert(idx, y_min, y_max);
        }

        self.cached_scroll_offset = new_scroll;
    }

    /// Translate cached hitboxes to match a new scroll offset when the visible range is unchanged.
    /// Returns true if translation was applied.
    pub fn translate_cached_range_to_scroll(
        &mut self,
        start: usize,
        end: usize,
        new_scroll: f32,
    ) -> bool {
        if !self.can_translate_for_scroll(start, end, new_scroll) {
            return false;
        }
        let delta = self.cached_scroll_offset - new_scroll;
        self.translate_for_scroll(delta, new_scroll);
        true
    }

    /// Add a hitbox from layout
    pub fn add(&mut self, region: &HitboxRegion, message_id: &str, message_y: f32) {
        let idx = self.hitboxes.len();

        let bounds = Bounds::new(
            point(
                region.bounds.origin.x,
                px(message_y) + region.bounds.origin.y,
            ),
            region.bounds.size,
        );

        // Insert into spatial grid
        let y_min: f32 = bounds.origin.y.into();
        let height: f32 = bounds.size.height.into();
        let y_max: f32 = y_min + height;
        self.grid.insert(idx, y_min, y_max);

        self.hitboxes.push(ManagedHitbox {
            bounds,
            kind: region.kind.clone(),
            message_id: message_id.to_string(),
        });
    }

    /// Find hitbox at position using spatial grid for O(n/k) lookup
    pub fn hit_test(&self, pos: Point<Pixels>) -> Option<&ManagedHitbox> {
        let y: f32 = pos.y.into();

        // Get candidates from the grid cell containing this Y coordinate
        let candidates = self.grid.candidates(y);

        // Search candidates in reverse order for proper z-ordering (last added = on top)
        for &idx in candidates.iter().rev() {
            if let Some(hitbox) = self.hitboxes.get(idx)
                && point_in_bounds(pos, &hitbox.bounds)
            {
                // Skip message background for regular hit testing
                if !matches!(hitbox.kind, HitboxKind::MessageBackground { .. }) {
                    return Some(hitbox);
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{HitboxKind, HitboxRegion};
    use gpui::size;

    fn approx_eq(a: f32, b: f32) {
        assert!(
            (a - b).abs() < 0.01,
            "expected values to be close: left={a}, right={b}"
        );
    }

    fn mention_region(x: f32, y: f32, w: f32, h: f32) -> HitboxRegion {
        HitboxRegion {
            bounds: Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
            kind: HitboxKind::Mention {
                username: "tester".into(),
            },
        }
    }

    #[test]
    fn translate_cached_range_to_scroll_updates_bounds_and_cache_scroll() {
        let mut manager = HitboxManager::new();
        manager.add(&mention_region(10.0, 5.0, 24.0, 10.0), "m1", 100.0);
        manager.validate_cache(3, 7, 120.0);

        let translated = manager.translate_cached_range_to_scroll(3, 7, 140.0);
        assert!(
            translated,
            "expected translation for matching visible range"
        );

        let translated_y: f32 = manager.hitboxes[0].bounds.origin.y.into();
        approx_eq(translated_y, 85.0);
        approx_eq(manager.cached_scroll_offset, 140.0);

        let hit = manager.hit_test(point(px(12.0), px(88.0)));
        assert!(hit.is_some(), "translated hitbox should still be hittable");
    }

    #[test]
    fn translate_cached_range_to_scroll_respects_guards() {
        let mut manager = HitboxManager::new();
        manager.add(&mention_region(0.0, 0.0, 20.0, 10.0), "m1", 50.0);
        manager.validate_cache(1, 4, 100.0);

        // Tiny deltas should not trigger translation.
        assert!(!manager.translate_cached_range_to_scroll(1, 4, 100.05));

        // Different visible ranges should not reuse cached hitboxes.
        assert!(!manager.translate_cached_range_to_scroll(2, 5, 130.0));
    }
}
