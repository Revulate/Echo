//! The derived day-divider row.

use super::*;

/// Fixed height of a derived day-divider row. Independent of chat density and
/// font scale: the row is a separator, not a message.
pub(super) const DAY_DIVIDER_ROW_HEIGHT: f32 = 28.0;

/// Size of the centered ISO date label.
pub(super) const DAY_DIVIDER_LABEL_FONT_SIZE: f32 = 11.0;

/// Height of the label box; the label is centered inside it.
pub(super) const DAY_DIVIDER_LABEL_HEIGHT: f32 = 16.0;

/// Thickness of the flexible rules flanking the label.
pub(super) const DAY_DIVIDER_RULE_HEIGHT: f32 = 1.0;

/// Gap between the label box and each rule.
pub(super) const DAY_DIVIDER_LABEL_GAP: f32 = 8.0;

/// Horizontal breathing room inside the label box.
pub(super) const DAY_DIVIDER_LABEL_PADDING_X: f32 = 4.0;

/// Shortest rule still worth painting in a very narrow split.
pub(super) const DAY_DIVIDER_MIN_RULE_WIDTH: f32 = 2.0;

/// Fallback advance ratio for the fixed-width ISO digits when a background
/// layout worker shapes without a window.
pub(super) const DAY_DIVIDER_FALLBACK_ADVANCE_RATIO: f32 = 0.62;

/// Build the layout of a derived day-divider row: a centered ISO `YYYY-MM-DD`
/// label flanked by flexible 1px rules, in a fixed 28px row.
///
/// The row deliberately has no timestamp column, badges, username, reply
/// affordance, highlight, moderation hitbox, or message-background hitbox, so it
/// cannot be selected, replied to, moderated, or copied.
pub(super) fn build_day_divider_layout(
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
pub(super) fn day_divider_rule(x: f32, y: f32, width: f32, color: Hsla) -> LayoutSegment {
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
pub(super) fn measure_day_divider_label(text: &SharedString, window: Option<&mut Window>) -> f32 {
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
