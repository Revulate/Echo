//! Moderation row: ban (behind a confirmation), unban, timeouts and the
//! inline warn prompt. Every effect rechecks moderation authority first.

use super::{PendingBanConfirmation, UserCard};
use crate::backend;
use crate::components::overlays::{ConfirmationModal, ConfirmationState};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{
    Escape, Input, InputEvent, InputState, apply_standard_input_chrome,
};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;

impl UserCard {
    fn open_ban_confirmation(
        &mut self,
        channel: &str,
        username: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.is_open
            || !self.can_moderate_current_target()
            || !self
                .state
                .channel
                .as_deref()
                .is_some_and(|current| current.eq_ignore_ascii_case(channel))
            || !self
                .state
                .username
                .as_deref()
                .is_some_and(|current| current.eq_ignore_ascii_case(username))
            || !crate::components::common::moderation_policy::recheck_moderation_target(
                channel, username,
            )
        {
            return;
        }
        let channel = channel.to_string();
        let username = username.to_string();
        let auth_generation = backend::auth::credential_generation();
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let confirm_active = Arc::clone(&active);
        let cancel_active = Arc::clone(&active);
        let modal = cx.new(ConfirmationModal::new);
        let focus = modal.read(cx).focus_handle.clone();
        modal.update(cx, |modal, cx| {
            modal.state = ConfirmationState {
                is_open: true,
                title: "Ban user?".to_string(),
                message: format!("Ban @{username} in #{}?", channel.trim_start_matches('#')),
                confirm_text: "Ban".to_string(),
                cancel_text: "Cancel".to_string(),
                is_destructive: true,
                confirm_focused: false,
                ..Default::default()
            };
            modal.on_confirm = Some(Arc::new(move || {
                if confirm_active.swap(false, std::sync::atomic::Ordering::AcqRel)
                    && backend::auth::credential_generation() == auth_generation
                    && crate::components::common::moderation_policy::recheck_moderation_target(
                        &channel, &username,
                    )
                {
                    backend::ban_user(&channel, &username, None);
                }
            }));
            modal.on_cancel = Some(Arc::new(move || {
                cancel_active.store(false, std::sync::atomic::Ordering::Release);
            }));
            cx.notify();
        });
        let previous_focus = window.focused(cx);
        let host = window.window_handle();
        let subscription = cx.observe(&modal, move |_, modal, cx| {
            if !modal.read(cx).state.is_open {
                let previous_focus = previous_focus.clone();
                cx.defer(move |cx| {
                    let _ = host.update(cx, |_, window, cx| {
                        if let Some(focus) = previous_focus {
                            window.focus(&focus, cx);
                        }
                    });
                });
            }
            cx.notify();
        });
        self.ban_confirmation = Some(PendingBanConfirmation {
            modal,
            active,
            _subscription: subscription,
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn open_warn_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.warn_input.clone() {
            Some(input) => input,
            None => {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).with_placeholder("Reason shown to the user…")
                });
                let subscription = cx.subscribe(&input, |this, _input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter) {
                        this.submit_warn(cx);
                    }
                });
                self.subscriptions.push(subscription);
                self.warn_input = Some(input.clone());
                input
            }
        };
        input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        let focus_handle = input.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        self.warn_prompt_open = true;
        cx.notify();
    }

    fn submit_warn(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.warn_input.clone() else {
            return;
        };
        let reason = input.read(cx).value().trim().to_string();
        if reason.is_empty() {
            return;
        }
        if let (Some(channel), Some(username)) =
            (self.state.channel.clone(), self.state.username.clone())
            && crate::components::common::moderation_policy::recheck_moderation_target(
                &channel, &username,
            )
        {
            backend::send_message_or_command_async(channel, format!("/warn {username} {reason}"));
        }
        self.warn_prompt_open = false;
        cx.notify();
    }

    pub(super) fn render_mod_actions(
        &self,
        channel: &str,
        username: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let channel = channel.to_string();
        let username = username.to_string();
        let is_banned =
            crate::backend::components::mod_log::is_user_likely_banned(&channel, &username);

        const TIMEOUTS: [(&str, u64); 4] =
            [("Purge", 1), ("10m", 600), ("1h", 3600), ("1d", 86400)];

        let buttons = div()
            .px_2()
            .py_1p5()
            .border_t_1()
            .border_color(colors::border())
            .flex()
            .items_center()
            .gap_1()
            .child(render_compact_mod_button(
                if is_banned {
                    CardModAction::Unban
                } else {
                    CardModAction::Ban
                },
                channel.clone(),
                username.clone(),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .children(TIMEOUTS.into_iter().map(|(label, seconds)| {
                        render_compact_mod_button(
                            CardModAction::Timeout { label, seconds },
                            channel.clone(),
                            username.clone(),
                            cx,
                        )
                    })),
            )
            .child(render_warn_button(&username, self.warn_prompt_open, cx));

        div().flex().flex_col().child(buttons).when_some(
            self.warn_input.clone().filter(|_| self.warn_prompt_open),
            |el, input| el.child(render_warn_prompt(input, cx)),
        )
    }
}

