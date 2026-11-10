//! The activity rail inside the main window: its sidebar, the Events feed
//! rows with their AutoMod and suspicious-user actions, and marking what the
//! user has seen as read.

use super::*;

fn is_automod_notification_event(event: &backend::TwitchEvent) -> bool {
    matches!(
        &event.event_type,
        backend::types::TwitchEventType::AutomodMessageHold
            | backend::types::TwitchEventType::AutomodMessageUpdate
    )
}

fn pending_automod_message_id(event: &backend::TwitchEvent) -> Option<String> {
    if !matches!(
        &event.event_type,
        backend::types::TwitchEventType::AutomodMessageHold
    ) {
        return None;
    }

    event
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|message_id| !message_id.is_empty())
        .map(str::to_string)
}

/// The user a suspicious-user notification is about, and its reported status.
#[derive(Clone)]
struct SuspiciousUserTarget {
    channel: String,
    user_login: String,
    user_id: Option<String>,
    status: String,
}

impl SuspiciousUserTarget {
    /// The two treatment actions offered for the user's current status.
    fn offered_actions(&self) -> [backend::commands::helix::SuspiciousUserAction; 2] {
        use backend::commands::helix::SuspiciousUserAction::{Clear, Monitor, Restrict};
        let is_restricted = self.status.eq_ignore_ascii_case("restricted");
        let is_monitored = self.status.eq_ignore_ascii_case("active_monitoring")
            || self.status.eq_ignore_ascii_case("monitored");
        if is_restricted {
            [Monitor, Clear]
        } else if is_monitored {
            [Restrict, Clear]
        } else {
            [Monitor, Restrict]
        }
    }
}

fn suspicious_user_notification_target(
    event: &backend::TwitchEvent,
) -> Option<SuspiciousUserTarget> {
    if !matches!(
        &event.event_type,
        backend::types::TwitchEventType::SuspiciousUserMessage
            | backend::types::TwitchEventType::SuspiciousUserUpdate
    ) {
        return None;
    }

    let channel = event.channel.trim().trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return None;
    }

    let user_login = event
        .user_login
        .as_deref()
        .or(event.user_name.as_deref())
        .unwrap_or_default()
        .trim()
        .trim_start_matches('@')
        .to_lowercase();
    let user_id = event
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if user_login.is_empty() && user_id.is_none() {
        return None;
    }

    Some(SuspiciousUserTarget {
        channel,
        user_login,
        user_id,
        status: event
            .status
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
}

fn accent_action_button(
    id: SharedString,
    label: &'static str,
    accent: Hsla,
    on_click: impl Fn() + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(Radii::sm())
        .border_1()
        .border_color(accent.opacity(0.45))
        .bg(accent.opacity(0.12))
        .cursor_pointer()
        .hover(move |s| s.bg(accent.opacity(0.2)))
        .on_activate(move |_event, _window, _cx| on_click())
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(accent)
                .child(label),
        )
}

fn automod_action_button(
    label: &'static str,
    message_id: String,
    allow: bool,
    event_timestamp: i64,
) -> Stateful<Div> {
    let accent = if allow {
        colors::success()
    } else {
        colors::error()
    };

    let id = SharedString::from(format!("automod-{label}-{message_id}"));
    accent_action_button(id, label, accent, move || {
        backend::manage_held_automod_message_async(message_id.clone(), allow, event_timestamp);
    })
}

fn suspicious_user_action_button(
    target: &SuspiciousUserTarget,
    action: backend::commands::helix::SuspiciousUserAction,
    event_timestamp: i64,
) -> Stateful<Div> {
    let (label, accent) = match action {
        backend::commands::helix::SuspiciousUserAction::Monitor => ("Monitor", colors::info()),
        backend::commands::helix::SuspiciousUserAction::Restrict => ("Restrict", colors::warning()),
        backend::commands::helix::SuspiciousUserAction::Clear => ("Clear", colors::text_muted()),
    };

    let id = SharedString::from(format!(
        "suspicious-{label}-{}-{event_timestamp}",
        target.user_login
    ));
    let target = target.clone();
    accent_action_button(id, label, accent, move || {
        backend::manage_suspicious_user_status_async(
            target.channel.clone(),
            target.user_login.clone(),
            target.user_id.clone(),
            action,
            event_timestamp,
        );
    })
}

