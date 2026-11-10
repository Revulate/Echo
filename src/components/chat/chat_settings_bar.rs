//! Chat settings control bar - displays current ROOMSTATE for a channel
//!
//! Two surfaces live here:
//!
//! * The header pills used by the adaptive native channel popup.
//! * [`RoomModeDescriptor`] plus the `RoomStateIndicator` rail that the composer
//!   renders at its right edge. Main split headers don't carry ROOMSTATE: the
//!   rail is the single persistent location, so several simultaneously active
//!   modes can never crowd out channel/title/metadata.
//!
//! Pure rendering functions matching the pattern in chat_ui.rs.

use crate::backend::irc::room_state::{FollowersOnly, RoomState};
use crate::components::overlays::{
    clear_global_tooltip_if_source, clear_global_tooltip_if_source_in_window, show_text_tooltip,
};
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::StatefulInteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::time::Duration;

pub fn active_room_state_label_count(room_state: Option<&RoomState>) -> usize {
    let Some(state) = room_state else {
        return 0;
    };
    usize::from(state.emote_only)
        + usize::from(state.subscribers_only)
        + usize::from(state.r9k)
        + usize::from(state.slow_mode.is_some())
        + usize::from(!matches!(state.followers_only, FollowersOnly::Disabled))
}

/// Clear any room-state tooltip owned by one header instance. This is needed
/// when a native popup closes while the pointer is still over one of its
/// pills, because the element will disappear before it can receive unhover.
pub fn clear_chat_settings_tooltips(tooltip_scope: u64) {
    for key in ["emote", "sub", "r9k", "slow", "follow"] {
        clear_global_tooltip_if_source(&format!("roomstate-{tooltip_scope}-{key}"));
    }
}

/// Render compact room-state tags for the context row in the unified split header.
///
/// Only active modes are shown. This keeps the header informative without
/// spending narrow-split space on five permanently inactive badges.
pub fn render_chat_settings_tags_inline(
    room_state: Option<&RoomState>,
    tooltip_scope: u64,
) -> impl IntoElement + use<> {
    let state_loaded = room_state.is_some();
    let state = room_state.cloned().unwrap_or_default();

    let slow_mode = state.slow_mode.map(|duration| duration.as_secs());
    let followers_only = match &state.followers_only {
        FollowersOnly::Disabled => None,
        FollowersOnly::All => Some(("Followers".to_string(), "Followers-only: On".to_string())),
        FollowersOnly::Minimum(duration) => {
            let minutes = duration.as_secs() / 60;
            Some((
                format!("Followers {minutes}m"),
                format!("Followers-only: {minutes}m"),
            ))
        }
    };

    div()
        .id("chat-settings-inline-tags")
        .flex()
        .items_center()
        .gap(px(Spacing::XS))
        .min_w_0()
        .overflow_hidden()
        .when(state_loaded && state.emote_only, |el| {
            el.child(mode_pill_compact_with_tooltip(
                tooltip_scope,
                "emote",
                "Emote",
                true,
                "Emote-only: On",
            ))
        })
        .when(state_loaded && state.subscribers_only, |el| {
            el.child(mode_pill_compact_with_tooltip(
                tooltip_scope,
                "sub",
                "Subs",
                true,
                "Subscribers-only: On",
            ))
        })
        .when(state_loaded && state.r9k, |el| {
            el.child(mode_pill_compact_with_tooltip(
                tooltip_scope,
                "r9k",
                "Unique",
                true,
                "Unique chat (R9K): On",
            ))
        })
        .when_some(slow_mode.filter(|_| state_loaded), |el, seconds| {
            el.child(mode_pill_compact_with_tooltip(
                tooltip_scope,
                "slow",
                &format!("Slow {seconds}s"),
                true,
                &format!(
                    "Configured slow mode: {seconds}s. Your remaining wait appears in the composer; moderators, broadcasters, and VIPs are exempt."
                ),
            ))
        })
        .when_some(
            followers_only.filter(|_| state_loaded),
            |el, (label, tooltip)| {
                el.child(mode_pill_compact_with_tooltip(
                    tooltip_scope,
                    "follow",
                    &label,
                    true,
                    &tooltip,
                ))
            },
        )
}

