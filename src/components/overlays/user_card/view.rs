//! Per-frame view model and rendering of the card: header, action row,
//! metadata status, and the card/drag-shield pointer handling.

use super::badges::{
    render_7tv_badge, render_badge, render_cosmetic_image,
    subscription_summary_from_live_and_badges,
};
use super::{TwitchBlockAction, UserCard, UserCardState};
use crate::backend;
use crate::backend::cosmetics_core::{ResolvedUserCosmetics, SevenTvBadge};
use crate::backend::twitch_api::UserCardFetchStatus;
use crate::components::overlays::text_tooltip_on_hover;
use crate::components::overlays::traits::OverlayPointerIsolation;
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Sizes};
use crate::utils::{copy_text, parse_hex_color_opt, truncate_text};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// What `UserCard::render` paints this frame, resolved by `build_view`.
struct UserCardView {
    channel: String,
    username: String,
    hide_user_details: bool,
    hide_mod_actions: bool,
    header: HeaderModel,
    info: UserInfoModel,
}

/// Header contents. Streamer mode has already replaced identifying fields
/// with placeholders.
struct HeaderModel {
    display_name: String,
    username: String,
    user_color: Hsla,
    badges: Vec<String>,
    avatar_url: Option<String>,
    avatar_is_animated: bool,
    cosmetic_badge: Option<SevenTvBadge>,
    is_live: bool,
    user_id: Option<String>,
    meta_lines: Vec<String>,
    /// Meta rows reserved on the first frame so late results never shrink the card.
    reserved_meta_lines: usize,
    bio: Option<String>,
    /// `None` hides the meta/bio block; `Some(true)` while the profile is loading.
    profile_pending: Option<bool>,
}

/// Action row and metadata fetch status shown under the header.
struct UserInfoModel {
    seven_tv_profile_url: Option<String>,
    has_profile: bool,
    has_follow: bool,
    has_subscription: bool,
    profile_status: UserCardFetchStatus,
    follow_status: UserCardFetchStatus,
    subscription_status: UserCardFetchStatus,
    twitch_block_action: Option<TwitchBlockAction>,
}

impl Render for UserCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            self.ban_confirmation = None;
            self.metadata_refresh_task = None;
            return div().into_any_element();
        }

        self.start_metadata_refresh_waker(cx);

        let UserCardView {
            channel,
            username,
            hide_user_details,
            hide_mod_actions,
            header,
            info,
        } = self.build_view(window, cx);
        let viewport = window.viewport_size();
        let card_fit =
            crate::utils::fit_overlay_to_viewport(viewport, Sizes::USER_CARD_WIDTH, 450.0, 8.0);
        if !self.hosted_in_window {
            if !self.state.x.is_finite() || !self.state.y.is_finite() {
                self.state.x = (f32::from(viewport.width) - card_fit.width) / 2.0;
                self.state.y = (f32::from(viewport.height) - card_fit.height) / 2.0;
            }
            self.state.x = crate::utils::clamp_overlay_axis(
                self.state.x,
                card_fit.width,
                f32::from(viewport.width),
                card_fit.margin_x,
            );
            self.state.y = crate::utils::clamp_overlay_axis(
                self.state.y,
                card_fit.height,
                f32::from(viewport.height),
                card_fit.margin_y,
            );
        }

        let card = div()
            .id("user-card")
            .bg(colors::bg_elevated())
            .overflow_hidden()
            .flex()
            .flex_col();
        let card = if self.hosted_in_window {
            card.size_full()
                .border_1()
                .border_color(colors::border())
                .on_mouse_move(cx.listener(|this, _event: &MouseMoveEvent, _window, cx| {
                    if crate::utils::should_start_drag() {
                        #[cfg(target_os = "windows")]
                        crate::utils::start_window_drag();
                        #[cfg(not(target_os = "windows"))]
                        _window.start_window_move();
                    }
                    this.is_dragging = false;
                    cx.stop_propagation();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        crate::utils::cancel_pending_drag();
                        this.is_dragging = false;
                        cx.stop_propagation();
                    }),
                )
        } else {
            card.absolute()
                .left(px(self.state.x))
                .top(px(self.state.y))
                .w(px(card_fit.width))
                .max_h(px(card_fit.height))
                .rounded(Radii::lg())
                .border_1()
                .border_color(colors::accent().opacity(0.2))
                .shadow_md()
                // The handlers below are added after the shared isolation
                // chain, so they run first and own the left button and moves.
                .block_overlay_pointer_passthrough()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.is_dragging = false;
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                    cx.stop_propagation();
                    if this.is_dragging {
                        this.apply_drag(event, cx);
                    }
                }))
        };
        let card = card
            .child(self.render_header(header, &channel, cx))
            .child(self.render_user_info(window, &username, info, hide_user_details, cx))
            .child(self.render_recent_messages(hide_user_details, cx))
            .when(
                !hide_mod_actions && self.can_moderate_current_target(),
                |el| el.child(self.render_mod_actions(&channel, &username, cx)),
            );

        let card = if self.is_dragging && !self.hosted_in_window {
            // Full-window drag shield: keeps the drag alive when the pointer
            // outruns the card. Same handler ordering as the card itself.
            div()
                .absolute()
                .inset_0()
                .block_overlay_pointer_passthrough()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                    cx.stop_propagation();
                    this.apply_drag(event, cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.is_dragging = false;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(card)
                .into_any_element()
        } else {
            card.into_any_element()
        };
        if let Some(confirmation) = self.ban_confirmation.as_ref()
            && confirmation.modal.read(cx).state.is_open
        {
            div()
                .size_full()
                .relative()
                .when(!self.hosted_in_window, |el| el.absolute().inset_0())
                .on_key_down(|_, _, cx| cx.stop_propagation())
                .child(card)
                .child(confirmation.modal.clone())
                .into_any_element()
        } else {
            card
        }
    }
}

