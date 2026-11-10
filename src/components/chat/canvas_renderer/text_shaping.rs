//! Text shaping and layout building
//!
//! Converts message tokens into pre-shaped text lines and layout segments.
//! Text around emotes is split into separate runs with appropriate styling.
//! Emoji characters are rendered as images using the selected style (Twitter, Apple, etc).

use gpui::*;
use smallvec::SmallVec;
use unicode_linebreak::{BreakOpportunity, linebreaks};
use unicode_segmentation::UnicodeSegmentation;

use crate::backend::emoji::{TextSegment, parse_emoji};
use crate::backend::emotes::chat_display_url_shared_for_scale;
use crate::backend::irc::{get_user_color, resolve_user_color};
use crate::backend::settings;
use crate::backend::slots::render_units::{EmoteRenderUnit, SlotRenderUnit, resolve_render_units};
use crate::backend::slots::resolver::SlotResolver;
use crate::backend::{ChatMessage, ReplyInfo};
use crate::theme::colors;
use echo_chat_render::native_emote_dimensions::logical_native_dimensions;
use echo_emote_core::EmoteProvider;

use super::bidi::BidiAnalysis;
use super::font_metrics::{FONT_METRICS, scaled_font_size};
use super::layout::{
    BadgeTooltipMeta, CanvasMessageLayout, EMBED_ACCENT_WIDTH, EMBED_MIN_HEIGHT, EMBED_PADDING,
    EMBED_TEXT_THUMBNAIL_GAP, EMBED_TITLE_DESC_GAP, EmbedPreviewGeometry, EmoteTooltipMeta,
    HighlightType, HitboxKind, HitboxRegion, ImageType, LayeredEmote, LayeredTooltipEmote,
    LayoutSegment, MESSAGE_PADDING, MESSAGE_PADDING_LEFT, MESSAGE_PADDING_RIGHT,
    REPLY_CONNECTOR_ARM, REPLY_CONNECTOR_GAP, REPLY_CONNECTOR_LEFT, REPLY_PREVIEW_MAX_CHARS,
    ShapedLineData, TIMESTAMP_GAP, TextRunData, badge_gap, badge_margin_right, badge_size,
    calculate_emote_dimensions, emoji_size, emote_size, get_cached_color, get_cached_timestamp,
    hash_gpui_text_runs, is_animated_url, is_svg_image_url, line_height, reply_indicator_height,
    timestamp_width,
};

/// Content width a wrapped line must keep to the right of its indent.
///
/// The wrapped-line indent comes from the message header (timestamp column, or
/// badges + username), which does not shrink with the pane. Without a floor the
/// indent can pass the right boundary in a narrow pane and every grapheme wraps
/// onto its own line. Roughly two average glyphs.
const MIN_WRAPPED_CONTENT_WIDTH: f32 = 24.0;

/// A row layout must leave enough room for a short, useful run of card text.
/// Otherwise the thumbnail moves above the text instead of squeezing it into a
/// column of one-word lines.
const EMBED_MIN_HORIZONTAL_TEXT_WIDTH: f32 = 120.0;
const EMBED_TEXT_ONLY_MIN_HEIGHT: f32 = 76.0;
const EMBED_PLATFORM_LINE_HEIGHT: f32 = 14.0;
const EMBED_TITLE_LINE_HEIGHT: f32 = 18.0;
const EMBED_BODY_LINE_HEIGHT: f32 = 16.0;
const EMBED_TITLE_MAX_LINES: u8 = 2;
const EMBED_BODY_MAX_LINES: u8 = 3;
use super::line_buffer::LineBuffer;
use super::seventv_paint::SevenTvLinePaint;
use super::text_run_intern;

#[cfg(test)]
use crate::backend::slots::MessageSlot;

use crate::backend;
use dashmap::DashMap;
use std::{cell::Cell, sync::LazyLock, time::Instant};

/// Badge URL cache keyed by (badge_key, channel_id) hash
/// PERF: Uses u64 hash key to avoid string allocation on every lookup
/// Memory optimization: Limited to 1000 entries to prevent unbounded growth
static BADGE_URL_CACHE: LazyLock<DashMap<u64, Option<String>>> =
    LazyLock::new(|| DashMap::with_capacity(500));
static BADGE_INFO_CACHE: LazyLock<DashMap<u64, std::sync::Arc<BadgeRenderInfo>>> =
    LazyLock::new(|| DashMap::with_capacity(500));

struct BadgeRenderInfo {
    name: SharedString,
    description: Option<SharedString>,
    tooltip_meta: std::sync::Arc<BadgeTooltipMeta>,
    background: Option<Hsla>,
}

/// Backing color for FFZ custom moderator badges, whose artwork is drawn on
/// a transparent background.
const FFZ_MOD_BADGE_BACKGROUND: u32 = 0x34AE0A;

const LAYOUT_TRACE_SLOW_MS: f64 = 8.0;

/// Content opacity applied to messages flagged as similar/repeated: such rows
/// dim toward the background instead of being highlighted.
const SIMILAR_MESSAGE_DIM_OPACITY: f32 = 0.55;

/// Fixed height of a derived day-divider row. Independent of chat density and
/// font scale: the row is a separator, not a message.
const DAY_DIVIDER_ROW_HEIGHT: f32 = 28.0;
/// Size of the centered ISO date label.
const DAY_DIVIDER_LABEL_FONT_SIZE: f32 = 11.0;
/// Height of the label box; the label is centered inside it.
const DAY_DIVIDER_LABEL_HEIGHT: f32 = 16.0;
/// Thickness of the flexible rules flanking the label.
const DAY_DIVIDER_RULE_HEIGHT: f32 = 1.0;
/// Gap between the label box and each rule.
const DAY_DIVIDER_LABEL_GAP: f32 = 8.0;
/// Horizontal breathing room inside the label box.
const DAY_DIVIDER_LABEL_PADDING_X: f32 = 4.0;
/// Shortest rule still worth painting in a very narrow split.
const DAY_DIVIDER_MIN_RULE_WIDTH: f32 = 2.0;
/// Fallback advance ratio for the fixed-width ISO digits when a background
/// layout worker shapes without a window.
const DAY_DIVIDER_FALLBACK_ADVANCE_RATIO: f32 = 0.62;

static LAYOUT_TRACE_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("ECHO_LAYOUT_TRACE").is_some()
        || std::env::var_os("ECHO_LAYOUT_TRACE_VERBOSE").is_some()
});
static LAYOUT_TRACE_VERBOSE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("ECHO_LAYOUT_TRACE_VERBOSE").is_some());

thread_local! {
    static LAYOUT_BUILD_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct LayoutTraceGuard {
    enabled: bool,
    verbose: bool,
    depth: usize,
    started_at: Instant,
    message_id: String,
    channel_id: String,
    finished: bool,
}

impl LayoutTraceGuard {
    fn enter(
        msg: &ChatMessage,
        channel_id: &str,
        container_width: f32,
        show_timestamp: bool,
        uses_window_text_metrics: bool,
    ) -> Self {
        let enabled = *LAYOUT_TRACE_ENABLED;
        if !enabled {
            return Self {
                enabled,
                verbose: false,
                depth: 0,
                started_at: Instant::now(),
                message_id: String::new(),
                channel_id: String::new(),
                finished: true,
            };
        }

        let depth = LAYOUT_BUILD_DEPTH.with(|depth| {
            let next = depth.get() + 1;
            depth.set(next);
            next
        });
        let verbose = *LAYOUT_TRACE_VERBOSE_ENABLED;
        let guard = Self {
            enabled,
            verbose,
            depth,
            started_at: Instant::now(),
            message_id: msg.id.to_string(),
            channel_id: channel_id.to_string(),
            finished: false,
        };

        if verbose {
            let content_chars = msg.content.chars().count();
            crate::log_perf_render!(
                debug,
                "layout.build.start depth={} msg={} channel={} slots={} chars={} reply={} width={:.1} timestamp={} window_metrics={}",
                depth,
                msg.id,
                channel_id,
                msg.slots.len(),
                content_chars,
                msg.reply.is_some(),
                container_width,
                show_timestamp,
                uses_window_text_metrics
            );
        }
        if depth > 1 {
            crate::log_perf_render!(
                warn,
                "layout.build.reentrant depth={} msg={} channel={}",
                depth,
                msg.id,
                channel_id
            );
        }

        guard
    }

    fn finish(&mut self, layout: &CanvasMessageLayout) {
        if !self.enabled || self.finished {
            return;
        }

        self.finished = true;
        let elapsed_ms = self.started_at.elapsed().as_secs_f64() * 1000.0;
        if self.verbose {
            crate::log_perf_render!(
                debug,
                "layout.build.finish depth={} msg={} channel={} elapsed_ms={:.2} height={:.1} lines={} segments={} shaped_lines={} hitboxes={} animated={}",
                self.depth,
                self.message_id,
                self.channel_id,
                elapsed_ms,
                layout.height,
                layout.line_count,
                layout.segments.len(),
                layout.shaped_lines.len(),
                layout.hitbox_regions.len(),
                layout.animated_emote_count
            );
        } else if elapsed_ms >= LAYOUT_TRACE_SLOW_MS {
            crate::log_perf_render!(
                warn,
                "layout.build.slow depth={} msg={} channel={} elapsed_ms={:.2} height={:.1} lines={} segments={} shaped_lines={} hitboxes={} animated={}",
                self.depth,
                self.message_id,
                self.channel_id,
                elapsed_ms,
                layout.height,
                layout.line_count,
                layout.segments.len(),
                layout.shaped_lines.len(),
                layout.hitbox_regions.len(),
                layout.animated_emote_count
            );
        }
    }
}

impl Drop for LayoutTraceGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }

        if !self.finished {
            crate::log_perf_render!(
                error,
                "layout.build.unfinished depth={} msg={} channel={} elapsed_ms={:.2}",
                self.depth,
                self.message_id,
                self.channel_id,
                self.started_at.elapsed().as_secs_f64() * 1000.0
            );
        }

        LAYOUT_BUILD_DEPTH.with(|depth| {
            depth.set(depth.get().saturating_sub(1));
        });
    }
}

/// Maximum number of badge URLs to cache (prevents memory leak)
const BADGE_CACHE_MAX_SIZE: usize = 1000;

/// Compute hash key for badge lookup (avoids string allocation)
#[inline]
fn badge_cache_key(badge_key: &str, channel_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    badge_key.hash(&mut hasher);
    channel_id.hash(&mut hasher);
    hasher.finish()
}

/// Drop resolved badge URLs and names so a changed badge set is picked up.
pub fn clear_badge_render_caches() {
    BADGE_URL_CACHE.clear();
    BADGE_INFO_CACHE.clear();
}

/// Get cached badge URL (avoids FFI call every frame)
fn get_cached_badge_url(badge_key: &str, channel_id: &str) -> Option<String> {
    if backend::has_ffz_authority_badge(badge_key, channel_id) {
        return backend::get_badge_url(badge_key, channel_id);
    }
    let cache_key = badge_cache_key(badge_key, channel_id);

    if let Some(cached) = BADGE_URL_CACHE.get(&cache_key) {
        return cached.clone();
    }

    let url = backend::get_badge_url(badge_key, channel_id);

    if BADGE_URL_CACHE.len() >= BADGE_CACHE_MAX_SIZE {
        let to_evict = BADGE_CACHE_MAX_SIZE / 10;
        let keys: Vec<u64> = BADGE_URL_CACHE
            .iter()
            .take(to_evict)
            .map(|r| *r.key())
            .collect();
        for key in keys {
            BADGE_URL_CACHE.remove(&key);
        }
    }

    if let Some(ref resolved) = url {
        BADGE_URL_CACHE.insert(cache_key, Some(resolved.clone()));
    }
    url
}

#[inline]
fn badge_info_cache_key(badge_key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    badge_key.hash(&mut hasher);
    hasher.finish()
}

fn get_cached_badge_info(badge_key: &str, channel_id: &str) -> std::sync::Arc<BadgeRenderInfo> {
    if backend::has_ffz_authority_badge(badge_key, channel_id) {
        return std::sync::Arc::new(parse_badge_info(badge_key, channel_id));
    }

    let cache_key = badge_info_cache_key(badge_key);
    if let Some(cached) = BADGE_INFO_CACHE.get(&cache_key) {
        return cached.clone();
    }

    let info = std::sync::Arc::new(parse_badge_info(badge_key, channel_id));
    if BADGE_INFO_CACHE.len() >= BADGE_CACHE_MAX_SIZE {
        let to_evict = BADGE_CACHE_MAX_SIZE / 10;
        let keys: Vec<u64> = BADGE_INFO_CACHE
            .iter()
            .take(to_evict)
            .map(|entry| *entry.key())
            .collect();
        for key in keys {
            BADGE_INFO_CACHE.remove(&key);
        }
    }
    BADGE_INFO_CACHE.insert(cache_key, info.clone());
    info
}

/// Find break opportunities in text using UAX #14 Unicode Line Breaking Algorithm
/// Returns a vector of byte positions where the text can be broken
///
/// This is more sophisticated than simple whitespace splitting - it handles:
/// - URLs (breaks after `/`, `-`, `_`, etc.)
/// - CJK text (breaks between characters)
/// - Punctuation (breaks after certain punctuation marks)
#[inline]
fn find_break_opportunities(text: &str) -> Vec<usize> {
    let mut breaks = Vec::with_capacity(text.len() / 4 + 1);
    for (byte_pos, opportunity) in linebreaks(text) {
        match opportunity {
            BreakOpportunity::Mandatory => breaks.push(byte_pos),
            BreakOpportunity::Allowed => breaks.push(byte_pos),
        }
    }
    breaks
}

#[inline]
fn literal_space_width(builder: &mut LayoutBuilder<'_>) -> f32 {
    builder.measure_text_width(" ", false).max(2.0)
}

#[inline]
fn scaled_media_gap_for_height(media_height: f32) -> f32 {
    (media_height.max(emote_size()) * 0.16).ceil()
}

#[inline]
fn media_boundary_gap_for_height(builder: &mut LayoutBuilder<'_>, media_height: f32) -> f32 {
    literal_space_width(builder).max(scaled_media_gap_for_height(media_height))
}

#[inline]
fn previous_media_boundary_gap(builder: &mut LayoutBuilder<'_>) -> f32 {
    let media_height = builder.last_media_height.unwrap_or_else(emote_size);
    media_boundary_gap_for_height(builder, media_height)
}

/// Add spacing before text-like content based on the previous token.
///
/// This preserves the "no space between emotes" option while still guaranteeing
/// readable separation between emotes and text.
#[inline]
fn advance_before_text_like(builder: &mut LayoutBuilder<'_>) {
    if builder.current_x <= builder.content_start_x {
        return;
    }

    let literal_space_width = literal_space_width(builder);
    let state = boundary_spacing_state(builder, literal_space_width);
    let advance = echo_render_core::spacing::advance_before_text_like(state);
    if advance > 0.0 {
        builder.advance_by(advance);
    }
}

/// Add spacing before link-like content (URL / mention) so it behaves like text boundaries.
#[inline]
fn advance_before_link_like(builder: &mut LayoutBuilder<'_>) {
    if builder.current_x <= builder.content_start_x {
        return;
    }

    let space = literal_space_width(builder);
    match builder.last_token_type {
        LastTokenType::Text if !builder.last_text_ended_with_whitespace => {
            builder.advance_by(space);
        }
        LastTokenType::Link => builder.advance_by(space),
        LastTokenType::Emote => {
            let gap = previous_media_boundary_gap(builder);
            builder.advance_by(gap);
        }
        _ => {}
    }
}

