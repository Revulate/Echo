//! 7TV name-paint recipe attached to a shaped username line.
//!
//! The recipe is the layout-time half of Echo's 7TV name paints: it captures
//! the paint, the base color, the painted byte range, and a content hash of
//! every render-relevant field. Compositing, blurring, and caching stay in the
//! application renderer, which owns GPUI glyph rasterization and the media
//! cache.

use echo_cosmetics_core::{RgbaColor, SevenTvGradientKind, SevenTvNamePaint, SevenTvRadialShape};
use std::hash::{Hash, Hasher};
use std::ops::Range;

use super::layout::is_animated_url;

/// Per-line 7TV paint description attached to a `ShapedLineData` at layout time.
#[derive(Clone, Debug)]
pub struct SevenTvLinePaint {
    pub paint: SevenTvNamePaint,
    /// The username color already selected by Twitch/Echo. SevenTV image and
    /// gradient layers use CSS `currentColor` beneath transparent pixels; this
    /// is what keeps Staff Shine yellow while its white texture moves over it.
    pub base_color: RgbaColor,
    /// Byte range of the painted username inside the shaped line's text
    /// (the line also contains the `: ` separator).
    pub byte_range: Range<usize>,
    /// Whether shadow layers should be composited (7TV paint-shadow setting).
    pub include_shadows: bool,
    /// Content hash of every render-relevant paint field. Cosmetic updates
    /// produce a different hash, so stale cache entries simply age out of the
    /// LRU instead of needing explicit invalidation.
    recipe_hash: u64,
}

impl SevenTvLinePaint {
    pub fn new(
        paint: SevenTvNamePaint,
        base_color: RgbaColor,
        byte_range: Range<usize>,
        include_shadows: bool,
    ) -> Self {
        let mut hasher = rustc_hash::FxHasher::default();
        hash_paint(&paint, &mut hasher);
        hash_color(base_color, &mut hasher);
        include_shadows.hash(&mut hasher);
        byte_range.start.hash(&mut hasher);
        byte_range.end.hash(&mut hasher);
        let recipe_hash = hasher.finish();
        Self {
            paint,
            base_color,
            byte_range,
            include_shadows,
            recipe_hash,
        }
    }

    #[inline]
    pub fn is_animated(&self) -> bool {
        self.image_layers().any(|(_, animated)| animated)
    }

    #[inline]
    pub fn image_layers(&self) -> impl Iterator<Item = (&str, bool)> {
        self.paint.gradients.iter().filter_map(|layer| {
            layer
                .image_url
                .as_deref()
                .map(|url| (url, layer.image_is_animated || is_animated_url(url)))
        })
    }

    /// Content hash of every render-relevant paint field.
    #[inline]
    pub fn recipe_hash(&self) -> u64 {
        self.recipe_hash
    }
}

fn hash_paint(paint: &SevenTvNamePaint, hasher: &mut impl Hasher) {
    paint.id.hash(hasher);
    paint.colors.len().hash(hasher);
    for color in &paint.colors {
        hash_color(*color, hasher);
    }
    paint.gradients.len().hash(hasher);
    for gradient in &paint.gradients {
        match gradient.kind {
            SevenTvGradientKind::Solid => 0u8.hash(hasher),
            SevenTvGradientKind::Linear => 1u8.hash(hasher),
            SevenTvGradientKind::Radial(SevenTvRadialShape::Circle) => 2u8.hash(hasher),
            SevenTvGradientKind::Radial(SevenTvRadialShape::Ellipse) => 3u8.hash(hasher),
        }
        gradient.angle_degrees.to_bits().hash(hasher);
        gradient.repeat.hash(hasher);
        gradient.image_url.hash(hasher);
        gradient.image_is_animated.hash(hasher);
        gradient.opacity.to_bits().hash(hasher);
        gradient.stops.len().hash(hasher);
        for stop in &gradient.stops {
            stop.at.to_bits().hash(hasher);
            hash_color(stop.color, hasher);
        }
    }
    paint.image_urls.hash(hasher);
    paint.shadows.len().hash(hasher);
    for shadow in &paint.shadows {
        shadow.x_offset.to_bits().hash(hasher);
        shadow.y_offset.to_bits().hash(hasher);
        shadow.radius.to_bits().hash(hasher);
        hash_color(shadow.color, hasher);
    }
    if let Some(stroke) = paint.text.stroke.as_ref() {
        stroke.width.to_bits().hash(hasher);
        hash_color(stroke.color, hasher);
    }
    paint.text.weight.hash(hasher);
}

#[inline]
fn hash_color(color: RgbaColor, hasher: &mut impl Hasher) {
    [color.r, color.g, color.b, color.a].hash(hasher);
}