fn mode_pill_compact(label: &str, active: bool) -> impl IntoElement + use<> {
    div()
        .px(px(Spacing::XS_PLUS))
        .h(px(18.0))
        .rounded(Radii::sm())
        .border_1()
        .border_color(if active {
            colors::accent_transparent(0.20)
        } else {
            colors::border_subtle().opacity(0.6)
        })
        .bg(if active {
            colors::accent_transparent(0.08)
        } else {
            colors::bg_elevated().opacity(0.35)
        })
        .flex()
        .items_center()
        .child(
            div()
                .text_size(px(FontSize::XS))
                .whitespace_nowrap()
                .text_color(if active {
                    colors::accent_hover()
                } else {
                    colors::text_muted()
                })
                .child(label.to_string()),
        )
}

/// One Twitch chat mode, in the order the composer rail sheds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomModeKind {
    Followers,
    Slow,
    Subscribers,
    EmoteOnly,
    Unique,
}

impl RoomModeKind {
    /// Stable tooltip/element suffix. Kept distinct from the popup header pill
    /// keys so a rail tooltip can never clear a popup header tooltip.
    pub fn key(self) -> &'static str {
        match self {
            Self::Followers => "follow",
            Self::Slow => "slow",
            Self::Subscribers => "sub",
            Self::EmoteOnly => "emote",
            Self::Unique => "r9k",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Followers => "Followers-only",
            Self::Slow => "Slow mode",
            Self::Subscribers => "Subscribers-only",
            Self::EmoteOnly => "Emote-only",
            Self::Unique => "Unique chat (R9K)",
        }
    }
}

/// A single active mode, resolved once per render from [`RoomState`].
#[derive(Clone, Debug)]
pub struct RoomModeDescriptor {
    pub kind: RoomModeKind,
    /// Compact duration painted next to the icon (`10m`, `10s`). Modes without
    /// a duration are icon-only.
    pub duration_label: Option<String>,
    /// Full sentence that explains the mode rather than a bare one-letter
    /// label.
    pub tooltip: String,
}

fn plural(value: u64, noun: &str) -> String {
    if value == 1 {
        format!("{value} {noun}")
    } else {
        format!("{value} {noun}s")
    }
}

/// Compact follower requirement (`30m`, `2h`, `7d`) for the rail chip.
fn compact_follow_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    if minutes >= 1440 && minutes.is_multiple_of(1440) {
        format!("{}d", minutes / 1440)
    } else if minutes >= 60 && minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// Prose follower requirement for the tooltip sentence.
fn prose_follow_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    if minutes >= 1440 && minutes.is_multiple_of(1440) {
        plural(minutes / 1440, "day")
    } else if minutes >= 60 && minutes.is_multiple_of(60) {
        plural(minutes / 60, "hour")
    } else {
        plural(minutes, "minute")
    }
}

/// Every active chat mode for one channel, highest-priority first.
///
/// The order is the shed order used by the rail's condensed tier: followers and
/// slow mode change what a viewer can do right now, so they survive longest.
pub fn active_room_modes(room_state: Option<&RoomState>) -> Vec<RoomModeDescriptor> {
    let Some(state) = room_state else {
        return Vec::new();
    };
    let mut modes = Vec::with_capacity(5);

    match state.followers_only {
        FollowersOnly::Disabled => {}
        FollowersOnly::All => modes.push(RoomModeDescriptor {
            kind: RoomModeKind::Followers,
            duration_label: None,
            tooltip: "Followers-only — you must follow this channel to chat.".to_string(),
        }),
        FollowersOnly::Minimum(duration) => modes.push(RoomModeDescriptor {
            kind: RoomModeKind::Followers,
            duration_label: Some(compact_follow_duration(duration)),
            tooltip: format!(
                "Followers-only — account must have followed for {}.",
                prose_follow_duration(duration)
            ),
        }),
    }

    if let Some(duration) = state.slow_mode {
        let seconds = duration.as_secs();
        modes.push(RoomModeDescriptor {
            kind: RoomModeKind::Slow,
            duration_label: Some(format!("{seconds}s")),
            tooltip: format!(
                "Configured slow mode — one message every {}. Your remaining wait appears in the composer; moderators, broadcasters, and VIPs are exempt.",
                plural(seconds, "second")
            ),
        });
    }

    if state.subscribers_only {
        modes.push(RoomModeDescriptor {
            kind: RoomModeKind::Subscribers,
            duration_label: None,
            tooltip: "Subscribers-only — only subscribers and moderators can chat.".to_string(),
        });
    }

    if state.emote_only {
        modes.push(RoomModeDescriptor {
            kind: RoomModeKind::EmoteOnly,
            duration_label: None,
            tooltip: "Emote-only — messages must contain nothing but emotes.".to_string(),
        });
    }

    if state.r9k {
        modes.push(RoomModeDescriptor {
            kind: RoomModeKind::Unique,
            duration_label: None,
            tooltip: "Unique chat (R9K) — messages repeating recent chat are rejected.".to_string(),
        });
    }

    modes
}

