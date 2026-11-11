//! Pre-computed layout data structures for canvas rendering
//!
//! All layout calculations happen once when a message arrives or container
//! resizes, not on every frame. This enables O(1) render time per message.
#![allow(clippy::large_enum_variant)] // Size difference is acceptable for layout types
#![allow(clippy::too_many_arguments)] // Layout functions need context at once

use echo_core::version_registry::VersionRegistry;
use gpui::{Bounds, FontWeight, Hsla, Pixels, SharedString, TextRun};
use smallvec::SmallVec;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::ports;

// ============================================================================
// Layout Cache Key
// ============================================================================

#[inline]
pub fn hash_gpui_text_runs(runs: &[TextRun]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    for run in runs {
        run.len.hash(&mut hasher);
        run.font.hash(&mut hasher);
        run.color.hash(&mut hasher);
        run.background_color.hash(&mut hasher);
        run.underline.hash(&mut hasher);
        run.strikethrough.hash(&mut hasher);
    }
    hasher.finish()
}

/// Width bucket size in pixels for layout caching.
pub const WIDTH_BUCKET_PX: f32 = 8.0;

/// Quantize width down to 8px buckets for layout caching.
///
/// Wider buckets reuse more cached layouts while a resize is in progress.
/// Correctness: we quantize *down* (floor) so cached layouts never assume a width
/// larger than the actual container. This prevents right-edge clipping/overdraw
/// when the container shrinks but remains within the same bucket.
#[inline]
pub fn quantize_width(width: f32) -> u32 {
    if !width.is_finite() || width <= 0.0 {
        return 0;
    }
    (width / WIDTH_BUCKET_PX).floor() as u32
}

/// Convert a quantized width bucket back into a pixel width.
///
/// For q > 0, this is the *bucket lower bound* width we build layouts against.
#[inline]
pub fn width_from_quantized(quantized_width: u32) -> f32 {
    quantized_width as f32 * WIDTH_BUCKET_PX
}

/// Cache key combining message ID, quantized width, and timestamp visibility for layout caching.
/// Allows reusing cached layouts when resizing back to a previous width
/// Uses SharedString for O(1) clone (Arc-backed) vs O(n) String clone
/// NOTE: show_timestamp affects header visibility, so each variant needs separate layouts.
///
/// Timestamp *shape* is deliberately not a key field. Every compiled-formatter
/// generation change goes through `set_message_timestamp_pattern`, which also
/// bumps `LAYOUT_SETTINGS_VERSION`; `maybe_invalidate_for_settings_change`
/// observes that and clears the whole layout cache, so a retained layout can
/// never outlive the formatter generation that shaped its timestamp column.
/// The generation is keyed directly where per-entry invalidation is cheaper
/// than a sweep: the formatted-string cache and the shaped width probe.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LayoutCacheKey {
    pub message_id: SharedString,
    pub quantized_width: u32,
    /// Whether timestamp is shown (affects header layout width/visibility)
    pub show_timestamp: bool,
}

impl LayoutCacheKey {
    /// Create a cache key from a message ID string reference
    #[inline]
    pub fn new(message_id: &str, container_width: f32, show_timestamp: bool) -> Self {
        Self {
            message_id: SharedString::from(message_id),
            quantized_width: quantize_width(container_width),
            show_timestamp,
        }
    }

    /// Create a cache key from a SharedString (zero-allocation fast path)
    /// Use this when the message_id is already a SharedString to avoid allocation
    #[inline]
    pub fn from_shared(
        message_id: SharedString,
        container_width: f32,
        show_timestamp: bool,
    ) -> Self {
        Self {
            message_id,
            quantized_width: quantize_width(container_width),
            show_timestamp,
        }
    }

    /// Create a cache key from a SharedString and pre-quantized width.
    /// Lets hot render loops quantize once per frame instead of once per row.
    #[inline]
    pub fn from_shared_quantized(
        message_id: SharedString,
        quantized_width: u32,
        show_timestamp: bool,
    ) -> Self {
        Self {
            message_id,
            quantized_width,
            show_timestamp,
        }
    }
}

// ============================================================================
// Layout Constants
// ============================================================================

/// Base line height for messages (at Normal density)
pub const LINE_HEIGHT_BASE: f32 = 26.0;

/// Current line height for the chat density and font scale settings.
#[inline]
pub fn line_height() -> f32 {
    LINE_HEIGHT_BASE * ports::chat_density_multiplier() * ports::font_scale()
}

/// Vertical padding on message (top and bottom)
pub const MESSAGE_PADDING_VERTICAL: f32 = 2.0;

/// Horizontal padding on left side of message
pub const MESSAGE_PADDING_LEFT: f32 = 6.0;

/// Horizontal padding on right side of message
pub const MESSAGE_PADDING_RIGHT: f32 = 12.0;

/// Gap after timestamp before badges/username (about one space width)
pub const TIMESTAMP_GAP: f32 = 4.0;

/// Base badge size in pixels (at 1.0x scale)
pub const BADGE_SIZE_BASE: f32 = 18.0;

/// Get current badge size scaled by font size
#[inline]
pub fn badge_size() -> f32 {
    BADGE_SIZE_BASE * ports::font_scale() * ports::badge_scale()
}

/// Base gap between individual badges (at 1.0x scale)
pub const BADGE_GAP_BASE: f32 = 3.0;

/// Get current badge gap scaled by font size
#[inline]
pub fn badge_gap() -> f32 {
    BADGE_GAP_BASE * ports::font_scale()
}

/// Base gap after all badges before username (at 1.0x scale)
pub const BADGE_MARGIN_RIGHT_BASE: f32 = 4.0;

/// Get current badge margin right scaled by font size
#[inline]
pub fn badge_margin_right() -> f32 {
    BADGE_MARGIN_RIGHT_BASE * ports::font_scale()
}

/// Twitch and BTTV base emote geometry is 28x28. Keep this as the 100%
/// value; provider aspect ratios are preserved below.
pub const EMOTE_SIZE_BASE: f32 = 28.0;

/// Get current emote size based on settings scale factor
/// Called during layout, not every frame
#[inline]
pub fn emote_size() -> f32 {
    // Emotes should track the text scale so they stay visually consistent with the font size.
    // emote_scale remains an additional user-controlled multiplier on top.
    EMOTE_SIZE_BASE * ports::font_scale() * ports::emote_scale()
}

/// Emoji image size.
///
/// Emoji should match emote sizing so unicode emoji and Twitch/7TV/BTTV emotes
/// render at consistent dimensions.
#[inline]
pub fn emoji_size() -> f32 {
    emote_size()
}

/// Calculate scaled emote dimensions preserving aspect ratio
///
/// Given logical 1x dimensions and the configured nominal size, scales both
/// axes uniformly. Message wrapping owns the available viewport width.
///
/// Provider aspect ratios are preserved without an arbitrary width cap. The
/// dimensions are logical 1x pixels, scaled uniformly from the configured
/// nominal 28px emote size.
///
/// # Arguments
/// * `source_width` - Original emote width (None if unknown)
/// * `source_height` - Original emote height (None if unknown)
/// * `base_size` - Target size (typically from `emote_size()`)
///
/// # Returns
/// (scaled_width, scaled_height) tuple
#[inline]
pub fn calculate_emote_dimensions(
    source_width: Option<u32>,
    source_height: Option<u32>,
    base_size: f32,
) -> (f32, f32) {
    match (source_width, source_height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let source_scale = base_size / EMOTE_SIZE_BASE;
            (w as f32 * source_scale, h as f32 * source_scale)
        }
        _ => (base_size, base_size), // Unknown dimensions: square
    }
}

