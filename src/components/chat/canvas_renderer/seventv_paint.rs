//! Exact 7TV name-paint compositing.
//!
//! Replicates 7TV CSS paint semantics over the canvas renderer's
//! shaped usernames:
//!
//! - continuous `linear-gradient` / `radial-gradient` / `repeating-*` fills with
//!   exact stop positions and angles across the whole username box
//! - image-texture paints clipped to the glyphs (`background-clip: text`)
//! - `drop-shadow` layers with real gaussian blur shaped by the glyph coverage
//!
//! The glyph coverage mask comes from the local GPUI fork
//! (`Window::rasterize_shaped_line_mask`, see `docs/GPUI_FORK.md`).
//!
//! Where the window's renderer supports it (`Window::supports_painted_text`),
//! the composite is rebuilt on the GPU each frame (`Window::paint_painted_text`):
//! Echo keeps one 8-bit mask per username holding the glyph coverage and each
//! drop shadow's final blurred weight, and the shader adds the stroke, the fill
//! (sampling an image paint's current frame from the atlas) and the shadows.
//! A paint's fill alpha does not depend on its colors or texture frame when
//! the base color is opaque, which is what lets the shadow weights be
//! computed once.
//!
//! Otherwise fill generation, blurring and layer compositing happen here on the
//! CPU once per (username, paint, font size, scale, fill frame) and are cached
//! as a flattened `RenderImage`. Animated textured paints with shadows or a
//! stroke cache those once and composite only the fill per frame.

use gpui::{
    Bounds, Corners, PAINTED_TEXT_LAYER_CIRCLE, PAINTED_TEXT_LAYER_ELLIPSE,
    PAINTED_TEXT_LAYER_IMAGE, PAINTED_TEXT_LAYER_LINEAR, PAINTED_TEXT_LAYER_SOLID,
    PAINTED_TEXT_MAX_LAYERS, PAINTED_TEXT_MAX_SHADOWS, PAINTED_TEXT_MAX_STOPS, PaintMask,
    PaintedText, PaintedTextLayer, PaintedTextStop, Point, RenderImage, ShapedLine, Window, point,
    px,
};
use image::{Frame, RgbaImage};
use lru::LruCache;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use crate::backend::cosmetics_core::{
    MAX_7TV_SHADOW_RENDER_LAYERS, RgbaColor, SevenTvGradientKind, SevenTvNamePaint,
    SevenTvPaintShadow, SevenTvRadialShape, css_gradient_direction, gaussian_box_blur_radii,
    project_rect_bounds, repeating_gradient_ratio, sample_gradient_stops,
};
use crate::cache::loader as image_cache;

use super::layout::is_animated_url;

/// Byte budget for cached composited username textures.
///
/// A typical painted username at 2x scale is roughly 500x60 device pixels
/// (~120 KB BGRA). Multi-shadow padding can grow entries to roughly 5.7x with
/// three shadows. Animated paints use a cache key per frame for the unpadded
/// fill sprite only (their padded shadow/stroke sprite is shared by every
/// frame); eviction mid-animation makes every frame a permanent miss. 48 MiB
/// fits a realistic animated working set while bounding worst-case memory
/// like the other renderer caches (see GlobalLayoutCache / decoded-media budgets).
const COMPOSITE_CACHE_BYTE_BUDGET: usize = 48 * 1024 * 1024;

/// Byte budget for GPU name-paint masks: one 8-bit mask per username, plus a
/// block per drop shadow, instead of a BGRA composite per animation frame.
const GPU_MASK_CACHE_BYTE_BUDGET: usize = 16 * 1024 * 1024;

/// Longest edge of a GPU paint mask tile (the sprite atlas texture limit).
const MAX_PAINT_MASK_EDGE: usize = 16_384;
static COMPOSITE_RETIREMENTS_QUEUED: AtomicU64 = AtomicU64::new(0);

pub use echo_chat_render::seventv_paint::SevenTvLinePaint;

/// A cached paint ready to paint over the username.
#[derive(Clone)]
pub struct PaintedLineOverlay(OverlayKind);

#[derive(Clone)]
enum OverlayKind {
    /// A CPU composite: usually one sprite. Animated image-texture paints with
    /// shadows or a stroke are split into a frame-independent shadow/stroke
    /// sprite (`under`) and a per-frame fill sprite, so the blur runs once
    /// instead of once per frame.
    Composite {
        under: Option<OverlaySprite>,
        sprite: OverlaySprite,
    },
    /// Rebuilt on the GPU from a cached mask; `fill` is the image layer's
    /// current texture.
    Gpu {
        mask: Arc<GpuPaintMask>,
        fill: Option<Arc<RenderImage>>,
    },
}

/// A username's GPU paint mask and the shader instance that recombines it.
struct GpuPaintMask {
    mask: PaintMask,
    instance: PaintedText,
    /// Placement relative to the shaped line's paint origin, in logical pixels.
    geometry: [f32; 4],
}

impl GpuPaintMask {
    fn bounds(&self, line_origin: Point<gpui::Pixels>) -> Bounds<gpui::Pixels> {
        let [x, y, width, height] = self.geometry;
        Bounds::new(
            line_origin + point(px(x), px(y)),
            gpui::size(px(width), px(height)),
        )
    }
}

impl Drop for GpuPaintMask {
    fn drop(&mut self) {
        crate::cache::queue_paint_mask_drop(self.mask.id());
    }
}

/// One cached composite texture.
#[derive(Clone)]
struct OverlaySprite {
    image: Arc<CompositeRenderImage>,
    /// Placement relative to the shaped line's paint origin, in logical pixels.
    offset_x: f32,
    offset_y: f32,
    width: f32,
    height: f32,
}

/// Final owner for a composited name-paint image.
///
/// GPUI keeps every painted `RenderImage` in each window's sprite atlas until
/// `App::drop_image` is called. The composite cache is not the only owner:
/// retained viewport commands clone `PaintedLineOverlay`, so retiring directly
/// on LRU eviction can remove a texture that a prepared command will repaint.
/// Queue the atlas removal only when the cache and every prepared command have
/// released their shared owner.
struct CompositeRenderImage(Arc<RenderImage>);

impl Drop for CompositeRenderImage {
    fn drop(&mut self) {
        COMPOSITE_RETIREMENTS_QUEUED.fetch_add(1, Ordering::Relaxed);
        crate::cache::queue_render_image_drop(Arc::clone(&self.0));
    }
}

impl PaintedLineOverlay {
    /// Paint with the frame-dependent part as an animated image slot, so a
    /// later frame can be patched in (`Window::patch_animated_image`). Returns
    /// whether the slot was recorded (the paint was not fully clipped).
    pub fn paint_animated(
        &self,
        line_origin: Point<gpui::Pixels>,
        key: u64,
        window: &mut Window,
    ) -> bool {
        match &self.0 {
            OverlayKind::Composite { under, sprite } => {
                if let Some(under) = under {
                    under.paint(line_origin, window);
                }
                let bounds = sprite.bounds(line_origin);
                window
                    .paint_animated_image(
                        key,
                        bounds,
                        bounds,
                        Corners::default(),
                        Arc::clone(&sprite.image.0),
                        0,
                        false,
                    )
                    .unwrap_or(false)
            }
            OverlayKind::Gpu { mask, fill } => window
                .paint_painted_text(
                    mask.bounds(line_origin),
                    &mask.mask,
                    fill.as_ref().map(|image| (image, 0)),
                    mask.instance,
                    Some(key),
                )
                .unwrap_or(false),
        }
    }