/// One-line summary of every active mode, for the rich header tooltip and the
/// collapsed rail button.
pub fn room_modes_summary(modes: &[RoomModeDescriptor]) -> Option<String> {
    if modes.is_empty() {
        return None;
    }
    Some(
        modes
            .iter()
            .map(|mode| match &mode.duration_label {
                Some(label) => format!("{} {label}", mode.kind.title()),
                None => mode.kind.title().to_string(),
            })
            .collect::<Vec<_>>()
            .join(" • "),
    )
}

const RAIL_CHIP_HEIGHT_PX: f32 = 22.0;
const RAIL_CHIP_ICON_PX: f32 = 16.0;
const RAIL_CHIP_PADDING_X_PX: f32 = Spacing::XS;
const RAIL_CHIP_GAP_PX: f32 = Spacing::XS;
const RAIL_PADDING_X_PX: f32 = Spacing::XS_PLUS;
/// Average glyph advance of a rail label, 0.6em of its text size.
const RAIL_LABEL_CHAR_PX: f32 = RAIL_LABEL_TEXT_PX * 0.6;
const RAIL_LABEL_TEXT_PX: f32 = FontSize::XS;
const RAIL_COLLAPSED_BUTTON_PX: f32 = 28.0;
const RAIL_TARGET_LABEL_MAX_PX: f32 = 92.0;
const RAIL_POPOVER_WIDTH_PX: f32 = 248.0;

/// Surplus width required before the rail may use each tier. These are the
/// spec's bands: the composer's own text area is reserved before any of it.
const RAIL_FULL_SURPLUS_PX: f32 = 160.0;
const RAIL_CONDENSED_SURPLUS_PX: f32 = 72.0;
const RAIL_COLLAPSED_SURPLUS_PX: f32 = 32.0;
/// The condensed tier shows at most this many indicators before `+N`.
const RAIL_CONDENSED_VISIBLE: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomStateRailTier {
    /// Every mode with its duration.
    Full,
    /// Two modes plus a `+N` overflow chip.
    Condensed,
    /// One 28x28 chat-modes button whose popover lists everything.
    Collapsed,
}

/// Resolved rail geometry. `width` is authoritative: the composer subtracts
/// exactly this from its editable text width, so the measurement cache stays
/// keyed on the width text is actually shaped at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomStateRailPlan {
    pub tier: RoomStateRailTier,
    pub width: f32,
    pub visible: usize,
    pub overflow: usize,
    pub show_target_label: bool,
}

impl RoomStateRailPlan {
    pub fn is_collapsed(self) -> bool {
        self.tier == RoomStateRailTier::Collapsed
    }
}

fn estimate_label_width(label: &str) -> f32 {
    label.chars().count() as f32 * RAIL_LABEL_CHAR_PX
}

fn chip_width(mode: &RoomModeDescriptor) -> f32 {
    let label_width = mode
        .duration_label
        .as_deref()
        .map(|label| 2.0 + estimate_label_width(label))
        .unwrap_or(0.0);
    RAIL_CHIP_PADDING_X_PX * 2.0 + RAIL_CHIP_ICON_PX + label_width
}

fn overflow_chip_width(overflow: usize) -> f32 {
    RAIL_CHIP_PADDING_X_PX * 2.0 + estimate_label_width(&format!("+{overflow}"))
}

fn target_label_width(target_label: &str) -> f32 {
    (RAIL_CHIP_PADDING_X_PX * 2.0 + estimate_label_width(target_label))
        .min(RAIL_TARGET_LABEL_MAX_PX)
}

