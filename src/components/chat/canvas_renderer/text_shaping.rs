//! Text shaping and layout building.
//!
//! Converts message tokens into pre-shaped text lines and layout segments.
//! Text around emotes is split into separate runs with appropriate styling.
//! Emoji characters are rendered as images in the selected emoji style.

mod badges;
mod builder;
mod day_divider;
mod embeds;
mod emotes;
mod trace;

pub use self::badges::clear_badge_render_caches;
use self::badges::{get_cached_badge_info, get_cached_badge_url};
use self::builder::{LastTokenType, LayoutBuilder};
use self::day_divider::build_day_divider_layout;
use self::embeds::{embed_platform_for, preview_source_dimensions, shape_embed_preview_geometry};
use self::emotes::{EmoteSlotSource, add_emote_render_unit, add_emote_render_unit_with_rules};
use self::trace::LayoutTraceGuard;
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

use super::bounded_map::ClearOnFullMap;
use super::layout::{
    BadgeTooltipMeta, CanvasMessageLayout, EMBED_ACCENT_WIDTH, EMBED_MIN_HEIGHT, EMBED_PADDING,
    EMBED_TEXT_THUMBNAIL_GAP, EMBED_TITLE_DESC_GAP, EmbedPreviewGeometry, EmoteTooltipMeta,
    HighlightType, HitboxKind, HitboxRegion, ImageType, LayeredEmote, LayeredTooltipEmote,
    LayoutSegment, MESSAGE_PADDING_LEFT, MESSAGE_PADDING_RIGHT, MESSAGE_PADDING_VERTICAL,
    REPLY_CONNECTOR_ARM, REPLY_CONNECTOR_GAP, REPLY_CONNECTOR_LEFT, REPLY_PREVIEW_MAX_CHARS,
    ShapedLineData, TIMESTAMP_GAP, TextRunData, badge_gap, badge_margin_right, badge_size,
    calculate_emote_dimensions, emoji_size, emote_size, get_cached_color, get_cached_timestamp,
    hash_gpui_text_runs, is_animated_url, is_svg_image_url, line_height, reply_indicator_height,
    timestamp_width,
};
use super::seventv_paint::SevenTvLinePaint;
use super::trace_span::{TRACE_SLOW_MS, TraceSpan, trace_switches};
use crate::backend;
use echo_chat_render::font_metrics::{font_metrics, scaled_font_size};
use echo_chat_render::line_buffer::LineBuffer;
use echo_chat_render::text_run_intern;
use echo_render_core::bidi::BidiAnalysis;
use std::{cell::Cell, sync::LazyLock};

#[cfg(test)]
use crate::backend::slots::MessageSlot;

/// Content width a wrapped line must keep to the right of its indent.
///
/// The wrapped-line indent comes from the message header (timestamp column, or
/// badges + username), which does not shrink with the pane. Without a floor the
/// indent can pass the right boundary in a narrow pane and every grapheme wraps
/// onto its own line. Roughly two average glyphs.
const MIN_WRAPPED_CONTENT_WIDTH: f32 = 24.0;