/// Segoe UI 10pt text occupies a roughly 16px logical line box on Windows.
pub const TEXT_HEIGHT_BASE: f32 = 16.0;

/// Get current text height scaled by font size
/// Used for baseline alignment with badges
#[inline]
pub fn text_height() -> f32 {
    TEXT_HEIGHT_BASE * ports::font_scale()
}

/// Selection highlight color
pub const SELECTION_COLOR: Hsla = Hsla {
    h: 210.0 / 360.0,
    s: 0.8,
    l: 0.6,
    a: 0.3,
};

// ============================================================================
// Embed Preview Constants (Large thumbnail on left style)
// ============================================================================

/// Embed accent stripe width (left border)
pub const EMBED_ACCENT_WIDTH: f32 = 4.0;

/// Embed content padding (inner spacing)
pub const EMBED_PADDING: f32 = 12.0;

/// Gap between thumbnail and text
pub const EMBED_TEXT_THUMBNAIL_GAP: f32 = 12.0;

/// Minimum embed total height
pub const EMBED_MIN_HEIGHT: f32 = 100.0;

/// Gap between embed title and description
pub const EMBED_TITLE_DESC_GAP: f32 = 6.0;

/// Embed card corner radius
pub const EMBED_BORDER_RADIUS: f32 = 6.0;

/// The complete geometry chosen while shaping an embed preview. The element
/// renderer consumes this verbatim so card layout cannot diverge from the
/// shaped height or thumbnail policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmbedPreviewGeometry {
    /// Rendered thumbnail width. Zero means the card is text-only.
    pub thumbnail_width: f32,
    /// Rendered thumbnail height. Zero means the card is text-only.
    pub thumbnail_height: f32,
    /// Place the thumbnail above the text instead of beside it.
    pub thumbnail_stacked: bool,
    /// Spacing between a rendered thumbnail and its text section.
    pub thumbnail_gap: f32,
    /// Number of visible lines reserved for each optional text field.
    pub title_lines: u8,
    pub description_lines: u8,
    pub subtitle_lines: u8,
}

// ============================================================================
// Reply Indicator Constants
// ============================================================================

/// Base height of the reply indicator row (at 1.0x scale)
pub const REPLY_INDICATOR_HEIGHT_BASE: f32 = 20.0;

/// Get current reply indicator height scaled by font size
#[inline]
pub fn reply_indicator_height() -> f32 {
    REPLY_INDICATOR_HEIGHT_BASE * ports::font_scale()
}

/// Left offset for reply connector line from message edge
pub const REPLY_CONNECTOR_LEFT: f32 = 10.0;

/// Width of the vertical connector line
pub const REPLY_CONNECTOR_WIDTH: f32 = 2.0;

/// Width of the horizontal connector arm
pub const REPLY_CONNECTOR_ARM: f32 = 8.0;

/// Gap between connector and @username
pub const REPLY_CONNECTOR_GAP: f32 = 12.0;

/// Maximum characters for parent preview text
pub const REPLY_PREVIEW_MAX_CHARS: usize = 50;

// ============================================================================
// Timestamp Format
// ============================================================================

// ============================================================================
// Highlight Types (extracted from message.rs)
// ============================================================================

/// Highlight type enumeration matching HighlightType
#[derive(Clone, Debug, PartialEq)]
pub enum HighlightType {
    Whisper,
    Mention,
    User(String),
    ParticipatedReply(String),
    Badge(String),
    HighlightedMessage,
    FirstMessage,
    ReturningChatter,
    Subscription,
    Raid,
    BitsBadge,
    Announcement(Option<String>),
    WatchStreak,
    Phrase(String),
    CustomPhrase(String, Option<String>),
    CustomUser(String, Option<String>),
    CustomBadge(String, Option<String>),
    SelfMessage,
    Elevated,
    ChannelPointRedeem { reward: String, cost: u32 },
}

/// Convert from backend's HighlightType to our HighlightType
impl From<&echo_core::message::HighlightType> for HighlightType {
    fn from(frb: &echo_core::message::HighlightType) -> Self {
        use echo_core::message::HighlightType as BH;
        match frb {
            BH::Whisper => HighlightType::Whisper,
            BH::Mention => HighlightType::Mention,
            BH::User { username } => HighlightType::User(username.clone()),
            BH::ParticipatedReply { username } => {
                HighlightType::ParticipatedReply(username.clone())
            }
            BH::Badge { badge } => HighlightType::Badge(badge.clone()),
            BH::HighlightedMessage => HighlightType::HighlightedMessage,
            BH::FirstMessage => HighlightType::FirstMessage,
            BH::ReturningChatter => HighlightType::ReturningChatter,
            BH::Subscription => HighlightType::Subscription,
            BH::Raid => HighlightType::Raid,
            BH::BitsBadge => HighlightType::BitsBadge,
            BH::Announcement { color } => HighlightType::Announcement(color.clone()),
            BH::WatchStreak => HighlightType::WatchStreak,
            BH::Phrase { pattern } => HighlightType::Phrase(pattern.clone()),
            BH::CustomPhrase { pattern, color } => {
                HighlightType::CustomPhrase(pattern.clone(), color.clone())
            }
            BH::CustomUser { username, color } => {
                HighlightType::CustomUser(username.clone(), color.clone())
            }
            BH::CustomBadge { badge, color } => {
                HighlightType::CustomBadge(badge.clone(), color.clone())
            }
            BH::SelfMessage => HighlightType::SelfMessage,
            BH::Elevated => HighlightType::Elevated,
            BH::ChannelPointRedeem {
                reward_title,
                reward_cost,
                ..
            } => HighlightType::ChannelPointRedeem {
                reward: reward_title.clone(),
                cost: *reward_cost,
            },
        }
    }
}

/// AutoMod / low-trust row treatment, read from `MessageFlags` at layout-build
/// time and painted as a background tint plus left border. Colors resolve at
/// paint time (mirroring `highlight_type`) so settings changes apply live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LowTrustTreatment {
    #[default]
    None,
    /// AutoMod-held message (grey; uses `automod_highlight_color`).
    AutoMod,
    /// Suspicious user under active monitoring.
    Monitored,
    /// Suspicious user with a restriction applied.
    Restricted,
}

// ============================================================================
// Core Layout Structures
// ============================================================================

/// Pre-computed layout for a single message
///
/// Built once on message arrival or container resize.
/// Enables O(1) render time - just iterate segments and paint.
#[derive(Clone, Debug)]
pub struct CanvasMessageLayout {
    /// Pre-computed SharedString for GPUI element ID (avoids clone per frame)
    pub message_id_shared: SharedString,

    /// Total computed height in pixels
    pub height: f32,