fn chip_row_width(widths: impl IntoIterator<Item = f32>) -> f32 {
    let mut total = 0.0;
    let mut count = 0usize;
    for width in widths {
        total += width;
        count += 1;
    }
    total + count.saturating_sub(1) as f32 * RAIL_CHIP_GAP_PX
}

/// Choose the richest rail tier that both clears its surplus threshold and
/// physically fits inside `surplus`.
///
/// `surplus` is the input width left over after the composer's guaranteed text
/// area, the action rail, and the draft-preview rail. The rail is width-only:
/// nothing here can change composer height.
pub fn plan_room_state_rail(
    modes: &[RoomModeDescriptor],
    target_label: Option<&str>,
    surplus: f32,
) -> Option<RoomStateRailPlan> {
    if modes.is_empty() || surplus < RAIL_COLLAPSED_SURPLUS_PX {
        return None;
    }
    let padding = RAIL_PADDING_X_PX * 2.0;
    let label_width = target_label.map(target_label_width);

    if modes.len() <= RAIL_CONDENSED_VISIBLE && surplus >= RAIL_FULL_SURPLUS_PX {
        let chips = chip_row_width(modes.iter().map(chip_width));
        for show_label in [true, false] {
            let extra = match (show_label, label_width) {
                (true, Some(width)) => width + RAIL_CHIP_GAP_PX,
                (true, None) => continue,
                _ => 0.0,
            };
            let width = padding + chips + extra;
            if width <= surplus {
                return Some(RoomStateRailPlan {
                    tier: RoomStateRailTier::Full,
                    width,
                    visible: modes.len(),
                    overflow: 0,
                    show_target_label: show_label,
                });
            }
        }
    }

    if surplus >= RAIL_CONDENSED_SURPLUS_PX {
        let visible = modes.len().min(RAIL_CONDENSED_VISIBLE);
        let overflow = modes.len() - visible;
        let mut widths: Vec<f32> = modes.iter().take(visible).map(chip_width).collect();
        if overflow > 0 {
            widths.push(overflow_chip_width(overflow));
        }
        let chips = chip_row_width(widths);
        for show_label in [true, false] {
            let extra = match (show_label, label_width) {
                (true, Some(width)) => width + RAIL_CHIP_GAP_PX,
                (true, None) => continue,
                _ => 0.0,
            };
            let width = padding + chips + extra;
            if width <= surplus {
                return Some(RoomStateRailPlan {
                    tier: RoomStateRailTier::Condensed,
                    width,
                    visible,
                    overflow,
                    show_target_label: show_label,
                });
            }
        }
    }

    let collapsed_width = padding + RAIL_COLLAPSED_BUTTON_PX;
    (collapsed_width <= surplus).then_some(RoomStateRailPlan {
        tier: RoomStateRailTier::Collapsed,
        width: collapsed_width,
        visible: 0,
        overflow: modes.len(),
        show_target_label: false,
    })
}

/// Icon for one mode. Only `user-plus` and `smile` exist as bundled SVGs, so
/// the remaining three use the same typographic affordances the rest of the
/// composer/header chrome already relies on. Every one carries a full tooltip.
fn render_mode_icon(kind: RoomModeKind, color: Hsla) -> AnyElement {
    let glyph = match kind {
        RoomModeKind::Followers => {
            return svg()
                .path("icons/user-plus.svg")
                .size(px(13.0))
                .text_color(color)
                .into_any_element();
        }
        RoomModeKind::EmoteOnly => {
            return svg()
                .path("icons/smile.svg")
                .size(px(13.0))
                .text_color(color)
                .into_any_element();
        }
        RoomModeKind::Slow => "⏳",
        RoomModeKind::Subscribers => "★",
        RoomModeKind::Unique => "≠",
    };
    div()
        .text_size(px(FontSize::XS))
        .text_color(color)
        .child(glyph)
        .into_any_element()
}

fn rail_chip_shell() -> Div {
    div()
        .h(px(RAIL_CHIP_HEIGHT_PX))
        .px(px(RAIL_CHIP_PADDING_X_PX))
        .rounded(Radii::sm())
        .border_1()
        .border_color(colors::accent_transparent(0.20))
        .bg(colors::accent_transparent(0.08))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(Spacing::XXS))
        .flex_shrink_0()
}