    /// The image a patch shows for the current frame: the fill sprite of a CPU
    /// composite, or the fill texture of a GPU paint (`None` while it loads).
    pub fn patch_image(&self) -> Option<&Arc<RenderImage>> {
        match &self.0 {
            OverlayKind::Composite { sprite, .. } => Some(&sprite.image.0),
            OverlayKind::Gpu { fill, .. } => fill.as_ref(),
        }
    }

    /// Placement of the frame-dependent part relative to the line origin
    /// (`[x, y, width, height]`, logical pixels).
    pub fn geometry(&self) -> [f32; 4] {
        match &self.0 {
            OverlayKind::Composite { sprite, .. } => [
                sprite.offset_x,
                sprite.offset_y,
                sprite.width,
                sprite.height,
            ],
            OverlayKind::Gpu { mask, .. } => mask.geometry,
        }
    }

    /// Paint at the shaped line's paint origin.
    #[inline]
    pub fn paint(&self, line_origin: Point<gpui::Pixels>, window: &mut Window) {
        match &self.0 {
            OverlayKind::Composite { under, sprite } => {
                if let Some(under) = under {
                    under.paint(line_origin, window);
                }
                sprite.paint(line_origin, window);
            }
            OverlayKind::Gpu { mask, fill } => {
                let _ = window.paint_painted_text(
                    mask.bounds(line_origin),
                    &mask.mask,
                    fill.as_ref().map(|image| (image, 0)),
                    mask.instance,
                    None,
                );
            }
        }
    }
}

impl OverlaySprite {
    #[inline]
    fn bounds(&self, line_origin: Point<gpui::Pixels>) -> Bounds<gpui::Pixels> {
        Bounds::new(
            line_origin + point(px(self.offset_x), px(self.offset_y)),
            gpui::size(px(self.width), px(self.height)),
        )
    }

    #[inline]
    fn paint(&self, line_origin: Point<gpui::Pixels>, window: &mut Window) {
        let bounds = self.bounds(line_origin);
        let _ = window.paint_image(
            bounds,
            bounds,
            Corners::default(),
            Arc::clone(&self.image.0),
            0,
            false,
        );
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct CompositeKey {
    recipe_hash: u64,
    text_hash: u64,
    font_size_bits: u32,
    line_height_bits: u32,
    scale_bits: u32,
    /// Content identity of the resolved fill textures (see
    /// `resolve_fill_images`): per layer the URL plus the displayed frame index
    /// for animated textures, so each frame composites and caches separately.
    /// Always zero for `CompositeLayer::ShadowStroke`, which is frame-independent.
    fill_image_id: u64,
    layer: CompositeLayer,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum CompositeLayer {
    /// Stroke, fill, and shadows flattened into one sprite.
    Full,
    /// Stroke plus drop shadows on the padded canvas, without the fill.
    ShadowStroke,
    /// Fill only, over the unpadded glyph mask.
    Fill,
    /// The GPU paint mask, independent of the fill frame.
    GpuMask,
}

struct CompositeCache {
    entries: LruCache<CompositeKey, OverlaySprite>,
    total_bytes: usize,
}

impl CompositeCache {
    fn empty() -> Self {
        Self {
            entries: LruCache::unbounded(),
            total_bytes: 0,
        }
    }

    fn overlay_bytes(overlay: &OverlaySprite) -> usize {
        overlay
            .image
            .0
            .size(0)
            .width
            .0
            .max(0)
            .saturating_mul(overlay.image.0.size(0).height.0.max(0)) as usize
            * 4
    }

    fn insert(
        &mut self,
        key: CompositeKey,
        overlay: OverlaySprite,
    ) -> SmallVec<[OverlaySprite; 4]> {
        self.insert_with_budget(key, overlay, COMPOSITE_CACHE_BYTE_BUDGET)
    }

    fn insert_with_budget(
        &mut self,
        key: CompositeKey,
        overlay: OverlaySprite,
        byte_budget: usize,
    ) -> SmallVec<[OverlaySprite; 4]> {
        let bytes = Self::overlay_bytes(&overlay);
        let mut retired = SmallVec::new();
        if bytes > byte_budget / 16 {
            retired.push(overlay);
            return retired;
        }
        if let Some(previous) = self.entries.push(key, overlay) {
            self.total_bytes = self
                .total_bytes
                .saturating_sub(Self::overlay_bytes(&previous.1));
            retired.push(previous.1);
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        while self.total_bytes > byte_budget {
            let Some((_, evicted)) = self.entries.pop_lru() else {
                self.total_bytes = 0;
                break;
            };
            self.total_bytes = self
                .total_bytes
                .saturating_sub(Self::overlay_bytes(&evicted));
            retired.push(evicted);
        }
        retired
    }
}

static COMPOSITE_CACHE: LazyLock<Mutex<CompositeCache>> =
    LazyLock::new(|| Mutex::new(CompositeCache::empty()));

fn lock_composite_cache(
    cache: &Mutex<CompositeCache>,
) -> (MutexGuard<'_, CompositeCache>, Option<CompositeCache>) {
    match cache.lock() {
        Ok(cache) => (cache, None),
        Err(poisoned) => {
            let mut cache_guard = poisoned.into_inner();
            let retired = std::mem::replace(&mut *cache_guard, CompositeCache::empty());
            cache.clear_poison();
            crate::log_perf_render!(warn, "Reset poisoned 7TV name-paint composite cache");
            (cache_guard, Some(retired))
        }
    }
}

/// `(entries, decoded_bytes, byte_budget, retirements_queued)` for periodic
/// memory attribution.
///
/// Includes the GPU paint masks, whose bytes are what those paints keep in RAM.
pub(crate) fn composite_cache_diagnostics() -> (usize, usize, usize, u64) {
    let (cache, retired) = lock_composite_cache(&COMPOSITE_CACHE);
    let entries = cache.entries.len();
    let total_bytes = cache.total_bytes;
    drop(cache);
    drop(retired);
    let masks = GPU_MASK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    (
        entries + masks.entries.len(),
        total_bytes + masks.total_bytes,
        COMPOSITE_CACHE_BYTE_BUDGET + GPU_MASK_CACHE_BYTE_BUDGET,
        COMPOSITE_RETIREMENTS_QUEUED.load(Ordering::Relaxed),
    )
}

/// LRU of GPU paint masks by username, paint and size.
struct GpuMaskCache {
    entries: LruCache<CompositeKey, Arc<GpuPaintMask>>,
    total_bytes: usize,
}

static GPU_MASK_CACHE: LazyLock<Mutex<GpuMaskCache>> = LazyLock::new(|| {
    Mutex::new(GpuMaskCache {
        entries: LruCache::unbounded(),
        total_bytes: 0,
    })
});

fn gpu_mask_bytes(mask: &GpuPaintMask) -> usize {
    mask.mask.byte_len() + std::mem::size_of::<GpuPaintMask>()
}

fn cached_gpu_mask(key: &CompositeKey) -> Option<Arc<GpuPaintMask>> {
    let mut cache = GPU_MASK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.entries.get(key).cloned()
}

fn cache_gpu_mask(key: CompositeKey, mask: Arc<GpuPaintMask>) {
    let bytes = gpu_mask_bytes(&mask);
    let mut retired = SmallVec::<[Arc<GpuPaintMask>; 4]>::new();
    {
        let mut cache = GPU_MASK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if bytes > GPU_MASK_CACHE_BYTE_BUDGET / 16 {
            return;
        }
        if let Some((_, previous)) = cache.entries.push(key, mask) {
            cache.total_bytes = cache.total_bytes.saturating_sub(gpu_mask_bytes(&previous));
            retired.push(previous);
        }
        cache.total_bytes = cache.total_bytes.saturating_add(bytes);
        while cache.total_bytes > GPU_MASK_CACHE_BYTE_BUDGET {
            let Some((_, evicted)) = cache.entries.pop_lru() else {
                cache.total_bytes = 0;
                break;
            };
            cache.total_bytes = cache.total_bytes.saturating_sub(gpu_mask_bytes(&evicted));
            retired.push(evicted);
        }
    }
    // Dropped outside the lock; prepared commands may still hold them.
    drop(retired);
}

#[cfg(test)]
mod composite_cache_state_tests {
    use super::*;
    use core::prelude::v1::test;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn test_key(id: u64) -> CompositeKey {
        CompositeKey {
            recipe_hash: id,
            text_hash: 0,
            font_size_bits: 0,
            line_height_bits: 0,
            scale_bits: 0,
            fill_image_id: 0,
            layer: CompositeLayer::Full,
        }
    }

    fn test_overlay(width: u32, height: u32) -> OverlaySprite {
        OverlaySprite {
            image: Arc::new(CompositeRenderImage(Arc::new(RenderImage::new(vec![
                Frame::new(RgbaImage::new(width, height)),
            ])))),
            offset_x: 0.0,
            offset_y: 0.0,
            width: width as f32,
            height: height as f32,
        }
    }

    #[test]
    fn poison_replaces_inconsistent_cache_with_empty_state() {
        let cache = Mutex::new(CompositeCache::empty());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut cache = cache.lock().expect("test cache should start healthy");
            cache.total_bytes = 123;
            panic!("poison test cache");
        }));

        assert!(cache.is_poisoned());
        let (cache_guard, retired) = lock_composite_cache(&cache);
        assert!(cache_guard.entries.is_empty());
        assert_eq!(cache_guard.total_bytes, 0);
        drop(cache_guard);
        assert_eq!(retired.as_ref().map(|cache| cache.total_bytes), Some(123));
        drop(retired);
        assert!(!cache.is_poisoned());

        let (cache_guard, retired) = lock_composite_cache(&cache);
        assert!(retired.is_none());
        assert!(cache_guard.entries.is_empty());
        assert_eq!(cache_guard.total_bytes, 0);
    }

    #[test]
    fn replacement_retires_old_owner_and_preserves_accounting() {
        let mut cache = CompositeCache::empty();
        let first = test_overlay(1, 1);
        let old_owner = Arc::downgrade(&first.image);

        assert!(cache.insert(test_key(1), first).is_empty());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.total_bytes, 4);

        let retired = cache.insert(test_key(1), test_overlay(1, 1));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.total_bytes, 4);
        assert_eq!(retired.len(), 1);
        assert!(old_owner.upgrade().is_some());