    /// All visual segments in render order.
    ///
    /// RAM: the layout-owning lists below are exact-size `Vec`s rather than
    /// inline `SmallVec`s. Layouts are cached by the thousand, and inline
    /// capacity sized for the long tail (8 segments of 688 B, 4 shaped lines
    /// of 1,120 B) made every entry about 12 KB, mostly empty.
    /// `rebuild_paint_commands` trims them once the layout is built.
    pub segments: Vec<LayoutSegment>,

    /// Pre-shaped text spans, one per run of text between images or breaks.
    pub shaped_lines: Vec<ShapedLineData>,

    /// Pre-computed direct paint commands for the unified message canvas.
    ///
    /// Avoids scanning `segments` and allocating temporary Vecs every frame.
    pub text_paint_commands: SmallVec<[TextPaintCommand; 4]>,
    pub image_paint_commands: Vec<ImagePaintCommand>,

    /// Hitboxes for interactive elements (badges, username, emotes, links).
    pub hitbox_regions: Vec<HitboxRegion>,

    /// Highlight type (colors computed at render time for live updates)
    pub highlight_type: Option<HighlightType>,

    /// AutoMod / low-trust row treatment (background tint + left border),
    /// resolved from the message flags at build time and painted alongside
    /// `highlight_type`.
    pub low_trust: LowTrustTreatment,

    /// Original source text for clipboard (includes username prefix)
    pub source_text: String,

    /// Character X positions for source_text (for accurate selection highlighting).
    ///
    /// Inline cap of 48: source_text includes the username prefix ("Author: msg body"),
    /// so it is longer than any single shaped line. Typical Twitch messages with a
    /// username (avg ~10 chars) + body (~30 chars) + separator fit in ~48 entries.
    /// Messages longer than 48 chars spill to heap. Cap reduced from 128 (512 B) to
    /// 48 (192 B), saving 320 B per cached layout. Across ~8 000 cached layouts that
    /// is ~2.5 MB of inline footprint recovered.
    pub source_char_positions: SmallVec<[f32; 48]>,

    /// Mention highlight regions (x, y, width, height for background rectangles)
    pub mention_highlights: SmallVec<[(f32, f32, f32, f32); 4]>,

    /// Number of lines (for multi-line messages)
    pub line_count: u32,

    /// Whether this message has a reply indicator row
    pub has_reply_indicator: bool,

    /// Y positions for each line (for dynamic line height selection hit testing)
    /// Index i = Y position where line i starts, relative to message origin
    pub line_y_positions: SmallVec<[f32; 8]>,

    /// Emote resolution version when this layout was created
    /// Used to detect when emotes have loaded/changed and layout needs rebuilding
    pub emote_version_at_creation: u64,

    /// Count of animated emotes in this message (for render optimization)
    /// When > 0, message needs per-animation-tick updates.
    /// When 0, message can skip work during animation-only frames.
    pub animated_emote_count: u8,

    /// Channel this layout belongs to (for channel-aware staleness checking)
    /// Enables surgical invalidation - only rebuild layouts for channels where emotes changed
    pub channel: Arc<str>,

    /// Channel-specific emote version when this layout was created
    /// Used with VersionRegistry::channel_emote_version() for targeted invalidation
    pub channel_emote_version_at_creation: u64,

    /// Whether text positions were measured with a live GPUI Window.
    ///
    /// Background layout workers intentionally build without a Window so they can warm
    /// heights off the UI thread. Those fallback-metric layouts must not become the
    /// final visible geometry because their text widths can differ from GPUI shaping.
    pub uses_window_text_metrics: bool,

    /// Whether segments contain non-canvas element types (InlineBox, EmbedPreview, ReplyIndicator).
    /// When false, render_message_at_position can skip the O(segments) iteration loop entirely.
    /// Most messages are text+images only, so this saves per-message per-frame branching.
    pub has_element_segments: bool,

    /// Media URLs whose provider and remembered dimensions were unavailable while
    /// this layout was built. Once one becomes resident, `is_stale_for_channel`
    /// requests only this layout's normal targeted refresh path.
    pub pending_native_dimension_urls: SmallVec<[SharedString; 4]>,

    /// Channel whose loading badge set had slots reserved from the IRC
    /// `badges` tag; the layout is stale once that set has loaded.
    pub reserved_badge_channel: Option<Arc<str>>,

    /// `message_content_fingerprint` of the message this layout was shaped
    /// from. Cache keys carry only the message id, so a row edited in place
    /// (CLEARMSG placeholder, stacked timeout text, echo reconcile) must not
    /// reuse a layout of its earlier content.
    pub content_fingerprint: u64,
    /// `f32` bits of the device scale factor the layout was built for; it
    /// selects the emote source tier, so windows at other scales rebuild.
    pub scale_bits: u32,
    /// Whether the row was shaped with inline moderation buttons.
    pub mod_buttons: bool,
}

impl CanvasMessageLayout {
    /// Create a new empty layout
    pub fn new(message_id: impl Into<SharedString>, channel: &str) -> Self {
        // Accept already-shared IDs from production messages instead of
        // forcing an Arc<str> -> String -> SharedString copy during every build.
        let message_id_shared = message_id.into();
        let channel_arc = Arc::from(channel);
        Self {
            message_id_shared,
            height: line_height() + MESSAGE_PADDING_VERTICAL * 2.0,
            segments: Vec::new(),
            shaped_lines: Vec::new(),
            text_paint_commands: SmallVec::new(),
            image_paint_commands: Vec::new(),
            hitbox_regions: Vec::new(),
            highlight_type: None,
            low_trust: LowTrustTreatment::None,
            source_text: String::new(),
            source_char_positions: SmallVec::new(),
            mention_highlights: SmallVec::new(),
            line_count: 1,
            has_reply_indicator: false,
            line_y_positions: SmallVec::new(),
            emote_version_at_creation: VersionRegistry::emote_resolution_version(),
            animated_emote_count: 0, // Incremented during layout building
            channel: channel_arc,
            channel_emote_version_at_creation: VersionRegistry::channel_emote_version(channel),
            uses_window_text_metrics: false,
            has_element_segments: false, // Set during layout building when non-canvas segments are added
            pending_native_dimension_urls: SmallVec::new(),
            reserved_badge_channel: None,
            content_fingerprint: 0,
            scale_bits: 0,
            mod_buttons: false,
        }
    }

    /// Whether this layout renders a link to `url`.
    pub fn links_to(&self, url: &str) -> bool {
        let url = url.trim();
        self.hitbox_regions.iter().any(|region| {
            matches!(&region.kind, HitboxKind::Url { url: link, .. } if link.trim() == url)
        })
    }

    /// Source text length in characters (Unicode scalar values).
    ///
    /// Selection indices in this renderer are char-based, not byte-based.
    #[inline]
    pub fn source_char_len(&self) -> usize {
        self.source_char_positions.len().saturating_sub(1)
    }

    /// Mark this layout as created for the current emote versions.
    ///
    /// Use this when a layout is freshly built or intentionally reused as a
    /// temporary fallback. Both global and channel-specific versions must move
    /// together; updating only the global version leaves channel-stale fallbacks
    /// permanently stale.
    #[inline]
    pub fn mark_emote_versions_current(&mut self, emote_resolution_version: u64) {
        self.emote_version_at_creation = emote_resolution_version;
        self.channel_emote_version_at_creation =
            VersionRegistry::channel_emote_version(&self.channel);
    }