impl UserCard {
    /// Resolve everything one frame paints. Besides reading caches this
    /// binds the note editor, queues the BTTV Pro lookup, rebuilds the
    /// recent-message rows and retargets the animation waker, in that order.
    fn build_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> UserCardView {
        let username = self.state.username.clone().unwrap_or_default();
        let hide_user_details = backend::streamer_mode::should_hide_user_card_details();
        let hide_mod_actions = backend::streamer_mode::should_hide_mod_actions();
        let twitch_profile = if hide_user_details {
            None
        } else {
            backend::twitch_api::get_cached_user_profile(&username)
        };
        let fallback_display_name = self
            .state
            .display_name
            .clone()
            .or_else(|| self.user_context.latest_display_name.clone())
            .or_else(|| {
                twitch_profile
                    .as_ref()
                    .map(|profile| profile.display_name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_else(|| username.clone());
        let target_user_id = self.resolved_user_id(twitch_profile.as_ref());
        let twitch_block_action = if hide_user_details || hide_mod_actions {
            None
        } else {
            target_user_id.as_deref().and_then(|user_id| {
                backend::blocked_users::block_action_state(user_id, &username).map(|blocked| {
                    TwitchBlockAction {
                        user: backend::blocked_users::BlockedUser {
                            user_id: user_id.to_string(),
                            user_login: username.clone(),
                            display_name: fallback_display_name.clone(),
                        },
                        blocked,
                    }
                })
            })
        };
        let seven_tv_cosmetics = target_user_id
            .as_deref()
            .and_then(backend::cosmetics::get_cached_user_cosmetics);
        let show_7tv_paints = backend::settings::get_show_seventv_name_paints();
        let show_7tv_badges = backend::settings::get_show_seventv_cosmetic_badges();
        let show_7tv_avatars = backend::settings::get_show_seventv_profile_avatars();
        let display_name = if show_7tv_paints {
            cosmetic_display_name(&fallback_display_name, seven_tv_cosmetics.as_ref())
        } else {
            fallback_display_name.clone()
        };
        let user_color = if show_7tv_paints {
            cosmetic_user_color(seven_tv_cosmetics.as_ref(), self.get_user_color())
        } else {
            self.get_user_color()
        };
        let seven_tv_avatar = show_7tv_avatars
            .then(|| {
                seven_tv_cosmetics.as_ref().and_then(|cosmetics| {
                    cosmetics
                        .avatar_url
                        .clone()
                        .map(|url| (url, cosmetics.avatar_is_animated))
                })
            })
            .flatten();
        let (avatar_url, avatar_is_animated) = match seven_tv_avatar {
            Some((url, is_animated)) => (Some(url), is_animated),
            None => (
                twitch_profile
                    .as_ref()
                    .and_then(|profile| profile.profile_image_url.clone()),
                false,
            ),
        };
        let channel = self.state.channel.clone().unwrap_or_default();
        let shows_follow_metadata = Self::shows_follow_metadata(&channel, hide_user_details);
        let shows_subscription_metadata =
            Self::shows_subscription_metadata(&channel, &username, hide_user_details);
        let (follow_info, follow_status) = if shows_follow_metadata {
            (
                backend::twitch_api::get_cached_user_follow(
                    &channel,
                    &username,
                    target_user_id.as_deref(),
                ),
                backend::twitch_api::get_user_follow_status(
                    &channel,
                    &username,
                    target_user_id.as_deref(),
                ),
            )
        } else {
            (None, UserCardFetchStatus::idle())
        };
        let (subscription_info, subscription_status) = if shows_subscription_metadata {
            (
                backend::twitch_api::get_cached_user_subscription(
                    &channel,
                    &username,
                    target_user_id.as_deref(),
                ),
                backend::twitch_api::get_user_subscription_status(
                    &channel,
                    &username,
                    target_user_id.as_deref(),
                ),
            )
        } else {
            (None, UserCardFetchStatus::idle())
        };
        let profile_status = if hide_user_details {
            UserCardFetchStatus::idle()
        } else {
            backend::twitch_api::get_user_profile_status(&username)
        };
        let note_entry = if hide_user_details {
            None
        } else {
            backend::user_notes::get_user_note_entry(&username)
        };
        let current_note = note_entry
            .as_ref()
            .map(|note| note.note.as_str())
            .unwrap_or_default();
        if !hide_user_details {
            self.ensure_note_input(window, cx, &username, current_note);
        }
        backend::queue_bttv_pro_badge_lookup(&channel, &username, target_user_id.as_deref());
        let cosmetic_badge = show_7tv_badges
            .then(|| {
                seven_tv_cosmetics
                    .as_ref()
                    .and_then(|cosmetics| cosmetics.badge.clone())
            })
            .flatten();
        let badges = self.merged_badges(&channel, &username, target_user_id.as_deref());
        let header_display_name = if hide_user_details {
            "User hidden".to_string()
        } else {
            display_name.clone()
        };
        let header_username = if hide_user_details {
            "hidden".to_string()
        } else {
            username.clone()
        };
        let header_user_color = if hide_user_details {
            colors::text_secondary()
        } else {
            user_color
        };
        let header_badges = if hide_user_details {
            Vec::new()
        } else {
            badges.clone()
        };
        let header_avatar_url = if hide_user_details { None } else { avatar_url };
        let header_avatar_is_animated = !hide_user_details && avatar_is_animated;
        let header_cosmetic_badge = if hide_user_details {
            None
        } else {
            cosmetic_badge
        };
        let header_is_live = !hide_user_details
            && backend::twitch_api::get_cached_user_live_status(&username) == Some(true);
        self.ensure_recent_rows(&channel, hide_user_details, window.scale_factor());

        self.retarget_animation_waker(
            header_avatar_is_animated
                .then(|| header_avatar_url.clone())
                .flatten(),
            header_cosmetic_badge
                .as_ref()
                .filter(|badge| badge.is_animated)
                .map(|badge| badge.url.clone()),
            cx,
        );

        let header_user_id = if hide_user_details {
            None
        } else {
            target_user_id.clone()
        };
        let can_view_follow_date = backend::settings::get_show_follow_date()
            && backend::is_authenticated()
            && !backend::irc::is_anonymous_mode()
            && backend::irc::can_moderate_channel(&channel)
            && !(follow_status.auth_generation == backend::auth::credential_generation()
                && matches!(
                    follow_status.state,
                    backend::twitch_api::UserCardFetchState::Forbidden
                        | backend::twitch_api::UserCardFetchState::AuthRequired
                ));
        let mut header_meta_lines: Vec<String> = Vec::new();
        if !hide_user_details {
            let mut first_line: Vec<String> = Vec::new();
            if backend::settings::get_show_follower_count()
                && let Some(count) = follow_info.as_ref().and_then(|info| info.follower_count)
            {
                first_line.push(format!("Followers: {}", format_count_compact(count)));
            }
            if let Some(created) = twitch_profile
                .as_ref()
                .and_then(|profile| profile.created_at.as_deref())
                .and_then(format_twitch_date)
            {
                first_line.push(format!("Created: {}", created));
            }
            header_meta_lines.push(first_line.join("   "));
            if can_view_follow_date {
                let followed = follow_info
                    .as_ref()
                    .and_then(|info| info.followed_at.as_deref())
                    .and_then(format_twitch_date);
                header_meta_lines.push(
                    followed
                        .map(|date| format!("\u{2665} Following since {date}"))
                        .unwrap_or_default(),
                );
            }
            let subscription =
                subscription_summary_from_live_and_badges(subscription_info.as_ref(), &badges);
            if shows_subscription_metadata || subscription.is_some() {
                header_meta_lines.push(
                    subscription
                        .map(|summary| format!("\u{2605} {summary}"))
                        .unwrap_or_default(),
                );
            }
            if backend::settings::get_show_pronouns() {
                header_meta_lines.push(
                    backend::pronouns::get_pronouns(&username)
                        .map(|pronouns| format!("Pronouns: {pronouns}"))
                        .unwrap_or_default(),
                );
            }
        }
        // Late empty/forbidden results must not shrink the card after its first frame.
        let header_meta_reserved_lines = if hide_user_details {
            0
        } else {
            *self
                .header_meta_reserved_lines
                .get_or_insert(header_meta_lines.len())
        };
        let header_bio = if hide_user_details {
            None
        } else {
            twitch_profile
                .as_ref()
                .and_then(|profile| profile.description.as_deref())
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(|description| truncate_text(description, 140))
        };
        UserCardView {
            header: HeaderModel {
                display_name: header_display_name,
                username: header_username,
                user_color: header_user_color,
                badges: header_badges,
                avatar_url: header_avatar_url,
                avatar_is_animated: header_avatar_is_animated,
                cosmetic_badge: header_cosmetic_badge,
                is_live: header_is_live,
                user_id: header_user_id,
                meta_lines: header_meta_lines,
                reserved_meta_lines: header_meta_reserved_lines,
                bio: header_bio,
                profile_pending: (!hide_user_details).then_some(matches!(
                    profile_status.state,
                    backend::twitch_api::UserCardFetchState::Idle
                        | backend::twitch_api::UserCardFetchState::Loading
                )),
            },
            info: UserInfoModel {
                seven_tv_profile_url: seven_tv_profile_url(seven_tv_cosmetics.as_ref()),
                has_profile: twitch_profile.is_some(),
                has_follow: follow_info.is_some(),
                has_subscription: subscription_info.is_some(),
                profile_status,
                follow_status,
                subscription_status,
                twitch_block_action,
            },
            channel,
            username,
            hide_user_details,
            hide_mod_actions,
        }
    }

    /// Twitch badges from the opening message (or the latest buffered one),
    /// followed by any third-party badges not already present.
    fn merged_badges(&self, channel: &str, username: &str, user_id: Option<&str>) -> Vec<String> {
        let mut badges = if self.state.badges.is_empty() {
            self.user_context.latest_badges.clone()
        } else {
            self.state.badges.clone()
        };
        for badge_key in backend::get_external_badge_keys(channel, username, user_id) {
            if badges
                .iter()
                .any(|existing| backend::badges::badge_key_matches(existing, &badge_key))
            {
                continue;
            }
            badges.push(badge_key);
        }
        badges
    }

    fn render_header(
        &self,
        header: HeaderModel,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let HeaderModel {
            display_name,
            username,
            user_color,
            badges,
            avatar_url,
            avatar_is_animated,
            cosmetic_badge,
            is_live,
            user_id,
            meta_lines,
            reserved_meta_lines,
            bio,
            profile_pending,
        } = header;
        let login_for_copy = username.clone();
        let id_for_copy = user_id;
        let bio_loading = bio.is_none() && profile_pending == Some(true);

        let card_x = self.state.x;
        let card_y = self.state.y;

        div()
            .px_3()
            .py_2()
            .flex_none()
            .bg(colors::bg_plate())
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .items_start()
            .gap_2p5()
            .cursor(if self.hosted_in_window {
                CursorStyle::OpenHand
            } else {
                CursorStyle::Arrow
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    if this.hosted_in_window {
                        crate::utils::arm_window_drag();
                    } else {
                        this.is_dragging = true;
                        let mouse_x: f32 = event.position.x.into();
                        let mouse_y: f32 = event.position.y.into();
                        this.drag_offset = point(px(mouse_x - card_x), px(mouse_y - card_y));
                    }
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.hosted_in_window {
                        crate::utils::cancel_pending_drag();
                    }
                    this.is_dragging = false;
                    cx.stop_propagation();
                }),
            )
            .child(render_user_card_avatar(
                avatar_url.as_deref(),
                avatar_is_animated,
                &display_name,
                user_color,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                div()
                                    .id("user-card-copy-name")
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(user_color)
                                    .cursor_pointer()
                                    .hover(|s| s.opacity(0.8))
                                    .on_activate({
                                        let login = login_for_copy.clone();
                                        move |_, _, _| copy_text(&login)
                                    })
                                    .child(display_name.clone()),
                            )
                            .when(is_live, |row| {
                                row.child(
                                    div()
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded(Radii::sm())
                                        .border_1()
                                        .border_color(colors::error().opacity(0.45))
                                        .bg(colors::error().opacity(0.14))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(colors::error())
                                        .child("LIVE"),
                                )
                            })
                            .children(
                                badges
                                    .iter()
                                    .filter(|badge| {
                                        backend::settings::should_render_badge_key(badge)
                                    })
                                    .take(4)
                                    .map(|badge| render_badge(badge, channel)),
                            )
                            .when_some(cosmetic_badge, |el, badge| {
                                el.child(render_7tv_badge(&badge))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("user-card-copy-login")
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(colors::text_secondary()))
                                    .on_activate({
                                        let login = login_for_copy.clone();
                                        move |_, _, _| copy_text(&login)
                                    })
                                    .child(format!("@{}", username)),
                            )
                            .when_some(id_for_copy, |el, id| {
                                el.child(
                                    div()
                                        .id("user-card-copy-id")
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .cursor_pointer()
                                        .hover(|s| s.text_color(colors::text_secondary()))
                                        .on_activate({
                                            let id_text = id.clone();
                                            move |_, _, _| copy_text(&id_text)
                                        })
                                        .child(format!("ID: {} \u{29C9}", id)),
                                )
                            }),
                    )
                    .when(profile_pending.is_some(), |el| {
                        let bio_tooltip = bio.clone();
                        el.child(
                            div()
                                .min_h(px(reserved_meta_lines as f32 * 18.0 + 32.0))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .children(
                                    meta_lines
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, line)| !line.is_empty())
                                        .map(|(index, line)| {
                                            div()
                                                .id(SharedString::from(format!(
                                                    "user-card-meta-{index}"
                                                )))
                                                .h(px(16.0))
                                                .flex_none()
                                                .line_height(px(16.0))
                                                .text_ellipsis()
                                                .text_xs()
                                                .text_color(colors::text_secondary())
                                                .child(line.clone())
                                                .on_hover(text_tooltip_on_hover(
                                                    format!("user-card-meta-{index}"),
                                                    line.clone(),
                                                ))
                                        }),
                                )
                                .child(
                                    div()
                                        .id("user-card-bio")
                                        .h(px(32.0))
                                        .flex_none()
                                        .line_height(px(16.0))
                                        .line_clamp(2)
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .when_some(bio, |el, bio| el.child(bio))
                                        .when_some(bio_tooltip, |el, bio| {
                                            el.on_hover(text_tooltip_on_hover(
                                                "user-card-bio".to_string(),
                                                bio,
                                            ))
                                        })
                                        .when(bio_loading, |el| {
                                            el.child(
                                                div()
                                                    .mt(px(6.0))
                                                    .h(px(4.0))
                                                    .w_full()
                                                    .rounded(px(2.0))
                                                    .bg(colors::bg_hover()),
                                            )
                                        }),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .id("user-card-close")
                    .role(Role::Button)
                    .aria_label("Close user card")
                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                        "user-card-close".to_string(),
                        "Close".to_string(),
                    ))
                    .size(px(Sizes::BUTTON_SM))
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.state.close();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child(crate::icons::CLOSE_GLYPH),
                    ),
            )
    }