        drop(retired);
        assert!(old_owner.upgrade().is_none());
    }

    #[test]
    fn budget_eviction_honors_lru_promotion() {
        let mut cache = CompositeCache::empty();
        let byte_budget = 64;
        for id in 0..16 {
            assert!(
                cache
                    .insert_with_budget(test_key(id), test_overlay(1, 1), byte_budget)
                    .is_empty()
            );
        }
        assert!(cache.entries.get(&test_key(0)).is_some());

        let retired = cache.insert_with_budget(test_key(16), test_overlay(1, 1), byte_budget);
        assert_eq!(retired.len(), 1);
        assert!(cache.entries.peek(&test_key(0)).is_some());
        assert!(cache.entries.peek(&test_key(1)).is_none());
        assert!(cache.total_bytes <= byte_budget);
    }

    #[test]
    fn oversized_entry_does_not_displace_working_set() {
        let mut cache = CompositeCache::empty();
        let byte_budget = 64;
        assert!(
            cache
                .insert_with_budget(test_key(1), test_overlay(1, 1), byte_budget)
                .is_empty()
        );

        let retired = cache.insert_with_budget(test_key(2), test_overlay(1, 2), byte_budget);
        assert_eq!(retired.len(), 1);
        assert!(cache.entries.peek(&test_key(1)).is_some());
        assert!(cache.entries.peek(&test_key(2)).is_none());
        assert_eq!(cache.total_bytes, 4);
    }
}

/// Resolve (composite or fetch from cache) the exact paint overlay for a shaped
/// username line. Image-texture paints still produce a `currentColor`
/// composite while their source frame downloads/decodes, then naturally move
/// to a frame-keyed cache entry once decoded media is available.
pub fn overlay_for_line(
    spec: &SevenTvLinePaint,
    shaped_line: &ShapedLine,
    font_size: f32,
    line_height: f32,
    window: &mut Window,
) -> Option<PaintedLineOverlay> {
    let (fill_images, fill_image_id) = resolve_fill_images(&spec.paint);

    let mut text_hasher = rustc_hash::FxHasher::default();
    shaped_line.text.as_ref().hash(&mut text_hasher);
    let key = CompositeKey {
        recipe_hash: spec.recipe_hash(),
        text_hash: text_hasher.finish(),
        font_size_bits: font_size.to_bits(),
        line_height_bits: line_height.to_bits(),
        scale_bits: window.scale_factor().to_bits(),
        fill_image_id,
        layer: CompositeLayer::Full,
    };

    if gpu_name_paint_enabled() && window.supports_painted_text() && gpu_paint_supported(spec) {
        let mask_key = CompositeKey {
            fill_image_id: 0,
            layer: CompositeLayer::GpuMask,
            ..key.clone()
        };
        let mask = cached_gpu_mask(&mask_key).or_else(|| {
            let raster = PaintRaster::new(spec, shaped_line, line_height, window)?;
            let mask = Arc::new(build_gpu_mask(spec, &raster)?);
            cache_gpu_mask(mask_key, Arc::clone(&mask));
            Some(mask)
        });
        // A mask that cannot be built (too tall for the atlas) falls through
        // to the CPU composite.
        if let Some(mask) = mask {
            let fill = spec
                .paint
                .gradients
                .iter()
                .position(|layer| layer.image_url.is_some())
                .and_then(|index| fill_images.get(index).cloned().flatten());
            return Some(PaintedLineOverlay(OverlayKind::Gpu { mask, fill }));
        }
    }

    if !splits_shadow_stroke_layer(spec) {
        let sprite = match cached_sprite(&key) {
            Some(sprite) => sprite,
            None => {
                let raster = PaintRaster::new(spec, shaped_line, line_height, window)?;
                let sprite = composite_full(spec, &raster, &fill_pixels(&fill_images))?;
                cache_sprites([(key, sprite.clone())]);
                sprite
            }
        };
        return Some(PaintedLineOverlay(OverlayKind::Composite {
            under: None,
            sprite,
        }));
    }

    let under_key = CompositeKey {
        fill_image_id: 0,
        layer: CompositeLayer::ShadowStroke,
        ..key.clone()
    };
    let fill_key = CompositeKey {
        layer: CompositeLayer::Fill,
        ..key
    };
    let mut under = cached_sprite(&under_key);
    let mut sprite = cached_sprite(&fill_key);
    if under.is_none() || sprite.is_none() {
        let raster = PaintRaster::new(spec, shaped_line, line_height, window)?;
        let mut built = SmallVec::<[(CompositeKey, OverlaySprite); 2]>::new();
        if under.is_none() {
            under = composite_shadow_stroke(spec, &raster);
            if let Some(under) = &under {
                built.push((under_key, under.clone()));
            }
        }
        if sprite.is_none() {
            sprite = composite_fill(spec, &raster, &fill_pixels(&fill_images));
            if let Some(sprite) = &sprite {
                built.push((fill_key, sprite.clone()));
            }
        }
        cache_sprites(built);
    }
    Some(PaintedLineOverlay(OverlayKind::Composite {
        under: Some(under?),
        sprite: sprite?,
    }))
}

/// `ECHO_GPU_NAME_PAINT=0` composites every name paint on the CPU.
fn gpu_name_paint_enabled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| {
        std::env::var("ECHO_GPU_NAME_PAINT").map_or(true, |value| value.trim() != "0")
    });
    *ENABLED
}

