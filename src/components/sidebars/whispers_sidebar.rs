//! Activity Whispers content renderer.

use crate::backend::{self, WhisperThread};
use crate::components::ui::{ButtonExt, PressToClick};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use crate::utils::format_sidebar_timestamp;
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Render the Whispers section inside the shared Activity rail.
pub fn render_whispers_activity_content<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    threads: std::sync::Arc<Vec<WhisperThread>>,
    on_mark_read: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let on_mark_read = std::rc::Rc::new(on_mark_read);
    let compose_block_reason = crate::backend::bridge::get_whisper_compose_block_reason();

    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(render_whispers_toolbar(&threads, on_mark_read, cx))
        .when_some(compose_block_reason.as_deref(), |el, reason| {
            el.child(render_whispers_compose_status(reason))
        })
        .child(if threads.is_empty() {
            render_whispers_empty_state(compose_block_reason.as_deref()).into_any_element()
        } else {
            render_whispers_list(threads, compose_block_reason.is_none()).into_any_element()
        })
}

fn render_whispers_toolbar<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    threads: &[WhisperThread],
    on_mark_read: std::rc::Rc<F>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let unread_total = threads.iter().fold(0u32, |total, thread| {
        total.saturating_add(thread.unread_count)
    });

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
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child("Recent Twitch conversations"),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("open-whispers-split-btn")
                        .px_2()
                        .h(px(24.0))
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .bg(colors::accent().opacity(0.12))
                        .hover(|style| style.bg(colors::accent().opacity(0.2)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_activate(|_event, _window, cx| {
                            backend::push_ui_action(backend::UiAction::OpenWhispersSplit);
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::accent_text())
                                .child("Open split"),
                        ),
                )
                .when(unread_total > 0, |actions| {
                    actions.child(
                        div()
                            .id("mark-whispers-read-btn")
                            .px_2()
                            .h(px(24.0))
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .hover(|style| style.bg(colors::bg_hover()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_activate(cx.listener({
                                let on_mark_read = on_mark_read.clone();
                                move |_this, _event: &ClickEvent, _window, cx| {
                                    on_mark_read(cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .child("Mark all read"),
                            ),
                    )
                }),
        )
}

fn compact_whisper_compose_reason(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("anonymously") {
        "Whispers are unavailable in anonymous mode".to_string()
    } else if lower.contains("re-authenticated") || lower.contains("chat token") {
        "Whispers require a re-authenticated Twitch account".to_string()
    } else if lower.contains("active twitch account") {
        "Log in to a Twitch account to use whispers".to_string()
    } else if lower.contains("connecting") || lower.contains("reconnecting") {
        "Whispers will be available once Twitch IRC reconnects".to_string()
    } else if lower.contains("disconnected") || lower.contains("connection is ready") {
        "Whispers will be available once Twitch IRC is ready".to_string()
    } else {
        "Whispers are temporarily unavailable".to_string()
    }
}

fn render_whispers_compose_status(reason: &str) -> impl IntoElement + use<> {
    div()
        .mx_3()
        .mt_2()
        .px_3()
        .py_2()
        .rounded(Radii::md())
        .bg(colors::warning().opacity(0.12))
        .border_1()
        .border_color(colors::warning().opacity(0.28))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::warning())
                        .child(compact_whisper_compose_reason(reason)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_secondary())
                        .child("Reply actions use the active authenticated chat input."),
                ),
        )
}

fn render_whispers_empty_state(compose_block_reason: Option<&str>) -> impl IntoElement + use<> {
    let (icon, message) = if let Some(reason) = compose_block_reason {
        ("🔒", compact_whisper_compose_reason(reason))
    } else {
        ("💬", "No whispers yet".to_string())
    };

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
                .child(
                    div()
                        .text_3xl()
                        .text_color(colors::text_muted())
                        .child(SharedString::from(icon)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child(message),
                ),
        )
}

fn render_whispers_list(
    threads: std::sync::Arc<Vec<WhisperThread>>,
    can_compose: bool,
) -> impl IntoElement {
    let item_count = threads.len();
    let hide_whisper_content = backend::streamer_mode::should_hide_whisper_content();

    uniform_list(
        ElementId::Name("whispers-list".into()),
        item_count,
        move |range, _window, _cx| {
            range
                .map(|idx| {
                    let thread = &threads[idx];
                    let username = thread.username.clone();
                    let username_for_action = username.clone();
                    let username_for_mark_read = username.clone();
                    let unread = thread.unread_count;
                    let timestamp = format_sidebar_timestamp(thread.last_timestamp);
                    let visible_username = if hide_whisper_content {
                        if unread > 0 {
                            "Unread whisper".to_string()
                        } else {
                            "Whisper thread".to_string()
                        }
                    } else {
                        username.clone()
                    };
                    let preview = if hide_whisper_content {
                        "Whisper hidden in streamer mode".to_string()
                    } else if thread.last_message.chars().count() > 50 {
                        let truncated: String = thread.last_message.chars().take(47).collect();
                        format!("{truncated}…")
                    } else {
                        thread.last_message.clone()
                    };
                    let copy_text = super::format_activity_copy_text(
                        "Whisper",
                        &visible_username,
                        "Direct message",
                        &preview,
                    );

                    div()
                        .id(SharedString::from(format!("whisper-{idx}")))
                        .w_full()
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .bg(if unread > 0 {
                            colors::accent().opacity(0.08)
                        } else {
                            colors::bg_surface()
                        })
                        .hover(|s| s.bg(colors::bg_hover()))
                        .when(can_compose, |row| {
                            row.on_activate(move |_event, _window, _cx| {
                                backend::push_ui_action(backend::UiAction::ComposeWhisper {
                                    username: username_for_action.clone(),
                                    channel: None,
                                });
                            })
                        })
                        .on_right_click(move |_event, _window, _cx| {
                            backend::push_ui_action(backend::UiAction::MarkWhisperRead {
                                username: username_for_mark_read.clone(),
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
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(colors::text_primary())
                                                .child(visible_username),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .when(!timestamp.is_empty(), |s| {
                                                    s.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(colors::text_muted())
                                                            .child(timestamp),
                                                    )
                                                })
                                                .when(unread > 0, |s| {
                                                    s.child(
                                                        div()
                                                            .px_2()
                                                            .py_0p5()
                                                            .rounded(Radii::xl())
                                                            .bg(colors::accent())
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .font_weight(FontWeight::BOLD)
                                                                    .text_color(colors::text_on_accent_contrast())
                                                                    .child(format!("{unread}")),
                                                            ),
                                                    )
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors::text_secondary())
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(preview),
                                )
                                .child(render_whisper_row_actions(
                                    idx,
                                    username,
                                    unread,
                                    can_compose,
                                    copy_text,
                                )),
                        )
                })
                .collect()
        },
    )
    .flex_1()
    .w_full()
}

fn render_whisper_row_actions(
    index: usize,
    username: String,
    unread: u32,
    can_compose: bool,
    copy_text: String,
) -> impl IntoElement {
    let username_for_reply = username.clone();
    let username_for_mark_read = username;

    div()
        .pt_1()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(if can_compose {
                    SharedString::from("Reply opens the Whispers split. Right-click marks read.")
                } else if unread > 0 {
                    SharedString::from("Replies are unavailable. Mark read keeps the thread tidy.")
                } else {
                    SharedString::from("Replies are unavailable until whisper sending is restored.")
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(super::render_activity_copy_action(
                    format!("whisper-{index}-copy"),
                    copy_text,
                ))
                .when(can_compose, |el| {
                    el.child(
                        div()
                            .id(("whisper-reply", index))
                            .px_2()
                            .h(px(22.0))
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .bg(colors::accent().opacity(0.12))
                            .hover(|s| s.bg(colors::accent().opacity(0.2)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_activate(move |_event, _window, _cx| {
                                backend::push_ui_action(backend::UiAction::ComposeWhisper {
                                    username: username_for_reply.clone(),
                                    channel: None,
                                });
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(colors::accent_text())
                                    .child("Reply"),
                            ),
                    )
                })
                .when(unread > 0, |el| {
                    el.child(
                        div()
                            .id(("whisper-mark-read", index))
                            .px_2()
                            .h(px(22.0))
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .hover(|s| s.bg(colors::bg_hover()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_activate(move |_event, _window, _cx| {
                                backend::push_ui_action(backend::UiAction::MarkWhisperRead {
                                    username: username_for_mark_read.clone(),
                                });
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .child("Mark read"),
                            ),
                    )
                }),
        )
}
