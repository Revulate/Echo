//! Activity Mentions content renderer.

use crate::backend::{self, ChatMessage};
use crate::components::ui::{ButtonExt, PressToClick};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use crate::utils::{format_sidebar_timestamp, parse_optional_hex_or};
use gpui::prelude::FluentBuilder;
use gpui::*;

fn mention_preview_text(msg: &ChatMessage, hide_mention_content: bool) -> String {
    if hide_mention_content {
        return "Mention hidden in streamer mode".to_string();
    }

    let content = msg.content.as_ref();

    if content.chars().count() > 60 {
        let truncated: String = content.chars().take(57).collect();
        format!("{}…", truncated)
    } else {
        content.to_string()
    }
}

/// Render the Mentions section inside the shared Activity rail.
pub fn render_mentions_activity_content<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    mentions: std::sync::Arc<Vec<(String, ChatMessage)>>,
    on_clear: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let on_clear = std::rc::Rc::new(on_clear);

    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(render_mentions_toolbar(&mentions, on_clear, cx))
        .child(if mentions.is_empty() {
            render_mentions_empty_state().into_any_element()
        } else {
            render_mentions_list(mentions).into_any_element()
        })
}

fn render_mentions_toolbar<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    mentions: &[(String, ChatMessage)],
    on_clear: std::rc::Rc<F>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let count = mentions.len();

    div()
        .h(px(38.0))
        .w_full()
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(colors::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Highlights from every open channel"),
                )
                .when(count == 100, |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child("Latest 100"),
                    )
                }),
        )
        .when(count > 0, |toolbar| {
            toolbar.child(
                div()
                    .id("clear-mentions-btn")
                    .px_2()
                    .h(px(24.0))
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .hover(|style| style.bg(colors::bg_hover()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_activate(cx.listener({
                        let on_clear = on_clear.clone();
                        move |_this, _event: &ClickEvent, _window, cx| {
                            on_clear(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child("Clear"),
                    ),
            )
        })
}

fn render_mentions_empty_state() -> impl IntoElement {
    div()
        .flex_1()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(div().text_3xl().text_color(colors::text_muted()).child("@"))
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child("No mentions yet"),
                ),
        )
}

fn render_mentions_list(mentions: std::sync::Arc<Vec<(String, ChatMessage)>>) -> impl IntoElement {
    let item_count = mentions.len();
    let hide_mention_content = backend::streamer_mode::should_hide_mention_content();

    uniform_list(
        ElementId::Name("mentions-list".into()),
        item_count,
        move |range, _window, _cx| {
            range
                .map(|idx| {
                    let (channel, msg) = &mentions[idx];
                    let sender = if hide_mention_content {
                        "Mention".to_string()
                    } else {
                        msg.display_name.to_string()
                    };
                    let color = msg.color.clone();
                    let jump_channel = channel.clone();
                    let jump_message_id = msg.id.to_string();
                    let dismiss_channel = channel.clone();
                    let dismiss_message_id = msg.id.to_string();
                    let timestamp = format_sidebar_timestamp(msg.timestamp);
                    let visible_channel = if hide_mention_content {
                        "Channel hidden".to_string()
                    } else {
                        channel.to_string()
                    };
                    let sender_color = if hide_mention_content {
                        colors::text_secondary()
                    } else {
                        parse_optional_hex_or(&color, colors::text_primary())
                    };
                    let preview = mention_preview_text(msg, hide_mention_content);
                    let copy_text = super::format_activity_copy_text(
                        "Mention",
                        &sender,
                        &visible_channel,
                        &preview,
                    );

                    div()
                        .id(SharedString::from(format!("mention-{}", idx)))
                        .w_full()
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .hover(|s| s.bg(colors::bg_hover()))
                        .on_activate(move |_event, _window, _cx| {
                            backend::push_ui_action(backend::UiAction::JumpToMessage {
                                channel: jump_channel.clone(),
                                message_id: jump_message_id.clone(),
                            });
                        })
                        .on_right_click(move |_event, _window, _cx| {
                            backend::push_ui_action(backend::UiAction::DismissMention {
                                channel: dismiss_channel.clone(),
                                message_id: dismiss_message_id.clone(),
                            });
                        })
                        .border_b_1()
                        .border_color(colors::border())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors::text_muted())
                                                .child(visible_channel),
                                        )
                                        .when(!timestamp.is_empty(), |s| {
                                            s.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors::text_muted())
                                                    .child(timestamp),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(sender_color)
                                        .child(sender.to_string()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors::text_secondary())
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(preview),
                                )
                                .child(div().pt_1().child(super::render_activity_copy_action(
                                    format!("mention-{idx}-copy"),
                                    copy_text,
                                ))),
                        )
                })
                .collect()
        },
    )
    .flex_1()
    .w_full()
}
