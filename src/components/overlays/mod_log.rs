//! Mod Log Viewer
//!
//! Displays recent moderation actions (bans, timeouts, message deletions)
//! in a modal overlay.
//!
//! Note: This Entity-based component is for planned mod log panel.

use super::traits::{Openable, OverlayPointerIsolation};
use crate::backend::{self, ModAction, ModActionType};
use crate::components::common::mod_buttons::{ModButtonStyle, render_mod_button};
use crate::components::common::moderation_policy;
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::utils::format_time_24h_sec;
use gpui::prelude::FluentBuilder;
use gpui::*;

const MOD_LOG_REFRESH_INTERVAL_MS: u64 = 400;
const MOD_LOG_REFRESH_TICKS: usize = 20;

/// Mod log state
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModLogFilter {
    #[default]
    All,
    Action(ModActionType),
    Roles,
    Warnings,
    Shield,
}

#[derive(Default)]
pub struct ModLogState {
    pub is_open: bool,
    pub actions: Vec<ModAction>,
    pub filter: ModLogFilter,
}

impl Openable for ModLogState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

/// Mod Log Viewer component
pub struct ModLogViewer {
    pub state: ModLogState,
    refresh_waker_running: bool,
    /// Bumped whenever the backing action list is replaced or cleared. Together
    /// with the active filter it keys `filtered_indices`, so a repaint that
    /// changed neither does not re-scan the list.
    actions_generation: u64,
    filtered_cache_key: Option<(u64, ModLogFilter)>,
    filtered_indices: Vec<usize>,
}

impl ModLogViewer {
    pub fn new() -> Self {
        Self {
            state: ModLogState::default(),
            refresh_waker_running: false,
            actions_generation: 0,
            filtered_cache_key: None,
            filtered_indices: Vec::new(),
        }
    }
}

impl Default for ModLogViewer {
    fn default() -> Self {
        Self::new()
    }
}

impl ModLogViewer {
    pub fn open(&mut self) {
        Openable::open(&mut self.state);
        self.state.filter = ModLogFilter::All;
        self.refresh();
    }

    pub fn close(&mut self) {
        Openable::close(&mut self.state);
    }

    pub fn refresh(&mut self) -> bool {
        let next = backend::bridge::get_mod_actions(100);
        let changed = self.state.actions.len() != next.len()
            || self.state.actions.first().map(mod_action_signature)
                != next.first().map(mod_action_signature);
        self.state.actions = next;
        if changed {
            self.actions_generation = self.actions_generation.wrapping_add(1);
        }
        changed
    }

    /// Refresh `filtered_indices` only when the action list or the filter moved.
    fn ensure_filtered_indices(&mut self) {
        let key = (self.actions_generation, self.state.filter);
        if self.filtered_cache_key == Some(key) {
            return;
        }
        let filter = self.state.filter;
        self.filtered_indices.clear();
        self.filtered_indices.extend(
            self.state
                .actions
                .iter()
                .enumerate()
                .filter(|(_, action)| mod_log_filter_matches(filter, action.action_type))
                .map(|(index, _)| index),
        );
        self.filtered_cache_key = Some(key);
    }

