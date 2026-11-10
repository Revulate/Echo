//! Shared shell for Echo's single global activity rail.

use crate::components::ui::ButtonExt;
use crate::state::ActivitySection;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::rc::Rc;

type SelectSection<T> = Rc<dyn Fn(ActivitySection, &mut Context<T>)>;

/// Below this rail width the four tab labels can no longer sit beside numeric
/// count badges, so the badges collapse to a presence dot instead of pushing the
/// labels out of their tabs.
const COMPACT_RAIL_WIDTH_PX: f32 = 240.0;
const TAB_COUNT_PILL_HEIGHT_PX: f32 = 15.0;

/// Canonical clipboard text for one Activity row.
///
/// Callers pass the already privacy-masked strings they render, so streamer
/// mode cannot leak hidden channel, username, or message content through Copy.
pub(crate) fn format_activity_copy_text(
    source: &str,
    title: &str,
    context: &str,
    body: &str,
) -> String {
    [source, title, context, body]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Small discoverable clipboard action shared by every Activity row type.
pub(crate) fn render_activity_copy_action(id: String, copy_text: String) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .h(px(22.0))
        .px_2()
        .rounded(Radii::sm())
        .cursor_pointer()
        .hover(|style| style.bg(colors::hover_surface()))
        .flex()
        .items_center()
        .justify_center()
        .on_activate(move |_event, _window, cx| {
            crate::utils::copy_text(&copy_text);
            cx.stop_propagation();
        })
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_secondary())
                .child("Copy"),
        )
}

/// Render the one cross-channel Activity rail around its combined feed or a
/// source-specific filter. Source tabs show unread attention independently;
/// retained-history size remains visible through the content itself.
///
/// `width` is resolved by the caller against the viewport: the rail is not
/// entitled to its full width when that would leave the chat pane nothing.
#[allow(clippy::too_many_arguments)]
pub fn render_activity_sidebar<
    T: 'static,
    S: Fn(ActivitySection, &mut Context<T>) + 'static,
    C: Fn(&mut Context<T>) + 'static,
>(
    width: f32,
    section: ActivitySection,
    mention_count: usize,
    unread_whispers: u32,
    event_count: usize,
    content: AnyElement,
    on_select: S,
    on_close: C,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, S, C> {
    let on_select: SelectSection<T> = Rc::new(on_select);
    let on_close = Rc::new(on_close);
    let compact = width < COMPACT_RAIL_WIDTH_PX;

    div()
        .id("activity-sidebar")
        .w(px(width.max(0.0)))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .bg(colors::bg_surface())
        .border_l_1()
        .border_color(colors::border())
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(42.0))
                .w_full()
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child("Activity"),
                        )
                        .when(!compact, |header| {
                            header.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .child("Every channel"),
                            )
                        }),
                )
                .child(
                    div()
                        .id("close-activity-btn")
                        .role(Role::Button)
                        .aria_label("Close activity")
                        .on_hover(crate::components::overlays::text_tooltip_on_hover(
                            "close-activity-btn".to_string(),
                            "Close".to_string(),
                        ))
                        .flex_shrink_0()
                        .w(px(26.0))
                        .h(px(26.0))
                        .rounded(Radii::md())
                        .cursor_pointer()
                        .hover(|style| style.bg(colors::bg_hover()))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_activate(cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
                            on_close(cx);
                            cx.stop_propagation();
                        }))
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .child(crate::icons::CLOSE_GLYPH),
                        ),
                ),
        )
        .child(
            div()
                .h(px(36.0))
                .w_full()
                .px_2()
                .flex()
                .items_end()
                .gap_1()
                .border_b_1()
                .border_color(colors::border())
                .child(render_activity_tab(
                    "activity-tab-all",
                    "All",
                    None,
                    ActivitySection::All,
                    section == ActivitySection::All,
                    compact,
                    on_select.clone(),
                    cx,
                ))
                .child(render_activity_tab(
                    "activity-tab-mentions",
                    "Mention",
                    Some(mention_count),
                    ActivitySection::Mentions,
                    section == ActivitySection::Mentions,
                    compact,
                    on_select.clone(),
                    cx,
                ))
                .child(render_activity_tab(
                    "activity-tab-whispers",
                    "Whisper",
                    Some(unread_whispers as usize),
                    ActivitySection::Whispers,
                    section == ActivitySection::Whispers,
                    compact,
                    on_select.clone(),
                    cx,
                ))
                .child(render_activity_tab(
                    "activity-tab-events",
                    "Events",
                    Some(event_count),
                    ActivitySection::Events,
                    section == ActivitySection::Events,
                    compact,
                    on_select,
                    cx,
                )),
        )
        .child(content)
}

#[allow(clippy::too_many_arguments)]
fn render_activity_tab<T: 'static>(
    id: &'static str,
    label: &'static str,
    count: Option<usize>,
    section: ActivitySection,
    active: bool,
    compact: bool,
    on_select: SelectSection<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let count = count.unwrap_or(0);
    let count_label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };

    div()
        .id(id)
        .relative()
        .h(px(32.0))
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .px(px(Spacing::XXS))
        .rounded_t(Radii::md())
        .cursor_pointer()
        .bg(if active {
            colors::accent().opacity(0.1)
        } else {
            colors::bg_surface()
        })
        .hover(|style| style.bg(colors::bg_hover()))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(Spacing::XXS))
        .on_activate(cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
            on_select(section, cx);
            cx.stop_propagation();
        }))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(if active {
                    colors::accent()
                } else {
                    colors::text_secondary()
                })
                .child(label),
        )
        .when(count > 0 && compact, |tab| {
            tab.child(
                div()
                    .flex_shrink_0()
                    .w(px(5.0))
                    .h(px(5.0))
                    .rounded(Radii::full())
                    .bg(if active {
                        colors::accent()
                    } else {
                        colors::text_muted()
                    }),
            )
        })
        .when(count > 0 && !compact, |tab| {
            tab.child(
                div()
                    .flex_shrink_0()
                    .min_w(px(TAB_COUNT_PILL_HEIGHT_PX))
                    .h(px(TAB_COUNT_PILL_HEIGHT_PX))
                    .line_height(px(TAB_COUNT_PILL_HEIGHT_PX))
                    .px(px(Spacing::XS))
                    .rounded(Radii::full())
                    .bg(if active {
                        colors::accent().opacity(0.18)
                    } else {
                        colors::bg_elevated()
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(FontSize::XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if active {
                                colors::accent()
                            } else {
                                colors::text_muted()
                            })
                            .child(count_label),
                    ),
            )
        })
        .when(active, |tab| {
            tab.child(
                div()
                    .absolute()
                    .left_2()
                    .right_2()
                    .bottom_0()
                    .h(px(2.0))
                    .rounded_t(Radii::xs())
                    .bg(colors::accent()),
            )
        })
}