fn render_warn_button(target: &str, prompt_open: bool, cx: &mut Context<UserCard>) -> AnyElement {
    let color = colors::warning();
    div()
        .id(SharedString::from(format!("user-card-mod-Warn-{target}")))
        .px_1p5()
        .py_0p5()
        .min_w(px(26.0))
        .rounded(Radii::sm())
        .border_1()
        .border_color(color.opacity(0.4))
        .when(prompt_open, |el| el.bg(color.opacity(0.12)))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |s| s.bg(color.opacity(0.12)))
        .on_activate(cx.listener(move |this, _, window, cx| {
            if this.warn_prompt_open {
                this.warn_prompt_open = false;
                cx.notify();
            } else {
                this.open_warn_prompt(window, cx);
            }
        }))
        .child(div().text_xs().text_color(color).child("Warn"))
        .into_any_element()
}

fn render_warn_prompt(input: Entity<InputState>, cx: &mut Context<UserCard>) -> AnyElement {
    let has_reason = !input.read(cx).value().trim().is_empty();
    let focus_handle = input.read(cx).focus_handle().clone();
    let color = colors::warning();
    div()
        .px_2()
        .pb_1p5()
        .flex()
        .items_center()
        .gap_1()
        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
            this.warn_prompt_open = false;
            if let Some(focus_handle) = this.host_focus.clone() {
                window.focus(&focus_handle, cx);
            }
            cx.notify();
            cx.stop_propagation();
        }))
        .child(
            apply_standard_input_chrome(
                div()
                    .id("user-card-warn-shell")
                    .flex_1()
                    .min_w_0()
                    .min_h(px(24.0))
                    .px_2()
                    .cursor_text()
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        window.focus(&focus_handle, cx);
                        window.prevent_default();
                    }),
                true,
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(colors::text_primary())
                    .child(
                        Input::new(&input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            ),
        )
        .child(
            div()
                .id("user-card-warn-send")
                .px_2()
                .py_0p5()
                .rounded(Radii::sm())
                .border_1()
                .border_color(color.opacity(if has_reason { 0.6 } else { 0.2 }))
                .text_xs()
                .text_color(if has_reason {
                    color
                } else {
                    colors::text_muted()
                })
                .when(has_reason, |el| {
                    el.cursor_pointer()
                        .hover(move |s| s.bg(color.opacity(0.12)))
                })
                .on_activate(cx.listener(|this, _, _window, cx| this.submit_warn(cx)))
                .child("Send warning"),
        )
        .into_any_element()
}

/// One of the card's compact moderation buttons.
#[derive(Clone, Copy)]
enum CardModAction {
    /// Asks for confirmation before banning.
    Ban,
    Unban,
    Timeout {
        label: &'static str,
        seconds: u64,
    },
}

impl CardModAction {
    fn label(self) -> &'static str {
        match self {
            Self::Ban => "Ban",
            Self::Unban => "Unban",
            Self::Timeout { label, .. } => label,
        }
    }

    fn color(self) -> Hsla {
        match self {
            Self::Ban => colors::error(),
            Self::Unban => colors::success(),
            Self::Timeout { .. } => colors::text_secondary(),
        }
    }
}

/// Compact bordered mod button. Sends the command and keeps
/// the card open so follow-up actions (escalating timeouts etc.) are easy.
fn render_compact_mod_button(
    action: CardModAction,
    channel: String,
    target: String,
    cx: &mut Context<UserCard>,
) -> AnyElement {
    let label = action.label();
    let color = action.color();
    div()
        .id(SharedString::from(format!(
            "user-card-mod-{label}-{target}"
        )))
        .px_1p5()
        .py_0p5()
        .min_w(px(26.0))
        .rounded(Radii::sm())
        .border_1()
        .border_color(color.opacity(0.4))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |s| s.bg(color.opacity(0.12)))
        .on_activate(cx.listener(move |this, _, window, cx| {
            let command = match action {
                CardModAction::Ban => {
                    this.open_ban_confirmation(&channel, &target, window, cx);
                    return;
                }
                CardModAction::Unban => format!("/unban {target}"),
                CardModAction::Timeout { seconds, .. } => format!("/timeout {target} {seconds}"),
            };
            if crate::components::common::moderation_policy::recheck_moderation_target(
                &channel, &target,
            ) {
                backend::send_message_or_command_async(channel.clone(), command);
            }
            cx.notify();
        }))
        .child(div().text_xs().text_color(color).child(label))
        .into_any_element()
}
