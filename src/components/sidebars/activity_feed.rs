//! Chronological cross-channel Activity feed.
//!
//! The source-specific views remain available as filters, but this is the
//! default surface: mentions, whisper threads, and Twitch events share one row
//! grammar and one newest-first timeline.

use crate::backend::{self, ChatMessage, TwitchEvent, WhisperThread};
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use crate::utils::{format_sidebar_timestamp, parse_optional_hex_or};
use gpui::prelude::FluentBuilder;
use gpui::*;
use rustc_hash::FxHasher;
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone)]
enum ActivityFeedItem {
    Mention {
        channel: String,
        visible_channel: String,
        message_id: String,
        sender: String,
        sender_color: Hsla,
        preview: String,
        timestamp: i64,
    },
    Whisper {
        username: String,
        visible_username: String,
        preview: String,
        unread: u32,
        timestamp: i64,
    },
    Event {
        title: String,
        channel: String,
        preview: String,
        timestamp: i64,
    },
}

impl ActivityFeedItem {
    fn timestamp(&self) -> i64 {
        match self {
            Self::Mention { timestamp, .. }
            | Self::Whisper { timestamp, .. }
            | Self::Event { timestamp, .. } => *timestamp,
        }
    }
}

fn mention_preview(message: &ChatMessage, hide_content: bool) -> String {
    if hide_content {
        return "Mention hidden in streamer mode".to_string();
    }

    crate::utils::truncate_text(&message.content, 84)
}

thread_local! {
    /// UI-thread cache for the merged snapshot keyed by `merged_feed_signature`.
    ///
    /// An open sidebar re-renders with every frame the window paints, but the
    /// merged rows only change when a source history, the streamer-mode mask, or
    /// the theme does.
    static MERGED_FEED_CACHE: RefCell<Option<(u64, Arc<Vec<ActivityFeedItem>>)>> =
        const { RefCell::new(None) };
}

/// Change key for the merged feed snapshot.
///
/// Mirrors the mention/whisper sources of `sidebar_cache_signature` in the polling
/// coordinator (those caches are rebuilt unconditionally, so buffer identity cannot
/// stand in for content), extended with the event history plus the masking and theme
/// inputs that are baked into every row.
fn merged_feed_signature(
    mentions: &[(String, ChatMessage)],
    whispers: &[WhisperThread],
    events: &[TwitchEvent],
    hide_mentions: bool,
    hide_whispers: bool,
) -> u64 {
    let mut hasher = FxHasher::default();
    hide_mentions.hash(&mut hasher);
    hide_whispers.hash(&mut hasher);
    backend::streamer_mode::should_hide_mod_actions().hash(&mut hasher);
    backend::streamer_mode::should_hide_automod_terms().hash(&mut hasher);
    backend::settings::get_theme_settings_version().hash(&mut hasher);

    mentions.len().hash(&mut hasher);
    for (channel, message) in mentions {
        channel.hash(&mut hasher);
        message.id.hash(&mut hasher);
        (Arc::as_ptr(&message.slots).cast::<()>() as usize).hash(&mut hasher);
    }

    whispers.len().hash(&mut hasher);
    for thread in whispers {
        thread.username.hash(&mut hasher);
        thread.unread_count.hash(&mut hasher);
        thread.last_timestamp.hash(&mut hasher);
        thread.last_message.hash(&mut hasher);
    }

    events.len().hash(&mut hasher);
    for event in events {
        event.timestamp.hash(&mut hasher);
        event.channel.hash(&mut hasher);
    }

    hasher.finish()
}

/// Build one newest-first snapshot from the already-bounded source histories.
/// The merged vector adds no new residency policy; at most the retained rows
/// from the existing mention, whisper, and event stores are represented.
fn merged_activity_items(
    mentions: &[(String, ChatMessage)],
    whispers: &[WhisperThread],
    events: &[TwitchEvent],
) -> Arc<Vec<ActivityFeedItem>> {
    let hide_mentions = backend::streamer_mode::should_hide_mention_content();
    let hide_whispers = backend::streamer_mode::should_hide_whisper_content();
    let signature = merged_feed_signature(mentions, whispers, events, hide_mentions, hide_whispers);

    if let Some(cached) = MERGED_FEED_CACHE.with_borrow(|cache| {
        cache
            .as_ref()
            .filter(|(key, _)| *key == signature)
            .map(|(_, items)| items.clone())
    }) {
        return cached;
    }

    let mut items = Vec::with_capacity(mentions.len() + whispers.len() + events.len());

    items.extend(
        mentions
            .iter()
            .map(|(channel, message)| ActivityFeedItem::Mention {
                channel: channel.clone(),
                visible_channel: if hide_mentions {
                    "Channel hidden".to_string()
                } else {
                    channel.to_string()
                },
                message_id: message.id.to_string(),
                sender: if hide_mentions {
                    "Mention".to_string()
                } else {
                    message.display_name.to_string()
                },
                sender_color: if hide_mentions {
                    colors::text_secondary()
                } else {
                    parse_optional_hex_or(&message.color, colors::text_primary())
                },
                preview: mention_preview(message, hide_mentions),
                timestamp: message.timestamp,
            }),
    );

    items.extend(whispers.iter().map(|thread| ActivityFeedItem::Whisper {
        username: thread.username.clone(),
        visible_username: if hide_whispers {
            if thread.unread_count > 0 {
                "Unread whisper".to_string()
            } else {
                "Whisper thread".to_string()
            }
        } else {
            thread.username.clone()
        },
        preview: if hide_whispers {
            "Whisper hidden in streamer mode".to_string()
        } else {
            crate::utils::truncate_text(&thread.last_message, 84)
        },
        unread: thread.unread_count,
        timestamp: thread.last_timestamp,
    }));

    items.extend(events.iter().map(|event| ActivityFeedItem::Event {
        title: backend::twitch_event_title(&event.event_type).to_string(),
        channel: if event.channel.is_empty() {
            "Twitch".to_string()
        } else {
            event.channel.to_string()
        },
        preview: crate::utils::truncate_text(&backend::format_twitch_event_text(event), 84),
        timestamp: event.timestamp,
    }));

    items.sort_by_key(|item| std::cmp::Reverse(item.timestamp()));
    let items = Arc::new(items);
    MERGED_FEED_CACHE.with_borrow_mut(|cache| *cache = Some((signature, items.clone())));
    items
}