/// Add spacing before emote-like content (emote / emoji).
///
/// Policy:
/// - text/link -> emote: media-aware gap unless text already ended with whitespace
/// - emote -> emote: media-aware gap controlled by remove_emote_spacing setting
#[inline]
fn advance_before_emote_like(
    builder: &mut LayoutBuilder<'_>,
    remove_emote_spacing: bool,
    incoming_media_height: f32,
) {
    if builder.current_x <= builder.content_start_x {
        return;
    }

    let literal_space_width = literal_space_width(builder);
    let state = boundary_spacing_state(builder, literal_space_width);
    let advance = echo_render_core::spacing::advance_before_emote_like(
        state,
        remove_emote_spacing,
        incoming_media_height,
    );
    if advance > 0.0 {
        builder.advance_by(advance);
    }
}

#[inline]
fn boundary_spacing_state(
    builder: &LayoutBuilder<'_>,
    literal_space_width: f32,
) -> echo_render_core::spacing::BoundarySpacingState {
    let prior_kind = match builder.last_token_type {
        LastTokenType::None => echo_render_core::spacing::PriorContentKind::None,
        LastTokenType::Text => echo_render_core::spacing::PriorContentKind::Text,
        LastTokenType::Link => echo_render_core::spacing::PriorContentKind::Link,
        LastTokenType::Emote => echo_render_core::spacing::PriorContentKind::Emote,
    };
    echo_render_core::spacing::BoundarySpacingState {
        at_line_start: builder.current_x <= builder.content_start_x,
        prior_kind,
        authored_whitespace: builder.last_text_ended_with_whitespace,
        previous_media_height: builder.last_media_height,
        literal_space_width,
        base_emote_size: emote_size(),
    }
}

#[inline]
fn contains_url(text: &str) -> bool {
    text.contains("http://") || text.contains("https://")
}

#[inline]
fn emote_is_animated(emote: &crate::backend::emotes::Emote, render_url: &str) -> bool {
    if emote.is_animated || is_animated_url(render_url) {
        return true;
    }

    emote.url_4x.as_deref().is_some_and(is_animated_url)
        || emote.url_2x.as_deref().is_some_and(is_animated_url)
        || emote.url_1x.as_deref().is_some_and(is_animated_url)
        || is_animated_url(&emote.url)
}

/// Use per-character prefix measurement for short runs to keep hit-testing and selection
/// tightly aligned with painted glyph positions (kerning/ligatures/subpixel).
const PRECISE_CHAR_POSITION_MAX: usize = 64;

#[inline]
fn push_wrapped_non_whitespace_run(builder: &mut LayoutBuilder<'_>, run: &str, color: Hsla) {
    let run_width = builder.measure_text_width(run, false);
    let available_width = builder.right_boundary - builder.current_x;

    if run_width > available_width && builder.current_x > MESSAGE_PADDING_LEFT {
        builder.wrap_line();
    }

    let line_width = builder.right_boundary - builder.content_start_x;
    if run_width > line_width {
        let breaks = find_break_opportunities(run);
        if breaks.len() > 1 {
            let mut start = 0;
            for &break_pos in &breaks {
                if break_pos == 0 || break_pos <= start {
                    continue;
                }
                let segment = run.get(start..break_pos).unwrap_or("");
                if segment.is_empty() {
                    start = break_pos;
                    continue;
                }
                let segment_width = builder.measure_text_width(segment, false);
                if builder.current_x + segment_width > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }
                builder.add_text_run(segment, color, false, false);
                start = break_pos;
            }
            if start < run.len() {
                let remaining = run.get(start..).unwrap_or("");
                let remaining_width = builder.measure_text_width(remaining, false);
                if builder.current_x + remaining_width > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }
                builder.add_text_run(remaining, color, false, false);
            }
        } else {
            for grapheme in run.graphemes(true) {
                let grapheme_width = builder.measure_text_width(grapheme, false);
                if builder.current_x + grapheme_width > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }
                builder.add_text_run(grapheme, color, false, false);
            }
        }
    } else {
        builder.add_text_run(run, color, false, false);
    }
}

/// Append one text fragment while preserving authored horizontal whitespace.
/// Hard line breaks are handled by `push_wrapped_text_preserving_breaks` so
/// they never get measured as an ordinary space or folded into the next run.
fn push_wrapped_text_fragment(builder: &mut LayoutBuilder<'_>, text: &str, color: Hsla) {
    let mut run_start = 0usize;
    let mut current_kind_is_whitespace: Option<bool> = None;

    for (idx, ch) in text.char_indices() {
        let is_ws = ch.is_whitespace();
        match current_kind_is_whitespace {
            None => current_kind_is_whitespace = Some(is_ws),
            Some(kind) if kind != is_ws => {
                let run = &text[run_start..idx];
                if kind {
                    let ws_width = builder.measure_text_width(run, false);
                    if builder.current_x + ws_width > builder.right_boundary
                        && builder.current_x > MESSAGE_PADDING_LEFT
                    {
                        builder.wrap_line();
                    } else {
                        builder.add_text_run(run, color, false, false);
                        builder.last_token_type = LastTokenType::Text;
                    }
                } else {
                    advance_before_text_like(builder);
                    push_wrapped_non_whitespace_run(builder, run, color);
                    builder.last_token_type = LastTokenType::Text;
                }
                run_start = idx;
                current_kind_is_whitespace = Some(is_ws);
            }
            _ => {}
        }
    }

    if let Some(kind) = current_kind_is_whitespace {
        let run = &text[run_start..];
        if !run.is_empty() {
            if !kind {
                advance_before_text_like(builder);
            }
            if kind {
                let ws_width = builder.measure_text_width(run, false);
                if builder.current_x + ws_width > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                } else {
                    builder.add_text_run(run, color, false, false);
                    builder.last_token_type = LastTokenType::Text;
                }
            } else {
                push_wrapped_non_whitespace_run(builder, run, color);
                builder.last_token_type = LastTokenType::Text;
            }
        }
    }
}

/// Preserve authored LF, CRLF, and CR line structure before applying the
/// width-aware wrapping used for each hard line. This is especially important
/// for ASCII art: treating a newline as a measured whitespace glyph destroys
/// both its rows and its alignment as the chat width changes.
fn push_wrapped_text_preserving_breaks(builder: &mut LayoutBuilder<'_>, text: &str, color: Hsla) {
    let bytes = text.as_bytes();
    let mut fragment_start = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] != b'\r' && bytes[index] != b'\n' {
            index += 1;
            continue;
        }

        push_wrapped_text_fragment(builder, &text[fragment_start..index], color);
        let break_start = index;
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 2;
        } else {
            index += 1;
        }
        builder.hard_break(&text[break_start..index]);
        fragment_start = index;
    }

    push_wrapped_text_fragment(builder, &text[fragment_start..], color);
}

/// Add a text segment after an inline media element.
///
/// Emoji parsing splits a source text slot around each emoji, but ordinary
/// ASCII slots already are a single text segment. Keeping their shared
/// handling here lets the ASCII fast path bypass emoji parsing without
/// changing the media-boundary spacing or clipboard-source behavior.
fn push_text_segment(builder: &mut LayoutBuilder<'_>, text: &str, color: Hsla) {
    let display_text = if matches!(builder.last_token_type, LastTokenType::Emote) {
        let trimmed = text.trim_start_matches(char::is_whitespace);
        let stripped_len = text.len().saturating_sub(trimmed.len());
        if stripped_len > 0 {
            builder.append_source_text_at_current_x(&text[..stripped_len]);
        }
        trimmed
    } else {
        text
    };
    if display_text.is_empty() {
        return;
    }

    push_wrapped_text_preserving_breaks(builder, display_text, color);
}

/// Count the clamped lines an embed text field needs at its shaped width. The
/// painter uses the same count and fixed line height, keeping the element tree
/// within the exact height reserved by the message layout.
fn embed_text_line_count(
    builder: &mut LayoutBuilder<'_>,
    text: Option<&str>,
    text_width: f32,
    is_bold: bool,
    max_lines: u8,
) -> u8 {
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return 0;
    };
    let measured_width = builder.measure_text_width(text, is_bold);
    (measured_width / text_width.max(1.0))
        .ceil()
        .clamp(1.0, f32::from(max_lines)) as u8
}

fn preview_source_dimensions(
    builder: &mut LayoutBuilder<'_>,
    url: &str,
    fallback: (f32, f32),
) -> (f32, f32) {
    if let Some((width, height)) = super::native_dimensions::decoded_image_size(url) {
        (width as f32, height as f32)
    } else {
        builder.track_pending_native_dimension_url(url);
        fallback
    }
}

/// The thumbnail mode caps height; native aspect ratio determines width.
fn shape_embed_preview_geometry(
    builder: &mut LayoutBuilder<'_>,
    preview_width: f32,
    metadata: &crate::backend::link_types::EmbedMetadata,
) -> (EmbedPreviewGeometry, f32) {
    let inner_width = (preview_width - EMBED_ACCENT_WIDTH - EMBED_PADDING * 2.0).max(0.0);
    let thumbnail_mode = settings::get_embed_thumbnail_mode();
    let thumbnail_visible = !crate::backend::streamer_mode::should_hide_link_thumbnails()
        && crate::backend::embed_thumbnails::should_render_thumbnail(
            thumbnail_mode,
            metadata.thumbnail_url.as_deref(),
        )
        && inner_width > 0.0;
    let (thumbnail_width, thumbnail_height) = thumbnail_visible
        .then(|| crate::backend::embed_thumbnails::thumbnail_dimensions(thumbnail_mode))
        .flatten()
        .map(|(configured_width, configured_height)| {
            let (source_width, source_height) = preview_source_dimensions(
                builder,
                metadata.thumbnail_url.as_deref().unwrap_or_default(),
                (configured_width, configured_height),
            );
            crate::backend::embed_thumbnails::fit_thumbnail_to_height(
                source_width,
                source_height,
                inner_width,
                configured_height,
            )
        })
        .unwrap_or((0.0, 0.0));
    let thumbnail_stacked = thumbnail_width > 0.0
        && inner_width - thumbnail_width - EMBED_TEXT_THUMBNAIL_GAP
            < EMBED_MIN_HORIZONTAL_TEXT_WIDTH;
    let text_width = if thumbnail_width > 0.0 && !thumbnail_stacked {
        inner_width - thumbnail_width - EMBED_TEXT_THUMBNAIL_GAP
    } else {
        inner_width
    };
    let title_lines = embed_text_line_count(
        builder,
        metadata.title.as_deref(),
        text_width,
        true,
        EMBED_TITLE_MAX_LINES,
    );
    let description_lines = embed_text_line_count(
        builder,
        metadata.description.as_deref(),
        text_width,
        false,
        EMBED_BODY_MAX_LINES,
    );
    let subtitle_lines = embed_text_line_count(
        builder,
        metadata.subtitle.as_deref(),
        text_width,
        false,
        EMBED_BODY_MAX_LINES,
    );

    let mut text_height = EMBED_PLATFORM_LINE_HEIGHT;
    let mut text_field_count = 1;
    for (lines, line_height) in [
        (title_lines, EMBED_TITLE_LINE_HEIGHT),
        (description_lines, EMBED_BODY_LINE_HEIGHT),
        (subtitle_lines, EMBED_BODY_LINE_HEIGHT),
    ] {
        if lines > 0 {
            text_height += f32::from(lines) * line_height;
            text_field_count += 1;
        }
    }
    text_height += (text_field_count - 1) as f32 * EMBED_TITLE_DESC_GAP;

    let content_height = if thumbnail_width > 0.0 {
        if thumbnail_stacked {
            thumbnail_height + EMBED_TEXT_THUMBNAIL_GAP + text_height
        } else {
            thumbnail_height.max(text_height)
        }
    } else {
        text_height
    };
    let minimum_height = if thumbnail_width > 0.0 {
        EMBED_MIN_HEIGHT
    } else {
        EMBED_TEXT_ONLY_MIN_HEIGHT
    };
    let height = (content_height + EMBED_PADDING * 2.0).max(minimum_height);

    (
        EmbedPreviewGeometry {
            thumbnail_width,
            thumbnail_height,
            thumbnail_stacked,
            thumbnail_gap: EMBED_TEXT_THUMBNAIL_GAP,
            title_lines,
            description_lines,
            subtitle_lines,
        },
        height,
    )
}

