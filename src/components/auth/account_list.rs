//! Account list component for the account switcher dropdown
//!
//! Displays the list of authenticated accounts with options to switch,
//! remove accounts, add new accounts, and toggle anonymous mode.

use crate::backend::{self, Account};
use crate::components::auth::render_profile_avatar;
use crate::components::ui::ButtonExt;
use crate::components::ui::menu::{MenuAction, MenuNav, menu_item};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Render the account switcher's rows. Every row is a menu item of the
/// dropdown: the returned actions follow row order for keyboard navigation.
pub fn render_account_list_inline(
    accounts: &[Account],
    anonymous_mode: bool,
    nav: &MenuNav,
) -> (AnyElement, Vec<MenuAction>) {
    let needs_reauth = backend::bridge::active_account_needs_reauth();
    let mut actions: Vec<MenuAction> = Vec::with_capacity(accounts.len() + 3);
    let mut rows: Vec<AnyElement> = Vec::with_capacity(accounts.len() + 5);

    if needs_reauth {
        rows.push(render_reauth_button_inline(nav, &mut actions).into_any_element());
        rows.push(
            div()
                .w_full()
                .h(px(1.0))
                .bg(colors::border())
                .into_any_element(),
        );
    }
    for (index, account) in accounts.iter().enumerate() {
        rows.push(render_account_item_inline(index, account, nav, &mut actions).into_any_element());
    }
    rows.push(render_add_account_button_inline(nav, &mut actions).into_any_element());
    rows.push(
        div()
            .w_full()
            .h(px(1.0))
            .bg(colors::border())
            .into_any_element(),
    );
    rows.push(render_anonymous_mode_inline(anonymous_mode, nav, &mut actions).into_any_element());

    (
        div()
            .w_full()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element(),
        actions,
    )
}

/// Menu row at the next keyboard position, running `action` on a completed
/// click or Enter.
fn account_menu_row(
    id: impl Into<ElementId>,
    nav: &MenuNav,
    actions: &mut Vec<MenuAction>,
    action: impl Fn() + 'static,
) -> Stateful<Div> {
    let action: MenuAction = std::rc::Rc::new(move |_window: &mut Window, _cx: &mut App| action());
    let index = actions.len();
    actions.push(action.clone());
    menu_item(id, index, nav, action).px_3().py_2().gap_2()
}

fn render_account_item_inline(
    index: usize,
    account: &Account,
    nav: &MenuNav,
    actions: &mut Vec<MenuAction>,
) -> impl IntoElement + use<> {
    let username = account.username.clone();
    let display_name = account
        .display_name
        .clone()
        .unwrap_or_else(|| account.username.clone());
    let profile_url = account.profile_image_url.clone();
    let is_active = account.is_active;
    let missing_runtime_token = backend::bridge::saved_account_missing_runtime_token(&username);
    let needs_reauth =
        missing_runtime_token || (is_active && backend::bridge::active_account_needs_reauth());
    let hide_account_names = backend::streamer_mode::should_hide_account_names();
    let account_label = if hide_account_names {
        if is_active {
            "Active account".to_string()
        } else {
            format!("Account {}", index + 1)
        }
    } else {
        display_name.clone()
    };
    let account_status = if needs_reauth {
        "Re-authentication required".to_string()
    } else if hide_account_names {
        if is_active {
            "Active".to_string()
        } else {
            "Logged in".to_string()
        }
    } else if is_active {
        format!("@{} · Active", username)
    } else {
        format!("@{}", username)
    };
    let avatar_label = if hide_account_names {
        "Account".to_string()
    } else {
        display_name.clone()
    };
    let username_for_switch = username.clone();
    let username_for_remove = username.clone();
    let display_name_for_remove = display_name.clone();
    let needs_reauth_for_action = needs_reauth;

    account_menu_row(
        SharedString::from(format!("account-{}", username)),
        nav,
        actions,
        move || {
            if !is_active {
                if needs_reauth_for_action {
                    backend::auth::trigger_reauth();
                } else {
                    backend::switch_account(&username_for_switch);
                }
            }
        },
    )
    .child(render_profile_avatar(
        profile_url.as_deref(),
        &avatar_label,
        28.0,
    ))
    .child(
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_primary())
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(account_label),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if needs_reauth {
                        colors::error()
                    } else {
                        colors::text_muted()
                    })
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(account_status),
            ),
    )
    .when(is_active && !needs_reauth, |el| {
        el.child(
            div()
                .text_xs()
                .text_color(colors::accent_text())
                .child(SharedString::from("✓")),
        )
    })
    .when(needs_reauth, |el| {
        el.child(
            div()
                .px_2()
                .py_0p5()
                .rounded(Radii::full())
                .bg(colors::error().opacity(0.12))
                .text_xs()
                .text_color(colors::error())
                .child("Re-authenticate"),
        )
    })
    .child(
        div()
            .id(SharedString::from(format!(
                "remove-{}",
                username_for_remove.clone()
            )))
            .role(Role::Button)
            .aria_label("Remove account")
            .on_hover(crate::components::overlays::text_tooltip_on_hover(
                format!("remove-account-{username_for_remove}"),
                "Remove account".to_string(),
            ))
            .size(px(Spacing::XXL))
            .rounded(Radii::sm())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| {
                let mut c = colors::error();
                c.a = 0.2;
                s.bg(c)
            })
            .on_activate(move |_event, _window, _cx| {
                backend::push_ui_action(backend::UiAction::ConfirmRemoveAccount {
                    username: username_for_remove.clone(),
                    display_name: display_name_for_remove.clone(),
                });
            })
            .child(
                div()
                    .text_xs()
                    .text_color(colors::error())
                    .child(crate::icons::CLOSE_GLYPH),
            ),
    )
}