/// Render the default combined Activity timeline.
pub fn render_combined_activity_content<
    T: 'static,
    M: Fn(&mut Context<T>) + 'static,
    C: Fn(&mut Context<T>) + 'static,
>(
    mentions: Arc<Vec<(String, ChatMessage)>>,
    whispers: Arc<Vec<WhisperThread>>,
    events: Vec<TwitchEvent>,
    unread_count: usize,
    on_mark_all_read: M,
    on_clear_all: C,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, M, C> {
    let can_compose_whisper = backend::bridge::get_whisper_compose_block_reason().is_none();
    let items = merged_activity_items(&mentions, &whispers, &events);
    let item_count = items.len();
    let on_mark_all_read = std::rc::Rc::new(on_mark_all_read);
    let on_clear_all = std::rc::Rc::new(on_clear_all);

    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(38.0))
                .w_full()
                .px_3()
                .flex_shrink_0()
                .border_b_1()
                .border_color(colors::border())
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Newest activity across every open channel"),
                )
                .when(unread_count > 0 || item_count > 0, |toolbar| {
                    toolbar.child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(unread_count > 0, |actions| {
                                actions.child(
                                    div()
                                        .id("activity-mark-all-read")
                                        .h(px(24.0))
                                        .px_2()
                                        .rounded(Radii::sm())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(colors::bg_hover()))
                                        .flex()
                                        .items_center()
                                        .on_activate(cx.listener({
                                            let on_mark_all_read = on_mark_all_read.clone();
                                            move |_this, _event, _window, cx| {
                                                on_mark_all_read(cx);
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
                            })
                            .when(item_count > 0, |actions| {
                                actions.child(
                                    div()
                                        .id("activity-clear-all")
                                        .h(px(24.0))
                                        .px_2()
                                        .rounded(Radii::sm())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(colors::bg_hover()))
                                        .flex()
                                        .items_center()
                                        .on_activate(cx.listener({
                                            let on_clear_all = on_clear_all.clone();
                                            move |_this, _event, _window, cx| {
                                                on_clear_all(cx);
                                                cx.stop_propagation();
                                            }
                                        }))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors::text_secondary())
                                                .child("Clear all"),
                                        ),
                                )
                            }),
                    )
                }),
        )
        .child(if item_count == 0 {
            render_empty_feed().into_any_element()
        } else {
            uniform_list(
                ElementId::Name("activity-combined-list".into()),
                item_count,
                move |range, _window, _cx| {
                    range
                        .map(|idx| render_feed_item(idx, items[idx].clone(), can_compose_whisper))
                        .collect()
                },
            )
            .flex_1()
            .w_full()
            .into_any_element()
        })
}

fn render_empty_feed() -> impl IntoElement {
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
                .child(div().text_3xl().text_color(colors::text_muted()).child("◎"))
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child("No activity yet"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Mentions, whispers, and channel events appear here"),
                ),
        )
}

