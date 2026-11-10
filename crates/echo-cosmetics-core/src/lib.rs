//! 7TV cosmetic parsing and resolution.
//!
//! This module is intentionally pure: it parses 7TV user/cosmetic payloads and
//! resolves the active badge/name paint without doing network IO. The chat
//! renderer and standalone verification tools both use this code path.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaColor {
    /// 7TV cosmetic colors are packed as RGBA in a signed GraphQL `Int`.
    pub fn from_7tv_int(value: i64) -> Self {
        let raw = (value as i32) as u32;
        Self {
            r: ((raw >> 24) & 0xff) as u8,
            g: ((raw >> 16) & 0xff) as u8,
            b: ((raw >> 8) & 0xff) as u8,
            a: (raw & 0xff) as u8,
        }
    }

    pub fn to_hex_rgb(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Interpolate toward `other` in premultiplied-alpha space, as CSS
    /// gradients do (css-images-3 §3.4.2), and return straight RGBA.
    ///
    /// Straight-alpha mixing drags a transparent stop's RGB into the visible
    /// color, so red to transparent black would pass through a dark band.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let from_alpha = self.a as f32 / 255.0;
        let to_alpha = other.a as f32 / 255.0;
        let alpha = from_alpha + (to_alpha - from_alpha) * t;
        if alpha <= f32::EPSILON {
            return Self::default();
        }
        let mix = |from: u8, to: u8| -> u8 {
            let from = from as f32 * from_alpha;
            let to = to as f32 * to_alpha;
            ((from + (to - from) * t) / alpha).round().clamp(0.0, 255.0) as u8
        };
        Self {
            r: mix(self.r, other.r),
            g: mix(self.g, other.g),
            b: mix(self.b, other.b),
            a: (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvPaintShadow {
    pub x_offset: f32,
    pub y_offset: f32,
    pub radius: f32,
    pub color: RgbaColor,
}

pub const MAX_7TV_SHADOW_RENDER_LAYERS: usize = 8;

const MAX_7TV_SHADOW_OFFSET: f32 = 64.0;
const MAX_7TV_SHADOW_BLUR: f32 = 100.0;

impl SevenTvPaintShadow {
    /// Server-controlled values size composite canvases, so keep them finite and bounded.
    fn bounded(x_offset: f32, y_offset: f32, radius: f32, color: RgbaColor) -> Self {
        let clamp = |value: f32, min: f32, max: f32| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                0.0
            }
        };
        Self {
            x_offset: clamp(x_offset, -MAX_7TV_SHADOW_OFFSET, MAX_7TV_SHADOW_OFFSET),
            y_offset: clamp(y_offset, -MAX_7TV_SHADOW_OFFSET, MAX_7TV_SHADOW_OFFSET),
            radius: clamp(radius, 0.0, MAX_7TV_SHADOW_BLUR),
            color,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvPaintStroke {
    pub width: f32,
    pub color: RgbaColor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SevenTvGradientStop {
    pub at: f32,
    pub color: RgbaColor,
}

/// Radial gradient ending shape (CSS `radial-gradient` semantics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SevenTvRadialShape {
    #[default]
    Circle,
    Ellipse,
}

/// How a paint gradient layer is evaluated across the username box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SevenTvGradientKind {
    /// A v4 single-color layer. Unlike a gradient or image layer, this
    /// replaces `currentColor` instead of painting over it.
    Solid,
    /// CSS `linear-gradient(angle, ...)`; the angle lives in `angle_degrees`.
    #[default]
    Linear,
    /// CSS `radial-gradient(shape at center, ...)` with farthest-corner sizing.
    Radial(SevenTvRadialShape),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SevenTvPaintGradient {
    pub kind: SevenTvGradientKind,
    pub angle_degrees: f32,
    pub repeat: bool,
    pub image_url: Option<String>,
    /// Explicit animation state from the v4 image-set metadata. Paint WebP
    /// URLs do not encode this reliably, so URL suffix guessing is insufficient.
    pub image_is_animated: bool,
    pub stops: Vec<SevenTvGradientStop>,
    /// Layer opacity multiplier (7TV v4 paint layers carry per-layer opacity).
    pub opacity: f32,
}

impl Default for SevenTvPaintGradient {
    fn default() -> Self {
        Self {
            kind: SevenTvGradientKind::Linear,
            angle_degrees: 90.0,
            repeat: false,
            image_url: None,
            image_is_animated: false,
            stops: Vec::new(),
            opacity: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SevenTvTextTransform {
    Uppercase,
    Lowercase,
    Capitalize,
}

impl SevenTvTextTransform {
    fn parse(value: &str) -> Option<Self> {
        let normalized = value
            .chars()
            .filter(|ch| !matches!(*ch, '-' | '_' | ' '))
            .flat_map(char::to_lowercase)
            .collect::<String>();
        match normalized.as_str() {
            "uppercase" | "upper" => Some(Self::Uppercase),
            "lowercase" | "lower" => Some(Self::Lowercase),
            "capitalize" | "capitalise" | "titlecase" | "title" => Some(Self::Capitalize),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvPaintText {
    pub weight: Option<u16>,
    pub transform: Option<SevenTvTextTransform>,
    pub variant: Option<String>,
    pub stroke: Option<SevenTvPaintStroke>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvNamePaint {
    pub id: String,
    pub name: String,
    pub colors: Vec<RgbaColor>,
    pub gradients: Vec<SevenTvPaintGradient>,
    pub image_urls: Vec<String>,
    pub text: SevenTvPaintText,
    pub shadows: Vec<SevenTvPaintShadow>,
}

impl SevenTvNamePaint {
    pub fn transformed_display_name(&self, display_name: &str) -> String {
        match self.text.transform {
            Some(SevenTvTextTransform::Uppercase) => display_name.to_uppercase(),
            Some(SevenTvTextTransform::Lowercase) => display_name.to_lowercase(),
            Some(SevenTvTextTransform::Capitalize) => capitalize_display_name(display_name),
            None => display_name.to_string(),
        }
    }

    /// A single color that best represents this paint.
    ///
    /// Used for hitbox/user-card accents and legacy solid-paint fallback. Chat
    /// name paints are rendered as a single final glyph composite, rather than
    /// painting this color underneath a second antialiased glyph mask.
    pub fn representative_color(&self) -> Option<RgbaColor> {
        self.gradients
            .iter()
            .find_map(|gradient| {
                gradient
                    .stops
                    .iter()
                    .map(|stop| stop.color)
                    .find(|color| color.a > 0)
            })
            .or_else(|| self.colors.iter().copied().find(|color| color.a > 0))
            .or_else(|| self.colors.first().copied())
    }
}

fn capitalize_display_name(display_name: &str) -> String {
    let mut chars = display_name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result = String::new();
    result.extend(first.to_uppercase());
    result.extend(chars.flat_map(char::to_lowercase));
    result
}

fn stable_hash64(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn image_paint_fallback_colors(image_urls: &[String]) -> Vec<RgbaColor> {
    let Some(first_url) = image_urls.first() else {
        return Vec::new();
    };
    let hash = stable_hash64(first_url);
    let channel = |shift: u32, base: u8, span: u8| -> u8 {
        base.saturating_add(((hash >> shift) as u8) % span)
    };
    vec![
        RgbaColor {
            r: channel(0, 96, 128),
            g: channel(8, 96, 128),
            b: channel(16, 96, 128),
            a: 255,
        },
        RgbaColor {
            r: channel(24, 48, 160),
            g: channel(32, 48, 160),
            b: channel(40, 48, 160),
            a: 255,
        },
    ]
}

/// Sample a CSS gradient stop list at `ratio` in `[0, 1]`: linear interpolation
/// in premultiplied RGBA (css-images-3 §3.4.2), returned as straight RGBA.
pub fn sample_gradient_stops(stops: &[SevenTvGradientStop], ratio: f32) -> Option<RgbaColor> {
    let first = *stops.first()?;
    let ratio = ratio.clamp(0.0, 1.0);
    if ratio <= first.at {
        return Some(first.color);
    }

    for window in stops.windows(2) {
        let left = window[0];
        let right = window[1];
        if ratio <= right.at {
            let span = (right.at - left.at).abs();
            let t = if span <= f32::EPSILON {
                0.0
            } else {
                (ratio - left.at) / (right.at - left.at)
            };
            return Some(left.color.lerp(right.color, t));
        }
    }

    stops.last().map(|stop| stop.color)
}

/// Map a raw gradient-line ratio into the stop domain for CSS repeating
/// gradients. The gradient line tiles with period `last_stop - first_stop`
/// (css-images-3 §3.4.3 repeating gradients), including the region before the
/// first stop. A non-positive period degenerates to clamping (the
/// non-repeating behavior), matching how such gradients collapse anyway.
pub fn repeating_gradient_ratio(stops: &[SevenTvGradientStop], ratio: f32) -> f32 {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return ratio.clamp(0.0, 1.0);
    };
    let period = last.at - first.at;
    if period <= f32::EPSILON {
        return ratio.clamp(0.0, 1.0);
    }
    first.at + (ratio - first.at).rem_euclid(period)
}

/// Direction vector for a CSS `linear-gradient` angle (0deg points up, angles grow
/// clockwise), in a Y-down coordinate system.
pub fn css_gradient_direction(angle_degrees: f32) -> (f32, f32) {
    let radians = angle_degrees.to_radians();
    (radians.sin(), -radians.cos())
}

/// Minimum and maximum projections of a `width x height` rect's corners onto
/// `direction`. The span between them is the CSS gradient-line length
/// (`|w*sin A| + |h*cos A|`), so `(projection - min) / (max - min)` is the exact CSS
/// linear-gradient ratio for a point.
pub fn project_rect_bounds(direction: (f32, f32), width: f32, height: f32) -> (f32, f32) {
    let corners = [
        (0.0, 0.0),
        (width.max(0.0), 0.0),
        (0.0, height.max(0.0)),
        (width.max(0.0), height.max(0.0)),
    ];
    let mut min_projection = f32::INFINITY;
    let mut max_projection = f32::NEG_INFINITY;
    for (x, y) in corners {
        let projection = x * direction.0 + y * direction.1;
        min_projection = min_projection.min(projection);
        max_projection = max_projection.max(projection);
    }
    (min_projection, max_projection)
}

/// Box radii for three successive box blurs approximating a gaussian with
/// standard deviation `sigma` (a CSS `drop-shadow` blur, in device pixels).
///
/// The box widths are the odd width `wl` at or below the ideal width and
/// `wl + 2`. The first `m` passes use `wl`, with `m` in `0..=3` chosen to
/// minimise `|sum((w * w - 1) / 12) - sigma^2|`, so the combined variance
/// tracks the requested one. Returns `None` when `sigma` is not finite or so
/// small that every pass would be a zero-radius copy (below about 0.58).
pub fn gaussian_box_blur_radii(sigma: f32) -> Option<[usize; 3]> {
    const PASSES: u32 = 3;
    if !sigma.is_finite() || sigma < 0.1 {
        return None;
    }

    let variance = sigma * sigma;
    let ideal_width = (12.0 * variance / PASSES as f32 + 1.0).sqrt();
    let mut lower = ideal_width.floor() as u32;
    if lower.is_multiple_of(2) {
        lower = lower.saturating_sub(1);
    }
    let lower = lower.max(1);
    let upper = lower + 2;
    let box_variance = |width: u32| (width as f32 * width as f32 - 1.0) / 12.0;
    let variance_error = |lower_passes: u32| {
        (lower_passes as f32 * box_variance(lower)
            + (PASSES - lower_passes) as f32 * box_variance(upper)
            - variance)
            .abs()
    };
    let lower_passes = (0..=PASSES)
        .min_by(|a, b| variance_error(*a).total_cmp(&variance_error(*b)))
        .unwrap_or(0);

    let radius = |width: u32| ((width - 1) / 2) as usize;
    let radii: [usize; 3] = std::array::from_fn(|pass| {
        if (pass as u32) < lower_passes {
            radius(lower)
        } else {
            radius(upper)
        }
    });
    radii.iter().any(|radius| *radius > 0).then_some(radii)
}

fn normalize_gradient_position(value: Option<f32>) -> Option<f32> {
    let position = value?;
    if !position.is_finite() {
        return None;
    }
    Some(if position.abs() > 1.0 {
        (position / 100.0).clamp(0.0, 1.0)
    } else {
        position.clamp(0.0, 1.0)
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvBadge {
    pub id: String,
    pub name: String,
    pub tooltip: String,
    pub url: String,
    pub is_animated: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvCosmeticsCatalog {
    pub paints: HashMap<String, SevenTvNamePaint>,
    pub badges: HashMap<String, SevenTvBadge>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvUserStyle {
    pub color: Option<RgbaColor>,
    pub paint_id: Option<String>,
    pub badge_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvUserProfile {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub avatar_is_animated: bool,
    pub style: SevenTvUserStyle,
    /// The user's 7TV personal emote set (emote set flag 4), if any.
    pub personal_emote_set_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedUserCosmetics {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub avatar_is_animated: bool,
    pub paint: Option<SevenTvNamePaint>,
    pub badge: Option<SevenTvBadge>,
    pub personal_emote_set_id: Option<String>,
}

pub fn parse_user_profile_json(json: &str) -> Result<SevenTvUserProfile, String> {
    let response: SevenTvUserResponse =
        serde_json::from_str(json).map_err(|error| format!("Invalid 7TV user JSON: {error}"))?;
    Ok(response.into_profile())
}

pub fn parse_cosmetics_catalog_json(json: &str) -> Result<SevenTvCosmeticsCatalog, String> {
    let response: SevenTvGraphqlResponse = serde_json::from_str(json)
        .map_err(|error| format!("Invalid 7TV cosmetics JSON: {error}"))?;
    let Some(data) = response.data else {
        return Err("7TV cosmetics response did not contain data".to_string());
    };
    Ok(data.cosmetics.into_catalog())
}

/// Parse the 7TV v4 GQL paint catalog (`query { paints { paints { ... } } }`).
///
/// The v4 API is the only source that still carries full paint layer data
/// (linear/radial gradient stops, repeating flags, image layers) and real
/// gaussian shadow blur radii; the v3 catalog returns paints with empty
/// `gradients` and null `color`.
pub fn parse_paints_v4_json(json: &str) -> Result<HashMap<String, SevenTvNamePaint>, String> {
    let response: V4PaintsResponse = serde_json::from_str(json)
        .map_err(|error| format!("Invalid 7TV v4 paints JSON: {error}"))?;
    let Some(data) = response.data else {
        return Err("7TV v4 paints response did not contain data".to_string());
    };
    Ok(data
        .paints
        .paints
        .into_iter()
        .map(|paint| {
            let resolved = paint.into_paint();
            (resolved.id.clone(), resolved)
        })
        .collect())
}

#[derive(Deserialize)]
struct V4PaintsResponse {
    data: Option<V4PaintsData>,
}

#[derive(Deserialize)]
struct V4PaintsData {
    paints: V4PaintsList,
}

#[derive(Deserialize)]
struct V4PaintsList {
    #[serde(default)]
    paints: Vec<V4PaintDto>,
}

#[derive(Deserialize)]
struct V4PaintDto {
    id: String,
    name: String,
    #[serde(default)]
    data: V4PaintDataDto,
}

#[derive(Default, Deserialize)]
struct V4PaintDataDto {
    #[serde(default)]
    layers: Vec<V4PaintLayerDto>,
    #[serde(default)]
    shadows: Vec<V4PaintShadowDto>,
}

#[derive(Deserialize)]
struct V4PaintLayerDto {
    #[serde(default = "default_layer_opacity")]
    opacity: f32,
    ty: V4PaintLayerTyDto,
}

fn default_layer_opacity() -> f32 {
    1.0
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum V4PaintLayerTyDto {
    PaintLayerTypeSingleColor {
        color: V4ColorDto,
    },
    PaintLayerTypeLinearGradient {
        #[serde(default)]
        angle: f32,
        #[serde(default)]
        repeating: bool,
        #[serde(default)]
        stops: Vec<V4GradientStopDto>,
    },
    PaintLayerTypeRadialGradient {
        #[serde(default)]
        repeating: bool,
        #[serde(default)]
        shape: Option<String>,
        #[serde(default)]
        stops: Vec<V4GradientStopDto>,
    },
    PaintLayerTypeImage {
        #[serde(default)]
        images: Vec<V4PaintImageDto>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Default, Deserialize)]
struct V4ColorDto {
    #[serde(default)]
    r: u8,
    #[serde(default)]
    g: u8,
    #[serde(default)]
    b: u8,
    #[serde(default)]
    a: u8,
}

impl V4ColorDto {
    fn into_color(self) -> RgbaColor {
        RgbaColor {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }
}

#[derive(Deserialize)]
struct V4GradientStopDto {
    #[serde(default)]
    at: f32,
    color: V4ColorDto,
}

#[derive(Deserialize)]
struct V4PaintShadowDto {
    color: V4ColorDto,
    #[serde(default, rename = "offsetX")]
    offset_x: f32,
    #[serde(default, rename = "offsetY")]
    offset_y: f32,
    #[serde(default)]
    blur: f32,
}

#[derive(Deserialize)]
struct V4PaintImageDto {
    url: String,
    #[serde(default)]
    scale: i64,
    #[serde(default, rename = "frameCount")]
    frame_count: u32,
}

impl V4PaintDto {
    fn into_paint(self) -> SevenTvNamePaint {
        let mut colors = Vec::new();
        let mut gradients = Vec::new();
        let mut image_urls = Vec::new();

        for layer in self.data.layers {
            let opacity = layer.opacity.clamp(0.0, 1.0);
            match layer.ty {
                V4PaintLayerTyDto::PaintLayerTypeSingleColor { color } => {
                    let color = color.into_color();
                    colors.push(color);
                    gradients.push(SevenTvPaintGradient {
                        kind: SevenTvGradientKind::Solid,
                        angle_degrees: 0.0,
                        repeat: false,
                        image_url: None,
                        image_is_animated: false,
                        stops: vec![
                            SevenTvGradientStop { at: 0.0, color },
                            SevenTvGradientStop { at: 1.0, color },
                        ],
                        opacity,
                    });
                }
                V4PaintLayerTyDto::PaintLayerTypeLinearGradient {
                    angle,
                    repeating,
                    stops,
                } => {
                    let stops = v4_gradient_stops(stops, &mut colors);
                    gradients.push(SevenTvPaintGradient {
                        kind: SevenTvGradientKind::Linear,
                        angle_degrees: angle,
                        repeat: repeating,
                        image_url: None,
                        image_is_animated: false,
                        stops,
                        opacity,
                    });
                }
                V4PaintLayerTyDto::PaintLayerTypeRadialGradient {
                    repeating,
                    shape,
                    stops,
                } => {
                    let shape = match shape.as_deref() {
                        Some(value) if value.eq_ignore_ascii_case("ellipse") => {
                            SevenTvRadialShape::Ellipse
                        }
                        _ => SevenTvRadialShape::Circle,
                    };
                    let stops = v4_gradient_stops(stops, &mut colors);
                    gradients.push(SevenTvPaintGradient {
                        kind: SevenTvGradientKind::Radial(shape),
                        angle_degrees: 0.0,
                        repeat: repeating,
                        image_url: None,
                        image_is_animated: false,
                        stops,
                        opacity,
                    });
                }
                V4PaintLayerTyDto::PaintLayerTypeImage { images } => {
                    if let Some((url, image_is_animated)) = choose_v4_paint_image(&images) {
                        image_urls.push(url.clone());
                        gradients.push(SevenTvPaintGradient {
                            kind: SevenTvGradientKind::Linear,
                            angle_degrees: 0.0,
                            repeat: false,
                            image_url: Some(url),
                            image_is_animated,
                            stops: Vec::new(),
                            opacity,
                        });
                    }
                }
                V4PaintLayerTyDto::Unknown => {}
            }
        }

        colors.dedup();
        image_urls.dedup();
        if colors.is_empty() && !image_urls.is_empty() {
            colors = image_paint_fallback_colors(&image_urls);
        }

        let shadows: Vec<SevenTvPaintShadow> = self
            .data
            .shadows
            .into_iter()
            .map(|shadow| {
                SevenTvPaintShadow::bounded(
                    shadow.offset_x,
                    shadow.offset_y,
                    shadow.blur,
                    shadow.color.into_color(),
                )
            })
            .collect();

        SevenTvNamePaint {
            id: self.id,
            name: self.name,
            colors,
            gradients,
            image_urls,
            text: SevenTvPaintText::default(),
            shadows,
        }
    }
}

fn v4_gradient_stops(
    stops: Vec<V4GradientStopDto>,
    colors: &mut Vec<RgbaColor>,
) -> Vec<SevenTvGradientStop> {
    let mut parsed: Vec<SevenTvGradientStop> = stops
        .into_iter()
        .map(|stop| SevenTvGradientStop {
            at: normalize_gradient_position(Some(stop.at)).unwrap_or(0.0),
            color: stop.color.into_color(),
        })
        .collect();
    normalize_gradient_stops(&mut parsed);
    for stop in &parsed {
        colors.push(stop.color);
    }
    parsed
}

/// Prefer an animated variant near 2x scale, mirroring how chat emotes pick their
/// display variants; fall back to the closest static scale.
fn choose_v4_paint_image(images: &[V4PaintImageDto]) -> Option<(String, bool)> {
    let score = |image: &V4PaintImageDto| -> i64 {
        let animated_bonus = if image.frame_count > 1 { 1_000 } else { 0 };
        let scale_penalty = (image.scale - 2).abs() * 10;
        let path = image.url.split(['?', '#']).next().unwrap_or(&image.url);
        let format_bonus = if path.ends_with(".webp") {
            3
        } else if path.ends_with(".gif") {
            2
        } else if path.ends_with(".png") {
            1
        } else {
            // Echo intentionally does not negotiate AVIF until the configured
            // decoder path supports it safely.
            0
        };
        animated_bonus - scale_penalty + format_bonus
    };
    images
        .iter()
        .filter(|image| !image.url.trim().is_empty())
        .filter(|image| {
            let path = image.url.split(['?', '#']).next().unwrap_or(&image.url);
            !path.ends_with(".avif")
        })
        .max_by_key(|image| score(image))
        .and_then(|image| normalize_7tv_url(&image.url).map(|url| (url, image.frame_count > 1)))
}

/// The paint and badge ids the profile selects that the catalog does not
/// contain, typically cosmetics published after the catalog was fetched.
pub fn unknown_cosmetic_ids<'a>(
    profile: &'a SevenTvUserProfile,
    catalog: &SevenTvCosmeticsCatalog,
) -> impl Iterator<Item = &'a str> {
    let paint_id = profile
        .style
        .paint_id
        .as_deref()
        .filter(|paint_id| !catalog.paints.contains_key(*paint_id));
    let badge_id = profile
        .style
        .badge_id
        .as_deref()
        .filter(|badge_id| !catalog.badges.contains_key(*badge_id));
    paint_id.into_iter().chain(badge_id)
}

/// Resolve a profile's active cosmetics against the catalog.
///
/// The name paint is the catalog entry for `style.paint_id` only. The 7TV
/// profile `style.color` (role color) never recolors chat names (the SevenTV
/// extension doesn't apply it there either), and a paint id missing from a
/// stale catalog must resolve to no paint rather than to that color.
pub fn resolve_user_cosmetics(
    profile: &SevenTvUserProfile,
    catalog: &SevenTvCosmeticsCatalog,
) -> ResolvedUserCosmetics {
    let paint = profile
        .style
        .paint_id
        .as_ref()
        .and_then(|paint_id| catalog.paints.get(paint_id))
        .cloned();

    let badge = profile
        .style
        .badge_id
        .as_ref()
        .and_then(|badge_id| catalog.badges.get(badge_id))
        .cloned();

    ResolvedUserCosmetics {
        user_id: profile.id.clone(),
        username: profile.username.clone(),
        display_name: profile.display_name.clone(),
        avatar_url: profile.avatar_url.clone(),
        avatar_is_animated: profile.avatar_is_animated,
        paint,
        badge,
        personal_emote_set_id: profile.personal_emote_set_id.clone(),
    }
}

fn normalize_7tv_host(host_url: &str) -> String {
    let trimmed = host_url.trim();
    let base = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{}", trimmed)
    } else {
        format!("https://{}", trimmed)
    };
    base.trim_end_matches('/').to_string()
}

fn normalize_7tv_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else if trimmed.starts_with("//") {
            format!("https:{}", trimmed)
        } else {
            format!("https://{}", trimmed)
        },
    )
}

fn build_asset_url(host_url: &str, file_name: &str) -> String {
    format!(
        "{}/{}",
        normalize_7tv_host(host_url),
        file_name.trim_start_matches('/')
    )
}

fn choose_badge_file(files: &[SevenTvImageFile]) -> Option<&SevenTvImageFile> {
    const PREFERRED: [&str; 6] = [
        "2x.webp", "2x.png", "1x.webp", "1x.png", "3x.webp", "4x.webp",
    ];
    PREFERRED
        .iter()
        .find_map(|name| {
            files
                .iter()
                .find(|file| file.name.eq_ignore_ascii_case(name))
        })
        .or_else(|| {
            files.iter().find(|file| {
                let name = file.name.to_ascii_lowercase();
                name.ends_with(".webp") || name.ends_with(".png") || name.ends_with(".gif")
            })
        })
}

#[derive(Deserialize)]
struct SevenTvUserResponse {
    id: String,
    username: String,
    display_name: String,
    #[serde(default)]
    user: Option<SevenTvUserInner>,
}

impl SevenTvUserResponse {
    fn into_profile(self) -> SevenTvUserProfile {
        let inner = self.user.unwrap_or_default();
        let style = inner.style.unwrap_or_default();
        let personal_emote_set_id = inner
            .emote_sets
            .into_iter()
            .find(|set| set.flags & SEVENTV_PERSONAL_EMOTE_SET_FLAG != 0 && !set.id.is_empty())
            .map(|set| set.id);
        SevenTvUserProfile {
            personal_emote_set_id,
            id: inner.id.unwrap_or(self.id),
            username: inner.username.unwrap_or(self.username),
            display_name: inner.display_name.unwrap_or(self.display_name),
            avatar_is_animated: inner
                .avatar_url
                .as_deref()
                .is_some_and(is_probably_animated_avatar_url),
            avatar_url: inner.avatar_url,
            style: SevenTvUserStyle {
                color: style.color.map(RgbaColor::from_7tv_int),
                paint_id: style.paint_id.filter(|id| !id.is_empty()),
                badge_id: style.badge_id.filter(|id| !id.is_empty()),
            },
        }
    }
}

fn is_probably_animated_avatar_url(url: &str) -> bool {
    let lower = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    lower.ends_with(".gif") || lower.ends_with(".webp")
}

#[derive(Default, Deserialize)]
struct SevenTvUserInner {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    style: Option<SevenTvStyleDto>,
    #[serde(default)]
    emote_sets: Vec<SevenTvEmoteSetRefDto>,
}

const SEVENTV_PERSONAL_EMOTE_SET_FLAG: u32 = 4;

#[derive(Default, Deserialize)]
struct SevenTvEmoteSetRefDto {
    #[serde(default)]
    id: String,
    #[serde(default)]
    flags: u32,
}

#[derive(Default, Deserialize)]
struct SevenTvStyleDto {
    #[serde(default)]
    color: Option<i64>,
    #[serde(default)]
    paint_id: Option<String>,
    #[serde(default)]
    badge_id: Option<String>,
}

#[derive(Deserialize)]
struct SevenTvGraphqlResponse {
    data: Option<SevenTvGraphqlData>,
}

#[derive(Deserialize)]
struct SevenTvGraphqlData {
    cosmetics: SevenTvCosmeticsDto,
}

#[derive(Default, Deserialize)]
struct SevenTvCosmeticsDto {
    #[serde(default)]
    paints: Vec<SevenTvPaintDto>,
    #[serde(default)]
    badges: Vec<SevenTvBadgeDto>,
}

impl SevenTvCosmeticsDto {
    fn into_catalog(self) -> SevenTvCosmeticsCatalog {
        let paints = self
            .paints
            .into_iter()
            .map(|paint| {
                let resolved = paint.into_paint();
                (resolved.id.clone(), resolved)
            })
            .collect();
        let badges = self
            .badges
            .into_iter()
            .filter_map(|badge| badge.into_badge())
            .map(|badge| (badge.id.clone(), badge))
            .collect();
        SevenTvCosmeticsCatalog { paints, badges }
    }
}

#[derive(Deserialize)]
struct SevenTvPaintDto {
    id: String,
    name: String,
    #[serde(default)]
    color: Option<i64>,
    #[serde(default)]
    gradients: Vec<SevenTvGradientDto>,
    #[serde(default)]
    shadows: Vec<SevenTvShadowDto>,
    #[serde(default)]
    text: Option<SevenTvTextDto>,
}

impl SevenTvPaintDto {
    fn into_paint(self) -> SevenTvNamePaint {
        let mut colors = Vec::new();
        let mut image_urls = Vec::new();
        let mut gradients = Vec::new();
        if let Some(color) = self.color {
            colors.push(RgbaColor::from_7tv_int(color));
        }
        for gradient in self.gradients {
            let parsed_gradient = gradient.into_gradient();
            if let Some(image_url) = parsed_gradient.image_url.as_ref() {
                image_urls.push(image_url.clone());
            }
            for stop in &parsed_gradient.stops {
                colors.push(stop.color);
            }
            gradients.push(parsed_gradient);
        }
        colors.dedup();
        image_urls.dedup();
        if colors.is_empty() && !image_urls.is_empty() {
            colors = image_paint_fallback_colors(&image_urls);
        }

        let mut shadows: Vec<SevenTvPaintShadow> = self
            .shadows
            .into_iter()
            .map(SevenTvShadowDto::into_shadow)
            .collect();
        let (text, mut text_shadows) = self
            .text
            .map(SevenTvTextDto::into_parts)
            .unwrap_or_else(|| (SevenTvPaintText::default(), Vec::new()));
        shadows.append(&mut text_shadows);
        shadows.dedup_by(|a, b| {
            (a.x_offset - b.x_offset).abs() <= f32::EPSILON
                && (a.y_offset - b.y_offset).abs() <= f32::EPSILON
                && (a.radius - b.radius).abs() <= f32::EPSILON
                && a.color == b.color
        });

        SevenTvNamePaint {
            id: self.id,
            name: self.name,
            colors,
            gradients,
            image_urls,
            text,
            shadows,
        }
    }
}

#[derive(Default, Deserialize)]
struct SevenTvGradientDto {
    #[serde(default)]
    angle: Option<f32>,
    #[serde(default)]
    repeat: bool,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    stops: Vec<SevenTvGradientStopDto>,
}

impl SevenTvGradientDto {
    fn into_gradient(self) -> SevenTvPaintGradient {
        let image_url = self.image_url.as_deref().and_then(normalize_7tv_url);
        let image_is_animated = image_url
            .as_deref()
            .is_some_and(legacy_paint_image_url_is_animated);
        let mut stops: Vec<SevenTvGradientStop> = self
            .stops
            .into_iter()
            .map(SevenTvGradientStopDto::into_stop)
            .collect();
        normalize_gradient_stops(&mut stops);

        SevenTvPaintGradient {
            kind: SevenTvGradientKind::Linear,
            angle_degrees: self.angle.unwrap_or(90.0),
            repeat: self.repeat,
            image_url,
            image_is_animated,
            stops,
            opacity: 1.0,
        }
    }
}

fn legacy_paint_image_url_is_animated(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".gif")
        || lower.contains("/animated/")
        || lower.contains("_animated.")
        || (lower.contains("cdn.7tv.app/paint/")
            && lower.ends_with(".webp")
            && !lower.contains("_static."))
}

#[derive(Deserialize)]
struct SevenTvGradientStopDto {
    #[serde(default)]
    at: Option<f32>,
    color: i64,
}

impl SevenTvGradientStopDto {
    fn into_stop(self) -> SevenTvGradientStop {
        SevenTvGradientStop {
            at: normalize_gradient_position(self.at).unwrap_or(0.0),
            color: RgbaColor::from_7tv_int(self.color),
        }
    }
}

fn normalize_gradient_stops(stops: &mut [SevenTvGradientStop]) {
    if stops.is_empty() {
        return;
    }

    let has_explicit_positions = stops.iter().any(|stop| stop.at > 0.0);
    if !has_explicit_positions && stops.len() > 1 {
        let denom = (stops.len() - 1) as f32;
        for (index, stop) in stops.iter_mut().enumerate() {
            stop.at = index as f32 / denom;
        }
        return;
    }

    stops.sort_by(|left, right| left.at.total_cmp(&right.at));
    let mut max_seen = 0.0f32;
    for stop in stops.iter_mut() {
        stop.at = stop.at.clamp(max_seen, 1.0);
        max_seen = stop.at;
    }
}

#[derive(Deserialize)]
struct SevenTvShadowDto {
    #[serde(default)]
    x_offset: f32,
    #[serde(default)]
    y_offset: f32,
    #[serde(default)]
    radius: f32,
    color: i64,
}

impl SevenTvShadowDto {
    fn into_shadow(self) -> SevenTvPaintShadow {
        SevenTvPaintShadow::bounded(
            self.x_offset,
            self.y_offset,
            self.radius,
            RgbaColor::from_7tv_int(self.color),
        )
    }
}

#[derive(Default, Deserialize)]
struct SevenTvTextDto {
    #[serde(default)]
    weight: Option<u16>,
    #[serde(default)]
    transform: Option<String>,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    shadows: Vec<SevenTvShadowDto>,
    #[serde(default)]
    stroke: Option<SevenTvStrokeDto>,
}

impl SevenTvTextDto {
    fn into_parts(self) -> (SevenTvPaintText, Vec<SevenTvPaintShadow>) {
        let transform = self
            .transform
            .as_deref()
            .and_then(SevenTvTextTransform::parse);
        let stroke = self.stroke.map(|stroke| SevenTvPaintStroke {
            width: stroke.width,
            color: RgbaColor::from_7tv_int(stroke.color),
        });
        let shadows = self
            .shadows
            .into_iter()
            .map(SevenTvShadowDto::into_shadow)
            .collect();
        (
            SevenTvPaintText {
                weight: self.weight,
                transform,
                variant: self.variant.filter(|variant| !variant.is_empty()),
                stroke,
            },
            shadows,
        )
    }
}

#[derive(Deserialize)]
struct SevenTvStrokeDto {
    color: i64,
    width: f32,
}

#[derive(Deserialize)]
struct SevenTvBadgeDto {
    id: String,
    name: String,
    tooltip: String,
    host: SevenTvImageHost,
}

impl SevenTvBadgeDto {
    fn into_badge(self) -> Option<SevenTvBadge> {
        let file = choose_badge_file(&self.host.files)?;
        Some(SevenTvBadge {
            id: self.id,
            name: self.name,
            tooltip: self.tooltip,
            url: build_asset_url(&self.host.url, &file.name),
            is_animated: file.frame_count > 1 || file.format.eq_ignore_ascii_case("GIF"),
        })
    }
}

#[derive(Deserialize)]
struct SevenTvImageHost {
    url: String,
    #[serde(default)]
    files: Vec<SevenTvImageFile>,
}

#[derive(Deserialize)]
struct SevenTvImageFile {
    name: String,
    #[serde(default)]
    frame_count: u32,
    #[serde(default)]
    format: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_profile_reads_personal_emote_set() {
        let json = r#"{"id":"1","username":"a","display_name":"A","user":{"emote_sets":[
            {"id":"channel-set","flags":0},{"id":"personal-set","flags":4}]}}"#;
        let profile = parse_user_profile_json(json).expect("profile parses");
        assert_eq!(
            profile.personal_emote_set_id.as_deref(),
            Some("personal-set")
        );

        let without = r#"{"id":"1","username":"a","display_name":"A","user":{}}"#;
        let profile = parse_user_profile_json(without).expect("profile parses");
        assert_eq!(profile.personal_emote_set_id, None);
    }

    #[test]
    fn profile_role_color_never_becomes_a_name_paint() {
        let role_color = RgbaColor::from_7tv_int(401323775);
        let mut profile = SevenTvUserProfile {
            id: "7tv-user".to_string(),
            style: SevenTvUserStyle {
                color: Some(role_color),
                paint_id: None,
                badge_id: None,
            },
            ..Default::default()
        };
        let mut catalog = SevenTvCosmeticsCatalog::default();
        assert_eq!(resolve_user_cosmetics(&profile, &catalog).paint, None);
        assert_eq!(unknown_cosmetic_ids(&profile, &catalog).count(), 0);

        profile.style.paint_id = Some("stale-paint".to_string());
        assert_eq!(
            unknown_cosmetic_ids(&profile, &catalog).collect::<Vec<_>>(),
            ["stale-paint"]
        );
        assert_eq!(
            resolve_user_cosmetics(&profile, &catalog).paint,
            None,
            "a paint id missing from the catalog must not fall back to the role color"
        );

        let image_only = SevenTvNamePaint {
            id: "stale-paint".to_string(),
            name: "Image".to_string(),
            gradients: vec![SevenTvPaintGradient {
                image_url: Some("https://cdn.7tv.app/paint/a.webp".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        catalog
            .paints
            .insert("stale-paint".to_string(), image_only.clone());
        assert_eq!(
            resolve_user_cosmetics(&profile, &catalog).paint,
            Some(image_only),
            "the catalog paint is used verbatim, without the role color in its colors"
        );
        assert_eq!(unknown_cosmetic_ids(&profile, &catalog).count(), 0);

        profile.style.badge_id = Some("new-badge".to_string());
        assert_eq!(
            unknown_cosmetic_ids(&profile, &catalog).collect::<Vec<_>>(),
            ["new-badge"]
        );
    }

    #[test]
    fn seven_tv_color_is_rgba() {
        let color = RgbaColor::from_7tv_int(401323775);
        assert_eq!(color.to_hex_rgb(), "#17ebb6");
        assert_eq!(color.a, 255);
    }

    #[test]
    fn gradient_stop_sampling_interpolates_between_stops() {
        let stops = [
            SevenTvGradientStop {
                at: 0.0,
                color: RgbaColor {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                },
            },
            SevenTvGradientStop {
                at: 1.0,
                color: RgbaColor {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                },
            },
        ];

        assert_eq!(sample_gradient_stops(&stops, 0.0).unwrap().r, 0);
        assert_eq!(sample_gradient_stops(&stops, 0.5).unwrap().r, 128);
        assert_eq!(sample_gradient_stops(&stops, 1.0).unwrap().r, 255);
    }

    fn box_blur_sigma(radii: [usize; 3]) -> f32 {
        radii
            .iter()
            .map(|radius| {
                let width = (2 * radius + 1) as f32;
                (width * width - 1.0) / 12.0
            })
            .sum::<f32>()
            .sqrt()
    }

    #[test]
    fn box_blur_radii_track_the_requested_sigma() {
        assert_eq!(gaussian_box_blur_radii(0.05), None);
        assert_eq!(gaussian_box_blur_radii(f32::NAN), None);
        assert_eq!(gaussian_box_blur_radii(f32::INFINITY), None);
        // Up to sigma^2 = 1/3 the closest plan is three zero-radius passes,
        // which would only copy the canvas six times.
        assert_eq!(gaussian_box_blur_radii(0.1), None);
        assert_eq!(gaussian_box_blur_radii(0.5), None);
        assert_eq!(gaussian_box_blur_radii(0.6), Some([0, 0, 1]));

        // The previous width plan blurred sigma 2 as 2.45.
        let sigma_two = box_blur_sigma(gaussian_box_blur_radii(2.0).unwrap());
        assert!(
            (sigma_two - 2.0).abs() < 0.17,
            "sigma 2 blurred as {sigma_two}"
        );

        let mut sigma = 2.0_f32;
        while sigma <= 200.0 {
            let radii = gaussian_box_blur_radii(sigma).unwrap();
            let effective = box_blur_sigma(radii);
            assert!(
                (effective - sigma).abs() <= sigma * 0.085,
                "sigma {sigma} blurred as {effective} with radii {radii:?}"
            );
            assert!(radii.windows(2).all(|pair| pair[0] <= pair[1]));
            assert!(radii[2] - radii[0] <= 1);
            sigma += 0.05;
        }
    }

    #[test]
    fn gradient_stop_sampling_interpolates_premultiplied() {
        let red = RgbaColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let transparent_black = RgbaColor::from_7tv_int(0);
        let stops = [
            SevenTvGradientStop {
                at: 0.0,
                color: red,
            },
            SevenTvGradientStop {
                at: 1.0,
                color: transparent_black,
            },
        ];

        let mid = sample_gradient_stops(&stops, 0.5).unwrap();
        assert_eq!((mid.r, mid.g, mid.b), (255, 0, 0));
        assert!((127..=128).contains(&mid.a), "alpha was {}", mid.a);

        let near_end = sample_gradient_stops(&stops, 0.9).unwrap();
        assert_eq!((near_end.r, near_end.g, near_end.b), (255, 0, 0));
        assert!((25..=26).contains(&near_end.a), "alpha was {}", near_end.a);

        let end = sample_gradient_stops(&stops, 1.0).unwrap();
        assert_eq!(end.a, 0);
    }
}