fn render_add_account_button_inline(
    nav: &MenuNav,
    actions: &mut Vec<MenuAction>,
) -> impl IntoElement + use<> {
    let highlighted = nav.is_highlighted(actions.len());
    account_menu_row("add-account-btn", nav, actions, backend::start_login)
        .bg(colors::accent().opacity(if highlighted { 0.2 } else { 0.1 }))
        .hover(|s| s.bg(colors::accent().opacity(0.2)))
        .child(
            div()
                .w(px(28.0))
                .h(px(28.0))
                .rounded(Radii::full())
                .bg({
                    let mut c = colors::accent();
                    c.a = 0.15;
                    c
                })
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .text_color(colors::accent_text())
                        .child("+"),
                ),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::accent_text())
                .child("Add Account"),
        )
}

fn render_reauth_button_inline(
    nav: &MenuNav,
    actions: &mut Vec<MenuAction>,
) -> impl IntoElement + use<> {
    let warning = colors::warning();
    let highlighted = nav.is_highlighted(actions.len());

    account_menu_row(
        "reauth-account-btn",
        nav,
        actions,
        backend::auth::trigger_reauth,
    )
    .bg(warning.opacity(if highlighted { 0.22 } else { 0.12 }))
    .hover(move |s| s.bg(warning.opacity(0.22)))
    .child(
        div()
            .w(px(28.0))
            .h(px(28.0))
            .rounded(Radii::full())
            .bg(warning.opacity(0.18))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(warning)
                    .child("!"),
            ),
    )
    .child(
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child("Re-authenticate"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child("Reconnect the active Twitch account"),
            ),
    )
}

fn render_anonymous_mode_inline(
    anonymous_mode: bool,
    nav: &MenuNav,
    actions: &mut Vec<MenuAction>,
) -> impl IntoElement + use<> {
    let highlighted = nav.is_highlighted(actions.len());
    account_menu_row("anonymous-mode-item", nav, actions, move || {
        backend::set_anonymous_mode(!anonymous_mode);
    })
    .when(anonymous_mode && !highlighted, |s| {
        s.bg(colors::accent().opacity(0.15))
    })
    .child(
        div()
            .w(px(28.0))
            .h(px(28.0))
            .rounded(Radii::full())
            .bg(colors::bg_void())
            .border_1()
            .border_color(colors::border())
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_muted())
                    .child(SharedString::from("👻")),
            ),
    )
    .child(
        div()
            .flex_1()
            .text_sm()
            .text_color(colors::text_primary())
            .child("Anonymous"),
    )
    .when(anonymous_mode, |el| {
        el.child(
            div()
                .text_xs()
                .text_color(colors::accent_text())
                .child(SharedString::from("✓")),
        )
    })
}