    fn render_user_info(
        &self,
        window: &mut Window,
        username: &str,
        info: UserInfoModel,
        hide_user_details: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let username_clone = username.to_string();
        let metadata_status_rows = if hide_user_details {
            Vec::new()
        } else {
            vec![
                build_metadata_status_row("Profile", &info.profile_status, info.has_profile)
                    .filter(|row| matches!(row.tone, InlineStatusTone::Warning)),
                build_metadata_status_row("Follow", &info.follow_status, info.has_follow),
                build_metadata_status_row(
                    "Subscription",
                    &info.subscription_status,
                    info.has_subscription,
                ),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
        };
        div()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .flex_col()
            .gap_1()
            .when(hide_user_details, |el| {
                el.child(render_streamer_mode_placeholder(
                    "User details hidden by streamer mode",
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(self.render_mention_button(&username_clone, cx))
                    .child(self.render_whisper_button(&username_clone, cx))
                    .when_some(info.twitch_block_action, |el, action| {
                        el.child(self.render_twitch_block_button(action, cx))
                    })
                    .when(!hide_user_details, |el| {
                        el.child(self.render_profile_button(&username_clone, cx))
                            .child(self.render_open_chat_button(&username_clone, cx))
                            .when_some(info.seven_tv_profile_url, |el, url| {
                                el.child(self.render_seven_tv_button(url, cx))
                            })
                    }),
            )
            .when(!hide_user_details, |el| {
                el.child(self.render_user_note_section(window, cx))
            })
            .when(!metadata_status_rows.is_empty(), |el| {
                el.child(
                    div().flex().flex_col().gap_1().children(
                        metadata_status_rows
                            .iter()
                            .cloned()
                            .map(render_metadata_status_row),
                    ),
                )
            })
            .when_some(self.note.error.clone(), |el, error| {
                el.child(render_inline_warning(&error))
            })
    }

    fn render_mention_button(&self, username: &str, cx: &mut Context<Self>) -> AnyElement {
        let username = username.to_string();
        let channel = self.state.channel.clone().unwrap_or_default();

        self.render_secondary_action_button(
            "Mention",
            colors::text_primary(),
            cx.listener(move |this, _, _window, cx| {
                backend::push_ui_action(backend::UiAction::InsertMention {
                    username: username.clone(),
                    channel: channel.clone(),
                    source_split_id: None,
                });
                this.update_state(UserCardState::default());
                cx.notify();
            }),
        )
    }

    fn render_whisper_button(
        &self,
        username: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let username = username.to_string();
        let channel = self.state.channel.clone();
        let color = colors::accent();

        div()
            .id("user-card-whisper")
            .px_2()
            .py_1()
            .rounded(Radii::sm())
            .bg(color.opacity(0.1))
            .cursor_pointer()
            .hover(|s| s.bg(color.opacity(0.2)))
            .on_activate(cx.listener({
                let username = username.clone();
                let channel = channel.clone();
                move |this, _, _window, cx| {
                    backend::push_ui_action(backend::UiAction::ComposeWhisper {
                        username: username.clone(),
                        channel: channel.clone(),
                    });
                    this.update_state(UserCardState::default());
                    cx.notify();
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child("Whisper"),
            )
    }

    fn render_twitch_block_button(
        &self,
        action: TwitchBlockAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if action.blocked { "Unblock" } else { "Block" };
        let color = if action.blocked {
            colors::success()
        } else {
            colors::error()
        };

        self.render_secondary_action_button(
            label,
            color,
            cx.listener(move |this, _, window, cx| {
                let session = backend::blocked_users::capture_active_session();
                let hosted_in_window = this.hosted_in_window;
                this.update_state(UserCardState::default());
                if hosted_in_window {
                    window.remove_window();
                }

                let session = match session {
                    Ok(session) if action.user.user_id != session.owner_user_id() => session,
                    Ok(_) => {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "Action unavailable".to_string(),
                            detail: Some(
                                "The active Twitch account cannot target itself.".to_string(),
                            ),
                        });
                        cx.notify();
                        return;
                    }
                    Err(error) => {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "Action unavailable".to_string(),
                            detail: Some(error),
                        });
                        cx.notify();
                        return;
                    }
                };

                if action.blocked {
                    backend::blocked_users::set_block_state_async_for_session(
                        session,
                        action.user.clone(),
                        false,
                    );
                } else {
                    backend::push_ui_action(backend::UiAction::ConfirmBlockUser {
                        user_id: action.user.user_id.clone(),
                        username: action.user.user_login.clone(),
                        display_name: action.user.display_name.clone(),
                        expected_owner_user_id: session.owner_user_id().to_string(),
                    });
                }
                cx.notify();
            }),
        )
    }

    fn render_profile_button(&self, username: &str, cx: &mut Context<Self>) -> AnyElement {
        let url = format!("https://twitch.tv/{}", username);

        self.render_secondary_action_button(
            "Profile",
            colors::text_primary(),
            cx.listener({
                let url = url.clone();
                move |this, _, _window, cx| {
                    queue_confirm_open_link(url.clone());
                    this.update_state(UserCardState::default());
                    cx.notify();
                }
            }),
        )
    }

    fn render_open_chat_button(&self, username: &str, cx: &mut Context<Self>) -> AnyElement {
        let url = format!("https://www.twitch.tv/popout/{username}/chat");

        self.render_secondary_action_button(
            "Chat",
            colors::text_primary(),
            cx.listener({
                let url = url.clone();
                move |this, _, _window, cx| {
                    queue_confirm_open_link(url.clone());
                    this.update_state(UserCardState::default());
                    cx.notify();
                }
            }),
        )
    }

    fn render_seven_tv_button(&self, url: String, cx: &mut Context<Self>) -> AnyElement {
        self.render_secondary_action_button(
            "7TV",
            colors::text_primary(),
            cx.listener(move |this, _, _window, cx| {
                queue_confirm_open_link(url.clone());
                this.update_state(UserCardState::default());
                cx.notify();
            }),
        )
    }

    fn render_secondary_action_button<F>(
        &self,
        label: &'static str,
        text_color: Hsla,
        on_click: F,
    ) -> AnyElement
    where
        F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    {
        div()
            .id(label)
            .px_2()
            .py_0p5()
            .rounded(Radii::sm())
            .border_1()
            .border_color(colors::border())
            .bg(colors::bg_surface())
            .cursor_pointer()
            .hover(|s| s.bg(colors::bg_hover()))
            .on_activate(on_click)
            .flex()
            .items_center()
            .justify_center()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(text_color)
                    .child(label),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
enum InlineStatusTone {
    Neutral,
    Warning,
}

#[derive(Clone)]
struct MetadataStatusRow {
    label: &'static str,
    message: String,
    tone: InlineStatusTone,
}

fn format_twitch_date(timestamp: &str) -> Option<String> {
    let timestamp = timestamp.trim();
    if timestamp.is_empty() {
        return None;
    }

    timestamp
        .split_once('T')
        .map(|(date, _)| date.to_string())
        .or_else(|| Some(timestamp.to_string()))
}

fn format_count_compact(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn build_metadata_status_row(
    label: &'static str,
    status: &backend::twitch_api::UserCardFetchStatus,
    has_cached_value: bool,
) -> Option<MetadataStatusRow> {
    use backend::twitch_api::UserCardFetchState;

    let (message, tone) = match status.state {
        UserCardFetchState::Idle => return None,
        UserCardFetchState::Ready if !status.stale => return None,
        UserCardFetchState::Loading if has_cached_value => (
            "Refreshing cached data…".to_string(),
            InlineStatusTone::Neutral,
        ),
        UserCardFetchState::Loading => ("Loading…".to_string(), InlineStatusTone::Neutral),
        UserCardFetchState::Ready if status.stale => {
            ("Showing cached data".to_string(), InlineStatusTone::Neutral)
        }
        UserCardFetchState::Unavailable => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Unavailable".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Forbidden => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Not permitted".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::AuthRequired => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Authentication required".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::RetryableError if has_cached_value => (
            "Showing cached data while retry is available".to_string(),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::RetryableError => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Retry available".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Error if has_cached_value => (
            "Showing cached data after a refresh failure".to_string(),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Error => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Could not load data".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Ready => return None,
    };

    Some(MetadataStatusRow {
        label,
        message,
        tone,
    })
}

fn render_metadata_status_row(row: MetadataStatusRow) -> impl IntoElement {
    let (bg, border, fg) = match row.tone {
        InlineStatusTone::Neutral => (
            colors::bg_surface(),
            colors::border(),
            colors::text_secondary(),
        ),
        InlineStatusTone::Warning => (
            colors::warning().opacity(0.08),
            colors::warning().opacity(0.18),
            colors::warning(),
        ),
    };

    div()
        .px_2()
        .py_1p5()
        .rounded(Radii::sm())
        .bg(bg)
        .border_1()
        .border_color(border)
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(row.label),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .overflow_hidden()
                .child(row.message),
        )
}

fn render_inline_warning(message: &str) -> impl IntoElement + use<> {
    div()
        .px_2()
        .py_1p5()
        .rounded(Radii::sm())
        .bg(colors::warning().opacity(0.1))
        .text_xs()
        .text_color(colors::warning())
        .child(message.to_string())
}

pub(super) fn render_streamer_mode_placeholder(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .rounded(Radii::sm())
        .bg(colors::bg_surface())
        .text_xs()
        .text_color(colors::text_secondary())
        .text_center()
        .child(message)
}

fn queue_confirm_open_link(url: String) {
    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
        url,
        mode: backend::ui_actions::LinkOpenMode::Configured,
    });
}

fn seven_tv_profile_url(cosmetics: Option<&ResolvedUserCosmetics>) -> Option<String> {
    cosmetics
        .map(|cosmetics| cosmetics.user_id.trim())
        .filter(|user_id| !user_id.is_empty())
        .map(|user_id| format!("https://7tv.app/users/{user_id}"))
}

fn cosmetic_display_name(
    fallback_display_name: &str,
    cosmetics: Option<&ResolvedUserCosmetics>,
) -> String {
    cosmetics
        .and_then(|cosmetics| cosmetics.paint.as_ref())
        .map(|paint| paint.transformed_display_name(fallback_display_name))
        .unwrap_or_else(|| fallback_display_name.to_string())
}

fn cosmetic_user_color(cosmetics: Option<&ResolvedUserCosmetics>, fallback_color: Hsla) -> Hsla {
    cosmetics
        .and_then(|cosmetics| cosmetics.paint.as_ref())
        .and_then(|paint| paint.representative_color())
        .map(rgba_to_hsla)
        .unwrap_or(fallback_color)
}

fn rgba_to_hsla(color: crate::backend::cosmetics_core::RgbaColor) -> Hsla {
    let mut parsed = parse_hex_color_opt(&color.to_hex_rgb()).unwrap_or(colors::accent());
    parsed.a = color.a as f32 / 255.0;
    parsed
}

fn render_user_card_avatar(
    avatar_url: Option<&str>,
    avatar_is_animated: bool,
    display_name: &str,
    user_color: Hsla,
) -> AnyElement {
    let avatar_url = avatar_url.filter(|_| !backend::streamer_mode::should_hide_avatars());
    let base = div()
        .size(px(56.0))
        .rounded(Radii::lg())
        .overflow_hidden()
        .bg(user_color.opacity(0.2))
        .flex()
        .items_center()
        .justify_center();

    if let Some(url) = avatar_url {
        base.child(render_cosmetic_image(
            url,
            56.0,
            ObjectFit::Cover,
            avatar_is_animated,
        ))
        .into_any_element()
    } else {
        base.child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(user_color)
                .child(
                    display_name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string(),
                ),
        )
        .into_any_element()
    }
}