/// Whether the GPU recipe covers this paint: within the instance limits, and
/// with fill alpha independent of the image texture (opaque base color), so
/// shadow weights can be computed without it.
fn gpu_paint_supported(spec: &SevenTvLinePaint) -> bool {
    let gradients = &spec.paint.gradients;
    let image_layers = gradients
        .iter()
        .filter(|layer| layer.image_url.is_some())
        .count();
    let stops: usize = gradients
        .iter()
        .filter(|layer| layer.image_url.is_none())
        .map(|layer| layer.stops.len())
        .sum();
    gradients.len() <= PAINTED_TEXT_MAX_LAYERS
        && stops <= PAINTED_TEXT_MAX_STOPS
        && image_layers <= 1
        && (image_layers == 0 || spec.base_color.a == u8::MAX)
}

/// Build a username's GPU paint mask: glyph coverage over the padded canvas,
/// then each drop shadow's blurred, offset weight, computed exactly as the CPU
/// composite computes them (texture-free fill, see `gpu_paint_supported`).
fn build_gpu_mask(spec: &SevenTvLinePaint, raster: &PaintRaster) -> Option<GpuPaintMask> {
    let (drop_shadows, stroke_rings) = collect_shadow_layers(spec, raster.scale);
    let drop_shadows = &drop_shadows[..drop_shadows.len().min(PAINTED_TEXT_MAX_SHADOWS)];
    let pad = CanvasPadding::for_layers(drop_shadows, &stroke_rings);
    let canvas_w = raster.mask_w + pad.left + pad.right;
    let canvas_h = raster.mask_h + pad.top + pad.bottom;
    let block = canvas_w * canvas_h;
    let blocks = 1 + drop_shadows.len();
    // The stacked blocks are one atlas tile; atlas textures stop at 16384.
    if canvas_w > MAX_PAINT_MASK_EDGE || canvas_h * blocks > MAX_PAINT_MASK_EDGE {
        return None;
    }
    let mut data = vec![0u8; block * blocks];

    for y in 0..raster.mask_h {
        let row = (y + pad.top) * canvas_w + pad.left;
        data[row..row + raster.mask_w]
            .copy_from_slice(&raster.mask.coverage[y * raster.mask_w..(y + 1) * raster.mask_w]);
    }
    if !drop_shadows.is_empty() {
        let mut reference = vec![0f32; block * 4];
        raster.stroke_into(&stroke_rings, &mut reference, canvas_w, canvas_h, pad);
        raster.fill_into(spec, &[], &mut reference, canvas_w, pad);
        let mut weights = Vec::with_capacity(drop_shadows.len());
        apply_drop_shadows(
            drop_shadows,
            &mut reference,
            None,
            Some(&mut weights),
            canvas_w,
            canvas_h,
        );
        for (index, weight) in weights.iter().enumerate() {
            let start = (index + 1) * block;
            for (dst, value) in data[start..start + block].iter_mut().zip(weight) {
                *dst = (value * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    let mask = PaintMask::new(canvas_w as u32, canvas_h as u32, blocks as u32, data)?;
    let instance = painted_text_instance(spec, raster, pad, drop_shadows, &stroke_rings);
    Some(GpuPaintMask {
        mask,
        instance,
        geometry: [
            (raster.mask_dev_x - pad.left as f32) / raster.scale,
            (raster.mask_dev_y - pad.top as f32) / raster.scale,
            canvas_w as f32 / raster.scale,
            canvas_h as f32 / raster.scale,
        ],
    })
}

/// The shader instance for a GPU paint: the paint's layers, stops and colors
/// in the same form `fill_color_at`, `stroke_into` and `apply_drop_shadows`
/// use them. The window fills in the tiles and bounds.
fn painted_text_instance(
    spec: &SevenTvLinePaint,
    raster: &PaintRaster,
    pad: CanvasPadding,
    drop_shadows: &[ShadowLayer],
    stroke_rings: &[ShadowLayer],
) -> PaintedText {
    let paint = &spec.paint;
    let geometry = gradient_geometry(paint, raster.box_w_dev, raster.box_h_dev);
    let mut text = PaintedText {
        layer_count: paint.gradients.len().min(PAINTED_TEXT_MAX_LAYERS) as u32,
        shadow_count: drop_shadows.len() as u32,
        ring_count: stroke_rings.len().min(text_ring_capacity()) as u32,
        fill_origin: [
            raster.mask_dev_x - pad.left as f32 + 0.5 - raster.box_x0_dev,
            raster.mask_dev_y - pad.top as f32 + 0.5 - raster.box_y0_dev,
        ],
        box_size: [raster.box_w_dev, raster.box_h_dev],
        base_color: premultiplied_rgba(spec.base_color, 1.0),
        fallback_color: premultiplied_rgba(
            paint.representative_color().unwrap_or(spec.base_color),
            1.0,
        ),
        ..PaintedText::default()
    };

    let mut stop_count = 0usize;
    let mut any = false;
    for (index, gradient) in paint
        .gradients
        .iter()
        .take(PAINTED_TEXT_MAX_LAYERS)
        .enumerate()
    {
        let opacity = gradient.opacity.clamp(0.0, 1.0);
        let mut layer = PaintedTextLayer {
            opacity,
            repeat: gradient.repeat as u32,
            ..PaintedTextLayer::default()
        };
        if gradient.image_url.is_some() {
            layer.kind = PAINTED_TEXT_LAYER_IMAGE;
            any |= opacity > 0.0;
        } else if !gradient.stops.is_empty() {
            layer.stop_start = stop_count as u32;
            layer.stop_count = gradient.stops.len() as u32;
            for stop in &gradient.stops {
                text.stops[stop_count] = PaintedTextStop {
                    at: stop.at,
                    color: [
                        stop.color.r as f32,
                        stop.color.g as f32,
                        stop.color.b as f32,
                        stop.color.a as f32,
                    ],
                };
                stop_count += 1;
            }
            match (gradient.kind, geometry.get(index).copied()) {
                (SevenTvGradientKind::Solid, _) => {
                    layer.kind = PAINTED_TEXT_LAYER_SOLID;
                    layer.solid = premultiplied_rgba(gradient.stops[0].color, opacity);
                }
                (
                    _,
                    Some(GradientGeometry::Linear {
                        direction,
                        min_proj,
                        span,
                    }),
                ) => {
                    layer.kind = PAINTED_TEXT_LAYER_LINEAR;
                    layer.params = [direction.0, direction.1, min_proj, span];
                }
                (_, Some(GradientGeometry::Circle { cx, cy, radius })) => {
                    layer.kind = PAINTED_TEXT_LAYER_CIRCLE;
                    layer.params = [cx, cy, radius, 0.0];
                }
                (_, Some(GradientGeometry::Ellipse { cx, cy, rx, ry })) => {
                    layer.kind = PAINTED_TEXT_LAYER_ELLIPSE;
                    layer.params = [cx, cy, rx, ry];
                }
                _ => {}
            }
            any |= opacity > 0.0 && layer.kind != 0;
        }
        text.layers[index] = layer;
    }
    text.use_fallback = (!any) as u32;

    if let Some(ring) = stroke_rings.first() {
        text.stroke_color = premultiplied_rgba(ring.color, 1.0);
    }
    for (slot, ring) in text.ring_offsets.iter_mut().zip(stroke_rings) {
        *slot = [ring.offset_x.round() as i32, ring.offset_y.round() as i32];
    }
    for (slot, shadow) in text.shadow_colors.iter_mut().zip(drop_shadows) {
        *slot = premultiplied_rgba(shadow.color, 1.0);
    }
    text
}

fn text_ring_capacity() -> usize {
    PaintedText::default().ring_offsets.len()
}

/// Premultiplied RGBA in `0..=1`, as the GPU paint shader takes colors.
fn premultiplied_rgba(color: RgbaColor, opacity: f32) -> [f32; 4] {
    let (b, g, r, a) = premultiplied_bgra(color, opacity);
    [r, g, b, a]
}

/// A fill texture's straight-alpha BGRA pixels for the CPU compositor:
/// borrowed when the image keeps them, else rebuilt once for this composite.
struct FillPixels<'a> {
    data: Cow<'a, [u8]>,
    width: i64,
    height: i64,
}

fn fill_pixels(images: &[Option<Arc<RenderImage>>]) -> SmallVec<[Option<FillPixels<'_>>; 4]> {
    images
        .iter()
        .map(|image| {
            let image = image.as_deref()?;
            let size = image.size(0);
            let data = match image.as_bytes(0) {
                Some(bytes) => Cow::Borrowed(bytes),
                None => Cow::Owned(image.with_pixels(0, <[u8]>::to_vec)?),
            };
            Some(FillPixels {
                data,
                width: size.width.0.max(1) as i64,
                height: size.height.0.max(1) as i64,
            })
        })
        .collect()
}

/// Whether an animated paint composites its stroke and shadows once, apart
/// from the per-frame fill.
///
/// The full composite paints stroke, then fill src-over, then each shadow
/// destination-over with its alpha blurred from the canvas alpha after the
/// fill. Because destination-over is associative, that equals the fill
/// src-over (stroke + shadows), provided the fill alpha feeding the blur does
/// not depend on the texture frame. Every image layer starts from
/// `currentColor`, so with an opaque base color its alpha is its opacity
/// whatever the texture holds; gradient and solid layers never read textures.
fn splits_shadow_stroke_layer(spec: &SevenTvLinePaint) -> bool {
    let has_shadows = spec.include_shadows && !spec.paint.shadows.is_empty();
    let has_stroke = spec
        .paint
        .text
        .stroke
        .as_ref()
        .is_some_and(|stroke| stroke.width > f32::EPSILON);
    spec.base_color.a == u8::MAX && (has_shadows || has_stroke) && spec.is_animated()
}

fn cached_sprite(key: &CompositeKey) -> Option<OverlaySprite> {
    let (mut cache, recovered) = lock_composite_cache(&COMPOSITE_CACHE);
    let cached = cache.entries.get(key).cloned();
    drop(cache);
    drop(recovered);
    cached
}

fn cache_sprites(sprites: impl IntoIterator<Item = (CompositeKey, OverlaySprite)>) {
    let (mut cache, recovered) = lock_composite_cache(&COMPOSITE_CACHE);
    let mut retired = SmallVec::<[OverlaySprite; 4]>::new();
    for (key, sprite) in sprites {
        retired.extend(cache.insert(key, sprite));
    }
    drop(cache);
    drop(retired);
    drop(recovered);
}

/// Resolve one source texture per paint layer.
///
/// A missing texture is not a missing paint: SevenTV's official component
/// leaves `currentColor` underneath an image layer while its URL loads. The
/// compositor therefore keeps a `None` slot after scheduling the decode and
/// still produces a base-color glyph composite immediately.
///
/// Also returns the content identity of the resolved textures for the
/// composite key: per layer, the URL plus the displayed frame index for a
/// decoded animation frame, the URL for a static texture, or a no-texture
/// marker. Pointer identity is unusable here: sliding-window refills hand out
/// a new `Arc` for the same frame (a permanent miss) and a freed frame's
/// address can be reused by a different frame (a wrong composite).
fn resolve_fill_images(paint: &SevenTvNamePaint) -> (SmallVec<[Option<Arc<RenderImage>>; 4]>, u64) {
    const NO_TEXTURE: u8 = 0;
    const STATIC: u8 = 1;
    const ANIMATED_FRAME: u8 = 2;

    let mut images = SmallVec::with_capacity(paint.gradients.len());
    let mut identity = rustc_hash::FxHasher::default();
    for (layer_index, layer) in paint.gradients.iter().enumerate() {
        layer_index.hash(&mut identity);
        let Some(url) = layer.image_url.as_deref() else {
            NO_TEXTURE.hash(&mut identity);
            images.push(None);
            continue;
        };

        let animated = layer.image_is_animated || is_animated_url(url);
        if animated {
            if let Some((image, _, frame_idx)) =
                image_cache::get_current_animated_render_image_with_metadata_and_frame(url)
            {
                ANIMATED_FRAME.hash(&mut identity);
                url.hash(&mut identity);
                frame_idx.hash(&mut identity);
                images.push(Some(image));
                continue;
            }
            if matches!(
                image_cache::get_animated_cache_state(url),
                image_cache::AnimatedCacheState::NotLoaded
            ) {
                image_cache::trigger_animated_decode_with_source(
                    url,
                    image_cache::PreloadSource::VisibleChat,
                );
            }
        }
        if let Some(image) = image_cache::get_cached_render_image(url) {
            STATIC.hash(&mut identity);
            url.hash(&mut identity);
            images.push(Some(image));
            continue;
        }

        image_cache::trigger_preload_single_with_source(
            url,
            image_cache::PreloadSource::VisibleChat,
        );
        NO_TEXTURE.hash(&mut identity);
        images.push(None);
    }
    (images, identity.finish())
}

struct ShadowLayer {
    offset_x: f32,
    offset_y: f32,
    sigma: f32,
    color: RgbaColor,
}

/// Glyph coverage mask plus the paint-box geometry shared by every layer.
struct PaintRaster {
    mask: gpui::ShapedLineRasterMask,
    scale: f32,
    mask_w: usize,
    mask_h: usize,
    box_x0_dev: f32,
    box_w_dev: f32,
    box_y0_dev: f32,
    box_h_dev: f32,
    mask_dev_x: f32,
    mask_dev_y: f32,
}

/// Canvas padding (device pixels) around the mask for stroke and shadow reach.
#[derive(Clone, Copy)]
struct CanvasPadding {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

impl CanvasPadding {
    const NONE: Self = Self {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    fn for_layers(drop_shadows: &[ShadowLayer], stroke_rings: &[ShadowLayer]) -> Self {
        let stroke_reach = stroke_rings
            .iter()
            .map(|layer| layer.offset_x.abs().max(layer.offset_y.abs()).ceil() as i32)
            .max()
            .unwrap_or(0);
        let (mut pad_l, mut pad_t, mut pad_r, mut pad_b) =
            (stroke_reach, stroke_reach, stroke_reach, stroke_reach);
        for layer in drop_shadows {
            let reach = ((layer.sigma * 3.0).ceil() as i32).clamp(0, 256);
            pad_l = pad_l.saturating_add((reach - layer.offset_x.floor() as i32).max(0));
            pad_r = pad_r.saturating_add((reach + layer.offset_x.ceil() as i32).max(0));
            pad_t = pad_t.saturating_add((reach - layer.offset_y.floor() as i32).max(0));
            pad_b = pad_b.saturating_add((reach + layer.offset_y.ceil() as i32).max(0));
        }
        Self {
            left: pad_l.max(0) as usize,
            top: pad_t.max(0) as usize,
            right: pad_r.max(0) as usize,
            bottom: pad_b.max(0) as usize,
        }
    }
}

impl PaintRaster {
    fn new(
        spec: &SevenTvLinePaint,
        shaped_line: &ShapedLine,
        line_height: f32,
        window: &mut Window,
    ) -> Option<Self> {
        let mask = window
            .rasterize_shaped_line_mask(shaped_line, px(line_height), Some(spec.byte_range.clone()))
            .ok()
            .flatten()?;

        let scale = mask.scale_factor.max(0.1);
        let mask_w = mask.size.width.0.max(0) as usize;
        let mask_h = mask.size.height.0.max(0) as usize;
        if mask_w == 0 || mask_h == 0 || mask.coverage.len() < mask_w * mask_h {
            return None;
        }

        let (box_x0, box_x1) = painted_box_x(shaped_line, &spec.byte_range);
        // CSS paints the name span's inline content area (font ascent + descent),
        // not the line box. GPUI centers that area in the line with the same
        // padding_top as `paint_line` and `rasterize_shaped_line_mask`.
        let content_h = f32::from(shaped_line.ascent + shaped_line.descent);
        let mask_dev_x = f32::from(mask.origin_offset.x) * scale;
        let mask_dev_y = f32::from(mask.origin_offset.y) * scale;
        Some(Self {
            scale,
            mask_w,
            mask_h,
            box_x0_dev: box_x0 * scale,
            box_w_dev: ((box_x1 - box_x0) * scale).max(1.0),
            box_y0_dev: (line_height - content_h) * 0.5 * scale,
            box_h_dev: (content_h * scale).max(1.0),
            mask_dev_x,
            mask_dev_y,
            mask,
        })
    }

    fn stroke_into(
        &self,
        stroke_rings: &[ShadowLayer],
        canvas: &mut [f32],
        canvas_w: usize,
        canvas_h: usize,
        pad: CanvasPadding,
    ) {
        let (mask_w, mask_h) = (self.mask_w, self.mask_h);
        for layer in stroke_rings {
            let (b, g, r, a) = premultiplied_bgra(layer.color, 1.0);
            let dx = pad.left as i32 + layer.offset_x.round() as i32;
            let dy = pad.top as i32 + layer.offset_y.round() as i32;
            for y in 0..mask_h {
                let cy = y as i32 + dy;
                if cy < 0 || cy >= canvas_h as i32 {
                    continue;
                }
                for x in 0..mask_w {
                    let coverage = self.mask.coverage[y * mask_w + x];
                    if coverage == 0 {
                        continue;
                    }
                    let cx = x as i32 + dx;
                    if cx < 0 || cx >= canvas_w as i32 {
                        continue;
                    }
                    let weight = coverage as f32 / 255.0;
                    let i = (cy as usize * canvas_w + cx as usize) * 4;
                    src_over(&mut canvas[i..i + 4], [b, g, r, a], weight);
                }
            }
        }
    }

    /// Source-over the paint fill onto `canvas`. An empty `fill_images`
    /// composites every image layer as its `currentColor` base.
    fn fill_into(
        &self,
        spec: &SevenTvLinePaint,
        fill_images: &[Option<FillPixels<'_>>],
        canvas: &mut [f32],
        canvas_w: usize,
        pad: CanvasPadding,
    ) {
        let (mask_w, mask_h) = (self.mask_w, self.mask_h);
        let geometry = gradient_geometry(&spec.paint, self.box_w_dev, self.box_h_dev);
        for y in 0..mask_h {
            for x in 0..mask_w {
                let coverage = self.mask.coverage[y * mask_w + x];
                if coverage == 0 {
                    continue;
                }
                let px_x = self.mask_dev_x + x as f32 + 0.5 - self.box_x0_dev;
                let px_y = self.mask_dev_y + y as f32 + 0.5 - self.box_y0_dev;
                let Some(color) = fill_color_at(
                    &spec.paint,
                    spec.base_color,
                    fill_images,
                    &geometry,
                    px_x,
                    px_y,
                    self.box_w_dev,
                    self.box_h_dev,
                ) else {
                    continue;
                };
                let (b, g, r, a) = color;
                let weight = coverage as f32 / 255.0;
                let i = ((y + pad.top) * canvas_w + (x + pad.left)) * 4;
                src_over(&mut canvas[i..i + 4], [b, g, r, a], weight);
            }
        }
    }

    /// Flatten a premultiplied canvas into a straight-alpha BGRA sprite placed
    /// so the mask lands at its rasterized position.
    fn sprite(
        &self,
        canvas: &[f32],
        canvas_w: usize,
        canvas_h: usize,
        pad: CanvasPadding,
    ) -> Option<OverlaySprite> {
        let mut bgra = vec![0u8; canvas_w * canvas_h * 4];
        for (dst, src) in bgra
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(canvas.as_chunks::<4>().0)
        {
            let alpha = src[3];
            if alpha <= f32::EPSILON {
                continue;
            }
            dst[0] = ((src[0] / alpha) * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[1] = ((src[1] / alpha) * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[2] = ((src[2] / alpha) * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }

        let buffer = RgbaImage::from_raw(canvas_w as u32, canvas_h as u32, bgra)?;
        let image = Arc::new(CompositeRenderImage(Arc::new(RenderImage::new(vec![
            Frame::new(buffer),
        ]))));

        Some(OverlaySprite {
            image,
            offset_x: (self.mask_dev_x - pad.left as f32) / self.scale,
            offset_y: (self.mask_dev_y - pad.top as f32) / self.scale,
            width: canvas_w as f32 / self.scale,
            height: canvas_h as f32 / self.scale,
        })
    }
}

/// Stroke, fill, and drop shadows flattened into one padded sprite.
fn composite_full(
    spec: &SevenTvLinePaint,
    raster: &PaintRaster,
    fill_images: &[Option<FillPixels<'_>>],
) -> Option<OverlaySprite> {
    let (drop_shadows, stroke_rings) = collect_shadow_layers(spec, raster.scale);
    let pad = CanvasPadding::for_layers(&drop_shadows, &stroke_rings);
    let canvas_w = raster.mask_w + pad.left + pad.right;
    let canvas_h = raster.mask_h + pad.top + pad.bottom;
    let mut canvas = vec![0f32; canvas_w * canvas_h * 4];

    raster.stroke_into(&stroke_rings, &mut canvas, canvas_w, canvas_h, pad);
    raster.fill_into(spec, fill_images, &mut canvas, canvas_w, pad);
    apply_drop_shadows(&drop_shadows, &mut canvas, None, None, canvas_w, canvas_h);
    raster.sprite(&canvas, canvas_w, canvas_h, pad)
}

/// Stroke plus drop shadows without the fill, on the same padded canvas as
/// `composite_full`. The shadow alphas are blurred from a reference canvas
/// that also holds the texture-free fill (see `splits_shadow_stroke_layer`).
fn composite_shadow_stroke(spec: &SevenTvLinePaint, raster: &PaintRaster) -> Option<OverlaySprite> {
    let (drop_shadows, stroke_rings) = collect_shadow_layers(spec, raster.scale);
    let pad = CanvasPadding::for_layers(&drop_shadows, &stroke_rings);
    let canvas_w = raster.mask_w + pad.left + pad.right;
    let canvas_h = raster.mask_h + pad.top + pad.bottom;
    let mut under = vec![0f32; canvas_w * canvas_h * 4];

    raster.stroke_into(&stroke_rings, &mut under, canvas_w, canvas_h, pad);
    let mut reference = under.clone();
    raster.fill_into(spec, &[], &mut reference, canvas_w, pad);
    apply_drop_shadows(
        &drop_shadows,
        &mut reference,
        Some(&mut under),
        None,
        canvas_w,
        canvas_h,
    );
    raster.sprite(&under, canvas_w, canvas_h, pad)
}

/// Fill only, over the unpadded mask; painted src-over the shadow/stroke
/// sprite. The unpadded origin is the padded one shifted by whole device
/// pixels, so both sprites stay on the same pixel grid.
fn composite_fill(
    spec: &SevenTvLinePaint,
    raster: &PaintRaster,
    fill_images: &[Option<FillPixels<'_>>],
) -> Option<OverlaySprite> {
    let (canvas_w, canvas_h) = (raster.mask_w, raster.mask_h);
    let mut canvas = vec![0f32; canvas_w * canvas_h * 4];
    raster.fill_into(
        spec,
        fill_images,
        &mut canvas,
        canvas_w,
        CanvasPadding::NONE,
    );
    raster.sprite(&canvas, canvas_w, canvas_h, CanvasPadding::NONE)
}

/// Composite each drop shadow destination-over `canvas`, its alpha blurred
/// from the current `canvas` alpha. `under`, when given, receives the same
/// shadow weights destination-over its own content; `weights_out` receives
/// each shadow's weight per canvas pixel.
fn apply_drop_shadows(
    drop_shadows: &[ShadowLayer],
    canvas: &mut [f32],
    mut under: Option<&mut [f32]>,
    mut weights_out: Option<&mut Vec<Vec<f32>>>,
    canvas_w: usize,
    canvas_h: usize,
) {
    let mut alpha = Vec::new();
    let mut blur_scratch = Vec::new();
    for layer in drop_shadows {
        alpha.resize(canvas_w * canvas_h, 0.0);
        for (dst, pixel) in alpha.iter_mut().zip(canvas.as_chunks::<4>().0) {
            *dst = pixel[3];
        }
        gaussian_blur_f32(
            &mut alpha,
            canvas_w,
            canvas_h,
            layer.sigma,
            &mut blur_scratch,
        );

        let (b, g, r, a) = premultiplied_bgra(layer.color, 1.0);
        let dx = layer.offset_x.round() as i32;
        let dy = layer.offset_y.round() as i32;
        let mut layer_weights = weights_out
            .is_some()
            .then(|| vec![0f32; canvas_w * canvas_h]);
        for y in 0..canvas_h as i32 {
            let source_y = y - dy;
            if source_y < 0 || source_y >= canvas_h as i32 {
                continue;
            }
            for x in 0..canvas_w as i32 {
                let source_x = x - dx;
                if source_x < 0 || source_x >= canvas_w as i32 {
                    continue;
                }
                let weight = alpha[source_y as usize * canvas_w + source_x as usize];
                if weight <= 0.0 {
                    continue;
                }
                let i = (y as usize * canvas_w + x as usize) * 4;
                destination_over(&mut canvas[i..i + 4], [b, g, r, a], weight);
                if let Some(under) = under.as_deref_mut() {
                    destination_over(&mut under[i..i + 4], [b, g, r, a], weight);
                }
                if let Some(layer_weights) = layer_weights.as_mut() {
                    layer_weights[y as usize * canvas_w + x as usize] = weight;
                }
            }
        }
        if let (Some(out), Some(layer_weights)) = (weights_out.as_deref_mut(), layer_weights) {
            out.push(layer_weights);
        }
    }
}

/// Destination-over blend of a premultiplied color (scaled by `weight`)
/// beneath a premultiplied destination pixel.
#[inline]
fn destination_over(dst: &mut [f32], src: [f32; 4], weight: f32) {
    let inv = 1.0 - dst[3];
    dst[0] += src[0] * weight * inv;
    dst[1] += src[1] * weight * inv;
    dst[2] += src[2] * weight * inv;
    dst[3] += src[3] * weight * inv;
}

/// Horizontal extent (logical px, relative to the line origin) of the painted
/// glyph cluster: from the first in-range glyph to the next glyph after the
/// range (or the line end).
fn painted_box_x(shaped_line: &ShapedLine, byte_range: &Range<usize>) -> (f32, f32) {
    let mut x0 = f32::INFINITY;
    let mut x1 = f32::INFINITY;
    for run in &shaped_line.runs {
        for glyph in &run.glyphs {
            let x = f32::from(glyph.position.x);
            if byte_range.contains(&glyph.index) {
                x0 = x0.min(x);
            } else if glyph.index >= byte_range.end {
                x1 = x1.min(x);
            }
        }
    }
    if !x0.is_finite() {
        x0 = 0.0;
    }
    if !x1.is_finite() {
        x1 = f32::from(shaped_line.width);
    }
    (x0, x1.max(x0 + 1.0))
}

fn collect_shadow_layers(
    spec: &SevenTvLinePaint,
    scale: f32,
) -> (Vec<ShadowLayer>, Vec<ShadowLayer>) {
    let mut drop_shadows = Vec::new();

    if spec.include_shadows {
        for shadow in spec.paint.shadows.iter().take(MAX_7TV_SHADOW_RENDER_LAYERS) {
            drop_shadows.push(shadow_layer_from(shadow, scale));
        }
    }

    let mut stroke_rings = Vec::new();
    if let Some(stroke) = spec.paint.text.stroke.as_ref()
        && stroke.width > f32::EPSILON
    {
        let radius = stroke.width.clamp(0.5, 3.0) * scale;
        let diagonal = radius * std::f32::consts::FRAC_1_SQRT_2;
        for (dx, dy) in [
            (radius, 0.0),
            (-radius, 0.0),
            (0.0, radius),
            (0.0, -radius),
            (diagonal, diagonal),
            (-diagonal, diagonal),
            (diagonal, -diagonal),
            (-diagonal, -diagonal),
        ] {
            stroke_rings.push(ShadowLayer {
                offset_x: dx,
                offset_y: dy,
                sigma: 0.0,
                color: stroke.color,
            });
        }
    }

    (drop_shadows, stroke_rings)
}

fn shadow_layer_from(shadow: &SevenTvPaintShadow, scale: f32) -> ShadowLayer {
    ShadowLayer {
        offset_x: shadow.x_offset * scale,
        offset_y: shadow.y_offset * scale,
        sigma: (shadow.radius * scale).max(0.0),
        color: shadow.color,
    }
}

/// Per-layer gradient constants that depend only on the paint box, computed
/// once per composite instead of once per pixel.
#[derive(Clone, Copy)]
enum GradientGeometry {
    None,
    Linear {
        direction: (f32, f32),
        min_proj: f32,
        span: f32,
    },
    Circle {
        cx: f32,
        cy: f32,
        radius: f32,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
    },
}

fn gradient_geometry(
    paint: &SevenTvNamePaint,
    box_w: f32,
    box_h: f32,
) -> SmallVec<[GradientGeometry; 4]> {
    paint
        .gradients
        .iter()
        .map(|gradient| match gradient.kind {
            SevenTvGradientKind::Solid => GradientGeometry::None,
            SevenTvGradientKind::Linear => {
                let direction = css_gradient_direction(gradient.angle_degrees);
                let (min_proj, max_proj) = project_rect_bounds(direction, box_w, box_h);
                GradientGeometry::Linear {
                    direction,
                    min_proj,
                    span: max_proj - min_proj,
                }
            }
            SevenTvGradientKind::Radial(shape) => {
                let cx = box_w * 0.5;
                let cy = box_h * 0.5;
                match shape {
                    SevenTvRadialShape::Circle => GradientGeometry::Circle {
                        cx,
                        cy,
                        radius: (cx * cx + cy * cy).sqrt().max(f32::EPSILON),
                    },
                    SevenTvRadialShape::Ellipse => GradientGeometry::Ellipse {
                        cx,
                        cy,
                        rx: (cx * std::f32::consts::SQRT_2).max(f32::EPSILON),
                        ry: (cy * std::f32::consts::SQRT_2).max(f32::EPSILON),
                    },
                }
            }
        })
        .collect()
}

/// Exact fill color for a device pixel within the paint box, compositing the
/// paint's layers in order. Returns premultiplied BGRA in `[0, 1]`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn fill_color_at(
    paint: &SevenTvNamePaint,
    base_color: RgbaColor,
    fill_images: &[Option<FillPixels<'_>>],
    geometry: &[GradientGeometry],
    x: f32,
    y: f32,
    box_w: f32,
    box_h: f32,
) -> Option<(f32, f32, f32, f32)> {
    let mut out = [0f32; 4];
    let mut any = false;

    for (layer_index, gradient) in paint.gradients.iter().enumerate() {
        let opacity = gradient.opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }

        if gradient.image_url.is_some() {
            let (b, g, r, a) = premultiplied_bgra(base_color, 1.0);
            let mut layer = [b, g, r, a];
            if let Some(sampled) = fill_images
                .get(layer_index)
                .and_then(Option::as_ref)
                .and_then(|image| sample_image_fill(image, x, y, box_w, box_h, gradient.repeat))
            {
                src_over(&mut layer, sampled, 1.0);
            }
            src_over(&mut out, layer, opacity);
            any = true;
            continue;
        }

        if gradient.stops.is_empty() {
            continue;
        }

        if gradient.kind == SevenTvGradientKind::Solid {
            if let Some(color) = gradient.stops.first().map(|stop| stop.color) {
                let (b, g, r, a) = premultiplied_bgra(color, opacity);
                src_over(&mut out, [b, g, r, a], 1.0);
                any = true;
            }
            continue;
        }

        let ratio = match geometry
            .get(layer_index)
            .copied()
            .unwrap_or(GradientGeometry::None)
        {
            GradientGeometry::None => continue,
            GradientGeometry::Linear {
                direction,
                min_proj,
                span,
            } => {
                if span.abs() <= f32::EPSILON {
                    0.5
                } else {
                    (x * direction.0 + y * direction.1 - min_proj) / span
                }
            }
            GradientGeometry::Circle { cx, cy, radius } => {
                ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / radius
            }
            GradientGeometry::Ellipse { cx, cy, rx, ry } => {
                (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt()
            }
        };
        let ratio = if gradient.repeat {
            repeating_gradient_ratio(&gradient.stops, ratio)
        } else {
            ratio.clamp(0.0, 1.0)
        };

        if let Some(color) = sample_gradient_stops(&gradient.stops, ratio) {
            let (b, g, r, a) = premultiplied_bgra(base_color, 1.0);
            let mut layer = [b, g, r, a];
            let (b, g, r, a) = premultiplied_bgra(color, 1.0);
            src_over(&mut layer, [b, g, r, a], 1.0);
            src_over(&mut out, layer, opacity);
            any = true;
        }
    }

    if !any {
        let color = paint.representative_color().unwrap_or(base_color);
        let (b, g, r, a) = premultiplied_bgra(color, 1.0);
        return Some((b, g, r, a));
    }
    Some((out[0], out[1], out[2], out[3]))
}

/// Sample the image paint texture with CSS `background-size: 100% 100%`
/// stretching over the paint box (the behavior 7TV paints rely on), with
/// optional tiling. Decoded frames are stored as straight-alpha BGRA.
#[inline]
fn sample_image_fill(
    image: &FillPixels<'_>,
    x: f32,
    y: f32,
    box_w: f32,
    box_h: f32,
    repeat: bool,
) -> Option<[f32; 4]> {
    let bytes = &image.data[..];
    let src_w = image.width;
    let src_h = image.height;
    if bytes.len() < (src_w * src_h * 4) as usize {
        return None;
    }

    let (mut fx, mut fy) = (x / box_w, y / box_h);
    if repeat {
        fx = fx.rem_euclid(1.0);
        fy = fy.rem_euclid(1.0);
    } else {
        fx = fx.clamp(0.0, 1.0);
        fy = fy.clamp(0.0, 1.0);
    }

    let sx = fx * src_w as f32 - 0.5;
    let sy = fy * src_h as f32 - 0.5;
    let x0 = sx.floor();
    let tx = sx - x0;
    let y0 = sy.floor();
    let ty = sy - y0;

    let fetch = |xi: i64, yi: i64| -> [f32; 4] {
        let (xi, yi) = if repeat {
            (xi.rem_euclid(src_w), yi.rem_euclid(src_h))
        } else {
            (xi.clamp(0, src_w - 1), yi.clamp(0, src_h - 1))
        };
        let i = ((yi * src_w + xi) * 4) as usize;
        let alpha = bytes[i + 3] as f32 / 255.0;
        [
            bytes[i] as f32 / 255.0 * alpha,
            bytes[i + 1] as f32 / 255.0 * alpha,
            bytes[i + 2] as f32 / 255.0 * alpha,
            alpha,
        ]
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (p00, p10) = (fetch(x0, y0), fetch(x0 + 1, y0));
    let (p01, p11) = (fetch(x0, y0 + 1), fetch(x0 + 1, y0 + 1));
    let mut out = [0f32; 4];
    for channel in 0..4 {
        let top = p00[channel] + (p10[channel] - p00[channel]) * tx;
        let bottom = p01[channel] + (p11[channel] - p01[channel]) * tx;
        out[channel] = top + (bottom - top) * ty;
    }
    Some(out)
}

#[inline]
fn premultiplied_bgra(color: RgbaColor, opacity: f32) -> (f32, f32, f32, f32) {
    let alpha = (color.a as f32 / 255.0) * opacity;
    (
        color.b as f32 / 255.0 * alpha,
        color.g as f32 / 255.0 * alpha,
        color.r as f32 / 255.0 * alpha,
        alpha,
    )
}

/// Source-over blend of a premultiplied color (scaled by `weight`) onto a
/// premultiplied destination pixel.
#[inline]
fn src_over(dst: &mut [f32], src: [f32; 4], weight: f32) {
    let inv = 1.0 - src[3] * weight;
    dst[0] = src[0] * weight + dst[0] * inv;
    dst[1] = src[1] * weight + dst[1] * inv;
    dst[2] = src[2] * weight + dst[2] * inv;
    dst[3] = src[3] * weight + dst[3] * inv;
}

/// Gaussian blur of a single-channel alpha buffer, approximated with three
/// box blurs whose combined variance matches `sigma` (see
/// `gaussian_box_blur_radii`).
fn gaussian_blur_f32(
    buf: &mut [f32],
    width: usize,
    height: usize,
    sigma: f32,
    scratch: &mut Vec<f32>,
) {
    let Some(radii) = gaussian_box_blur_radii(sigma) else {
        return;
    };

    scratch.resize(buf.len(), 0.0);
    for radius in radii {
        box_blur_horizontal(buf, scratch, width, height, radius);
        box_blur_vertical(scratch, buf, width, height, radius);
    }
}

fn box_blur_horizontal(src: &[f32], dst: &mut [f32], width: usize, height: usize, radius: usize) {
    if radius == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * radius + 1) as f32;
    for y in 0..height {
        let row = y * width;
        let mut acc = 0.0;
        for x in 0..width.min(radius + 1) {
            acc += src[row + x];
        }
        for x in 0..width {
            dst[row + x] = acc * norm;
            if x + radius + 1 < width {
                acc += src[row + x + radius + 1];
            }
            if x >= radius {
                acc -= src[row + x - radius];
            }
        }
    }
}

fn box_blur_vertical(src: &[f32], dst: &mut [f32], width: usize, height: usize, radius: usize) {
    if radius == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * radius + 1) as f32;
    for x in 0..width {
        let mut acc = 0.0;
        for y in 0..height.min(radius + 1) {
            acc += src[y * width + x];
        }
        for y in 0..height {
            dst[y * width + x] = acc * norm;
            if y + radius + 1 < height {
                acc += src[(y + radius + 1) * width + x];
            }
            if y >= radius {
                acc -= src[(y - radius) * width + x];
            }
        }
    }
}