/// Build a complete layout for a message
///
/// When `window` is provided, uses accurate GPUI text measurement (pixel-perfect).
/// When `window` is None, uses cached character widths with 4px buffer for variance.
pub fn build_message_layout(
    msg: &ChatMessage,
    channel_id: &str,
    container_width: f32,
    show_timestamp: bool,
    window: Option<&mut Window>,
) -> CanvasMessageLayout {
    let uses_window_text_metrics = window.is_some();
    let mut trace_guard = LayoutTraceGuard::enter(
        msg,
        channel_id,
        container_width,
        show_timestamp,
        uses_window_text_metrics,
    );
    let mut layout = CanvasMessageLayout::new(msg.id.clone(), channel_id);
    layout.uses_window_text_metrics = uses_window_text_metrics;
    layout.content_fingerprint = super::layout::message_content_fingerprint(msg);
    layout.scale_bits = super::layout_scale::current_bits();
    layout.mod_buttons = super::layout_mode::mod_buttons();

    if let Some(date_iso) = msg.day_divider_date() {
        build_day_divider_layout(&mut layout, date_iso, container_width, window);
        trace_guard.finish(&layout);
        return layout;
    }

    let ascii_art = crate::backend::ascii_art::classify(&msg.content);
    let collapse_ascii = ascii_art.is_collapsible() && settings::get_collapse_ascii_art();

    if collapse_ascii {
        let lh = line_height();
        layout.height = lh + MESSAGE_PADDING * 2.0;
        let placeholder = "[ASCII Art]";
        let run = TextRunData::new(placeholder.len(), colors::text_muted());
        let gpui_run = TextRun {
            len: placeholder.len(),
            font: Font {
                family: text_run_intern::system_font_family(),
                features: FontFeatures::default(),
                fallbacks: None,
                weight: run.font_weight,
                style: FontStyle::Normal,
            },
            color: run.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        layout.shaped_lines.push(ShapedLineData {
            text: placeholder.into(),
            gpui_runs: SmallVec::from_elem(gpui_run.clone(), 1),
            gpui_runs_hash: hash_gpui_text_runs(std::slice::from_ref(&gpui_run)),
            char_positions: SmallVec::new(),
            visual_char_positions: Vec::new(),
            visual_to_logical: Vec::new(),
            source_start_char: 0,
            source_len_chars: placeholder.chars().count(),
            seventv_paint: None,
        });
        layout.segments.push(LayoutSegment::Text {
            line_idx: 0,
            x: MESSAGE_PADDING_LEFT,
            y: MESSAGE_PADDING,
            width: 80.0,
            height: lh,
        });
        layout.source_text = placeholder.to_string();
        layout.line_count = 1;
        layout.rebuild_paint_commands();
        trace_guard.finish(&layout);
        return layout;
    }

    layout.highlight_type = msg.flags.highlight_type.as_ref().map(HighlightType::from);

    layout.low_trust = if msg.flags.is_automod {
        super::layout::LowTrustTreatment::AutoMod
    } else if msg.flags.is_restricted {
        super::layout::LowTrustTreatment::Restricted
    } else if msg.flags.is_monitored || msg.flags.is_suspicious_user {
        super::layout::LowTrustTreatment::Monitored
    } else {
        super::layout::LowTrustTreatment::None
    };

    let has_reply = msg.reply.is_some();
    layout.has_reply_indicator = has_reply;

    let content_y_offset = if let Some(ref reply) = msg.reply {
        crate::log_ui!(
            debug,
            "Building reply indicator for msg {} -> replying to @{} (parent: {})",
            msg.id,
            reply.parent_display_name,
            reply.parent_msg_id
        );
        build_reply_indicator(&mut layout, reply, container_width);
        reply_indicator_height()
    } else {
        0.0
    };

    let username_color =
        resolve_user_color(msg.user_id.as_deref(), &msg.sender, msg.color.as_deref())
            .as_deref()
            .map(get_cached_color)
            .unwrap_or(colors::text_primary());
    let show_7tv_paints = settings::get_show_seventv_name_paints();
    let show_7tv_badges = settings::get_show_seventv_cosmetic_badges();
    let show_7tv_shadows = settings::get_show_seventv_paint_shadows();
    let seven_tv_cosmetics = if show_7tv_paints || show_7tv_badges {
        msg.user_id
            .as_deref()
            .and_then(crate::backend::cosmetics::get_cached_user_cosmetics)
    } else {
        None
    };
    let mut effective_username_color = username_color;

    let is_action = msg.flags.is_action;
    let is_first_message = msg.flags.is_first_message;
    let is_returning_chatter =
        matches!(layout.highlight_type, Some(HighlightType::ReturningChatter));

    let text_layout_width = ascii_art.layout_width(container_width);
    let right_boundary = text_layout_width - MESSAGE_PADDING_RIGHT;

    let strike_deleted_content = msg.flags.is_deleted
        && msg.flags.deleted_presentation == echo_core::message::DeletedPresentation::Struck;
    let mut builder = LayoutBuilder::new_with_y_offset(
        right_boundary,
        show_timestamp,
        content_y_offset,
        window,
        strike_deleted_content,
    );

    if layout.mod_buttons && message_takes_mod_buttons(msg) {
        for button in settings::get_mod_buttons() {
            let label = button.label.trim();
            if label.is_empty() || button.command.trim().is_empty() {
                continue;
            }
            builder.add_mod_button(label, button.command.as_str(), button.is_destructive);
        }
    }

    if show_timestamp {
        let timestamp_x = builder.current_x;
        let timestamp_str = get_cached_timestamp(msg.timestamp);
        builder.add_text_run(&timestamp_str, colors::text_muted(), false, false);
        builder.flush_text_segment();
        builder.timestamp_column_end_x = timestamp_x + timestamp_width() + TIMESTAMP_GAP;
        builder.set_x(builder.timestamp_column_end_x);
    }

    if msg.flags.is_system {
        builder.mark_content_start();
    } else {
        backend::queue_bttv_pro_badge_lookup(&msg.channel, &msg.sender, msg.user_id.as_deref());

        if settings::get_show_live_chatter_indicator()
            && backend::twitch_api::get_cached_user_live_status(&msg.sender) == Some(true)
        {
            builder.add_inline_box("LIVE", colors::error(), px(3.0), px(1.0));
        }
        if is_first_message {
            builder.add_inline_box("NEW", colors::success(), px(3.0), px(1.0));
        }
        if is_returning_chatter {
            builder.add_inline_box("👋", colors::info(), px(2.0), px(1.0));
        }

        let mut has_visible_badges = false;
        for badge_key in msg.badges.iter() {
            if !settings::should_render_badge_key(badge_key) {
                continue;
            }

            if let Some(url) = get_cached_badge_url(badge_key, &msg.channel) {
                let info = get_cached_badge_info(badge_key, &msg.channel);
                let bs = badge_size();
                let bg = badge_gap();
                has_visible_badges = true;
                if builder.current_x + bs > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }
                builder.add_image(
                    &url,
                    bs,
                    bs,
                    false,
                    ImageType::Badge {
                        name: info.name.clone(),
                        description: info.description.clone(),
                        tooltip_meta: Some(info.tooltip_meta.clone()),
                        background: info.background,
                    },
                    None,
                );
                builder.advance(bs + bg);
            } else if echo_chat_render::ports::badge_set_pending(&msg.channel) {
                let bs = badge_size();
                has_visible_badges = true;
                builder.reserved_badge_channel = Some(std::sync::Arc::clone(&msg.channel));
                if builder.current_x + bs > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }
                builder.advance(bs + badge_gap());
            }
        }

        for badge_key in
            backend::get_external_badge_keys(&msg.channel, &msg.sender, msg.user_id.as_deref())
        {
            if msg
                .badges
                .iter()
                .any(|existing| crate::backend::badges::badge_key_matches(existing, &badge_key))
            {
                continue;
            }
            if !settings::should_render_badge_key(&badge_key) {
                continue;
            }

            if let Some(url) = get_cached_badge_url(&badge_key, &msg.channel) {
                let info = get_cached_badge_info(&badge_key, &msg.channel);
                let bs = badge_size();
                let bg = badge_gap();
                has_visible_badges = true;
                if builder.current_x + bs > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }
                builder.add_image(
                    &url,
                    bs,
                    bs,
                    false,
                    ImageType::Badge {
                        name: info.name.clone(),
                        description: info.description.clone(),
                        tooltip_meta: Some(info.tooltip_meta.clone()),
                        background: info.background,
                    },
                    None,
                );
                builder.advance(bs + bg);
            }
        }

        let has_7tv_badge = if show_7tv_badges {
            seven_tv_cosmetics
                .as_ref()
                .and_then(|cosmetics| cosmetics.badge.as_ref())
                .map(|badge| {
                    let bs = badge_size();
                    let bg = badge_gap();
                    let description = badge.tooltip.trim();
                    if builder.current_x + bs > builder.right_boundary
                        && builder.current_x > MESSAGE_PADDING_LEFT
                    {
                        builder.wrap_line();
                    }
                    builder.add_image(
                        &badge.url,
                        bs,
                        bs,
                        badge.is_animated,
                        ImageType::Badge {
                            name: badge.name.clone().into(),
                            description: (!description.is_empty())
                                .then(|| SharedString::from(description)),
                            tooltip_meta: Some(std::sync::Arc::new(BadgeTooltipMeta {
                                source_label: "7TV Badge".into(),
                                type_label: badge.name.clone().into(),
                                tier_label: None,
                                is_animated: badge.is_animated,
                            })),
                            background: None,
                        },
                        None,
                    );
                    builder.advance(bs + bg);
                    true
                })
                .unwrap_or(false)
        } else {
            false
        };

        if has_visible_badges || has_7tv_badge {
            builder.advance(badge_margin_right() - badge_gap());
        }

        let raw_display_name = msg.display_name.trim();
        let presented_display_name =
            settings::format_username_for_display(msg.sender.as_ref(), raw_display_name);

        let nickname = crate::backend::settings::apply_nickname_rules(
            msg.sender.as_ref(),
            raw_display_name,
            msg.user_id.as_deref(),
        );
        let base_display_name: &str = nickname.as_deref().unwrap_or(&presented_display_name);

        let maybe_paint = seven_tv_cosmetics
            .as_ref()
            .and_then(|cosmetics| cosmetics.paint.as_ref())
            .filter(|_| show_7tv_paints);
        let display_name = maybe_paint
            .map(|paint| paint.transformed_display_name(base_display_name))
            .unwrap_or_else(|| base_display_name.to_string());

        let username_width = builder.measure_text_width(&display_name, true);
        if builder.current_x + username_width > builder.right_boundary
            && builder.current_x > MESSAGE_PADDING_LEFT
        {
            builder.wrap_line();
        }

        let username_start_x = builder.current_x;

        if let Some(color) = maybe_paint.and_then(|paint| {
            add_7tv_painted_username(
                &mut builder,
                &display_name,
                paint,
                username_color,
                show_7tv_shadows,
            )
        }) {
            effective_username_color = color;
        } else {
            builder.add_text_run(&display_name, username_color, true, false);
        }

        debug_assert!(
            builder.current_x > username_start_x || msg.display_name.is_empty(),
            "Username '{}' has zero width - hitbox will not be clickable!",
            msg.display_name
        );

        builder.add_hitbox_guaranteed(HitboxKind::Username {
            login: SharedString::from(msg.sender.clone()),
            display_name: if raw_display_name.is_empty() {
                SharedString::from(msg.sender.clone())
            } else if raw_display_name == msg.display_name.as_ref() {
                SharedString::from(msg.display_name.clone())
            } else {
                SharedString::from(raw_display_name)
            },
            user_id: msg
                .user_id
                .as_ref()
                .map(|id| SharedString::from(id.clone())),
            color: Some(effective_username_color),
        });

        if is_action {
            builder.add_text_run(" ", effective_username_color, false, false);
        } else {
            builder.add_text_run(": ", colors::text_muted(), false, false);
        }
        builder.flush_text_segment();

        builder.mark_content_start();
    }

    let mut message_color = if msg.flags.is_system {
        colors::text_secondary()
    } else if is_action {
        // `/me` bodies take the Twitch username color, never a 7TV paint's
        // first stop, as SevenTV's UserMessage does.
        username_color
    } else {
        colors::text_primary()
    };
    if msg.flags.is_similar {
        message_color = message_color.opacity(SIMILAR_MESSAGE_DIM_OPACITY);
    }
    if msg.flags.is_deleted
        && msg.flags.deleted_presentation == echo_core::message::DeletedPresentation::Collapsed
    {
        message_color = colors::text_muted();
    }

    let resolver = SlotResolver::new(msg.channel.as_ref());
    let remove_emote_spacing = settings::get_remove_emote_spacing();
    let current_user = settings::get_username();
    let mut emoji_style = None;
    let has_message_mention_highlight =
        matches!(layout.highlight_type.as_ref(), Some(HighlightType::Mention));

    let render_units =
        resolve_render_units(&msg.slots, &resolver, settings::get_zero_width_enabled());
    let emote_display_rules = settings::emote_display_rules_snapshot();
    // Walks `msg.slots` beside the render units so a text-shown emote can use
    // the text that was typed. Only the rule path advances it.
    let mut emote_slot_cursor = 0usize;
    builder.monospace_text = ascii_art.uses_art_layout();
    const MAX_EMBEDS_PER_MESSAGE: usize = 2;
    let mut deferred_embeds: Vec<(
        std::sync::Arc<str>,
        u32,
        crate::backend::link_types::EmbedMetadata,
    )> = Vec::new();
    let upload_success_url = match msg.flags.system_type.as_ref() {
        Some(crate::backend::models::SystemMessageType::UploadSuccess { url, .. }) => {
            Some(url.as_str())
        }
        _ => None,
    };
    let hide_link_urls = crate::backend::streamer_mode::should_hide_link_urls();
    let upload_preview = upload_success_url.filter(|_| !hide_link_urls).map(|url| {
        let thumbnail_url = (!crate::backend::streamer_mode::should_hide_link_thumbnails())
            .then(|| crate::backend::upload_history::lookup_known_upload_url(url))
            .flatten()
            .and_then(|known| known.is_share_url.then_some(known.thumbnail_url))
            .flatten()
            .map(SharedString::from);
        (SharedString::from(url), thumbnail_url)
    });

    for (unit_index, unit) in render_units.iter().enumerate() {
        match unit {
            SlotRenderUnit::Text(text) => {
                builder.flush_layered_emote();

                let after_emote = matches!(builder.last_token_type, LastTokenType::Emote);
                let text = if after_emote {
                    text.trim_start_matches(char::is_whitespace)
                } else {
                    text.as_ref()
                };
                if text.is_empty() {
                    continue;
                }

                if after_emote {
                    builder.ensure_source_separator();
                }

                if text.is_ascii() {
                    push_text_segment(&mut builder, text, message_color);
                    continue;
                }

                let emoji_style = emoji_style.get_or_insert_with(settings::get_emoji_style);
                let segments = parse_emoji(text, &*emoji_style);

                for segment in segments {
                    match segment {
                        TextSegment::Text(text_part) => {
                            push_text_segment(&mut builder, &text_part, message_color);
                        }
                        TextSegment::Emoji {
                            char: emoji_char,
                            url,
                            name,
                        } => {
                            let had_text = builder.flush_text_segment();

                            if had_text {
                                builder.last_token_type = LastTokenType::Text;
                            }
                            let size = emoji_size();
                            advance_before_emote_like(&mut builder, remove_emote_spacing, size);

                            if builder.current_x + size > builder.right_boundary
                                && builder.current_x > MESSAGE_PADDING_LEFT
                            {
                                builder.wrap_line();
                            }

                            let source_range = builder.append_source_text_at_current_x(&emoji_char);

                            builder.add_image(
                                &url,
                                size,
                                size,
                                false,
                                ImageType::Emote {
                                    name: name.into(),
                                    tooltip_meta: None,
                                },
                                Some(source_range),
                            );
                            builder.advance(size);
                            builder.last_token_type = LastTokenType::Emote;
                        }
                    }
                }
            }
            SlotRenderUnit::Emote(emote_unit) => match emote_display_rules.as_deref() {
                None => add_emote_render_unit(
                    &mut builder,
                    &emote_unit.base,
                    &emote_unit.overlays,
                    &[],
                    remove_emote_spacing,
                ),
                Some(rules) => add_emote_render_unit_with_rules(
                    &mut builder,
                    emote_unit,
                    EmoteSlotSource {
                        slots: &msg.slots,
                        cursor: &mut emote_slot_cursor,
                    },
                    rules,
                    msg.channel.as_ref(),
                    message_color,
                    remove_emote_spacing,
                ),
            },
            SlotRenderUnit::TwitchGif {
                id: _,
                url,
                alt_text,
            } => {
                builder.flush_text_segment();
                builder.flush_layered_emote();
                if builder.current_x > MESSAGE_PADDING_LEFT {
                    builder.wrap_line();
                }

                let available_width = (builder.right_boundary - builder.current_x).max(1.0);
                let max_height = 100.0 * emote_size() / echo_chat_render::layout::EMOTE_SIZE_BASE;
                let (gif_width, gif_height) = if let Some((width, height)) =
                    echo_chat_render::ports::decoded_image_size(url)
                {
                    let scale = (available_width / width as f32).min(max_height / height as f32);
                    (width as f32 * scale, height as f32 * scale)
                } else {
                    builder.track_pending_native_dimension_url(url);
                    let size = max_height.min(available_width);
                    (size, size)
                };
                if !alt_text.starts_with(char::is_whitespace) {
                    builder.ensure_source_separator();
                }
                let source_range = builder.append_source_text_at_current_x(alt_text);
                builder.add_image(
                    url,
                    gif_width,
                    gif_height,
                    true,
                    ImageType::TwitchGif {
                        alt_text: SharedString::from(alt_text.clone()),
                    },
                    Some(source_range),
                );
                builder.advance(gif_width);
                builder.last_token_type = LastTokenType::Emote;
                if unit_index + 1 < render_units.len() {
                    builder.wrap_line();
                }
            }
            SlotRenderUnit::Url { url, embed_id } => {
                let embed_id = *embed_id;
                use crate::backend::bridge::{get_embed_metadata, is_embed_expanded_state};
                use crate::backend::settings::get_embed_display_mode;

                builder.flush_layered_emote();

                let hide_link_url = hide_link_urls;
                let suppress_rich_preview = upload_success_url.is_some()
                    || crate::backend::upload_history::is_known_upload_deletion_url(url);

                let embed_mode = get_embed_display_mode();
                let metadata_opt = if hide_link_url || suppress_rich_preview {
                    None
                } else {
                    get_embed_metadata(url)
                };

                let should_show_preview = !suppress_rich_preview
                    && match embed_mode {
                        crate::backend::settings::EmbedDisplayMode::None => false,
                        crate::backend::settings::EmbedDisplayMode::Click => {
                            is_embed_expanded_state(&msg.id, embed_id, url)
                        }
                        crate::backend::settings::EmbedDisplayMode::Always => {
                            metadata_opt.is_some()
                        }
                    };
                if should_show_preview
                    && deferred_embeds.len() < MAX_EMBEDS_PER_MESSAGE
                    && let Some(metadata) = metadata_opt
                {
                    deferred_embeds.push((url.clone(), embed_id, metadata));
                }

                if hide_link_url {
                    builder.flush_text_segment();

                    advance_before_link_like(&mut builder);

                    let display = "[link hidden]";
                    let url_width = builder.measure_text_width(display, false);

                    if builder.current_x + url_width > builder.right_boundary
                        && builder.current_x > MESSAGE_PADDING_LEFT
                    {
                        builder.wrap_line();
                    }

                    builder.add_text_run(display, colors::text_muted(), false, false);
                    builder.flush_text_segment();
                    builder.last_token_type = LastTokenType::Link;
                } else {
                    builder.flush_text_segment();

                    advance_before_link_like(&mut builder);

                    let display = SharedString::from(url.clone());
                    let url_width = builder.measure_text_width(&display, false);

                    if builder.current_x + url_width > builder.right_boundary
                        && builder.current_x > MESSAGE_PADDING_LEFT
                    {
                        builder.wrap_line();
                    }

                    let link_color = if crate::utils::is_url_opened(url) {
                        colors::opened_link()
                    } else {
                        colors::link()
                    };
                    builder.add_text_run(&display, link_color, false, true);
                    builder.add_hitbox(HitboxKind::Url {
                        url: SharedString::from(url.clone()),
                        display_text: display.clone(),
                        embed_id: if upload_success_url.is_some() {
                            crate::backend::upload_history::UPLOAD_LINK_NO_PREVIEW
                        } else {
                            embed_id
                        },
                    });

                    builder.flush_text_segment();
                    builder.last_token_type = LastTokenType::Link;
                }
            }
            SlotRenderUnit::Mention(username) => {
                builder.flush_text_segment();
                builder.flush_layered_emote();

                advance_before_link_like(&mut builder);

                let mention_text = if msg.flags.is_system {
                    username.to_string()
                } else {
                    format!("@{}", username)
                };
                let mention_width = builder.measure_text_width(&mention_text, true);

                if builder.current_x + mention_width > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }

                let mention_start_x = builder.current_x;
                let mention_y = builder.current_y;

                let mention_color = if username.eq_ignore_ascii_case(&msg.sender) {
                    msg.color
                        .as_ref()
                        .map(|c| get_cached_color(c))
                        .unwrap_or_else(colors::text_primary)
                } else {
                    get_user_color(username)
                        .map(|hex| get_cached_color(&hex))
                        .unwrap_or_else(colors::text_primary)
                };

                builder.add_text_run(&mention_text, mention_color, true, false);

                debug_assert!(
                    builder.current_x > mention_start_x,
                    "Mention '@{}' has zero width - hitbox will not be clickable!",
                    username
                );

                builder.add_hitbox_guaranteed(HitboxKind::Mention {
                    username: SharedString::from(username.clone()),
                });

                if !has_message_mention_highlight
                    && !msg.flags.is_system
                    && current_user
                        .as_ref()
                        .map(|u| u.eq_ignore_ascii_case(username.as_ref()))
                        .unwrap_or(false)
                {
                    builder.add_mention_highlight(mention_start_x, mention_y, mention_width);
                }

                builder.flush_text_segment();
                builder.last_token_type = if msg.flags.is_system {
                    LastTokenType::Text
                } else {
                    LastTokenType::Link
                };
            }

            SlotRenderUnit::Emoji { emoji, url } => {
                builder.flush_text_segment();
                builder.flush_layered_emote();

                let emoji_size = emoji_size();
                let emoji_width = emoji_size;
                advance_before_emote_like(&mut builder, remove_emote_spacing, emoji_size);

                if builder.current_x + emoji_width > builder.right_boundary
                    && builder.current_x > MESSAGE_PADDING_LEFT
                {
                    builder.wrap_line();
                }

                let source_range = builder.append_source_text_at_current_x(emoji);

                let emoji_label = SharedString::from(emoji.as_ref());
                builder.add_image(
                    url,
                    emoji_width,
                    emoji_size,
                    false,
                    ImageType::Emote {
                        name: emoji_label.clone(),
                        tooltip_meta: None,
                    },
                    Some(source_range),
                );

                builder.add_hitbox(HitboxKind::Emoji { emoji: emoji_label });

                builder.advance_by(emoji_width);
                builder.last_token_type = LastTokenType::Emote;
            }
        }
    }

    if msg.flags.is_automod && msg.flags.is_automod_pending {
        builder.flush_text_segment();
        let actions_width = builder.measure_text_width("  Allow    Deny", true);
        if builder.current_x + actions_width > builder.right_boundary
            && builder.current_x > MESSAGE_PADDING_LEFT
        {
            builder.wrap_line();
        }
        builder.add_text_run("  ", colors::text_muted(), false, false);
        builder.flush_text_segment();
        builder.add_text_run("Allow", colors::success(), true, true);
        builder.add_hitbox(HitboxKind::AutomodAction { allow: true });
        builder.flush_text_segment();
        builder.add_text_run("    ", colors::text_muted(), false, false);
        builder.flush_text_segment();
        builder.add_text_run("Deny", colors::error(), true, true);
        builder.add_hitbox(HitboxKind::AutomodAction { allow: false });
        builder.flush_text_segment();
    }

    if !deferred_embeds.is_empty() {
        use super::layout::{EmbedPlatform, LayoutSegment};
        use crate::backend::bridge::is_embed_expanded_state;
        use crate::backend::link_types::{LinkType, MediaKind};

        builder.flush_text_segment();
        builder.flush_layered_emote();
        if !builder.line_buffer.is_empty() {
            builder.commit_line();
            builder.current_line += 1;
            builder.line_y_positions.push(builder.current_y);
            builder.current_x = builder.content_start_x;
            builder.text_segment_start_x = builder.content_start_x;
            builder.last_emote_buffer_idx = None;
            builder.line_offsets.push(builder.source_offset);
        }

        for (url, embed_id, metadata) in deferred_embeds {
            builder.current_y += 8.0;

            let platform = match metadata.media.as_ref().map(|media| media.kind) {
                Some(MediaKind::Video) => EmbedPlatform::Video,
                Some(MediaKind::Image) | None => match metadata.link_info.link_type {
                    LinkType::SevenTvEmote | LinkType::SevenTvUser => EmbedPlatform::SevenTv,
                    LinkType::Spotify => EmbedPlatform::Spotify,
                    LinkType::Steam => EmbedPlatform::Steam,
                    LinkType::YouTube => EmbedPlatform::YouTube,
                    LinkType::TwitchClip => EmbedPlatform::TwitchClip,
                    LinkType::TwitchVod => EmbedPlatform::TwitchVod,
                    LinkType::TwitchChannel => EmbedPlatform::TwitchChannel,
                    LinkType::Twitter => EmbedPlatform::Twitter,
                    LinkType::Reddit => EmbedPlatform::Reddit,
                    LinkType::Image => EmbedPlatform::Image,
                    LinkType::Video => EmbedPlatform::Video,
                    LinkType::Generic => EmbedPlatform::Generic,
                },
            };

            let available_width = (builder.right_boundary - MESSAGE_PADDING_LEFT).max(0.0);
            let thumbnail_mode = settings::get_embed_thumbnail_mode();
            let image_width = if !crate::backend::streamer_mode::should_hide_link_thumbnails()
                && crate::backend::embed_thumbnails::should_render_thumbnail(
                    thumbnail_mode,
                    metadata.thumbnail_url.as_deref(),
                )
                && let Some((_, height_cap)) =
                    crate::backend::embed_thumbnails::thumbnail_dimensions(thumbnail_mode)
                && let Some((width, height)) = metadata
                    .thumbnail_url
                    .as_deref()
                    .and_then(super::native_dimensions::decoded_image_size)
            {
                width as f32 / height as f32 * height_cap
            } else {
                0.0
            };
            let preview_width = available_width
                .min(420.0_f32.max(image_width + EMBED_ACCENT_WIDTH + EMBED_PADDING * 2.0));
            let (geometry, preview_height) =
                shape_embed_preview_geometry(&mut builder, preview_width, &metadata);

            builder.segments.push(LayoutSegment::EmbedPreview {
                url: SharedString::from(url.clone()),
                platform,
                thumbnail_url: (geometry.thumbnail_width > 0.0)
                    .then(|| metadata.thumbnail_url.map(Into::into))
                    .flatten(),
                title: metadata.title.map(Into::into),
                description: metadata.description.map(Into::into),
                subtitle: metadata.subtitle.map(Into::into),
                duration: metadata.duration.map(Into::into),
                x: MESSAGE_PADDING_LEFT,
                y: builder.current_y,
                width: preview_width,
                height: preview_height,
                geometry,
                is_expanded: is_embed_expanded_state(&msg.id, embed_id, &url),
                is_loading: false,
            });

            builder.hitboxes.push(HitboxRegion {
                bounds: Bounds::new(
                    point(px(MESSAGE_PADDING_LEFT), px(builder.current_y)),
                    size(px(preview_width), px(preview_height)),
                ),
                kind: HitboxKind::EmbedPreview {
                    url: SharedString::from(url.clone()),
                    embed_id,
                    is_expanded: is_embed_expanded_state(&msg.id, embed_id, &url),
                },
            });

            if let Some(source_url) = metadata.media.as_ref().and_then(|media| {
                (media.kind == MediaKind::Image && !media.source_url.is_empty())
                    .then_some(media.source_url.as_str())
            }) && geometry.thumbnail_width > 0.0
                && geometry.thumbnail_height > 0.0
            {
                builder.hitboxes.push(HitboxRegion {
                    bounds: Bounds::new(
                        point(
                            px(MESSAGE_PADDING_LEFT + EMBED_ACCENT_WIDTH + EMBED_PADDING),
                            px(builder.current_y + EMBED_PADDING),
                        ),
                        size(px(geometry.thumbnail_width), px(geometry.thumbnail_height)),
                    ),
                    kind: HitboxKind::MediaInspector {
                        source_url: SharedString::from(source_url),
                        url: SharedString::from(url.clone()),
                        embed_id,
                    },
                });
            }

            builder.current_y += preview_height + 8.0;
            builder.current_x = builder.content_start_x;
            builder.last_token_type = LastTokenType::None;
        }
    }

    if let Some((url, thumbnail_url)) = upload_preview {
        builder.flush_text_segment();
        builder.flush_layered_emote();
        if !builder.line_buffer.is_empty() {
            builder.commit_line();
            builder.current_line += 1;
            builder.line_y_positions.push(builder.current_y);
            builder.current_x = builder.content_start_x;
            builder.text_segment_start_x = builder.content_start_x;
            builder.last_emote_buffer_idx = None;
            builder.line_offsets.push(builder.source_offset);
        }

        const UPLOAD_PREVIEW_HEIGHT: f32 = 56.0;
        builder.current_y += 6.0;
        let available_width = (builder.right_boundary - MESSAGE_PADDING_LEFT).max(0.0);
        let label_width = 144.0_f32.min(available_width * 0.4);
        let max_thumbnail_width = (available_width - label_width - 8.0).max(0.0);
        let (source_width, source_height) = thumbnail_url
            .as_ref()
            .map(|url| preview_source_dimensions(&mut builder, url.as_ref(), (56.0, 56.0)))
            .unwrap_or((56.0, 56.0));
        let (thumbnail_width, _) = crate::backend::embed_thumbnails::fit_thumbnail_to_height(
            source_width,
            source_height,
            max_thumbnail_width,
            UPLOAD_PREVIEW_HEIGHT,
        );
        let preview_width = (thumbnail_width + 8.0 + label_width).min(available_width);
        builder.segments.push(LayoutSegment::UploadPreview {
            url,
            thumbnail_url,
            thumbnail_width,
            x: MESSAGE_PADDING_LEFT,
            y: builder.current_y,
            width: preview_width,
            height: UPLOAD_PREVIEW_HEIGHT,
        });
        builder.current_y += UPLOAD_PREVIEW_HEIGHT + 8.0;
        builder.current_x = builder.content_start_x;
        builder.last_token_type = LastTokenType::None;
    }

    builder.finalize(&mut layout);

    layout.hitbox_regions.push(HitboxRegion {
        bounds: Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(container_width), px(layout.height)),
        ),
        kind: HitboxKind::MessageBackground {
            message_id: SharedString::from(msg.id.clone()),
        },
    });

    layout.rebuild_paint_commands();

    layout.has_element_segments = layout.segments.iter().any(|segment| match segment {
        LayoutSegment::InlineBox { .. }
        | LayoutSegment::EmbedPreview { .. }
        | LayoutSegment::UploadPreview { .. }
        | LayoutSegment::ReplyIndicator { .. } => true,
        LayoutSegment::Image {
            url,
            is_animated,
            overlays,
            ..
        } => {
            *is_animated
                || is_svg_image_url(url.as_ref())
                || overlays
                    .iter()
                    .any(|overlay| overlay.is_animated || is_svg_image_url(overlay.url.as_ref()))
        }
        LayoutSegment::LayeredImage { layers, .. } => layers
            .iter()
            .any(|layer| layer.is_animated || is_svg_image_url(layer.url.as_ref())),
        LayoutSegment::Text { .. } => false,
    });

    trace_guard.finish(&layout);
    layout
}