    /// Drop the spare capacity of the layout-owning lists. A built layout is
    /// immutable apart from rare in-place edits, and is cached for a long time.
    fn shrink_storage(&mut self) {
        self.segments.shrink_to_fit();
        self.shaped_lines.shrink_to_fit();
        self.image_paint_commands.shrink_to_fit();
        self.hitbox_regions.shrink_to_fit();
    }

    /// Rebuild the cached direct-paint command lists from `segments`, then trim
    /// the layout's storage to size.
    ///
    /// Call this after building or mutating `segments` during layout construction.
    pub fn rebuild_paint_commands(&mut self) {
        self.build_paint_commands();
        self.shrink_storage();
    }

    fn build_paint_commands(&mut self) {
        self.text_paint_commands.clear();
        self.image_paint_commands.clear();

        // Common case: few segments, keep this tight.
        self.text_paint_commands.reserve(self.shaped_lines.len());
        self.image_paint_commands.reserve(self.segments.len());

        for segment in &self.segments {
            match segment {
                LayoutSegment::Text { line_idx, x, y, .. } => {
                    self.text_paint_commands.push(TextPaintCommand {
                        line_idx: *line_idx,
                        x: *x,
                        y: *y,
                    });
                }
                LayoutSegment::Image {
                    url,
                    x,
                    y,
                    width,
                    height,
                    is_animated,
                    image_type,
                    overlays,
                    source_range,
                } => {
                    // SVG images still need the element path for decoding. Animated
                    // raster images can use the decoded-frame direct paint path once
                    // ready, with GPUI elements kept as a cold-cache fallback.
                    let has_element_layer = is_svg_image_url(url.as_ref())
                        || overlays.iter().any(|o| is_svg_image_url(o.url.as_ref()));
                    if has_element_layer {
                        continue;
                    }

                    // Base image.
                    self.image_paint_commands.push(ImagePaintCommand {
                        url: url.clone(),
                        x: *x,
                        y: *y,
                        width: *width,
                        height: *height,
                        is_animated: *is_animated,
                        is_emote: source_range.is_some(),
                        background: match image_type {
                            ImageType::Badge { background, .. } => *background,
                            _ => None,
                        },
                    });

                    // Zero-width overlays fill the base container.
                    for overlay in overlays {
                        self.image_paint_commands.push(ImagePaintCommand {
                            url: overlay.url.clone(),
                            x: *x,
                            y: *y,
                            width: *width,
                            height: *height,
                            is_animated: overlay.is_animated,
                            is_emote: source_range.is_some(),
                            background: None,
                        });
                    }
                }
                LayoutSegment::LayeredImage { layers, .. } => {
                    // SVG layers still need element rendering; animated raster layers
                    // can use decoded-frame direct paint once ready.
                    if layers
                        .iter()
                        .any(|layer| is_svg_image_url(layer.url.as_ref()))
                    {
                        continue;
                    }

                    for layer in layers {
                        self.image_paint_commands.push(ImagePaintCommand {
                            url: layer.url.clone(),
                            x: layer.x,
                            y: layer.y,
                            width: layer.width,
                            height: layer.height,
                            is_animated: layer.is_animated,
                            is_emote: true,
                            background: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Channel-aware staleness check (surgical, not global)
    ///
    /// This method provides targeted invalidation - it only returns true if
    /// emotes for THIS channel have changed, rather than invalidating when
    /// ANY channel's emotes change. This prevents cache thrashing when
    /// viewing multiple channels.
    ///
    /// Ready layouts use version checks only; unresolved native geometry also
    /// probes its short list of pending image URLs.
    #[inline]
    pub fn is_stale_for_channel(&self) -> bool {
        // Check channel-specific version first (most common case for invalidation)
        let current_channel_version = VersionRegistry::channel_emote_version(&self.channel);
        if self.channel_emote_version_at_creation != current_channel_version {
            return true;
        }

        if self
            .pending_native_dimension_urls
            .iter()
            .any(|url| ports::decoded_image_size(url.as_ref()).is_some())
        {
            return true;
        }

        if self
            .reserved_badge_channel
            .as_deref()
            .is_some_and(|channel| !ports::badge_set_pending(channel))
        {
            return true;
        }

        // Global/user retokenization is delivered as per-channel message snapshot
        // replacement. Keeping this check channel-local prevents unrelated splits
        // from reflowing when another split loads channel-specific emotes.
        false
    }

    /// Whether this layout must be rebuilt for a message whose
    /// `message_content_fingerprint` is `content_fingerprint`.
    #[inline]
    pub fn is_stale_for_message(&self, content_fingerprint: u64) -> bool {
        self.content_fingerprint != content_fingerprint || self.is_stale_for_channel()
    }
}

/// Fingerprint of every message field layout shaping reads, so an in-place
/// edit that keeps the message id still misses the layout cache.
pub fn message_content_fingerprint(msg: &echo_core::message::ChatMessage) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    msg.structure_hash.hash(&mut hasher);
    msg.content.hash(&mut hasher);
    msg.sender.hash(&mut hasher);
    msg.display_name.hash(&mut hasher);
    msg.color.hash(&mut hasher);
    msg.badges.hash(&mut hasher);
    msg.timestamp.hash(&mut hasher);
    msg.user_id.hash(&mut hasher);
    msg.twitch_emotes_tag.hash(&mut hasher);
    msg.twitch_gifs_tag.hash(&mut hasher);
    msg.reply_emote_offset.hash(&mut hasher);
    msg.emote_lookup_channel.hash(&mut hasher);
    match &msg.reply {
        Some(reply) => {
            1u8.hash(&mut hasher);
            reply.parent_msg_id.hash(&mut hasher);
            reply.parent_user_login.hash(&mut hasher);
            reply.parent_display_name.hash(&mut hasher);
            reply.parent_user_id.hash(&mut hasher);
            reply.parent_msg_body.hash(&mut hasher);
        }
        None => 0u8.hash(&mut hasher),
    }
    let flags = &msg.flags;
    [
        flags.is_action,
        flags.is_system,
        flags.is_usernotice,
        flags.is_first_message,
        flags.is_shared_chat,
        flags.is_deleted,
        flags.is_elevated_message,
        flags.is_hype_chat,
        flags.is_automod,
        flags.is_automod_pending,
        flags.is_suspicious_user,
        flags.is_restricted,
        flags.is_monitored,
        flags.is_similar,
    ]
    .hash(&mut hasher);
    flags.deleted_presentation.hash(&mut hasher);
    flags.highlight_type.hash(&mut hasher);
    flags.system_type.hash(&mut hasher);
    hasher.finish()
}

// ============================================================================
// Direct Paint Command Types
// ============================================================================

/// Precomputed command to paint a single text segment via the unified canvas.
#[derive(Clone, Debug)]
pub struct TextPaintCommand {
    pub line_idx: usize,
    pub x: f32,
    pub y: f32,
}

/// Precomputed command to paint a single image via the unified canvas.
#[derive(Clone, Debug)]
pub struct ImagePaintCommand {
    pub url: SharedString,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub is_animated: bool,
    /// True for inline/layered emotes; false for badges and other media.
    pub is_emote: bool,
    /// Fill painted behind the image (`ImageType::Badge::background`).
    pub background: Option<Hsla>,
}

/// A single visual segment within the message
#[derive(Clone, Debug)]
pub enum LayoutSegment {
    /// Shaped text span at position
    Text {
        /// Index into shaped_lines Vec
        line_idx: usize,
        /// X position relative to message origin
        x: f32,
        /// Y position relative to message origin (line * LINE_HEIGHT)
        y: f32,
        /// Width of this text segment
        width: f32,
        /// Height of this text segment
        height: f32,
    },

    /// Emote/badge image at position
    Image {
        /// Image URL
        url: SharedString,
        /// X position
        x: f32,
        /// Y position
        y: f32,
        /// Display width
        width: f32,
        /// Display height
        height: f32,
        /// Whether this is an animated image (GIF, WebP)
        is_animated: bool,
        /// Image type for tooltip
        image_type: ImageType,
        /// Overlay emotes (zero-width emotes stacked on this one)
        overlays: Vec<OverlayEmote>,
        /// Char range `(source_start_char, source_len_chars)` this image occupies
        /// in `CanvasMessageLayout::source_text`, for drag-selection/copy. `None`
        /// for badges, which are never selectable/copyable text.
        source_range: Option<(usize, usize)>,
    },

    /// Layered image with centered overlays
    ///
    /// This variant represents a base emote with zero-width overlays, where all layers
    /// are centered within a computed bounding box. Unlike the simple `Image` variant,
    /// this properly handles variable emote sizes (1x, 2x, 3x, 4x) and centers overlays.
    LayeredImage {
        /// All positioned layers (base + overlays) with absolute coordinates.
        /// Boxed: an inline array of 8 layers made every segment variant 688 B.
        layers: Box<[PositionedLayer]>,
        /// Bounding box X position
        x: f32,
        /// Bounding box Y position (bottom-aligned within line height)
        y: f32,
        /// Bounding box width (max of all layers)
        width: f32,
        /// Bounding box height (max of all layers)
        height: f32,
        /// Start char index of the base emote's text code within
        /// `CanvasMessageLayout::source_text` (drag-selection/copy support).
        /// Every layered emote has a code, so this is unconditional (unlike
        /// `Image::source_range`).
        source_start_char: usize,
        /// Number of chars the base emote's code occupies in `source_text`,
        /// starting at `source_start_char`. Excludes any synthesized separator
        /// space inserted before the code.
        source_len_chars: usize,
    },

    /// Inline box (NEW badge, returning chatter badge)
    InlineBox {
        /// Text content
        text: SharedString,
        /// X position
        x: f32,
        /// Y position
        y: f32,
        /// Box width
        width: f32,
        /// Box height
        height: f32,
        /// Background color
        bg_color: Hsla,
        /// Text color
        text_color: Hsla,
        /// Font size (defaults to text_xs)
        font_size: f32,
    },

    /// Embed preview card for URLs (YouTube, Twitch, direct media, etc.)
    EmbedPreview {
        /// URL being embedded
        url: SharedString,
        /// Platform type (youtube, twitch, direct video, twitter, reddit, image)
        platform: EmbedPlatform,
        /// Thumbnail URL (if available)
        thumbnail_url: Option<SharedString>,
        /// Title text
        title: Option<SharedString>,
        /// Description, distinct from the provider/site subtitle
        description: Option<SharedString>,
        /// Subtitle (channel name, author, subreddit)
        subtitle: Option<SharedString>,
        /// Duration string for videos ("3:45")
        duration: Option<SharedString>,
        /// X position
        x: f32,
        /// Y position
        y: f32,
        /// Preview width
        width: f32,
        /// Preview height
        height: f32,
        /// Shaping-owned thumbnail and text geometry
        geometry: EmbedPreviewGeometry,
        /// Whether expanded (full preview) or collapsed (compact)
        is_expanded: bool,
        /// Whether metadata is still loading
        is_loading: bool,
    },

    /// Compact Echo-upload preview, separate from third-party rich embeds.
    ///
    /// The source URL remains represented by its normal inline URL segment;
    /// this element only provides consistent upload feedback and never fetches
    /// a deletion URL.
    UploadPreview {
        /// Direct uploaded-file URL used for the optional thumbnail.
        url: SharedString,
        /// Present only when Echo recorded an image MIME type for this upload.
        thumbnail_url: Option<SharedString>,
        /// Aspect-ratio-derived width inside the bounded upload row.
        thumbnail_width: f32,
        /// X position.
        x: f32,
        /// Y position.
        y: f32,
        /// Preview width.
        width: f32,
        /// Preview height. The thumbnail is contained within this fixed row.
        height: f32,
    },

    /// Reply indicator row (L-shaped connector + @user + preview)
    ReplyIndicator {
        /// X position of connector line start
        connector_x: f32,
        /// Y position of connector top
        connector_y: f32,
        /// Height of vertical connector portion
        connector_height: f32,
        /// Parent username to display
        parent_display_name: SharedString,
        /// Parent username color
        parent_color: Hsla,
        /// Truncated parent message preview
        preview_text: SharedString,
        /// X position where @username text starts
        text_x: f32,
        /// Total width of the indicator row
        width: f32,
    },
}

/// Platform type for embed previews
#[derive(Clone, Debug, PartialEq)]
pub enum EmbedPlatform {
    /// 7TV emote or user page
    SevenTv,
    /// Spotify music or podcast resource
    Spotify,
    /// Steam Store, SteamDB, or ProtonDB application page
    Steam,
    /// YouTube video
    YouTube,
    /// Twitch clip
    TwitchClip,
    /// Twitch VOD
    TwitchVod,
    /// Twitch channel
    TwitchChannel,
    /// Twitter/X post
    Twitter,
    /// Reddit post
    Reddit,
    /// Direct image URL
    Image,
    /// Direct or Open Graph video, represented only by static metadata/poster.
    Video,
    /// Generic/unknown link
    Generic,
}

/// Type of image (for determining tooltip content)
#[derive(Clone, Debug, PartialEq)]
pub enum ImageType {
    TwitchGif {
        alt_text: SharedString,
    },
    Emote {
        name: SharedString,
        tooltip_meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
    },
    Badge {
        name: SharedString,
        description: Option<SharedString>,
        tooltip_meta: Option<std::sync::Arc<BadgeTooltipMeta>>,
        /// Fill painted behind the image, for artwork drawn on a transparent
        /// background (FFZ custom moderator badges).
        background: Option<Hsla>,
    },
}

/// Zero-width emote overlay (stacked on top of a base emote)
#[derive(Clone, Debug)]
pub struct OverlayEmote {
    pub url: SharedString,
    pub name: SharedString,
    pub is_animated: bool,
    pub tooltip_meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
}

/// Rich emote tooltip metadata (provider/source/channel details).
#[derive(Clone, Debug, PartialEq)]
pub struct EmoteTooltipMeta {
    pub provider_label: SharedString,
    pub source_label: SharedString,
    pub owner_channel: Option<SharedString>,
    pub creator_name: Option<SharedString>,
    pub original_name: Option<SharedString>,
    pub tooltip_preview_url: Option<SharedString>,
    pub native_width: Option<u32>,
    pub native_height: Option<u32>,
    /// The source is animated, whether or not it plays here.
    pub is_animated: bool,
    pub is_zero_width: bool,
    /// Provider id and persisted provider key ("Twitch", "SevenTV", ...) of the
    /// rendered emote, so menus act on this exact emote rather than its name.
    pub emote_id: SharedString,
    pub provider_key: &'static str,
    /// A display rule shows this animated emote's first frame.
    pub still_image: bool,
}

impl EmoteTooltipMeta {
    /// Whether previews of this emote should play.
    #[inline]
    pub fn animates(&self) -> bool {
        self.is_animated && !self.still_image
    }
}

/// A single emote entry shown in a layered-emote tooltip.
#[derive(Clone, Debug, PartialEq)]
pub struct LayeredTooltipEmote {
    pub name: SharedString,
    pub url: SharedString,
    pub meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
}

/// Rich badge tooltip metadata (type/source/tier details).
#[derive(Clone, Debug, PartialEq)]
pub struct BadgeTooltipMeta {
    pub source_label: SharedString,
    pub type_label: SharedString,
    pub tier_label: Option<SharedString>,
    pub is_animated: bool,
}

// ============================================================================
// Layered Emote System
// ============================================================================

/// A single layer in a layered emote (base or overlay)
#[derive(Clone, Debug)]
pub struct EmoteLayer {
    pub url: SharedString,
    pub name: SharedString,
    pub width: f32,
    pub height: f32,
    pub is_animated: bool,
    pub tooltip_meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
    /// X offset from layered emote origin (for centering within bounding box)
    pub offset_x: f32,
    /// Y offset from layered emote origin (for centering within bounding box)
    pub offset_y: f32,
}

/// A positioned layer in the final layout (after centering calculations)
#[derive(Clone, Debug)]
pub struct PositionedLayer {
    pub url: SharedString,
    pub name: SharedString,
    pub x: f32, // Absolute X position in layout
    pub y: f32, // Absolute Y position in layout
    pub width: f32,
    pub height: f32,
    pub is_animated: bool,
    pub tooltip_meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
}

/// A layered emote with base + overlays
///
/// Layered emote positioning:
/// 1. Calculate bounding box from all layers (base + overlays)
/// 2. Center each layer horizontally and bottom-align it vertically in that box
/// 3. Store computed offsets for rendering
///
/// Example: Wide emote (60x30) with overlay (20x20)
/// - Bounding box: 60x30 (max of all dimensions)
/// - Base offset: (0, 0) - already fills bounding box
/// - Overlay offset: (20, 10) - horizontally centered and bottom-aligned
#[derive(Clone, Debug)]
pub struct LayeredEmote {
    /// Base emote (always present)
    pub base: EmoteLayer,
    /// Zero-width overlay emotes
    pub overlays: SmallVec<[EmoteLayer; 4]>,
    /// Computed bounding box (lazy - calculated when first needed)
    bounds: Option<(f32, f32)>,
}

impl LayeredEmote {
    /// Create a new layered emote with just a base layer
    pub fn new(
        url: SharedString,
        name: SharedString,
        width: f32,
        height: f32,
        is_animated: bool,
        tooltip_meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
    ) -> Self {
        Self {
            base: EmoteLayer {
                url,
                name,
                width,
                height,
                is_animated,
                tooltip_meta,
                offset_x: 0.0,
                offset_y: 0.0,
            },
            overlays: SmallVec::new(),
            bounds: None,
        }
    }

    /// Add an overlay emote (zero-width)
    pub fn add_overlay(
        &mut self,
        url: SharedString,
        name: SharedString,
        width: f32,
        height: f32,
        is_animated: bool,
        tooltip_meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
    ) {
        self.overlays.push(EmoteLayer {
            url,
            name,
            width,
            height,
            is_animated,
            tooltip_meta,
            offset_x: 0.0, // Will be computed by compute_centered_layout()
            offset_y: 0.0,
        });
        // Invalidate bounds to force recomputation
        self.bounds = None;
    }

    /// Compute bounding box and position all layers
    fn compute_centered_layout(&mut self) {
        // Find bounding box (max width and height of all layers)
        let mut max_width = self.base.width;
        let mut max_height = self.base.height;

        for overlay in &self.overlays {
            max_width = max_width.max(overlay.width);
            max_height = max_height.max(overlay.height);
        }

        self.bounds = Some((max_width, max_height));

        // Center layers horizontally and bottom-align them vertically within the bounding box.
        // Bottom alignment keeps small zero-width overlays attached to the emote
        // baseline instead of floating in the middle.
        self.base.offset_x = (max_width - self.base.width) / 2.0;
        self.base.offset_y = max_height - self.base.height;

        // Position each overlay within bounding box
        for overlay in &mut self.overlays {
            overlay.offset_x = (max_width - overlay.width) / 2.0;
            overlay.offset_y = max_height - overlay.height;
        }
    }

    /// Get bounding box dimensions (lazy computation)
    ///
    /// Computes centered layout on first call, caches result
    pub fn bounds(&mut self) -> (f32, f32) {
        if self.bounds.is_none() {
            self.compute_centered_layout();
        }
        self.bounds.unwrap_or((0.0, 0.0))
    }

    /// Convert to positioned layers for final layout
    ///
    /// Takes the bounding box position and converts all layers to absolute positions
    pub fn into_positioned_layers(mut self, bbox_x: f32, bbox_y: f32) -> Box<[PositionedLayer]> {
        // Ensure layout is computed
        if self.bounds.is_none() {
            self.compute_centered_layout();
        }

        let mut layers = Vec::with_capacity(1 + self.overlays.len());

        // Add base layer
        layers.push(PositionedLayer {
            url: self.base.url,
            name: self.base.name,
            x: bbox_x + self.base.offset_x,
            y: bbox_y + self.base.offset_y,
            width: self.base.width,
            height: self.base.height,
            is_animated: self.base.is_animated,
            tooltip_meta: self.base.tooltip_meta,
        });

        // Add overlay layers
        for overlay in self.overlays {
            layers.push(PositionedLayer {
                url: overlay.url,
                name: overlay.name,
                x: bbox_x + overlay.offset_x,
                y: bbox_y + overlay.offset_y,
                width: overlay.width,
                height: overlay.height,
                is_animated: overlay.is_animated,
                tooltip_meta: overlay.tooltip_meta,
            });
        }

        layers.into_boxed_slice()
    }
}

/// Pre-shaped line data
#[derive(Clone, Debug)]
pub struct ShapedLineData {
    /// The raw text content
    pub text: SharedString,

    /// GPUI-ready styling runs, used for painting and element rendering.
    ///
    /// Avoids rebuilding `TextRun` structs every paint.
    pub gpui_runs: SmallVec<[TextRun; 4]>,
    /// Stable hash of GPUI text runs for direct-paint shaped-line cache keys.
    ///
    /// Computing this once at layout time avoids rebuilding style keys on
    /// each animation repaint.
    pub gpui_runs_hash: u64,

    /// Character X positions in logical source order.
    ///
    /// Inline cap of 24: typical chat messages average ~20 chars per wrapped line
    /// (measured at 400-800 px container with default font scale). Lines longer than
    /// 24 chars spill to heap, but that is rare enough to be cheaper than carrying
    /// 64×4 = 256 B of stack per line at all times. At ~2-4 shaped lines per message
    /// and ~8 000 cached messages this saves ~1.6–3.2 MB of inline footprint.
    pub char_positions: SmallVec<[f32; 24]>,

    /// Character X positions in visual display order used for hit testing.
    /// Length should match `source_len_chars + 1` when available.
    ///
    /// ONLY populated for BiDi/RTL text by text_shaping.rs; empty for pure-LTR (the
    /// common case). Stored as plain Vec so an empty value costs 24 B (3 words) on
    /// the stack and allocates nothing, versus 256 B of wasted inline SmallVec space.
    /// hit_test_char_positions() and logical_index_from_visual_boundary() both
    /// length-check and fall back to char_positions / identity when this is empty,
    /// so behavior is identical to the old SmallVec<[f32; 64]>.
    pub visual_char_positions: Vec<f32>,

    /// Visual-order to logical-order mapping for per-character hit testing.
    /// visual_to_logical[visual_idx] = logical_idx.
    ///
    /// ONLY populated for BiDi/RTL text by text_shaping.rs; empty for pure-LTR.
    /// Plain Vec for the same reason as visual_char_positions: 24 B stack, zero heap
    /// when empty. Saves 512 B of SmallVec<[usize; 64]> inline per shaped line for
    /// all LTR messages (~99 % of traffic). logical_index_from_visual_boundary()
    /// falls back to the identity mapping when this Vec is empty.
    pub visual_to_logical: Vec<usize>,

    /// Global (message) character offset where this shaped text begins.
    ///
    /// Units are Unicode scalar values (Rust `char` count), not UTF-8 bytes.
    pub source_start_char: usize,

    /// Length of this shaped text in characters (Unicode scalar values).
    pub source_len_chars: usize,

    /// Exact 7TV name-paint spec for the username glyphs inside this line.
    /// Composited and painted over the base run at paint time
    /// (see `seventv_paint::overlay_for_line`).
    pub seventv_paint: Option<std::sync::Arc<super::seventv_paint::SevenTvLinePaint>>,
}

impl Default for ShapedLineData {
    fn default() -> Self {
        Self {
            text: SharedString::default(),
            gpui_runs: SmallVec::new(),
            gpui_runs_hash: 0,
            char_positions: SmallVec::new(),
            visual_char_positions: Vec::new(),
            visual_to_logical: Vec::new(),
            source_start_char: 0,
            source_len_chars: 0,
            seventv_paint: None,
        }
    }
}

impl ShapedLineData {
    /// Select the position array used for hit testing.
    #[inline]
    pub fn hit_test_char_positions(&self) -> &[f32] {
        if self.visual_char_positions.len() == self.source_len_chars.saturating_add(1) {
            &self.visual_char_positions
        } else {
            &self.char_positions
        }
    }

    /// Map a visual boundary index to the logical caret index at that
    /// boundary. The caret before a left-to-right character is its own index;
    /// a right-to-left character has its logical start on its right edge, so
    /// the caret on its left edge is the index after it.
    #[inline]
    pub fn logical_index_from_visual_boundary(&self, visual_boundary_idx: usize) -> usize {
        let len = self.source_len_chars;
        let boundary = visual_boundary_idx.min(len);
        if self.visual_to_logical.len() != len
            || self.visual_char_positions.len() != len + 1
            || len == 0
        {
            return boundary;
        }
        let logical = if boundary < len {
            let start = self.visual_to_logical[boundary];
            if self.visual_char_is_rtl(boundary) {
                start + 1
            } else {
                start
            }
        } else {
            let start = self.visual_to_logical[len - 1];
            if self.visual_char_is_rtl(len - 1) {
                start
            } else {
                start + 1
            }
        };
        logical.min(len)
    }

    /// Whether the character at `visual_idx` sits in a right-to-left run, read
    /// off the mapping: logical indices fall from left to right inside one. A
    /// run of a single character has no neighbour to compare with and counts
    /// as left-to-right, which selects the same character either way.
    fn visual_char_is_rtl(&self, visual_idx: usize) -> bool {
        let logical = self.visual_to_logical[visual_idx];
        let next_is_previous_char = self
            .visual_to_logical
            .get(visual_idx + 1)
            .is_some_and(|next| next + 1 == logical);
        let previous_is_next_char = visual_idx
            .checked_sub(1)
            .is_some_and(|previous| self.visual_to_logical[previous] == logical + 1);
        next_is_previous_char || previous_is_next_char
    }
}

/// A text run with styling information
#[derive(Clone, Debug)]
pub struct TextRunData {
    /// Length in UTF-8 bytes
    pub len: usize,
    /// Font family
    pub font_family: SharedString,
    /// Font weight
    pub font_weight: FontWeight,
    /// Text color
    pub color: Hsla,
    /// Background color (for mentions, highlights)
    pub background_color: Option<Hsla>,
    /// Underline style (for URLs)
    pub underline: bool,
    /// Strikethrough style (for deleted message content)
    pub strikethrough: bool,
}

impl TextRunData {
    /// Create a text run with default font
    pub fn new(len: usize, color: Hsla) -> Self {
        Self {
            len,
            font_family: super::text_run_intern::system_font_family(),
            font_weight: FontWeight::NORMAL,
            color,
            background_color: None,
            underline: false,
            strikethrough: false,
        }
    }

    /// Set font weight
    pub fn bold(mut self) -> Self {
        self.font_weight = FontWeight::SEMIBOLD;
        self
    }

    /// Override font weight explicitly.
    pub fn with_font_weight(mut self, font_weight: FontWeight) -> Self {
        self.font_weight = font_weight;
        self
    }

    /// Set underline
    pub fn underlined(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Set strikethrough
    pub fn struck(mut self) -> Self {
        self.strikethrough = true;
        self
    }
}

// ============================================================================
// Hitbox System
// ============================================================================

/// Region for hit testing and interaction
#[derive(Clone, Debug)]
pub struct HitboxRegion {
    /// Bounds relative to message origin
    pub bounds: Bounds<Pixels>,
    /// Type of interactive element
    pub kind: HitboxKind,
}

/// Types of interactive elements
#[derive(Clone, Debug, PartialEq)]
pub enum HitboxKind {
    /// Emote - shows enlarged preview on hover
    Emote {
        name: SharedString,
        url: SharedString,
        meta: Option<std::sync::Arc<EmoteTooltipMeta>>,
    },
    /// Layered emotes - shows all emotes in stack (base + overlays)
    LayeredEmote {
        /// Tooltip entries for all emotes in stack (base + overlays)
        emotes: Vec<LayeredTooltipEmote>,
    },
    /// Badge - shows badge name and description
    Badge {
        name: SharedString,
        url: SharedString,
        description: Option<SharedString>,
        meta: Option<std::sync::Arc<BadgeTooltipMeta>>,
    },
    /// Username - shows user card on hover/click
    Username {
        login: SharedString,
        display_name: SharedString,
        user_id: Option<SharedString>,
        color: Option<Hsla>,
    },
    /// URL - shows full URL on hover
    Url {
        url: SharedString,
        display_text: SharedString,
        embed_id: u32,
    },
    /// Mention - shows user card
    Mention { username: SharedString },
    /// Message background - for context menu
    MessageBackground { message_id: SharedString },
    /// Unicode emoji - shows emoji tooltip
    Emoji { emoji: SharedString },
    /// Embed preview - click to expand/collapse or open
    EmbedPreview {
        url: SharedString,
        embed_id: u32,
        is_expanded: bool,
    },
    /// Image-only subregion of an embed preview. It keeps the original card
    /// URL for the context menu while carrying the classified media source
    /// for Echo's native inspector.
    MediaInspector {
        source_url: SharedString,
        url: SharedString,
        embed_id: u32,
    },
    /// Reply indicator - click to scroll to parent, right-click for thread viewer
    ReplyIndicator {
        parent_msg_id: SharedString,
        parent_display_name: SharedString,
        parent_preview: Option<SharedString>,
    },
    /// Inline AutoMod Allow/Deny action on a held-message row. The held message
    /// id is carried by the hitbox's `message_id` (the row id equals it).
    AutomodAction { allow: bool },
    /// Inline moderation button shown in moderation mode; `command` is the
    /// configured template, resolved against the row when clicked.
    ModAction {
        label: SharedString,
        command: SharedString,
    },
}

// ============================================================================
// URL Detection
// ============================================================================

/// Check if URL is for an animated image
///
/// Detection heuristics:
/// - .gif extension: Always animated
/// - WebP from 7TV: Likely animated (7TV serves WebP for animated emotes)
/// - /animated/ or _animated. in path: Explicit animated marker
/// - WebP from BTTV: May be animated (BTTV uses WebP for both)
/// - Twitch emote CDN: May return animated GIFs (no extension in URL)
///
/// Uses case-insensitive comparisons without String allocation
pub fn is_animated_url(url: &str) -> bool {
    // GIFs are always animated
    if ends_with_ignore_case(url, ".gif") {
        return true;
    }

    // Explicit animated markers in URL (case-insensitive)
    if contains_ignore_case(url, "/animated/") || contains_ignore_case(url, "_animated.") {
        return true;
    }

    // Twitch emote CDN - only /animated/ path serves animated GIF format
    // /default/ path always returns static PNG even for animated emotes
    if contains_ignore_case(url, "static-cdn.jtvnw.net/emoticons/v2/")
        && contains_ignore_case(url, "/animated/")
    {
        return true;
    }

    // WebP CAN be animated but we don't blanket-assume it here.
    // Both 7TV and BTTV provide accurate `is_animated` flags in their API data,
    // so we trust emote.is_animated rather than guessing from URL.
    // This heuristic is only for cases where the flag is missing/wrong.

    // AVIF can theoretically be animated, but Echo deliberately does not
    // negotiate AVIF until a secure portable decoder is available. Providers
    // are resolved to WebP/PNG before this animation heuristic is reached.

    false
}

/// Check if URL points at an SVG image.
///
/// SVG badges must use GPUI's element image path; the direct canvas image cache
/// decodes raster formats and would otherwise show a placeholder forever.
#[inline]
pub fn is_svg_image_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.get(path.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".svg"))
}

/// Case-insensitive ends_with check (ASCII only, no allocation)
#[inline]
fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len()
        && s.as_bytes()[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// Case-insensitive contains check (ASCII only, no allocation)
#[inline]
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    /// Thousands of layouts stay cached, so their inline size is a RAM budget.
    /// Before the layout lists moved to exact-size `Vec`s these were 12,096,
    /// 688 and 1,120 bytes.
    #[test]
    fn cached_layout_types_stay_compact() {
        use std::mem::size_of;
        let layout = size_of::<super::CanvasMessageLayout>();
        let segment = size_of::<super::LayoutSegment>();
        let line = size_of::<super::ShapedLineData>();
        eprintln!("CanvasMessageLayout={layout} LayoutSegment={segment} ShapedLineData={line}");
        assert!(layout <= 1_024, "CanvasMessageLayout is {layout} bytes");
        assert!(segment <= 256, "LayoutSegment is {segment} bytes");
        assert!(line <= 832, "ShapedLineData is {line} bytes");
    }
    use super::*;

    #[test]
    fn layered_emote_layers_are_bottom_centered() {
        let mut emote = LayeredEmote::new("base".into(), "base".into(), 24.0, 24.0, false, None);
        emote.add_overlay("overlay".into(), "overlay".into(), 12.0, 12.0, false, None);

        let layers = emote.into_positioned_layers(100.0, 200.0);

        assert_eq!(layers.len(), 2);
        assert!((layers[0].x - 100.0).abs() < 0.001);
        assert!((layers[0].y - 200.0).abs() < 0.001);
        assert!((layers[1].x - 106.0).abs() < 0.001);
        assert!((layers[1].y - 212.0).abs() < 0.001);
    }

    #[test]
    fn emote_dimensions_scale_logical_native_geometry() {
        let (wide_w, wide_h) = calculate_emote_dimensions(Some(168), Some(28), 28.0);
        assert!((wide_w - 168.0).abs() < 0.001);
        assert!((wide_h - 28.0).abs() < 0.001);

        let (tall_w, tall_h) = calculate_emote_dimensions(Some(28), Some(56), 42.0);
        assert!((tall_w - 42.0).abs() < 0.001);
        assert!((tall_h - 84.0).abs() < 0.001);
    }

    #[test]
    fn content_fingerprint_tracks_in_place_edits_but_not_row_parity() {
        let message = echo_core::message::ChatMessage {
            id: Arc::from("row"),
            content: Arc::from("original text"),
            structure_hash: 7,
            ..Default::default()
        };
        let original = message_content_fingerprint(&message);
        assert_eq!(original, message_content_fingerprint(&message.clone()));

        let mut parity = message.clone();
        parity.flags.is_alternate_row = !parity.flags.is_alternate_row;
        assert_eq!(original, message_content_fingerprint(&parity));

        let mut placeholder = message.clone();
        placeholder.content = Arc::from("<message deleted>");
        placeholder.structure_hash = 8;
        assert_ne!(original, message_content_fingerprint(&placeholder));

        let mut struck = message.clone();
        struck.flags.is_deleted = true;
        assert_ne!(original, message_content_fingerprint(&struck));

        // A split's presentation of the same deleted row must not share a layout.
        let mut collapsed = struck.clone();
        collapsed.flags.deleted_presentation = echo_core::message::DeletedPresentation::Collapsed;
        assert_ne!(
            message_content_fingerprint(&struck),
            message_content_fingerprint(&collapsed)
        );

        let mut highlighted = message;
        highlighted.flags.highlight_type = Some(echo_core::message::HighlightType::Mention);
        assert_ne!(original, message_content_fingerprint(&highlighted));

        let mut layout = CanvasMessageLayout::new("row", "channel");
        layout.content_fingerprint = original;
        assert!(layout.is_stale_for_message(original ^ 1));
    }

    #[test]
    fn animated_url_suffix_check_does_not_split_characters() {
        assert!(!is_animated_url("https://example.com/\u{20ac}\u{20ac}"));
        assert!(is_animated_url("https://example.com/\u{20ac}.GIF"));
    }
}