fn rail_tooltip_source(tooltip_scope: u64, key: &str) -> String {
    format!("roomrail-{tooltip_scope}-{key}")
}

fn with_rail_tooltip(element: Stateful<Div>, source: String, tooltip: String) -> Stateful<Div> {
    element.on_hover(move |hovered, window, _app| {
        if *hovered {
            show_text_tooltip(
                window.window_handle(),
                source.clone(),
                tooltip.clone(),
                window.mouse_position(),
            );
        } else {
            clear_global_tooltip_if_source_in_window(window.window_handle(), &source);
        }
    })
}

fn render_rail_chip(tooltip_scope: u64, mode: &RoomModeDescriptor) -> AnyElement {
    let source = rail_tooltip_source(tooltip_scope, mode.kind.key());
    let chip = rail_chip_shell()
        .id(SharedString::from(source.clone()))
        .child(render_mode_icon(mode.kind, colors::accent_hover()))
        .when_some(mode.duration_label.clone(), |chip, label| {
            chip.child(
                div()
                    .text_size(px(RAIL_LABEL_TEXT_PX))
                    .whitespace_nowrap()
                    .text_color(colors::accent_hover())
                    .child(label),
            )
        });
    with_rail_tooltip(chip, source, mode.tooltip.clone()).into_any_element()
}

fn render_rail_overflow_chip(
    tooltip_scope: u64,
    hidden: &[RoomModeDescriptor],
) -> Option<AnyElement> {
    let summary = room_modes_summary(hidden)?;
    let source = rail_tooltip_source(tooltip_scope, "overflow");
    let chip = rail_chip_shell()
        .id(SharedString::from(source.clone()))
        .child(
            div()
                .text_size(px(RAIL_LABEL_TEXT_PX))
                .whitespace_nowrap()
                .text_color(colors::accent_hover())
                .child(format!("+{}", hidden.len())),
        );
    Some(with_rail_tooltip(chip, source, format!("Also active: {summary}")).into_any_element())
}

fn render_rail_target_label(tooltip_scope: u64, target_label: &str) -> AnyElement {
    let source = rail_tooltip_source(tooltip_scope, "target");
    let chip = div()
        .id(SharedString::from(source.clone()))
        .h(px(RAIL_CHIP_HEIGHT_PX))
        .px(px(RAIL_CHIP_PADDING_X_PX))
        .max_w(px(RAIL_TARGET_LABEL_MAX_PX))
        .flex()
        .items_center()
        .flex_shrink_0()
        .overflow_hidden()
        .text_size(px(RAIL_LABEL_TEXT_PX))
        .text_color(colors::text_muted())
        .text_ellipsis()
        .whitespace_nowrap()
        .child(target_label.to_string());
    with_rail_tooltip(
        chip,
        source,
        format!("Chat modes for {target_label} — the active split"),
    )
    .into_any_element()
}

fn render_rail_collapsed_button(
    tooltip_scope: u64,
    modes: &[RoomModeDescriptor],
    popover_open: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let source = rail_tooltip_source(tooltip_scope, "collapsed");
    let summary = room_modes_summary(modes).unwrap_or_default();
    let tooltip = format!(
        "{} active — {summary}",
        plural(modes.len() as u64, "chat mode")
    );
    let button = div()
        .id(SharedString::from(source.clone()))
        .w(px(RAIL_COLLAPSED_BUTTON_PX))
        .h(px(RAIL_COLLAPSED_BUTTON_PX))
        .flex_shrink_0()
        .rounded(Radii::md())
        .border_1()
        .border_color(if popover_open {
            colors::accent_transparent(0.42)
        } else {
            colors::accent_transparent(0.20)
        })
        .bg(colors::accent_transparent(if popover_open {
            0.16
        } else {
            0.08
        }))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(colors::accent_transparent(0.16)))
        .role(Role::Button)
        .aria_label(SharedString::from(tooltip.clone()))
        .aria_expanded(popover_open)
        .on_activate(on_toggle)
        .child(
            div()
                .text_size(px(FontSize::XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::accent_hover())
                .child(modes.len().to_string()),
        );
    with_rail_tooltip(button, source, tooltip).into_any_element()
}