fn render_feed_item(index: usize, item: ActivityFeedItem, can_compose_whisper: bool) -> AnyElement {
    match item {
        ActivityFeedItem::Mention {
            channel,
            visible_channel,
            message_id,
            sender,
            sender_color,
            preview,
            timestamp,
        } => {
            let copy_text =
                super::format_activity_copy_text("Mention", &sender, &visible_channel, &preview);
            let jump_channel = channel.clone();
            let dismiss_channel = channel.clone();
            let dismiss_message_id = message_id.clone();
            feed_row(
                format!("activity-all-mention-{index}"),
                "Mention",
                colors::mention_border(),
                sender,
                sender_color,
                visible_channel,
                preview,
                timestamp,
                None,
                copy_text,
            )
            .on_activate(move |_event, _window, _cx| {
                backend::push_ui_action(backend::UiAction::JumpToMessage {
                    channel: jump_channel.clone(),
                    message_id: message_id.clone(),
                });
            })
            .child(dismiss_button(
                format!("activity-all-mention-{index}-dismiss"),
                "Dismiss mention",
                move |cx| {
                    backend::push_ui_action(backend::UiAction::DismissMention {
                        channel: dismiss_channel.clone(),
                        message_id: dismiss_message_id.clone(),
                    });
                    cx.stop_propagation();
                },
            ))
            .into_any_element()
        }
        ActivityFeedItem::Whisper {
            username,
            visible_username,
            preview,
            unread,
            timestamp,
        } => {
            let copy_text = super::format_activity_copy_text(
                "Whisper",
                &visible_username,
                "Direct message",
                &preview,
            );
            let username_for_action = username.clone();
            let username_for_mark_read = username.clone();
            let unread_label = (unread > 0).then(|| format!("{unread} unread"));
            feed_row(
                format!("activity-all-whisper-{index}"),
                "Whisper",
                colors::whisper_border(),
                visible_username,
                colors::text_primary(),
                "Direct message".to_string(),
                preview,
                timestamp,
                unread_label,
                copy_text,
            )
            .on_activate(move |_event, _window, _cx| {
                if can_compose_whisper {
                    backend::push_ui_action(backend::UiAction::ComposeWhisper {
                        username: username_for_action.clone(),
                        channel: None,
                    });
                } else {
                    backend::push_ui_action(backend::UiAction::ShowWarningToast {
                        message: "Whisper reply unavailable".to_string(),
                        detail: backend::bridge::get_whisper_compose_block_reason(),
                    });
                }
            })
            .when(unread > 0, |row| {
                row.child(dismiss_button(
                    format!("activity-all-whisper-{index}-dismiss"),
                    "Mark whisper read",
                    move |cx| {
                        backend::push_ui_action(backend::UiAction::MarkWhisperRead {
                            username: username_for_mark_read.clone(),
                        });
                        cx.stop_propagation();
                    },
                ))
            })
            .into_any_element()
        }
        ActivityFeedItem::Event {
            title,
            channel,
            preview,
            timestamp,
        } => {
            let copy_text = super::format_activity_copy_text("Event", &title, &channel, &preview);
            let event_timestamp = timestamp;
            feed_row(
                format!("activity-all-event-{index}"),
                "Event",
                colors::info(),
                title,
                colors::text_primary(),
                channel,
                preview,
                timestamp,
                None,
                copy_text,
            )
            .on_activate(move |_event, _window, _cx| {
                backend::push_ui_action(backend::UiAction::OpenNotifications {
                    filter: backend::ui_actions::NotificationFilter::All,
                });
            })
            .child(dismiss_button(
                format!("activity-all-event-{index}-dismiss"),
                "Dismiss event",
                move |cx| {
                    backend::remove_recent_twitch_event(event_timestamp);
                    cx.refresh_windows();
                    cx.stop_propagation();
                },
            ))
            .into_any_element()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn feed_row(
    id: String,
    source: &'static str,
    source_color: Hsla,
    title: String,
    title_color: Hsla,
    context: String,
    preview: String,
    timestamp: i64,
    status: Option<String>,
    copy_text: String,
) -> Stateful<Div> {
    let timestamp = format_sidebar_timestamp(timestamp);
    let copy_action_id = format!("{id}-copy");

    div()
        .id(SharedString::from(id))
        .relative()
        .w_full()
        .px_3()
        .py_2()
        .pr(px(Spacing::XXXL + Spacing::XS))
        .cursor_pointer()
        .border_b_1()
        .border_color(colors::border())
        .hover(|style| style.bg(colors::bg_hover()))
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .px_1p5()
                                        .h(px(18.0))
                                        .rounded(Radii::sm())
                                        .bg(source_color.opacity(0.12))
                                        .flex()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_size(px(FontSize::XS))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(source_color)
                                                .child(source),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(title_color)
                                        .child(title),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(timestamp),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(context),
                        )
                        .when_some(status, |row, status| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .px_1p5()
                                    .rounded(Radii::full())
                                    .bg(colors::accent().opacity(0.14))
                                    .child(
                                        div()
                                            .text_size(px(FontSize::XS))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors::accent_text())
                                            .child(status),
                                    ),
                            )
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child(preview),
                )
                .child(div().pt_1().child(super::render_activity_copy_action(
                    copy_action_id,
                    copy_text,
                ))),
        )
}

fn dismiss_button(
    id: String,
    label: &'static str,
    on_dismiss: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .absolute()
        .role(Role::Button)
        .aria_label(label)
        .right_2()
        .top_2()
        .w(px(22.0))
        .h(px(22.0))
        .rounded(Radii::sm())
        .cursor_pointer()
        .hover(|style| style.bg(colors::hover_surface()))
        .flex()
        .items_center()
        .justify_center()
        .on_activate(move |_event, _window, cx| on_dismiss(cx))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(colors::text_muted())
                .child(crate::icons::CLOSE_GLYPH),
        )
        .child(
            div()
                .absolute()
                .opacity(0.0)
                .text_size(px(1.0))
                .child(label),
        )
}