/// Build the layout of a derived day-divider row: a centered ISO `YYYY-MM-DD`
/// label flanked by flexible 1px rules, in a fixed 28px row.
///
/// The row deliberately has no timestamp column, badges, username, reply
/// affordance, highlight, moderation hitbox, or message-background hitbox, so it
/// cannot be selected, replied to, moderated, or copied.
fn build_day_divider_layout(
    layout: &mut CanvasMessageLayout,
    date_iso: &str,
    container_width: f32,
    window: Option<&mut Window>,
) {
    layout.height = DAY_DIVIDER_ROW_HEIGHT;
    layout.line_count = 1;
    layout.line_y_positions.push(0.0);

    let label_text = SharedString::from(date_iso);
    let text_width = measure_day_divider_label(&label_text, window);

    let left_edge = MESSAGE_PADDING_LEFT;
    let right_edge = (container_width - MESSAGE_PADDING_RIGHT).max(left_edge);
    let available = right_edge - left_edge;
    let label_width = (text_width + DAY_DIVIDER_LABEL_PADDING_X * 2.0).min(available);
    let label_x = left_edge + ((available - label_width) * 0.5).max(0.0);
    let label_y = ((DAY_DIVIDER_ROW_HEIGHT - DAY_DIVIDER_LABEL_HEIGHT) * 0.5).floor();
    let rule_y = ((DAY_DIVIDER_ROW_HEIGHT - DAY_DIVIDER_RULE_HEIGHT) * 0.5).floor();
    let rule_color = colors::border_subtle();

    let left_rule_width = label_x - DAY_DIVIDER_LABEL_GAP - left_edge;
    if left_rule_width >= DAY_DIVIDER_MIN_RULE_WIDTH {
        layout.segments.push(day_divider_rule(
            left_edge,
            rule_y,
            left_rule_width,
            rule_color,
        ));
    }

    let right_rule_x = label_x + label_width + DAY_DIVIDER_LABEL_GAP;
    let right_rule_width = right_edge - right_rule_x;
    if right_rule_width >= DAY_DIVIDER_MIN_RULE_WIDTH {
        layout.segments.push(day_divider_rule(
            right_rule_x,
            rule_y,
            right_rule_width,
            rule_color,
        ));
    }

    layout.segments.push(LayoutSegment::InlineBox {
        text: label_text,
        x: label_x,
        y: label_y,
        width: label_width,
        height: DAY_DIVIDER_LABEL_HEIGHT,
        bg_color: transparent_black(),
        text_color: colors::text_muted(),
        font_size: DAY_DIVIDER_LABEL_FONT_SIZE,
    });

    layout.has_element_segments = true;
    layout.rebuild_paint_commands();
}

/// One flexible rule of a day-divider row.
///
/// `InlineBox` is the only layout primitive that paints a solid colored rect at
/// an explicit size, so it doubles as the rule; the empty label keeps it a plain
/// fill.
fn day_divider_rule(x: f32, y: f32, width: f32, color: Hsla) -> LayoutSegment {
    LayoutSegment::InlineBox {
        text: SharedString::from(""),
        x,
        y,
        width,
        height: DAY_DIVIDER_RULE_HEIGHT,
        bg_color: color,
        text_color: color,
        font_size: DAY_DIVIDER_LABEL_FONT_SIZE,
    }
}