/// The `RoomStateIndicator` rail itself: width-only, vertically centred inside
/// whatever height the composer already resolved.
pub fn render_room_state_rail(
    tooltip_scope: u64,
    plan: RoomStateRailPlan,
    modes: &[RoomModeDescriptor],
    target_label: Option<&str>,
    popover_open: bool,
    on_toggle_popover: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let mut rail = div()
        .id("roomstate-rail")
        .h_full()
        .w(px(plan.width))
        .flex_shrink_0()
        .px(px(RAIL_PADDING_X_PX))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(RAIL_CHIP_GAP_PX))
        .overflow_hidden();

    if plan.show_target_label
        && let Some(label) = target_label
    {
        rail = rail.child(render_rail_target_label(tooltip_scope, label));
    }

    match plan.tier {
        RoomStateRailTier::Full | RoomStateRailTier::Condensed => {
            for mode in modes.iter().take(plan.visible) {
                rail = rail.child(render_rail_chip(tooltip_scope, mode));
            }
            if plan.overflow > 0
                && let Some(chip) = render_rail_overflow_chip(tooltip_scope, &modes[plan.visible..])
            {
                rail = rail.child(chip);
            }
        }
        RoomStateRailTier::Collapsed => {
            rail = rail.child(render_rail_collapsed_button(
                tooltip_scope,
                modes,
                popover_open,
                on_toggle_popover,
            ));
        }
    }

    rail.into_any_element()
}

/// Popover for the collapsed rail button. Lists every active mode with its full
/// explanation, so the narrowest composer still exposes the same information.
pub fn render_room_state_popover(
    modes: &[RoomModeDescriptor],
    on_dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id("roomstate-rail-popover")
        .w(px(RAIL_POPOVER_WIDTH_PX))
        .max_w_full()
        .p(px(Spacing::SM))
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .rounded(Radii::md())
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(Spacing::XS_PLUS))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down_out(on_dismiss)
        .child(
            div()
                .text_size(px(FontSize::XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_muted())
                .child("Chat modes"),
        )
        .children(modes.iter().map(|mode| {
            div()
                .flex()
                .items_start()
                .gap(px(Spacing::XS_PLUS))
                .child(
                    div()
                        .w(px(RAIL_CHIP_ICON_PX))
                        .h(px(RAIL_CHIP_ICON_PX))
                        .flex()
                        .items_center()
                        .justify_center()
                        .flex_shrink_0()
                        .child(render_mode_icon(mode.kind, colors::accent_hover())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(Spacing::HAIRLINE))
                        .child(
                            div()
                                .text_size(px(FontSize::XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child(match &mode.duration_label {
                                    Some(label) => format!("{} {label}", mode.kind.title()),
                                    None => mode.kind.title().to_string(),
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(FontSize::XS))
                                .text_color(colors::text_muted())
                                .child(mode.tooltip.clone()),
                        ),
                )
        }))
        .into_any_element()
}

/// Clear every rail tooltip owned by one composer scope. Splits can disappear
/// while the pointer still rests on a chip, which would otherwise strand it.
pub fn clear_room_state_rail_tooltips(tooltip_scope: u64) {
    for key in ["follow", "slow", "sub", "emote", "r9k"] {
        clear_global_tooltip_if_source(&rail_tooltip_source(tooltip_scope, key));
    }
    for key in ["overflow", "target", "collapsed"] {
        clear_global_tooltip_if_source(&rail_tooltip_source(tooltip_scope, key));
    }
}

fn mode_pill_compact_with_tooltip(
    tooltip_scope: u64,
    key: &str,
    label: &str,
    active: bool,
    tooltip: &str,
) -> impl IntoElement + use<> {
    let tooltip_text = tooltip.to_string();
    let source_id = format!("roomstate-{}-{}", tooltip_scope, key);

    div()
        .id(source_id.clone())
        .relative()
        .flex_shrink_0()
        .on_hover({
            let source_id = source_id.clone();
            let tooltip_text = tooltip_text.clone();
            move |hovered, window, _app| {
                if *hovered {
                    show_text_tooltip(
                        window.window_handle(),
                        source_id.clone(),
                        tooltip_text.clone(),
                        window.mouse_position(),
                    );
                } else {
                    clear_global_tooltip_if_source_in_window(window.window_handle(), &source_id);
                }
            }
        })
        .child(mode_pill_compact(label, active))
}