impl EchoApp {
    pub(super) fn render_activity_sidebar(
        &mut self,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (panel, mentions, whispers) = {
            let state = self.app_state.read(cx);
            (
                state.activity_panel,
                state.cached_mentions.clone(),
                state.cached_whispers.clone(),
            )
        };
        let mention_count = backend::get_unread_global_mention_count();
        let unread_whispers = whispers.iter().fold(0u32, |total, thread| {
            total.saturating_add(thread.unread_count)
        });
        let events = match panel.section {
            ActivitySection::All | ActivitySection::Events => backend::get_recent_twitch_events(50),
            ActivitySection::Mentions | ActivitySection::Whispers => Vec::new(),
        };
        let event_count = backend::get_unread_twitch_event_count();
        let unread_activity_count = mention_count
            .saturating_add(unread_whispers as usize)
            .saturating_add(event_count);

        let content = match panel.section {
            ActivitySection::All => {
                let app_state_for_read = self.app_state.clone();
                let app_state_for_clear = self.app_state.clone();
                crate::components::sidebars::render_combined_activity_content(
                    mentions,
                    whispers,
                    events,
                    unread_activity_count,
                    move |cx: &mut Context<Self>| {
                        backend::mark_all_activity_read();
                        app_state_for_read.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    move |cx: &mut Context<Self>| {
                        backend::clear_all_activity();
                        app_state_for_clear.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    cx,
                )
                .into_any_element()
            }
            ActivitySection::Mentions => {
                let app_state = self.app_state.clone();
                crate::components::sidebars::render_mentions_activity_content(
                    mentions,
                    move |cx: &mut Context<Self>| {
                        backend::clear_global_mentions();
                        app_state.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    cx,
                )
                .into_any_element()
            }
            ActivitySection::Whispers => {
                let app_state = self.app_state.clone();
                crate::components::sidebars::render_whispers_activity_content(
                    whispers,
                    move |cx: &mut Context<Self>| {
                        backend::mark_all_whisper_threads_read();
                        app_state.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    cx,
                )
                .into_any_element()
            }
            ActivitySection::Events => {
                self.render_activity_events_content(panel.event_filter, events, cx)
            }
        };

        let app_state_for_select = self.app_state.clone();
        let app_state_for_close = self.app_state.clone();
        crate::components::sidebars::render_activity_sidebar(
            width,
            panel.section,
            crate::components::sidebars::ActivityCounts {
                mentions: mention_count,
                unread_whispers,
                events: event_count,
            },
            content,
            move |section, cx: &mut Context<Self>| {
                app_state_for_select
                    .update(cx, |state, cx| state.select_activity_section(section, cx));
            },
            move |cx: &mut Context<Self>| {
                app_state_for_close.update(cx, |state, cx| state.close_activity(cx));
            },
            cx,
        )
        .into_any_element()
    }

    /// Clear unread Activity attention using the visible state of mounted chat canvases.
    ///
    /// Runs at UI-poll cadence, so it is gated twice: nothing can be cleared while
    /// both unread sets are empty, and an unchanged viewport cannot clear anything
    /// the pass that produced the same key did not already clear. The unread counts
    /// are part of the key so newly arrived attention always re-harvests.
    pub(super) fn mark_observed_activity_read(&mut self, cx: &Context<Self>) -> bool {
        let unread_mentions = backend::get_unread_global_mention_count();
        let unread_events = backend::get_unread_twitch_event_count();
        if unread_mentions == 0 && unread_events == 0 {
            self.activity_read_visibility_key = None;
            return false;
        }

        let mut key = {
            let mut hasher = FxHasher::default();
            unread_mentions.hash(&mut hasher);
            unread_events.hash(&mut hasher);
            hasher.finish()
        };
        for (split_id, chat_view) in &self.chat_views {
            let mut hasher = FxHasher::default();
            split_id.hash(&mut hasher);
            chat_view
                .read(cx)
                .canvas_list
                .read(cx)
                .visibility_generation()
                .hash(&mut hasher);
            key ^= hasher.finish();
        }
        if self.activity_read_visibility_key == Some(key) {
            return false;
        }
        self.activity_read_visibility_key = Some(key);

        let mut visible_mentions: Vec<(&str, &str)> = Vec::new();
        let mut live_following_channels = Vec::new();

        for chat_view in self.chat_views.values() {
            let view = chat_view.read(cx);
            let list = view.canvas_list.read(cx);
            if unread_mentions > 0 {
                visible_mentions.extend(
                    list.visible_message_ids()
                        .iter()
                        .map(|message_id| (view.channel.as_str(), message_id.as_ref())),
                );
            }
            if unread_events > 0 && list.is_live_following() {
                live_following_channels.push(view.channel.clone());
            }
        }

        let mentions = backend::mark_visible_global_mentions_read(&visible_mentions);
        let events = backend::mark_recent_twitch_events_read_for_channels(&live_following_channels);
        mentions || events
    }

    pub(super) fn render_notification_event_row(
        event: backend::TwitchEvent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = backend::twitch_event_title(&event.event_type).to_string();
        let body = backend::format_twitch_event_text(&event);
        let event_timestamp = event.timestamp;
        let dismiss_timestamp = event.timestamp;
        let timestamp = crate::utils::format_sidebar_timestamp(event.timestamp);
        let event_channel = event.channel.clone();
        let automod_message_id = pending_automod_message_id(&event);
        let suspicious_user_target = suspicious_user_notification_target(&event);
        let channel = if event.channel.is_empty() {
            "Twitch".to_string()
        } else {
            event.channel.to_string()
        };
        let copy_text = crate::components::sidebars::format_activity_copy_text(
            "Event", &title, &channel, &body,
        );
        let copy_action_id = {
            let mut hasher = FxHasher::default();
            event_timestamp.hash(&mut hasher);
            copy_text.hash(&mut hasher);
            format!("activity-event-{}-copy", hasher.finish())
        };

        let row = div()
            .id(SharedString::from(format!(
                "activity-event-{event_timestamp}"
            )))
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(colors::border())
            .on_right_click(cx.listener(move |_this, _, _window, cx| {
                backend::remove_recent_twitch_event(event_timestamp);
                cx.notify();
            }))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(div().max_w(px(120.0)).truncate().child(channel))
                            .when(!timestamp.is_empty(), |s| {
                                s.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(colors::text_muted())
                                        .child(timestamp),
                                )
                            })
                            .child(crate::components::sidebars::render_activity_copy_action(
                                copy_action_id,
                                copy_text,
                            ))
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "dismiss-activity-event-{dismiss_timestamp}"
                                    )))
                                    .role(Role::Button)
                                    .aria_label("Dismiss event")
                                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                                        format!("dismiss-activity-event-{dismiss_timestamp}"),
                                        "Dismiss".to_string(),
                                    ))
                                    // 24 px hit area in the 18 px slot it always had.
                                    .size(px(Spacing::XXL))
                                    .m(px(-Spacing::XS_PLUS / 2.0))
                                    .rounded(Radii::sm())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(colors::bg_hover()))
                                    .on_activate(cx.listener(move |_this, _event, _window, cx| {
                                        backend::remove_recent_twitch_event(dismiss_timestamp);
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(colors::text_muted())
                                            .child(crate::icons::CLOSE_GLYPH),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .pt_1()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child(body),
            )
            .when_some(automod_message_id, move |row, message_id| {
                row.child(
                    div()
                        .pt_2()
                        .flex()
                        .gap_2()
                        .child(automod_action_button(
                            "Allow",
                            message_id.clone(),
                            true,
                            event_timestamp,
                        ))
                        .child(automod_action_button(
                            "Deny",
                            message_id,
                            false,
                            event_timestamp,
                        )),
                )
            })
            .when_some(suspicious_user_target, move |row, target| {
                let actions = target
                    .offered_actions()
                    .map(|action| suspicious_user_action_button(&target, action, event_timestamp));
                row.child(div().pt_2().flex().gap_2().children(actions))
            });

        if event_channel.is_empty() {
            row.into_any_element()
        } else {
            row.cursor_pointer()
                .hover(|s| s.bg(colors::bg_hover()))
                .on_activate(cx.listener(move |this, _, _window, cx| {
                    this.focus_notification_channel(&event_channel, cx);
                    this.app_state.update(cx, |state, cx| {
                        state.close_activity(cx);
                    });
                }))
                .into_any_element()
        }
    }

    pub(super) fn focus_notification_channel(
        &mut self,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let channel = channel.trim_start_matches('#').to_lowercase();
        if channel.is_empty() {
            return false;
        }

        let target = {
            let app_state = self.app_state.read(cx);
            find_open_tab_channel_target(app_state, &channel)
        };

        let Some((tab_index, path, _resolved_channel)) = target else {
            crate::log_ui!(
                warn,
                "Notification target channel is not visible in any open tab: {}",
                channel
            );
            return false;
        };

        self.activate_tab_split(tab_index, path, cx);
        true
    }

    pub(super) fn render_activity_events_content(
        &mut self,
        notification_filter: backend::ui_actions::NotificationFilter,
        mut events: Vec<backend::TwitchEvent>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (description, empty_text) = match notification_filter {
            backend::ui_actions::NotificationFilter::All => {
                ("Recent channel events", "No recent channel events")
            }
            backend::ui_actions::NotificationFilter::Automod => {
                ("AutoMod review", "No recent AutoMod messages")
            }
        };
        if matches!(
            notification_filter,
            backend::ui_actions::NotificationFilter::Automod
        ) {
            events.retain(is_automod_notification_event);
        }
        let has_events = !events.is_empty();
        let content = if events.is_empty() {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child(empty_text),
                )
                .into_any_element()
        } else {
            div()
                .id("activity-event-list")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .children(
                    events
                        .into_iter()
                        .map(|event| Self::render_notification_event_row(event, cx)),
                )
                .into_any_element()
        };

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
                            .child(description),
                    )
                    .when(has_events, |toolbar| {
                        toolbar.child(
                            div()
                                .id("clear-activity-events")
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .cursor_pointer()
                                .hover(|style| style.text_color(colors::text_secondary()))
                                .on_activate(cx.listener(move |_this, _, _, cx| {
                                    match notification_filter {
                                        backend::ui_actions::NotificationFilter::All => {
                                            backend::clear_recent_twitch_events();
                                        }
                                        backend::ui_actions::NotificationFilter::Automod => {
                                            backend::clear_recent_automod_events();
                                        }
                                    }
                                    cx.notify();
                                    cx.stop_propagation();
                                }))
                                .child("Clear"),
                        )
                    }),
            )
            .child(content)
            .into_any_element()
    }
}