/// Measure the divider label at its own 11px size (the message font size and
/// timestamp pattern do not apply to derived rows).
fn measure_day_divider_label(text: &SharedString, window: Option<&mut Window>) -> f32 {
    let Some(window) = window else {
        return text.chars().count() as f32
            * DAY_DIVIDER_LABEL_FONT_SIZE
            * DAY_DIVIDER_FALLBACK_ADVANCE_RATIO;
    };

    let run = TextRun {
        len: text.len(),
        font: Font {
            family: text_run_intern::system_font_family(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::SEMIBOLD,
            style: FontStyle::Normal,
        },
        color: colors::text_muted(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    window
        .text_system()
        .shape_line(
            text.clone(),
            px(DAY_DIVIDER_LABEL_FONT_SIZE),
            std::slice::from_ref(&run),
            None,
        )
        .width()
        .as_f32()
}

/// Build reply indicator segments (L-shaped connector + @user + preview)
fn build_reply_indicator(
    layout: &mut CanvasMessageLayout,
    reply: &ReplyInfo,
    container_width: f32,
) {
    let parent_color = get_user_color(&reply.parent_user_login)
        .map(|hex| get_cached_color(&hex))
        .unwrap_or(colors::text_secondary());
    let presented_parent_name =
        settings::format_username_for_display(&reply.parent_user_login, &reply.parent_display_name);
    let nickname = settings::apply_nickname_rules(
        &reply.parent_user_login,
        &reply.parent_display_name,
        reply.parent_user_id.as_deref(),
    );
    let presented_parent_name = nickname.as_deref().unwrap_or(&presented_parent_name);

    let preview = reply
        .parent_msg_body
        .as_ref()
        .map(|body| {
            let body = body.trim();
            if contains_url(body) {
                body.to_string()
            } else if body.chars().count() > REPLY_PREVIEW_MAX_CHARS {
                let truncated: String = body
                    .chars()
                    .take(REPLY_PREVIEW_MAX_CHARS.saturating_sub(3))
                    .collect();
                format!("{}...", truncated)
            } else {
                body.to_string()
            }
        })
        .unwrap_or_default();

    let connector_x = MESSAGE_PADDING_LEFT + REPLY_CONNECTOR_LEFT;
    let text_x = connector_x + REPLY_CONNECTOR_ARM + REPLY_CONNECTOR_GAP;
    let available_width = container_width - text_x - MESSAGE_PADDING_RIGHT;
    let parent_display_name = SharedString::from(presented_parent_name);

    layout.segments.push(LayoutSegment::ReplyIndicator {
        connector_x,
        connector_y: MESSAGE_PADDING,
        connector_height: reply_indicator_height() - 4.0,
        parent_display_name: parent_display_name.clone(),
        parent_color,
        preview_text: preview.into(),
        text_x,
        width: available_width,
    });

    layout.hitbox_regions.push(HitboxRegion {
        bounds: Bounds::new(
            point(px(MESSAGE_PADDING_LEFT), px(MESSAGE_PADDING)),
            size(
                px(container_width - MESSAGE_PADDING_LEFT - MESSAGE_PADDING_RIGHT),
                px(reply_indicator_height()),
            ),
        ),
        kind: HitboxKind::ReplyIndicator {
            parent_msg_id: SharedString::from(reply.parent_msg_id.clone()),
            parent_display_name,
            parent_preview: reply
                .parent_msg_body
                .as_ref()
                .map(|body| SharedString::from(body.clone())),
        },
    });
}

fn rgba_to_hsla(color: crate::backend::cosmetics_core::RgbaColor) -> Hsla {
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let alpha = color.a as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;

    if (max - min).abs() <= f32::EPSILON {
        return hsla(0.0, 0.0, lightness, alpha);
    }

    let delta = max - min;
    let saturation = if lightness > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let hue = if (max - r).abs() <= f32::EPSILON {
        ((g - b) / delta + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() <= f32::EPSILON {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };

    hsla(hue, saturation, lightness, alpha)
}

fn add_7tv_painted_username(
    builder: &mut LayoutBuilder<'_>,
    display_name: &str,
    paint: &crate::backend::cosmetics_core::SevenTvNamePaint,
    fallback_color: Hsla,
    show_shadows: bool,
) -> Option<Hsla> {
    if display_name.is_empty() {
        return None;
    }

    let has_image_layer = paint
        .gradients
        .iter()
        .any(|layer| layer.image_url.is_some());
    let base_color = if has_image_layer {
        fallback_color
    } else {
        paint
            .representative_color()
            .map(rgba_to_hsla)
            .unwrap_or(fallback_color)
    };
    let current_color = hsla_to_rgba(fallback_color);
    let font_weight = font_weight_for_7tv_paint(paint);
    let byte_start = builder.pending_text.len();
    builder.add_text_run_weighted(
        display_name,
        base_color.alpha(0.0),
        true,
        false,
        font_weight,
    );
    let byte_end = builder.pending_text.len();

    builder.pending_seventv_paint = Some(std::sync::Arc::new(SevenTvLinePaint::new(
        paint.clone(),
        current_color,
        byte_start..byte_end,
        show_shadows,
    )));

    Some(base_color)
}

fn hsla_to_rgba(color: Hsla) -> crate::backend::cosmetics_core::RgbaColor {
    let rgb = color.to_rgb();
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    crate::backend::cosmetics_core::RgbaColor {
        r: channel(rgb.r),
        g: channel(rgb.g),
        b: channel(rgb.b),
        a: channel(rgb.a),
    }
}

fn font_weight_for_7tv_paint(
    paint: &crate::backend::cosmetics_core::SevenTvNamePaint,
) -> Option<FontWeight> {
    paint.text.weight.map(|weight| match weight {
        0..=450 => FontWeight::NORMAL,
        451..=550 => FontWeight::MEDIUM,
        551..=750 => FontWeight::SEMIBOLD,
        _ => FontWeight::BOLD,
    })
}

fn uses_bold_metrics(font_weight: Option<FontWeight>) -> bool {
    matches!(font_weight, Some(FontWeight::SEMIBOLD | FontWeight::BOLD))
}

/// Parse badge key to extract name and description
fn parse_badge_info(badge_key: &str, channel_login: &str) -> BadgeRenderInfo {
    let (badge_type_key, version_raw) = badge_key
        .split_once('/')
        .or_else(|| badge_key.split_once(':'))
        .map(|(name, version)| (name, Some(version.to_string())))
        .unwrap_or((badge_key, None));
    let external_provider = settings::external_badge_provider_for_key(badge_key);
    let external_name_is_in_version = matches!(
        external_provider,
        Some(
            settings::ExternalBadgeProvider::Ffz
                | settings::ExternalBadgeProvider::FfzBot
                | settings::ExternalBadgeProvider::Bttv
                | settings::ExternalBadgeProvider::BttvPro
        )
    );
    let display_type_key = if external_name_is_in_version {
        version_raw.as_deref().unwrap_or(badge_type_key)
    } else {
        badge_type_key
    };
    let name = humanize_badge_name(display_type_key);

    let (description, tier_label) = match (badge_type_key, version_raw.as_deref()) {
        ("subscriber", Some(version)) => {
            let parsed = version.parse::<u32>().ok();
            let tier = parsed.and_then(twitch_subscriber_tier_from_badge_version);
            let desc = match parsed {
                Some(v @ 1000) | Some(v @ 2000) | Some(v @ 3000) => {
                    Some(format!("Subscriber badge ({})", v))
                }
                Some(months) => Some(format!("{} month subscriber", months)),
                None => None,
            };
            (desc, tier)
        }
        ("founder", Some(version)) => {
            let parsed = version.parse::<u32>().ok();
            let tier = parsed.and_then(twitch_subscriber_tier_from_badge_version);
            let desc = parsed.map(|v| format!("Founder badge ({})", v));
            (desc, tier)
        }
        ("bits", Some(bits)) => (
            bits.parse::<u32>().ok().map(|b| format!("{} bits", b)),
            None,
        ),
        ("sub-gifter", Some(gifts)) => (
            gifts.parse::<u32>().ok().map(|g| format!("{} gifts", g)),
            None,
        ),
        ("follower", Some(version)) => (Some(format!("Follower badge ({})", version)), None),
        ("bttv-pro", Some("subscriber")) | ("betterttv-pro", Some("subscriber")) => {
            (Some("BetterTTV Pro Subscriber".to_string()), None)
        }
        _ => (None, None),
    };

    let is_ffz_authority_badge = backend::is_using_ffz_authority_badge(badge_key, channel_login);
    let source_label = if is_ffz_authority_badge {
        "FFZ Channel Badge"
    } else {
        external_provider
            .map(|provider| provider.source_label())
            .unwrap_or("Twitch Badge")
    };

    let tooltip_meta = BadgeTooltipMeta {
        source_label: source_label.into(),
        type_label: humanize_badge_name(display_type_key).into(),
        tier_label,
        is_animated: false,
    };

    BadgeRenderInfo {
        name: name.into(),
        description: description.map(Into::into),
        tooltip_meta: std::sync::Arc::new(tooltip_meta),
        background: (is_ffz_authority_badge
            && crate::backend::badges::is_moderator_badge_key(badge_key))
        .then(|| rgb(FFZ_MOD_BADGE_BACKGROUND).into()),
    }
}

fn twitch_subscriber_tier_from_badge_version(version: u32) -> Option<SharedString> {
    match version {
        1000 => Some("Tier 1".into()),
        2000 => Some("Tier 2".into()),
        3000 => Some("Tier 3".into()),
        _ => None,
    }
}

fn build_emote_tooltip_meta(
    emote: &crate::backend::emotes::Emote,
    still_image: bool,
) -> EmoteTooltipMeta {
    use crate::backend::emotes::{EmoteProvider, get_twitch_emote_by_id_any_cached};

    let canonical_twitch = if emote.provider == EmoteProvider::Twitch
        && (emote.twitch_owner_login.is_none() || emote.twitch_emote_type.is_none())
    {
        get_twitch_emote_by_id_any_cached(emote.id.as_ref())
    } else {
        None
    };
    let emote = canonical_twitch.as_deref().unwrap_or(emote);

    let provider_label: SharedString = match emote.provider {
        EmoteProvider::SevenTV => "7TV",
        EmoteProvider::BTTV => "BTTV",
        EmoteProvider::FFZ => "FFZ",
        EmoteProvider::Twitch => "Twitch",
        EmoteProvider::Emoji => "Emoji",
    }
    .into();

    let (source_label, owner_channel): (SharedString, Option<SharedString>) = match emote.provider {
        EmoteProvider::SevenTV => match emote.channel.as_deref() {
            Some("personal:7tv") => ("7TV Personal".into(), None),
            Some(channel) if !channel.is_empty() => (
                "7TV Channel".into(),
                emote.channel.clone().map(SharedString::from),
            ),
            _ => ("7TV Global".into(), None),
        },
        EmoteProvider::BTTV => match emote.channel.as_deref() {
            Some(channel) if !channel.is_empty() => (
                "BTTV Channel/Shared".into(),
                emote.channel.clone().map(SharedString::from),
            ),
            _ => ("BTTV Global".into(), None),
        },
        EmoteProvider::FFZ => match emote.channel.as_deref() {
            Some(channel) if !channel.is_empty() => (
                "FFZ Channel".into(),
                emote.channel.clone().map(SharedString::from),
            ),
            _ => ("FFZ Global".into(), None),
        },
        EmoteProvider::Twitch => {
            if let Some(emote_type) = emote.twitch_emote_type.as_deref() {
                let owner_from_fields = emote
                    .twitch_owner_login
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .map(SharedString::from);
                match emote_type {
                    "subscriptions" => ("Twitch Subscriber Emote".into(), owner_from_fields),
                    "follower" => ("Twitch Follower Emote".into(), owner_from_fields),
                    "bitstier" | "bits" => ("Twitch Bits Emote".into(), owner_from_fields),
                    "prime" => ("Twitch Prime Emote".into(), None),
                    "limitedtime" | "limited" => ("Twitch Limited Emote".into(), None),
                    "hypetrain" => ("Twitch Hype Train Emote".into(), None),
                    "rewards" => ("Twitch Rewards Emote".into(), None),
                    "turbo" => ("Twitch Turbo Emote".into(), None),
                    "globals" | "smilies" | "twofactor" => ("Twitch Global Emote".into(), None),
                    other => {
                        let pretty = humanize_compound_label(other);
                        (
                            SharedString::from(format!("Twitch {} Emote", pretty)),
                            owner_from_fields,
                        )
                    }
                }
            } else {
                match emote.channel.as_deref() {
                    Some(channel) if channel.starts_with("twitch:") => {
                        let category = channel.trim_start_matches("twitch:");
                        let pretty = match category {
                            "subscriptions" => "Subscriber".into(),
                            "follower" => "Follower".into(),
                            "bits" => "Bits".into(),
                            "channel_points" => "Channel Points".into(),
                            "prime" => "Prime".into(),
                            "turbo" => "Turbo".into(),
                            "rewards" => "Rewards".into(),
                            other => humanize_compound_label(other),
                        };
                        (SharedString::from(format!("Twitch {} Emote", pretty)), None)
                    }
                    Some(channel) if channel.starts_with("sub:") => {
                        let owner = channel.trim_start_matches("sub:");
                        let owner = if owner.is_empty() {
                            None
                        } else {
                            Some(owner.into())
                        };
                        ("Twitch Subscriber Emote".into(), owner)
                    }
                    Some(channel) if !channel.is_empty() => (
                        SharedString::from(format!(
                            "Twitch Emote • {}",
                            humanize_channel_display(channel)
                        )),
                        emote.channel.clone().map(SharedString::from),
                    ),
                    _ => ("Twitch Emote".into(), None),
                }
            }
        }
        EmoteProvider::Emoji => ("Emoji".into(), None),
    };

    let creator_name = emote.creator_name.clone().map(SharedString::from);
    let original_name = emote.original_name.clone().map(SharedString::from);
    let tooltip_preview_url = emote
        .url_4x
        .clone()
        .or_else(|| emote.url_2x.clone())
        .or_else(|| emote.url_1x.clone())
        .map(SharedString::from)
        .or_else(|| Some(SharedString::from(emote.url.clone())));

    EmoteTooltipMeta {
        provider_label,
        source_label,
        owner_channel,
        creator_name,
        original_name,
        tooltip_preview_url,
        native_width: emote.width,
        native_height: emote.height,
        is_animated: emote_is_animated(emote, &emote.url),
        is_zero_width: emote.zero_width,
        emote_id: SharedString::from(emote.id.clone()),
        provider_key: echo_core::emote_display_rules::provider_key(emote.provider),
        still_image,
    }
}

/// Lay out one base emote with its zero-width overlays.
///
/// `still[i]` forces the static first frame for the base (`i == 0`) or overlay
/// `i - 1`; missing entries animate as usual.
fn add_emote_render_unit(
    builder: &mut LayoutBuilder<'_>,
    base: &crate::backend::emotes::Emote,
    overlays: &[std::sync::Arc<crate::backend::emotes::Emote>],
    still: &[bool],
    remove_emote_spacing: bool,
) {
    let render_url = chat_display_url_shared_for_scale(base, super::layout_scale::current());

    let base_still = still.first().copied().unwrap_or(false);
    let is_animated = !base_still && emote_is_animated(base, &render_url);
    if crate::log::emote_trace_enabled() {
        crate::log_emotes!(
            debug,
            "[LAYOUT] Emote '{}': is_animated={} (flag={}, url_heuristic={}), provider={:?}, url={}, overlays={}",
            base.name,
            is_animated,
            base.is_animated,
            is_animated_url(&render_url),
            base.provider,
            render_url,
            overlays.len()
        );
    }

    let had_text = builder.flush_text_segment();

    builder.flush_layered_emote();

    if had_text {
        builder.last_token_type = LastTokenType::Text;
    }

    let ((base_width, base_height), base_dimensions_pending) =
        emote_dimensions_from_decoded_media(base, &render_url);
    if base_dimensions_pending {
        builder.track_pending_native_dimension_url(&render_url);
    }
    advance_before_emote_like(builder, remove_emote_spacing, base_height);

    let base_tooltip_meta = std::sync::Arc::new(build_emote_tooltip_meta(base, base_still));

    let mut layered_emote = LayeredEmote::new(
        SharedString::from(render_url),
        SharedString::from(base.name.clone()),
        base_width,
        base_height,
        is_animated,
        Some(std::sync::Arc::clone(&base_tooltip_meta)),
    );

    for (overlay_index, overlay) in overlays.iter().enumerate() {
        let overlay_url =
            chat_display_url_shared_for_scale(overlay, super::layout_scale::current());
        let overlay_still = still.get(overlay_index + 1).copied().unwrap_or(false);
        let overlay_is_animated = !overlay_still && emote_is_animated(overlay, &overlay_url);
        let ((overlay_width, overlay_height), overlay_dimensions_pending) =
            emote_dimensions_from_decoded_media(overlay, &overlay_url);
        if overlay_dimensions_pending {
            builder.track_pending_native_dimension_url(&overlay_url);
        }
        layered_emote.add_overlay(
            SharedString::from(overlay_url),
            SharedString::from(overlay.name.clone()),
            overlay_width,
            overlay_height,
            overlay_is_animated,
            Some(std::sync::Arc::new(build_emote_tooltip_meta(
                overlay,
                overlay_still,
            ))),
        );
    }

    let (bbox_width, _) = layered_emote.bounds();
    if builder.current_x + bbox_width > builder.right_boundary
        && builder.current_x > MESSAGE_PADDING_LEFT
    {
        builder.wrap_line();
    }

    let image_type = ImageType::Emote {
        name: SharedString::from(base.name.clone()),
        tooltip_meta: Some(base_tooltip_meta),
    };

    builder.pending_layered_emote = Some(PendingLayeredImage {
        emote: layered_emote,
        x: builder.current_x,
        image_type,
    });

    builder.advance(base_width);

    builder.last_token_type = LastTokenType::Emote;
}

/// The message's slots and a cursor over its emote slots. Render units keep
/// every emote slot in order (overlays included), so the n-th emote laid out is
/// the n-th emote slot.
struct EmoteSlotSource<'a> {
    slots: &'a [crate::backend::slots::MessageSlot],
    cursor: &'a mut usize,
}

impl EmoteSlotSource<'_> {
    fn next_emote_slot(&mut self) -> Option<usize> {
        while let Some(slot) = self.slots.get(*self.cursor) {
            let index = *self.cursor;
            *self.cursor += 1;
            if slot.emote_source_text().is_some() {
                return Some(index);
            }
        }
        None
    }
}

/// Lay out an emote unit under the active display rules.
///
/// An emote shown as text renders exactly as its code would as plain message
/// text, so it also ends the zero-width stack: the next image emote starts a new
/// unit as its base, the way a zero-width emote with no base renders.
fn add_emote_render_unit_with_rules(
    builder: &mut LayoutBuilder<'_>,
    unit: &EmoteRenderUnit,
    mut source: EmoteSlotSource<'_>,
    rules: &settings::CompiledEmoteDisplayRules,
    channel: &str,
    text_color: Hsla,
    remove_emote_spacing: bool,
) {
    let mut flags: SmallVec<[settings::EmoteDisplayFlags; 5]> =
        SmallVec::with_capacity(unit.overlays.len() + 1);
    let mut slot_indices: SmallVec<[Option<usize>; 5]> =
        SmallVec::with_capacity(unit.overlays.len() + 1);
    unit.for_each_emote(|emote| {
        flags.push(rules.flags_for(emote.provider, &emote.id, channel));
        slot_indices.push(source.next_emote_slot());
    });
    let emote_at = |index: usize| {
        if index == 0 {
            &unit.base
        } else {
            &unit.overlays[index - 1]
        }
    };

    let mut index = 0;
    while index < flags.len() {
        if flags[index].show_as_text {
            let emote = emote_at(index);
            let slot = slot_indices[index];
            let code = slot
                .and_then(|slot| source.slots[slot].emote_source_text())
                .unwrap_or(&emote.name);
            let glued = slot.is_some_and(|slot| {
                echo_core::slot_parser::emote_slot_is_glued(source.slots, slot)
            });
            add_emote_code_as_text(builder, emote, code, glued, text_color);
            index += 1;
            continue;
        }
        let end = (index + 1..flags.len())
            .find(|&next| flags[next].show_as_text)
            .unwrap_or(flags.len());
        let still: SmallVec<[bool; 5]> = flags[index..end]
            .iter()
            .map(|flags| flags.still_image)
            .collect();
        add_emote_render_unit(
            builder,
            emote_at(index),
            &unit.overlays[index..end - 1],
            &still,
            remove_emote_spacing,
        );
        index = end;
    }
}

/// Render an emote's `code` (what was typed) as message text. The code joins
/// the copy/selection source like any word, and an emote hitbox over it keeps
/// the tooltip (a still preview) and the emote context menu with "Show image
/// again" reachable.
fn add_emote_code_as_text(
    builder: &mut LayoutBuilder<'_>,
    emote: &crate::backend::emotes::Emote,
    code: &str,
    glued: bool,
    color: Hsla,
) {
    builder.flush_layered_emote();
    // The parser drops the whitespace before an emote and keeps the one after
    // it, so restore a separating space the way the text would have had one,
    // except after punctuation the emote was typed glued to, as in "(KEKW)".
    if matches!(builder.last_token_type, LastTokenType::Emote) {
        builder.ensure_source_separator();
    } else if !glued && !builder.last_text_ended_with_whitespace && !builder.source_text.is_empty()
    {
        push_text_segment(builder, " ", color);
    }
    builder.flush_text_segment();

    push_text_segment(builder, code, color);
    let render_url = chat_display_url_shared_for_scale(emote, super::layout_scale::current());
    builder.add_hitbox(HitboxKind::Emote {
        name: SharedString::from(emote.name.clone()),
        url: SharedString::from(render_url),
        meta: Some(std::sync::Arc::new(build_emote_tooltip_meta(emote, true))),
    });
    builder.flush_text_segment();
    builder.last_token_type = LastTokenType::Text;
}

/// Return the selected source URL's pixel density for native-size conversion.
///
/// Echo stores Twitch's `3.0` and BTTV's `3x` maximum variants in `url_4x`.
/// Twitch's normal `3.0` response is four times the 1x geometry (112px for a
/// 28px emote), so URL labels are not used as a literal divisor here. Historical
/// Twitch exceptions use their verified normalization factors, including the
/// fractional heart-emote density.
#[inline]
fn selected_emote_source_density(emote: &crate::backend::emotes::Emote, url: &str) -> f32 {
    echo_emote_core::model::emote_source_density(emote, url)
}

/// Provider geometry fixes the image box before pixels arrive. When it is
/// absent, resident or remembered sizes from any tier avoid a square fallback.
#[inline]
fn emote_dimensions_from_decoded_media(
    emote: &crate::backend::emotes::Emote,
    url: &str,
) -> ((f32, f32), bool) {
    let logical = emote
        .width
        .zip(emote.height)
        .filter(|(width, height)| *width > 0 && *height > 0)
        .or_else(|| {
            (emote.provider == EmoteProvider::Twitch)
                .then(|| echo_emote_core::twitch_base_dimensions(&emote.id))
        })
        .or_else(|| {
            [
                Some(url),
                emote.url_1x.as_deref(),
                emote.url_2x.as_deref(),
                emote.url_4x.as_deref(),
                Some(emote.url.as_ref()),
            ]
            .into_iter()
            .flatten()
            .find_map(|candidate| {
                logical_native_dimensions(
                    echo_chat_render::ports::decoded_image_size(candidate),
                    selected_emote_source_density(emote, candidate),
                    None,
                )
            })
        });
    let (width, height) = calculate_emote_dimensions(
        logical.map(|(width, _)| width),
        logical.map(|(_, height)| height),
        emote_size(),
    );
    ((width, height), logical.is_none())
}

/// Moderation buttons act on a chatter: skip system rows, the viewer's own
/// messages and the broadcaster's.
fn message_takes_mod_buttons(msg: &ChatMessage) -> bool {
    if msg.flags.is_system || msg.sender.is_empty() {
        return false;
    }
    let channel = msg.channel.trim_start_matches('#');
    !msg.sender.eq_ignore_ascii_case(channel)
        && !msg
            .sender
            .eq_ignore_ascii_case(&settings::get_username_lowercase())
}

fn humanize_compound_label(raw: &str) -> String {
    raw.split(['_', '-'])
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => {
                    let mut out = c.to_uppercase().collect::<String>();
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn humanize_channel_display(channel: &str) -> String {
    let mut chars = channel.chars();
    match chars.next() {
        Some(first) => {
            let mut out = first.to_uppercase().collect::<String>();
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}

/// Convert badge key to human-readable name
fn humanize_badge_name(key: &str) -> String {
    match key {
        "subscriber" => "Subscriber".to_string(),
        "moderator" => "Moderator".to_string(),
        "lead_moderator" | "lead-moderator" => "Lead Moderator".to_string(),
        "broadcaster" => "Broadcaster".to_string(),
        "vip" => "VIP".to_string(),
        "bits" => "Bits".to_string(),
        "sub-gifter" => "Sub Gifter".to_string(),
        "premium" => "Turbo".to_string(),
        "partner" => "Partner".to_string(),
        "founder" => "Founder".to_string(),
        "artist-badge" => "Artist".to_string(),
        other => {
            let mut out = String::with_capacity(other.len());
            for (i, word) in other.split('-').enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let mut chars = word.chars();
                if let Some(first) = chars.next() {
                    let up = first.to_uppercase().next().unwrap_or(first);
                    out.push(up);
                }
                out.extend(chars);
            }
            out
        }
    }
}

/// Type of the last token processed (for spacing decisions)
#[derive(Clone, Copy, Debug, PartialEq)]
enum LastTokenType {
    /// No token processed yet or at start of content
    None,
    /// Last token was text (including space)
    Text,
    /// Last token was an emote
    Emote,
    /// Last token was a URL, mention, or embed link
    Link,
}

struct PendingLayeredImage {
    emote: LayeredEmote,
    x: f32,
    image_type: ImageType,
}

/// Helper for building layout incrementally with dynamic line heights
struct LayoutBuilder<'a> {
    /// Right boundary for content (absolute X position)
    right_boundary: f32,
    /// Current X position
    current_x: f32,
    /// Current Y position (start of current line)
    current_y: f32,
    /// Current line index
    current_line: u32,

    /// Accumulated text for current segment
    pending_text: String,
    /// Number of chars in pending_text (Unicode scalar values)
    /// PERF: Avoids rescanning pending_text on flush.
    pending_char_count: usize,
    /// Text runs for current segment
    pending_runs: SmallVec<[TextRunData; 4]>,
    /// Start X of current text segment
    text_segment_start_x: f32,
    /// Exact 7TV paint spec for the username inside the current text segment.
    pending_seventv_paint: Option<std::sync::Arc<SevenTvLinePaint>>,
    /// PERF: Character positions for current segment (built during add_text_run, reused in flush)
    pending_char_positions: SmallVec<[f32; 24]>,

    /// X position where message content starts (after timestamp/badges/username)
    /// Used for indenting wrapped lines
    content_start_x: f32,
    /// Whether we've passed the username (content has started)
    content_started: bool,
    /// Whether timestamps are shown (affects wrap indent calculation)
    show_timestamp: bool,

    /// All completed segments (from committed lines)
    segments: SmallVec<[LayoutSegment; 8]>,
    /// All shaped line data
    shaped_lines: SmallVec<[ShapedLineData; 4]>,
    /// All hitbox regions
    /// PERF: most messages have 8-16 hitboxes
    /// (badges, username, mentions, emotes, URLs) so 16 avoids heap allocation
    hitboxes: SmallVec<[HitboxRegion; 16]>,
    /// Mention highlight regions (x, y, width, height)
    mention_highlights: SmallVec<[(f32, f32, f32, f32); 4]>,

    /// Selection map line offsets
    line_offsets: Vec<usize>,
    /// Total source text length in characters (Unicode scalar values)
    source_offset: usize,
    /// Absolute X positions for each character (for selection highlighting)
    source_char_positions: SmallVec<[f32; 48]>,
    /// Source text for clipboard (built alongside positions for consistency)
    source_text: String,

    /// Twitch/BTTV URLs that did not have decoded dimensions when this layout
    /// was built. They are transferred to the layout for a targeted refresh
    /// when cache residency later makes native geometry available.
    pending_native_dimension_urls: SmallVec<[SharedString; 4]>,
    reserved_badge_channel: Option<std::sync::Arc<str>>,

    /// Index of the last emote in the current line buffer (for zero-width overlay attachment)
    last_emote_buffer_idx: Option<usize>,
    /// Height of the most recent media-like token, used to scale visual gaps around large emotes.
    last_media_height: Option<f32>,

    /// Pending layered emote being built (for centering zero-width overlays)
    /// When a regular emote is encountered, a LayeredEmote is created here.
    /// Zero-width emotes are added as overlays to this pending emote.
    /// The pending emote is flushed when we encounter non-emote content or wrap/finalize.
    pending_layered_emote: Option<PendingLayeredImage>,

    /// Type of the last token processed (for spacing decisions between tokens)
    last_token_type: LastTokenType,
    /// Whether the most recently added text run ended with whitespace.
    /// Used to preserve text-like spacing at token boundaries (e.g. text + URL).
    last_text_ended_with_whitespace: bool,

    /// Buffer for current line items (committed on wrap or finalize)
    line_buffer: LineBuffer,

    /// Y positions for each committed line (for selection hit testing)
    line_y_positions: SmallVec<[f32; 8]>,

    /// Index where hitboxes for current line start (for Y adjustment after commit)
    line_hitbox_start_idx: usize,
    /// Index where mention highlights for current line start (for Y adjustment after commit)
    line_mention_highlight_start_idx: usize,

    /// Optional window for accurate GPUI text measurement (pixel-perfect when available)
    window: Option<&'a mut Window>,

    /// Whether message content text should be struck through.
    strike_content: bool,

    /// Measure and paint subsequent text runs with the ASCII-art monospace
    /// family. Enabled only after the timestamp/badges/username header so the
    /// header retains its normal UI typography.
    monospace_text: bool,

    /// Count of animated emotes encountered during layout building
    /// PERF: Tracked here, transferred to layout in finalize() for render optimization
    animated_emote_count: u8,

    /// PERF: line_height() cached for the lifetime of this build (2 Mutex locks per call).
    /// Settings changes force a full relayout via cache invalidation, so this is invariant.
    cached_line_height: f32,
    /// Where content starts after the timestamp column (and any mod buttons
    /// before it); wrapped lines indent here when timestamps are shown.
    timestamp_column_end_x: f32,
}

impl<'a> LayoutBuilder<'a> {
    fn new_with_y_offset(
        right_boundary: f32,
        show_timestamp: bool,
        y_offset: f32,
        mut window: Option<&'a mut Window>,
        strike_content: bool,
    ) -> Self {
        let initial_y = MESSAGE_PADDING + y_offset;
        let mut line_y_positions = SmallVec::new();
        line_y_positions.push(initial_y);

        let cached_line_height = line_height();
        if let Some(win) = window.as_deref_mut() {
            super::layout::refresh_timestamp_width_probe(win);
        }
        let cached_timestamp_width = timestamp_width();

        Self {
            right_boundary,
            current_x: MESSAGE_PADDING_LEFT,
            current_y: initial_y,
            current_line: 0,
            pending_text: String::new(),
            pending_char_count: 0,
            pending_runs: SmallVec::new(),
            text_segment_start_x: MESSAGE_PADDING_LEFT,
            pending_seventv_paint: None,
            pending_char_positions: SmallVec::new(),
            content_start_x: MESSAGE_PADDING_LEFT,
            content_started: false,
            show_timestamp,
            segments: SmallVec::new(),
            shaped_lines: SmallVec::new(),
            hitboxes: SmallVec::new(),
            mention_highlights: SmallVec::new(),
            line_offsets: vec![0],
            source_offset: 0,
            source_char_positions: SmallVec::new(),
            source_text: String::new(),
            pending_native_dimension_urls: SmallVec::new(),
            reserved_badge_channel: None,
            last_emote_buffer_idx: None,
            last_media_height: None,
            pending_layered_emote: None,
            last_token_type: LastTokenType::None,
            last_text_ended_with_whitespace: false,
            line_buffer: LineBuffer::new(cached_line_height),
            line_y_positions,
            line_hitbox_start_idx: 0,
            line_mention_highlight_start_idx: 0,
            window,
            strike_content,
            monospace_text: false,
            animated_emote_count: 0,
            cached_line_height,
            timestamp_column_end_x: MESSAGE_PADDING_LEFT + cached_timestamp_width + TIMESTAMP_GAP,
        }
    }

    /// Mark the start of message content (after username)
    /// Wrapped lines will indent to this position
    fn mark_content_start(&mut self) {
        self.content_start_x = self.current_x;
        self.content_started = true;
    }

    #[inline]
    fn track_pending_native_dimension_url(&mut self, url: &str) {
        if !self
            .pending_native_dimension_urls
            .iter()
            .any(|pending| pending.as_ref() == url)
        {
            self.pending_native_dimension_urls
                .push(SharedString::from(url));
        }
    }

    /// Measure text width accurately using GPUI when window is available, else use cached metrics
    fn measure_text_width(&mut self, text: &str, is_bold: bool) -> f32 {
        if self.monospace_text {
            if let Some(window) = self.window.as_mut() {
                let run = TextRun {
                    len: text.len(),
                    font: Font {
                        family: text_run_intern::mono_font_family(),
                        weight: if is_bold {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        },
                        ..Default::default()
                    },
                    color: colors::text_primary(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                return window
                    .text_system()
                    .shape_line(
                        SharedString::from(text),
                        px(scaled_font_size()),
                        std::slice::from_ref(&run),
                        None,
                    )
                    .width()
                    .as_f32();
            }

            return text.chars().count() as f32 * scaled_font_size() * 0.6;
        }
        if let Some(window) = self.window.as_mut() {
            FONT_METRICS.measure_string_gpui(text, is_bold, window)
        } else {
            FONT_METRICS.measure_string_bold(text, is_bold)
        }
    }

    /// Flush pending layered emote to line buffer
    ///
    /// Call this before adding non-emote content, wrapping, or finalizing.
    /// Converts the pending LayeredEmote with all its overlays into a LineItem.
    fn flush_layered_emote(&mut self) {
        if let Some(PendingLayeredImage {
            mut emote,
            x,
            image_type,
        }) = self.pending_layered_emote.take()
        {
            if emote.base.is_animated {
                self.animated_emote_count = self.animated_emote_count.saturating_add(1);
            }
            for overlay in &emote.overlays {
                if overlay.is_animated {
                    self.animated_emote_count = self.animated_emote_count.saturating_add(1);
                }
            }

            let (bbox_width, bbox_height) = emote.bounds();
            let overflow_width = (bbox_width - emote.base.width).max(0.0);
            if overflow_width > 0.0 {
                self.current_x += overflow_width;
                self.text_segment_start_x = self.current_x;
            }

            let (source_start_char, source_len_chars) =
                self.append_source_emote_code(emote.base.name.as_ref());

            self.line_buffer.add_layered_emote(
                emote,
                x,
                image_type,
                source_start_char,
                source_len_chars,
            );
            self.last_emote_buffer_idx = Some(self.line_buffer.len() - 1);
            self.last_media_height = Some(bbox_height);
        }
    }

    /// Splice a single separating space into `source_text`/`source_char_positions`
    /// if the content appended so far didn't already end in whitespace (and
    /// there's something to separate from at all -- never insert a leading
    /// space at the very start of the message).
    ///
    /// Two call sites need this, both working around the fact that
    /// `add_text_run_weighted` appends its text verbatim with no gap logic of
    /// its own, mirroring what the *visual* spacing functions
    /// (`advance_before_emote_like` / `advance_before_text_like`) already do for
    /// `current_x`:
    /// - `append_source_emote_code`, before splicing in an emote's code (e.g.
    ///   text "word" + emote -> "word Kappa", and back-to-back emotes ->
    ///   "Kappa Kappa" rather than "KappaKappa").
    /// - the `SlotRenderUnit::Text` handler, before appending text that
    ///   immediately follows an emote: that handler deliberately strips the
    ///   text's authored leading whitespace (relying on the synthesized visual
    ///   gap instead), so without this call `source_text` would glue the
    ///   emote's just-spliced code directly onto the next word.
    ///
    /// Position values pushed here are not meaningful for x-lookup (emotes are
    /// hit-tested via their own segment geometry, not a char-position binary
    /// search) -- they only need to exist so `source_char_positions.len()` keeps
    /// tracking `source_text.chars().count()`, the invariant `source_char_len()`
    /// and every range computation depend on.
    fn ensure_source_separator(&mut self) {
        if !self.last_text_ended_with_whitespace && !self.source_text.is_empty() {
            self.source_char_positions.push(self.current_x);
            self.source_text.push(' ');
            self.source_offset += 1;
            self.last_text_ended_with_whitespace = true;
        }
    }

    /// Append exact source text whose rendered object begins at `current_x`.
    ///
    /// Unicode emoji can contain several scalar values while occupying one image
    /// hitbox. Give each scalar the same boundary position so source ranges stay
    /// exact without allowing pointer selection to split the rendered image.
    fn append_source_text_at_current_x(&mut self, text: &str) -> (usize, usize) {
        let start_char = self.source_offset;
        let mut len_chars = 0usize;
        for ch in text.chars() {
            self.source_char_positions.push(self.current_x);
            self.source_text.push(ch);
            len_chars += 1;
        }
        self.source_offset += len_chars;
        if let Some(last) = text.chars().last() {
            self.last_text_ended_with_whitespace = last.is_whitespace();
        }

        (start_char, len_chars)
    }

    /// Append an emote's text code into the flat `source_text`/`source_char_positions`
    /// buffers so drag-selection and Ctrl+C copy include emote codes, mirroring how
    /// `add_text_run_weighted` builds these buffers for real text runs. See
    /// `ensure_source_separator` for the separator semantics.
    ///
    /// Clears `last_text_ended_with_whitespace` afterward (codes never end in
    /// whitespace) so a following emote or text run inserts its own separator
    /// in turn.
    ///
    /// Returns `(source_start_char, source_len_chars)` covering just the code
    /// itself; the synthesized separator (if any) is excluded from the range.
    fn append_source_emote_code(&mut self, code: &str) -> (usize, usize) {
        if code.is_empty() {
            return (self.source_offset, 0);
        }

        self.ensure_source_separator();
        self.append_source_text_at_current_x(code)
    }

    /// Add a text run to the current segment
    fn add_text_run(&mut self, text: &str, color: Hsla, bold: bool, underline: bool) {
        self.add_text_run_weighted(text, color, bold, underline, None);
    }

    fn add_text_run_weighted(
        &mut self,
        text: &str,
        color: Hsla,
        bold: bool,
        underline: bool,
        font_weight: Option<FontWeight>,
    ) {
        if text.is_empty() {
            return;
        }

        if self.pending_text.is_empty() {
            self.text_segment_start_x = self.current_x;
        }

        let len = text.len();
        let mut run = TextRunData::new(len, color);
        if self.monospace_text {
            run.font_family = text_run_intern::mono_font_family();
        }
        if bold {
            run = run.bold();
        }
        if let Some(font_weight) = font_weight {
            run = run.with_font_weight(font_weight);
        }
        if underline {
            run = run.underlined();
        }
        if self.strike_content && self.content_started {
            run = run.struck();
        }

        let measure_bold = font_weight.map_or(bold, |weight| uses_bold_metrics(Some(weight)));

        let start_x = self.current_x;
        let pending_before = self.pending_char_positions.len();
        self.current_x = FONT_METRICS.batch_char_positions_into(
            text,
            start_x,
            measure_bold,
            &mut self.source_char_positions,
            &mut self.pending_char_positions,
            &mut self.source_text,
        );
        let char_count = self
            .pending_char_positions
            .len()
            .saturating_sub(pending_before);

        if self.window.is_some() && char_count > 0 {
            let source_start = self.source_char_positions.len().saturating_sub(char_count);
            let has_precise_budget = char_count <= PRECISE_CHAR_POSITION_MAX;

            if has_precise_budget {
                let font_size = scaled_font_size();
                let run = TextRun {
                    len: text.len(),
                    font: Font {
                        family: if self.monospace_text {
                            text_run_intern::mono_font_family()
                        } else {
                            text_run_intern::system_font_family()
                        },
                        weight: if measure_bold {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        },
                        ..Default::default()
                    },
                    color: colors::text_primary(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let Some(window) = self.window.as_mut() else {
                    debug_assert!(
                        false,
                        "add_text_run_weighted: window was None inside is_some() guard"
                    );
                    return;
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    px(font_size),
                    std::slice::from_ref(&run),
                    None,
                );

                for (i, (byte_idx, _)) in text.char_indices().enumerate() {
                    let x = start_x + shaped.x_for_index(byte_idx).as_f32();
                    self.pending_char_positions[pending_before + i] = x;
                    self.source_char_positions[source_start + i] = x;
                }
                self.current_x = start_x + shaped.width().as_f32();
            } else {
                let accurate_width = self.measure_text_width(text, measure_bold);
                let estimated_width = (self.current_x - start_x).max(0.0);
                if estimated_width > f32::EPSILON {
                    let scale = accurate_width / estimated_width;
                    for i in 0..char_count {
                        let adjusted = start_x
                            + (self.pending_char_positions[pending_before + i] - start_x) * scale;
                        self.pending_char_positions[pending_before + i] = adjusted;
                        self.source_char_positions[source_start + i] = adjusted;
                    }
                } else {
                    for i in 0..char_count {
                        self.pending_char_positions[pending_before + i] = start_x;
                        self.source_char_positions[source_start + i] = start_x;
                    }
                }
                self.current_x = start_x + accurate_width;
            }
        }

        self.pending_text.push_str(text);
        self.pending_runs.push(run);
        self.pending_char_count += char_count;
        self.source_offset += char_count;
        self.last_text_ended_with_whitespace = text.chars().last().is_some_and(char::is_whitespace);
    }

    /// Add an image segment to the line buffer
    fn add_image(
        &mut self,
        url: &str,
        width: f32,
        height: f32,
        is_animated: bool,
        image_type: ImageType,
        source_range: Option<(usize, usize)>,
    ) {
        self.flush_text_segment();

        if is_animated {
            self.animated_emote_count = self.animated_emote_count.saturating_add(1);
        }

        let url_owned = SharedString::from(url);
        self.line_buffer.add_image(
            url_owned,
            self.current_x,
            width,
            height,
            is_animated,
            image_type,
            source_range,
        );

        self.last_emote_buffer_idx = self.line_buffer.last_image_index();
        self.last_media_height = Some(height);
    }

    /// Add an inline box (like NEW badge) to the line buffer
    fn add_inline_box(&mut self, text: &str, text_color: Hsla, px_h: Pixels, _px_v: Pixels) {
        self.flush_text_segment();

        let text_width = self.measure_text_width(text, false);
        let px_h_f32: f32 = px_h.into();
        let box_width = text_width + px_h_f32 * 2.0;
        let box_height = self.cached_line_height * 0.8;

        let mut bg_color = text_color;
        bg_color.a = 0.15;

        let text_owned = SharedString::from(text);
        self.line_buffer.add_inline_box(
            text_owned,
            self.current_x,
            box_width,
            box_height,
            bg_color,
            text_color,
            11.0 * settings::get_font_scale(),
        );

        self.current_x += box_width + badge_gap();
        self.text_segment_start_x = self.current_x;
    }

    /// Inline moderation button: a chip outside the selectable text with its
    /// own hitbox, placed before the timestamp.
    fn add_mod_button(&mut self, label: &str, command: &str, destructive: bool) {
        self.flush_text_segment();
        let chip_width = self.measure_text_width(label, false) + 3.0 * 2.0;
        if self.current_x > MESSAGE_PADDING_LEFT
            && self.current_x + chip_width > self.right_boundary
        {
            self.wrap_line();
        }
        let start_x = self.current_x;
        let color = if destructive {
            colors::error()
        } else {
            colors::text_muted()
        };
        self.add_inline_box(label, color, px(3.0), px(1.0));
        let width = (self.current_x - badge_gap() - start_x).max(1.0);
        self.hitboxes.push(HitboxRegion {
            bounds: Bounds::new(
                point(px(start_x), px(self.current_y)),
                size(px(width), px(self.cached_line_height)),
            ),
            kind: HitboxKind::ModAction {
                label: SharedString::from(label.to_string()),
                command: SharedString::from(command.to_string()),
            },
        });
    }

    /// Add a hitbox at current position
    fn add_hitbox(&mut self, kind: HitboxKind) {
        let text_width = self.current_x - self.text_segment_start_x;
        if text_width > 0.0 {
            self.hitboxes.push(HitboxRegion {
                bounds: Bounds::new(
                    point(px(self.text_segment_start_x), px(self.current_y)),
                    size(px(text_width), px(self.cached_line_height)),
                ),
                kind,
            });
        }
    }

    /// Add a hitbox GUARANTEED - even if text width is 0, creates a minimum clickable area
    /// This is critical for usernames and mentions to always be clickable
    fn add_hitbox_guaranteed(&mut self, kind: HitboxKind) {
        let text_width = self.current_x - self.text_segment_start_x;
        let effective_width = text_width.max(8.0);

        self.hitboxes.push(HitboxRegion {
            bounds: Bounds::new(
                point(px(self.text_segment_start_x), px(self.current_y)),
                size(px(effective_width), px(self.cached_line_height)),
            ),
            kind,
        });
    }

    /// Advance X position (for fixed-width elements)
    fn advance(&mut self, width: f32) {
        self.current_x = self.current_x.max(self.text_segment_start_x + width);
        self.text_segment_start_x = self.current_x;
    }

    /// Advance X position by a relative amount
    fn advance_by(&mut self, amount: f32) {
        self.current_x += amount;
        self.text_segment_start_x = self.current_x;
    }

    /// Set absolute X position (for fixed-width columns like timestamp)
    fn set_x(&mut self, x: f32) {
        self.current_x = x;
        self.text_segment_start_x = x;
    }

    /// Add mention highlight background
    fn add_mention_highlight(&mut self, x: f32, y: f32, width: f32) {
        self.mention_highlights
            .push((x, y, width, self.cached_line_height));
    }

    /// Flush pending text into the line buffer
    /// Returns true if text was actually flushed, false if buffer was empty
    fn flush_text_segment(&mut self) -> bool {
        if self.pending_text.is_empty() {
            return false;
        }

        let line_idx = self.shaped_lines.len();
        let seg_len_chars = self.pending_char_count;
        let seg_start_char = self.source_offset.saturating_sub(seg_len_chars);
        let width = self.current_x - self.text_segment_start_x;

        let mut char_positions = std::mem::take(&mut self.pending_char_positions);
        char_positions.push(self.current_x);

        let text: SharedString = std::mem::take(&mut self.pending_text).into();
        let runs = std::mem::take(&mut self.pending_runs);

        let mut gpui_runs: SmallVec<[TextRun; 4]> = SmallVec::with_capacity(runs.len().max(1));
        for run in &runs {
            gpui_runs.push(text_run_intern::gpui_run_from_data(run));
        }
        if gpui_runs.is_empty() {
            gpui_runs.push(text_run_intern::default_gpui_run(
                text.len(),
                colors::text_primary(),
            ));
        }

        let bidi = BidiAnalysis::new(text.as_ref());
        let (visual_char_positions, visual_to_logical) = if bidi.needs_reordering {
            (
                bidi.build_visual_char_positions(&char_positions, seg_len_chars)
                    .into_vec(),
                bidi.visual_to_logical.into_vec(),
            )
        } else {
            (Vec::new(), Vec::new())
        };

        self.shaped_lines.push(ShapedLineData {
            text,
            gpui_runs_hash: hash_gpui_text_runs(&gpui_runs),
            gpui_runs,
            char_positions,
            visual_char_positions,
            visual_to_logical,
            source_start_char: seg_start_char,
            source_len_chars: seg_len_chars,
            seventv_paint: self.pending_seventv_paint.take(),
        });

        self.line_buffer
            .add_text(line_idx, self.text_segment_start_x, width);

        self.text_segment_start_x = self.current_x;
        self.pending_char_count = 0;
        true
    }

    /// Commit the current line buffer to final segments.
    ///
    /// This finalizes the line, calculating the actual line height based on
    /// the tallest element and positioning all items with proper baseline alignment.
    fn commit_line(&mut self) {
        if self.line_buffer.is_empty() {
            return;
        }

        let line_buffer = std::mem::replace(
            &mut self.line_buffer,
            LineBuffer::new(self.cached_line_height),
        );
        let (new_segments, actual_height) = line_buffer.finalize(self.current_y);

        let y_adjustment = actual_height - self.cached_line_height;
        if y_adjustment > 0.0 {
            for hitbox in self.hitboxes[self.line_hitbox_start_idx..].iter_mut() {
                match &hitbox.kind {
                    HitboxKind::Username { .. }
                    | HitboxKind::Url { .. }
                    | HitboxKind::Mention { .. }
                    | HitboxKind::Emote { .. } => {
                        let old_y: f32 = hitbox.bounds.origin.y.into();
                        hitbox.bounds.origin.y = px(old_y + y_adjustment);
                    }
                    _ => {}
                }
            }

            for (_, y, _, _) in
                self.mention_highlights[self.line_mention_highlight_start_idx..].iter_mut()
            {
                *y += y_adjustment;
            }
        }

        for segment in &new_segments {
            match segment {
                LayoutSegment::Image {
                    url,
                    x,
                    y,
                    width,
                    height,
                    image_type,
                    overlays,
                    ..
                } => {
                    let hitbox_kind = if overlays.is_empty() {
                        match image_type {
                            ImageType::TwitchGif { alt_text } => HitboxKind::Url {
                                url: url.clone(),
                                display_text: alt_text.clone(),
                                embed_id: 0,
                            },
                            ImageType::Emote { name, tooltip_meta } => HitboxKind::Emote {
                                name: name.clone(),
                                url: url.clone(),
                                meta: tooltip_meta.clone(),
                            },
                            ImageType::Badge {
                                name,
                                description,
                                tooltip_meta,
                                ..
                            } => HitboxKind::Badge {
                                name: name.clone(),
                                url: url.clone(),
                                description: description.clone(),
                                meta: tooltip_meta.clone(),
                            },
                        }
                    } else {
                        let mut emotes = vec![];
                        if let ImageType::Emote { name, tooltip_meta } = image_type {
                            emotes.push(LayeredTooltipEmote {
                                name: name.clone(),
                                url: url.clone(),
                                meta: tooltip_meta.clone(),
                            });
                        }
                        for overlay in overlays {
                            emotes.push(LayeredTooltipEmote {
                                name: overlay.name.clone(),
                                url: overlay.url.clone(),
                                meta: overlay.tooltip_meta.clone(),
                            });
                        }
                        HitboxKind::LayeredEmote { emotes }
                    };

                    self.hitboxes.push(HitboxRegion {
                        bounds: Bounds::new(point(px(*x), px(*y)), size(px(*width), px(*height))),
                        kind: hitbox_kind,
                    });
                }
                LayoutSegment::LayeredImage {
                    layers,
                    x,
                    y,
                    width,
                    height,
                    ..
                } => {
                    let emotes = layers
                        .iter()
                        .map(|layer| LayeredTooltipEmote {
                            name: layer.name.clone(),
                            url: layer.url.clone(),
                            meta: layer.tooltip_meta.clone(),
                        })
                        .collect();

                    self.hitboxes.push(HitboxRegion {
                        bounds: Bounds::new(point(px(*x), px(*y)), size(px(*width), px(*height))),
                        kind: HitboxKind::LayeredEmote { emotes },
                    });
                }
                _ => {}
            }
        }

        self.segments.extend(new_segments);

        self.current_y += actual_height;

        self.line_hitbox_start_idx = self.hitboxes.len();
        self.line_mention_highlight_start_idx = self.mention_highlights.len();
    }

    /// Wrap to next line
    fn wrap_line(&mut self) {
        self.flush_text_segment();
        self.flush_layered_emote();

        self.commit_line();

        self.current_line += 1;

        self.line_y_positions.push(self.current_y);

        self.last_emote_buffer_idx = None;
        self.last_media_height = None;

        let indent = if self.content_started {
            if self.show_timestamp {
                self.timestamp_column_end_x
            } else {
                self.content_start_x
            }
        } else {
            MESSAGE_PADDING_LEFT
        };

        let indent =
            indent.min((self.right_boundary - MIN_WRAPPED_CONTENT_WIDTH).max(MESSAGE_PADDING_LEFT));

        self.current_x = indent;
        self.text_segment_start_x = indent;

        self.line_offsets.push(self.source_offset);
    }

    /// Commit an authored hard line break without shaping it as a whitespace
    /// glyph. Source and selection indices still retain the original newline
    /// bytes as Unicode scalar positions.
    fn hard_break(&mut self, source_break: &str) {
        self.flush_text_segment();
        for ch in source_break.chars() {
            self.source_char_positions.push(self.current_x);
            self.source_text.push(ch);
            self.source_offset += 1;
        }
        self.last_text_ended_with_whitespace = true;
        self.last_token_type = LastTokenType::None;
        self.wrap_line();
    }

    /// Finalize and write results to layout
    fn finalize(mut self, layout: &mut CanvasMessageLayout) {
        self.flush_text_segment();
        self.flush_layered_emote();

        self.commit_line();

        layout.height = self.current_y + MESSAGE_PADDING;
        layout.line_count = self.current_line + 1;

        self.source_char_positions.push(self.current_x);
        debug_assert_eq!(
            self.source_char_positions.len(),
            self.source_text.chars().count() + 1,
            "canvas source positions must retain one boundary per Unicode scalar"
        );

        let mut final_segments = std::mem::take(&mut layout.segments);
        final_segments.extend(self.segments);
        layout.segments = final_segments;

        layout.shaped_lines = self.shaped_lines.into_vec();

        let mut final_hitboxes = std::mem::take(&mut layout.hitbox_regions);
        final_hitboxes.extend(self.hitboxes);
        layout.hitbox_regions = final_hitboxes;
        layout.mention_highlights = self.mention_highlights;

        layout.line_y_positions = self.line_y_positions;

        layout.source_text = self.source_text;
        layout.source_char_positions = self.source_char_positions;
        layout.pending_native_dimension_urls = self.pending_native_dimension_urls;
        layout.reserved_badge_channel = self.reserved_badge_channel;

        layout.animated_emote_count = self.animated_emote_count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::sync::Arc;

    /// Builder fixture for the boundary-spacing tests.
    fn spacing_test_builder() -> LayoutBuilder<'static> {
        let mut builder = LayoutBuilder::new_with_y_offset(1000.0, false, 0.0, None, false);
        builder.content_start_x = MESSAGE_PADDING_LEFT;
        builder.current_x = MESSAGE_PADDING_LEFT + 120.0;
        builder
    }

    #[test]
    fn rtl_text_keeps_logical_order_in_source_text() {
        let rtl_text = "שלום עולם";
        let msg = ChatMessage {
            id: Arc::from("rtl-msg"),
            channel: Arc::from("test-channel"),
            sender: Arc::from("tester"),
            display_name: Arc::from("Tester"),
            content: Arc::from(rtl_text),
            slots: vec![MessageSlot::text(rtl_text)].into(),
            ..ChatMessage::default()
        };

        let layout = build_message_layout(&msg, "test-channel", 900.0, false, None);
        assert!(
            layout.source_text.contains(rtl_text),
            "source_text should preserve logical RTL order"
        );
    }

    #[test]
    fn ascii_text_slot_preserves_hard_breaks_and_source_text() {
        let body = "first row\r\nsecond row\nthird row\rfourth row";
        let msg = ChatMessage {
            id: Arc::from("ascii-breaks-msg"),
            channel: Arc::from("test-channel"),
            sender: Arc::from("tester"),
            display_name: Arc::from("Tester"),
            content: Arc::from(body),
            slots: vec![MessageSlot::text(body)].into(),
            ..ChatMessage::default()
        };

        let layout = build_message_layout(&msg, "test-channel", 900.0, false, None);

        assert!(
            layout.source_text.ends_with(body),
            "ASCII text must retain its original line endings for copy/selection"
        );
        assert_eq!(
            layout.line_count, 4,
            "each authored ASCII hard break should start the next layout line"
        );
    }

    #[test]
    fn text_after_unicode_emoji_gets_boundary_space() {
        let msg = ChatMessage {
            id: Arc::from("emoji-spacing-msg"),
            channel: Arc::from("test-channel"),
            sender: Arc::from("tester"),
            display_name: Arc::from("Tester"),
            content: Arc::from("a😀b"),
            slots: vec![MessageSlot::text("a😀b")].into(),
            ..ChatMessage::default()
        };

        let layout = build_message_layout(&msg, "test-channel", 900.0, false, None);
        let (emoji_x, emoji_width) = layout
            .segments
            .iter()
            .find_map(|segment| match segment {
                LayoutSegment::Image { x, width, .. } => Some((*x, *width)),
                _ => None,
            })
            .expect("expected unicode emoji image segment");

        let text_after_x = layout
            .segments
            .iter()
            .find_map(|segment| match segment {
                LayoutSegment::Text { line_idx, x, .. }
                    if layout
                        .shaped_lines
                        .get(*line_idx)
                        .is_some_and(|line| line.text.as_ref() == "b") =>
                {
                    Some(*x)
                }
                _ => None,
            })
            .expect("expected text segment after emoji");

        assert!(
            text_after_x > emoji_x + emoji_width,
            "text after emoji should start after a boundary gap"
        );
    }

    #[test]
    fn text_after_unicode_emoji_with_authored_space_uses_boundary_gap() {
        let msg = ChatMessage {
            id: Arc::from("emoji-authored-space-msg"),
            channel: Arc::from("test-channel"),
            sender: Arc::from("tester"),
            display_name: Arc::from("Tester"),
            content: Arc::from("a😀 b"),
            slots: vec![MessageSlot::text("a😀 b")].into(),
            ..ChatMessage::default()
        };

        let layout = build_message_layout(&msg, "test-channel", 900.0, false, None);
        let (emoji_x, emoji_width) = layout
            .segments
            .iter()
            .find_map(|segment| match segment {
                LayoutSegment::Image { x, width, .. } => Some((*x, *width)),
                _ => None,
            })
            .expect("expected unicode emoji image segment");

        let text_after_x = layout
            .segments
            .iter()
            .find_map(|segment| match segment {
                LayoutSegment::Text { line_idx, x, .. }
                    if layout
                        .shaped_lines
                        .get(*line_idx)
                        .is_some_and(|line| line.text.as_ref() == "b") =>
                {
                    Some(*x)
                }
                _ => None,
            })
            .expect("expected text segment after emoji");

        assert!(
            text_after_x > emoji_x + emoji_width,
            "authored whitespace after emoji should not reduce the media boundary gap"
        );
    }

    #[test]
    fn zero_width_overlay_reserves_layered_box_before_following_text() {
        let mut builder = spacing_test_builder();
        builder.mark_content_start();
        let base_x = builder.current_x;
        let mut emote = LayeredEmote::new("base".into(), "base".into(), 24.0, 24.0, false, None);
        emote.add_overlay("overlay".into(), "overlay".into(), 48.0, 24.0, false, None);
        builder.pending_layered_emote = Some(PendingLayeredImage {
            emote,
            x: base_x,
            image_type: ImageType::Emote {
                name: "base".into(),
                tooltip_meta: None,
            },
        });
        builder.advance_by(24.0);
        builder.last_token_type = LastTokenType::Emote;

        builder.flush_layered_emote();
        advance_before_text_like(&mut builder);
        builder.add_text_run("after", colors::text_primary(), false, false);

        let mut layout = CanvasMessageLayout::new("layered-spacing".to_string(), "test-channel");
        builder.finalize(&mut layout);

        let (layered_x, layered_width, layers) = layout
            .segments
            .iter()
            .find_map(|segment| match segment {
                LayoutSegment::LayeredImage {
                    x, width, layers, ..
                } => Some((*x, *width, layers)),
                _ => None,
            })
            .expect("expected layered emote segment");

        let text_after_x = layout
            .segments
            .iter()
            .find_map(|segment| match segment {
                LayoutSegment::Text { line_idx, x, .. }
                    if layout
                        .shaped_lines
                        .get(*line_idx)
                        .is_some_and(|line| line.text.as_ref() == "after") =>
                {
                    Some(*x)
                }
                _ => None,
            })
            .expect("expected text segment after layered emote");

        assert_eq!(layers.len(), 2);
        assert!((layered_x - base_x).abs() < 0.001);
        assert!((layered_width - 48.0).abs() < 0.001);
        assert!(
            text_after_x > layered_x + layered_width,
            "following text must reserve the whole zero-width overlay box"
        );
    }

    fn layout_for_wrapped_run(run: &str, right_boundary: f32) -> CanvasMessageLayout {
        let mut layout = CanvasMessageLayout::new("wrap-test".to_string(), "test-channel");
        let mut builder = LayoutBuilder::new_with_y_offset(right_boundary, false, 0.0, None, false);
        builder.mark_content_start();
        push_wrapped_non_whitespace_run(&mut builder, run, colors::text_primary());
        builder.finalize(&mut layout);
        layout
    }

    #[test]
    fn long_url_wraps_and_preserves_source_text() {
        let run =
            "https://example.com/very/long/path/with-many-segments-and-query?abcdef=1234567890";
        let layout = layout_for_wrapped_run(run, MESSAGE_PADDING_LEFT + 72.0);

        assert!(
            layout.line_count > 1,
            "expected URL to wrap onto multiple lines"
        );
        assert_eq!(layout.source_text, run);
        assert_eq!(layout.source_char_len(), run.chars().count());
    }

    #[test]
    fn cjk_text_wraps_without_whitespace() {
        let run = "漢字かな交じり文漢字かな交じり文漢字かな交じり文";
        let layout = layout_for_wrapped_run(run, MESSAGE_PADDING_LEFT + 52.0);

        assert!(
            layout.line_count > 1,
            "expected CJK text to wrap without ASCII whitespace"
        );
        assert_eq!(layout.source_text, run);
        assert_eq!(layout.source_char_len(), run.chars().count());
    }

    #[test]
    fn emoji_zwj_clusters_wrap_without_split_artifacts() {
        let run = "👨‍👩‍👧‍👦👨‍👩‍👧‍👦👨‍👩‍👧‍👦👨‍👩‍👧‍👦👨‍👩‍👧‍👦";
        let layout = layout_for_wrapped_run(run, MESSAGE_PADDING_LEFT + 64.0);

        assert!(
            layout.line_count > 1,
            "expected repeated emoji clusters to wrap"
        );
        assert_eq!(layout.source_text, run);

        for line in &layout.shaped_lines {
            let text = line.text.as_ref();
            assert!(
                !text.starts_with('\u{200D}') && !text.ends_with('\u{200D}'),
                "line split should not strand ZWJ at boundaries: {text:?}"
            );
        }
    }
}