/// Width an embed card takes when the split has room; a wider thumbnail
/// widens it further.
const EMBED_PREFERRED_WIDTH: f32 = 420.0;
/// Vertical gap after each trailing embed or upload block, and above embeds.
const TRAILING_BLOCK_GAP: f32 = 8.0;
/// Vertical gap above an upload preview.
const UPLOAD_PREVIEW_GAP_ABOVE: f32 = 6.0;
/// Height of an upload preview; thumbnails with no known size are square.
const UPLOAD_PREVIEW_HEIGHT: f32 = 56.0;
/// Upload label width cap, in pixels and as a share of the available width.
const UPLOAD_LABEL_MAX_WIDTH: f32 = 144.0;
const UPLOAD_LABEL_MAX_SHARE: f32 = 0.4;
/// Gap between an upload thumbnail and its label.
const UPLOAD_LABEL_GAP: f32 = 8.0;
/// Content opacity applied to messages flagged as similar/repeated: such rows
/// dim toward the background instead of being highlighted.
const SIMILAR_MESSAGE_DIM_OPACITY: f32 = 0.55;

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
                builder.wrap_unless_fits(segment_width);
                builder.add_text_run(segment, color, false, false);
                start = break_pos;
            }
            if start < run.len() {
                let remaining = run.get(start..).unwrap_or("");
                let remaining_width = builder.measure_text_width(remaining, false);
                builder.wrap_unless_fits(remaining_width);
                builder.add_text_run(remaining, color, false, false);
            }
        } else {
            for grapheme in run.graphemes(true) {
                let grapheme_width = builder.measure_text_width(grapheme, false);
                builder.wrap_unless_fits(grapheme_width);
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

/// Build a complete layout for a message
///
/// When `window` is provided, uses accurate GPUI text measurement (pixel-perfect).
/// When `window` is None, uses cached character widths with 4px buffer for variance.
/// Text shown instead of a collapsed ASCII-art message.
const ASCII_ART_PLACEHOLDER: &str = "[ASCII Art]";
/// Width of the placeholder's text segment.
const ASCII_ART_PLACEHOLDER_WIDTH: f32 = 80.0;

/// A one-line muted "[ASCII Art]" row in place of a collapsed ASCII-art message.
fn build_ascii_art_placeholder_layout(layout: &mut CanvasMessageLayout) {
    let lh = line_height();
    layout.height = lh + MESSAGE_PADDING_VERTICAL * 2.0;
    let placeholder = ASCII_ART_PLACEHOLDER;
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
        y: MESSAGE_PADDING_VERTICAL,
        width: ASCII_ART_PLACEHOLDER_WIDTH,
        height: lh,
    });
    layout.source_text = placeholder.to_string();
    layout.line_count = 1;
    layout.rebuild_paint_commands();
}

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
        build_ascii_art_placeholder_layout(&mut layout);
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
                has_visible_badges = true;
                builder.place_badge(&url, false, info.image_type());
            } else if echo_chat_render::ports::badge_set_pending(&msg.channel) {
                let bs = badge_size();
                has_visible_badges = true;
                builder.reserved_badge_channel = Some(std::sync::Arc::clone(&msg.channel));
                builder.wrap_unless_fits(bs);
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
                has_visible_badges = true;
                builder.place_badge(&url, false, info.image_type());
            }
        }

        let has_7tv_badge = if show_7tv_badges {
            seven_tv_cosmetics
                .as_ref()
                .and_then(|cosmetics| cosmetics.badge.as_ref())
                .map(|badge| {
                    let description = badge.tooltip.trim();
                    builder.place_badge(
                        &badge.url,
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
                    );
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
        builder.wrap_unless_fits(username_width);

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

                            builder.wrap_unless_fits(size);

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

                    builder.wrap_unless_fits(url_width);

                    builder.add_text_run(display, colors::text_muted(), false, false);
                    builder.flush_text_segment();
                    builder.last_token_type = LastTokenType::Link;
                } else {
                    builder.flush_text_segment();

                    advance_before_link_like(&mut builder);

                    let display = SharedString::from(url.clone());
                    let url_width = builder.measure_text_width(&display, false);

                    builder.wrap_unless_fits(url_width);

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

                builder.wrap_unless_fits(mention_width);

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

                builder.wrap_unless_fits(emoji_width);

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
        builder.wrap_unless_fits(actions_width);
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
        use crate::backend::link_types::MediaKind;

        builder.break_to_block();

        for (url, embed_id, metadata) in deferred_embeds {
            builder.current_y += TRAILING_BLOCK_GAP;

            let platform = match metadata.media.as_ref().map(|media| media.kind) {
                Some(MediaKind::Video) => EmbedPlatform::Video,
                Some(MediaKind::Image) | None => embed_platform_for(&metadata.link_info.link_type),
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
            let preview_width = available_width.min(
                EMBED_PREFERRED_WIDTH.max(image_width + EMBED_ACCENT_WIDTH + EMBED_PADDING * 2.0),
            );
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

            builder.current_y += preview_height + TRAILING_BLOCK_GAP;
            builder.current_x = builder.content_start_x;
            builder.last_token_type = LastTokenType::None;
        }
    }

    if let Some((url, thumbnail_url)) = upload_preview {
        builder.break_to_block();

        const SQUARE: (f32, f32) = (UPLOAD_PREVIEW_HEIGHT, UPLOAD_PREVIEW_HEIGHT);
        builder.current_y += UPLOAD_PREVIEW_GAP_ABOVE;
        let available_width = (builder.right_boundary - MESSAGE_PADDING_LEFT).max(0.0);
        let label_width = UPLOAD_LABEL_MAX_WIDTH.min(available_width * UPLOAD_LABEL_MAX_SHARE);
        let max_thumbnail_width = (available_width - label_width - UPLOAD_LABEL_GAP).max(0.0);
        let (source_width, source_height) = thumbnail_url
            .as_ref()
            .map(|url| preview_source_dimensions(&mut builder, url.as_ref(), SQUARE))
            .unwrap_or(SQUARE);
        let (thumbnail_width, _) = crate::backend::embed_thumbnails::fit_thumbnail_to_height(
            source_width,
            source_height,
            max_thumbnail_width,
            UPLOAD_PREVIEW_HEIGHT,
        );
        let preview_width = (thumbnail_width + UPLOAD_LABEL_GAP + label_width).min(available_width);
        builder.segments.push(LayoutSegment::UploadPreview {
            url,
            thumbnail_url,
            thumbnail_width,
            x: MESSAGE_PADDING_LEFT,
            y: builder.current_y,
            width: preview_width,
            height: UPLOAD_PREVIEW_HEIGHT,
        });
        builder.current_y += UPLOAD_PREVIEW_HEIGHT + TRAILING_BLOCK_GAP;
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
        connector_y: MESSAGE_PADDING_VERTICAL,
        connector_height: reply_indicator_height() - 4.0,
        parent_display_name: parent_display_name.clone(),
        parent_color,
        preview_text: preview.into(),
        text_x,
        width: available_width,
    });

    layout.hitbox_regions.push(HitboxRegion {
        bounds: Bounds::new(
            point(px(MESSAGE_PADDING_LEFT), px(MESSAGE_PADDING_VERTICAL)),
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

#[cfg(test)]
mod tests {
    use super::builder::PendingLayeredImage;
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