    fn start_refresh_waker(&mut self, cx: &mut Context<Self>) {
        if self.refresh_waker_running || !self.state.is_open {
            return;
        }

        self.refresh_waker_running = true;
        cx.spawn(async move |this, cx| {
            for _ in 0..MOD_LOG_REFRESH_TICKS {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(
                        MOD_LOG_REFRESH_INTERVAL_MS,
                    ))
                    .await;

                let keep_running = this
                    .update(cx, |viewer, cx| {
                        if !viewer.state.is_open {
                            viewer.refresh_waker_running = false;
                            return false;
                        }

                        if viewer.refresh() {
                            crate::backend::metrics::record_ui_notify_source(
                                "mod_log.refresh_waker",
                            );
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    return;
                }
            }

            let _ = this.update(cx, |viewer, _cx| {
                viewer.refresh_waker_running = false;
            });
        })
        .detach();
    }
}

impl Render for ModLogViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }

        self.start_refresh_waker(cx);
        self.ensure_filtered_indices();

        let actions = &self.state.actions;
        let filtered = &self.filtered_indices;
        let current_filter = self.state.filter;
        let modal_fit =
            crate::utils::fit_overlay_to_viewport(window.viewport_size(), 600.0, 500.0, 12.0);

        div()
            .id("mod-log-modal")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors::backdrop())
            .block_overlay_pointer_passthrough()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.close();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("mod-log-panel")
                    .w(px(modal_fit.width))
                    .max_h(px(modal_fit.height))
                    .bg(colors::bg_surface())
                    .rounded_lg()
                    .border_1()
                    .border_color(colors::border())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .block_overlay_pointer_passthrough()
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(colors::border())
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors::text_primary())
                                    .child("Mod Log"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(colors::text_secondary())
                                    .child(format!("{} actions", filtered.len())),
                            ),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .border_b_1()
                            .border_color(colors::border())
                            .flex()
                            .gap_2()
                            .child(self.render_state_filter_button(
                                "All",
                                ModLogFilter::All,
                                current_filter == ModLogFilter::All,
                                colors::text_secondary(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Bans",
                                ModLogFilter::Action(ModActionType::Ban),
                                current_filter == ModLogFilter::Action(ModActionType::Ban),
                                colors::signal_red(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Unbans",
                                ModLogFilter::Action(ModActionType::Unban),
                                current_filter == ModLogFilter::Action(ModActionType::Unban),
                                colors::success(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Timeouts",
                                ModLogFilter::Action(ModActionType::Timeout),
                                current_filter == ModLogFilter::Action(ModActionType::Timeout),
                                colors::signal_yellow(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Roles",
                                ModLogFilter::Roles,
                                current_filter == ModLogFilter::Roles,
                                colors::accent(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Warnings",
                                ModLogFilter::Warnings,
                                current_filter == ModLogFilter::Warnings,
                                colors::warning(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Shield",
                                ModLogFilter::Shield,
                                current_filter == ModLogFilter::Shield,
                                colors::link(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Deletions",
                                ModLogFilter::Action(ModActionType::Delete),
                                current_filter == ModLogFilter::Action(ModActionType::Delete),
                                colors::warning(),
                                cx,
                            ))
                            .child(self.render_state_filter_button(
                                "Clears",
                                ModLogFilter::Action(ModActionType::Clear),
                                current_filter == ModLogFilter::Action(ModActionType::Clear),
                                colors::link(),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .id("mod-log-list-inner")
                            .flex_1()
                            .overflow_y_scroll()
                            .px_4()
                            .py_2()
                            .when(filtered.is_empty(), |list| {
                                list.child(
                                    div()
                                        .py_4()
                                        .text_sm()
                                        .text_color(colors::text_muted())
                                        .child(if actions.is_empty() {
                                            "No moderation actions yet"
                                        } else {
                                            "No actions match this filter"
                                        }),
                                )
                            })
                            .children(
                                filtered
                                    .iter()
                                    .map(|index| render_mod_action(*index, &actions[*index])),
                            ),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .border_t_1()
                            .border_color(colors::border())
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("mod-log-clear-all")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(colors::bg_plate())
                                    .text_sm()
                                    .text_color(colors::text_secondary())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(colors::hover_surface()))
                                    .on_activate(|_, _, _| {
                                        backend::push_ui_action(
                                            backend::UiAction::ConfirmClearModLog,
                                        );
                                    })
                                    .child("Clear all"),
                            )
                            .child(
                                div()
                                    .id("mod-log-close")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(colors::accent())
                                    .text_sm()
                                    .text_color(colors::text_on_accent_contrast())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(colors::accent_hover()))
                                    .on_activate(cx.listener(|this, _, _window, cx| {
                                        this.close();
                                        cx.notify();
                                    }))
                                    .child("Close"),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn mod_action_signature(action: &ModAction) -> (ModActionType, &str, &str, &str, i64) {
    (
        action.action_type,
        action.username.as_str(),
        action.channel.as_str(),
        action.details.as_str(),
        action.timestamp,
    )
}

/// Render a filter button
fn render_filter_button(label: &'static str, is_active: bool, color: Hsla) -> Stateful<Div> {
    let bg = if is_active {
        colors::bg_elevated()
    } else {
        colors::bg_plate()
    };

    let border_color = if is_active { color } else { colors::border() };

    div()
        .id(label)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .text_sm()
        .text_color(color)
        .cursor_pointer()
        .hover(|s| s.bg(colors::hover_surface()))
        .child(label)
}

impl ModLogViewer {
    fn render_state_filter_button(
        &self,
        label: &'static str,
        filter: ModLogFilter,
        is_active: bool,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        render_filter_button(label, is_active, color).on_activate(cx.listener(
            move |this, _, _window, cx| {
                this.state.filter = filter;
                cx.notify();
            },
        ))
    }
}

fn mod_log_filter_matches(filter: ModLogFilter, action_type: ModActionType) -> bool {
    match filter {
        ModLogFilter::All => true,
        ModLogFilter::Action(filter_type) => filter_type == action_type,
        ModLogFilter::Roles => matches!(
            action_type,
            ModActionType::Mod | ModActionType::Unmod | ModActionType::Vip | ModActionType::Unvip
        ),
        ModLogFilter::Warnings => matches!(action_type, ModActionType::Warning),
        ModLogFilter::Shield => matches!(
            action_type,
            ModActionType::ShieldEnable | ModActionType::ShieldDisable
        ),
    }
}

/// Render a single mod action entry
fn render_mod_action(index: usize, action: &ModAction) -> impl IntoElement + use<> {
    let (icon, color, label) = match action.action_type {
        ModActionType::Ban => ("BAN", colors::signal_red(), "banned"),
        ModActionType::Unban => ("UNB", colors::success(), "unbanned"),
        ModActionType::Timeout => ("TO", colors::signal_yellow(), "timed out"),
        ModActionType::Delete => ("DEL", colors::warning(), "message deleted"),
        ModActionType::Clear => ("CLR", colors::link(), "chat cleared"),
        ModActionType::Mod => ("MOD", colors::accent(), "was granted moderator"),
        ModActionType::Unmod => ("UNM", colors::signal_yellow(), "lost moderator"),
        ModActionType::Vip => ("VIP", colors::success(), "was granted VIP"),
        ModActionType::Unvip => ("UVP", colors::warning(), "lost VIP"),
        ModActionType::Warning => ("WRN", colors::warning(), "warning"),
        ModActionType::ShieldEnable => ("SHD", colors::accent(), "shield enabled"),
        ModActionType::ShieldDisable => ("SHD", colors::text_secondary(), "shield disabled"),
    };

    let hide_details = backend::streamer_mode::should_hide_mod_actions();
    let username = masked_mod_action_username(action, hide_details);
    let details = masked_mod_action_details(action, hide_details);
    let channel = masked_mod_action_channel(action, hide_details);
    let timestamp_str = format_time_24h_sec(action.timestamp);
    let can_run_quick_action = QuickActionPolicy::for_action(action.action_type)
        .can_show(&action.channel, &action.username);
    let quick_actions = if hide_details || action.masked || !can_run_quick_action {
        None
    } else {
        Some(render_mod_action_buttons(index, action))
    };

    div()
        .py_2()
        .border_b_1()
        .border_color(colors::border())
        .flex()
        .gap_3()
        .items_start()
        .child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(color.opacity(0.2))
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child(icon),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .items_baseline()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child(username),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .child(label),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_tertiary())
                        .child(details),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(channel)
                        .child(timestamp_str),
                ),
        )
        .when_some(quick_actions, |row, quick_actions| row.child(quick_actions))
}

fn masked_mod_action_username(action: &ModAction, hide_details: bool) -> String {
    if !hide_details {
        return action.username.clone();
    }

    match action.action_type {
        ModActionType::Clear => "Hidden channel".to_string(),
        ModActionType::Ban
        | ModActionType::Unban
        | ModActionType::Timeout
        | ModActionType::Delete
        | ModActionType::Mod
        | ModActionType::Unmod
        | ModActionType::Vip
        | ModActionType::Unvip
        | ModActionType::Warning => "Hidden user".to_string(),
        ModActionType::ShieldEnable | ModActionType::ShieldDisable => "Hidden channel".to_string(),
    }
}

fn masked_mod_action_details(action: &ModAction, hide_details: bool) -> String {
    if !hide_details {
        return action.details.clone();
    }

    match action.action_type {
        ModActionType::Ban => "Ban details hidden in streamer mode".to_string(),
        ModActionType::Unban => "Unban details hidden in streamer mode".to_string(),
        ModActionType::Timeout => "Timeout details hidden in streamer mode".to_string(),
        ModActionType::Delete => "Deleted-message details hidden in streamer mode".to_string(),
        ModActionType::Clear => "Chat-clear details hidden in streamer mode".to_string(),
        ModActionType::Mod => "Moderator details hidden in streamer mode".to_string(),
        ModActionType::Unmod => "Moderator details hidden in streamer mode".to_string(),
        ModActionType::Vip => "VIP details hidden in streamer mode".to_string(),
        ModActionType::Unvip => "VIP details hidden in streamer mode".to_string(),
        ModActionType::Warning => "Warning details hidden in streamer mode".to_string(),
        ModActionType::ShieldEnable | ModActionType::ShieldDisable => {
            "Shield Mode details hidden in streamer mode".to_string()
        }
    }
}

fn masked_mod_action_channel(action: &ModAction, hide_details: bool) -> String {
    if hide_details {
        "Channel hidden".to_string()
    } else {
        action.channel.to_string()
    }
}

/// Which authority check admits a mod-log quick action, both at render time
/// and again at the bridge boundary.
#[derive(Clone, Copy)]
enum QuickActionPolicy {
    /// Ban/timeout/unban: any channel moderator.
    Moderation,
    /// Moderator/VIP role changes: broadcaster only.
    RoleManagement,
}

impl QuickActionPolicy {
    fn for_action(action_type: ModActionType) -> Self {
        match action_type {
            ModActionType::Mod
            | ModActionType::Unmod
            | ModActionType::Vip
            | ModActionType::Unvip => Self::RoleManagement,
            _ => Self::Moderation,
        }
    }

    fn can_show(self, channel: &str, username: &str) -> bool {
        match self {
            Self::Moderation => moderation_policy::can_moderate_target(channel, username, &[]),
            Self::RoleManagement => moderation_policy::can_manage_role_target(channel, username),
        }
    }

    fn recheck(self, channel: &str, username: &str) -> bool {
        match self {
            Self::Moderation => moderation_policy::recheck_moderation_target(channel, username),
            Self::RoleManagement => {
                moderation_policy::recheck_role_management_target(channel, username)
            }
        }
    }
}

/// One button offered next to a logged action, usually its inverse.
struct QuickAction {
    label: &'static str,
    style: ModButtonStyle,
    run: fn(&str, &str),
}

const UNBAN: QuickAction = QuickAction {
    label: "Unban",
    style: ModButtonStyle::Success,
    run: |channel, username| backend::bridge::unban_user(channel, username),
};
const UNTIMEOUT: QuickAction = QuickAction {
    label: "Untimeout",
    style: ModButtonStyle::Success,
    run: |channel, username| backend::bridge::unban_user(channel, username),
};
const BAN: QuickAction = QuickAction {
    label: "Ban",
    style: ModButtonStyle::Danger,
    run: |channel, username| backend::bridge::ban_user(channel, username, None),
};
const TIMEOUT_10M: QuickAction = QuickAction {
    label: "10m",
    style: ModButtonStyle::Warning,
    run: |channel, username| backend::bridge::timeout_user(channel, username, 600, None),
};
const UNMOD: QuickAction = QuickAction {
    label: "Unmod",
    style: ModButtonStyle::Warning,
    run: |channel, username| backend::bridge::unmod_user(channel, username),
};
const MOD: QuickAction = QuickAction {
    label: "Mod",
    style: ModButtonStyle::Success,
    run: |channel, username| backend::bridge::mod_user(channel, username),
};
const UNVIP: QuickAction = QuickAction {
    label: "Unvip",
    style: ModButtonStyle::Warning,
    run: |channel, username| backend::bridge::unvip_user(channel, username),
};
const VIP: QuickAction = QuickAction {
    label: "VIP",
    style: ModButtonStyle::Success,
    run: |channel, username| backend::bridge::vip_user(channel, username),
};

/// Quick actions for a logged action, in display order.
fn quick_actions(action_type: ModActionType) -> &'static [QuickAction] {
    match action_type {
        ModActionType::Ban => &[UNBAN],
        ModActionType::Unban => &[BAN],
        ModActionType::Timeout => &[UNTIMEOUT, BAN],
        ModActionType::Delete => &[BAN, TIMEOUT_10M],
        ModActionType::Mod => &[UNMOD],
        ModActionType::Unmod => &[MOD],
        ModActionType::Vip => &[UNVIP],
        ModActionType::Unvip => &[VIP],
        ModActionType::Clear
        | ModActionType::Warning
        | ModActionType::ShieldEnable
        | ModActionType::ShieldDisable => &[],
    }
}

fn render_mod_action_buttons(index: usize, action: &ModAction) -> impl IntoElement + use<> {
    let action_type = action.action_type;
    let policy = QuickActionPolicy::for_action(action_type);
    let channel = action.channel.clone();
    let username = action.username.clone();

    div()
        .id(SharedString::from(format!(
            "mod-log-actions-{index}-{}-{}-{}-{:?}",
            channel, username, action.timestamp, action_type
        )))
        .flex()
        .flex_wrap()
        .gap_1()
        .children(quick_actions(action_type).iter().map(|quick| {
            let channel = channel.clone();
            let username = username.clone();
            let run = quick.run;
            render_mod_button(quick.label, quick.style, move |_event, _window, _cx| {
                if policy.recheck(&channel, &username) {
                    run(&channel, &username);
                }
            })
        }))
}
