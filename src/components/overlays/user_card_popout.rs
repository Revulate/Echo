//! User card popout window
//!
//! Hosts the user card in its own floating OS window so it can be moved
//! anywhere (including outside the main Echo window), resized from its window
//! edges, and interacted with without fighting the chat underneath.
//!
//! Follows the same cross-window pattern as `emote_picker_popout`: a single
//! reusable window tracked by statics, with pending state applied on render
//! when a new user card is requested while the window is already open. All
//! card actions (mention, whisper, mod commands, toasts) already flow through
//! the global `backend::push_ui_action` queue, so no extra plumbing back to
//! the main window is needed.

use super::user_card::{UserCard, UserCardState};
use super::{ensure_global_tooltip_animation_waker, render_global_tooltip_overlay};
use crate::backend::hotkeys::{HotkeyAction, HotkeyMatchContext, MatchedHotkey};
use crate::backend::lock_utils::MutexExt;
use crate::theme::colors;
use crate::utils::persist_window_bounds;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// Flag indicating if the user card popout window is currently open
static USER_CARD_POPOUT_OPEN: AtomicBool = AtomicBool::new(false);
static USER_CARD_POPOUT_HANDLE: LazyLock<Mutex<Option<AnyWindowHandle>>> =
    LazyLock::new(|| Mutex::new(None));
/// Card state requested while the window was already open; applied on render.
static PENDING_USER_CARD_STATE: Mutex<Option<UserCardState>> = Mutex::new(None);

fn user_card_window_title(state: &UserCardState) -> String {
    let name = state
        .display_name
        .as_deref()
        .or(state.username.as_deref())
        .unwrap_or("User");
    match state.channel.as_deref() {
        Some(channel) if !channel.is_empty() => format!("{} \u{2014} #{}", name, channel),
        _ => name.to_string(),
    }
}

/// Mark the popout as closed (called from Drop or when the handle is stale).
pub fn mark_user_card_popout_closed() {
    USER_CARD_POPOUT_OPEN.store(false, Ordering::SeqCst);
    *USER_CARD_POPOUT_HANDLE.lock_or_recover() = None;
    *PENDING_USER_CARD_STATE.lock_or_recover() = None;
}

/// Open the user card popout window, or retarget + refocus it if already open.
pub fn open_user_card_popout(state: UserCardState, cx: &mut App) {
    if USER_CARD_POPOUT_OPEN.load(Ordering::SeqCst) {
        *PENDING_USER_CARD_STATE.lock_or_recover() = Some(state.clone());
        let handle = *USER_CARD_POPOUT_HANDLE.lock_or_recover();
        if let Some(handle) = handle
            && handle
                .update(cx, |_, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    window.refresh();
                })
                .is_ok()
        {
            return;
        }

        mark_user_card_popout_closed();
    }

    USER_CARD_POPOUT_OPEN.store(true, Ordering::SeqCst);

    let (width, height) = crate::backend::settings::get_user_card_popout_window_size();
    let window_size = size(px(width), px(height));
    let (bounds, display_id) = placement_near_click(&state, window_size, cx).unwrap_or_else(|| {
        crate::utils::popup_window_placement(
            crate::backend::settings::get_user_card_popout_window_position(),
            window_size,
            cx,
        )
    });

    let title = user_card_window_title(&state);
    let open_result = crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            kind: WindowKind::Floating,
            is_movable: true,
            is_minimizable: false,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(px(320.0), px(420.0))),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        },
        move |window, cx| {
            *USER_CARD_POPOUT_HANDLE.lock_or_recover() = Some(window.window_handle());
            let popout = cx.new(|cx| UserCardPopout::new(state.clone(), cx));
            let focus_handle = popout.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
            popout
        },
    );
    if open_result.is_err() {
        mark_user_card_popout_closed();
    }
}

/// Place a newly opened card next to the clicked name:
/// the click point, converted from the source window to screen space, sits a
/// third of the way across and a fifth of the way down the card, and the card
/// is kept inside the visible area of the display under the click. A card
/// without a click position (non-finite x/y, e.g. from `/usercard`) is centered
/// on the source window.
///
/// The source window's own display is used rather than a search by point:
/// window and display coordinates are logical per monitor, so with mixed DPI a
/// point from one monitor can land inside another monitor's logical bounds.
fn placement_near_click(
    state: &UserCardState,
    window_size: Size<Pixels>,
    cx: &mut App,
) -> Option<(Bounds<Pixels>, Option<DisplayId>)> {
    let (source_bounds, source_display) = cx
        .active_window()?
        .update(cx, |_, window, cx| (window.bounds(), window.display(cx)))
        .ok()?;
    let has_click = state.x.is_finite() && state.y.is_finite();
    let click = if has_click {
        point(
            source_bounds.origin.x + px(state.x),
            source_bounds.origin.y + px(state.y),
        )
    } else {
        source_bounds.center()
    };
    let display = source_display.or_else(|| {
        cx.displays()
            .into_iter()
            .find(|display| display.bounds().contains(&click))
    })?;
    let visible = display.visible_bounds();
    let fitted = size(
        window_size.width.min(visible.size.width),
        window_size.height.min(visible.size.height),
    );
    let requested = if has_click {
        click - point(fitted.width / 3.0, fitted.height / 5.0)
    } else {
        click - point(fitted.width / 2.0, fitted.height / 2.0)
    };
    let max = visible.bottom_right() - point(fitted.width, fitted.height);
    let origin = point(
        requested.x.clamp(visible.origin.x, max.x),
        requested.y.clamp(visible.origin.y, max.y),
    );
    Some((Bounds::new(origin, fitted), Some(display.id())))
}

/// Root entity of the user card popout window.
pub struct UserCardPopout {
    card: Entity<UserCard>,
    focus_handle: FocusHandle,
    window_title: String,
    last_persisted_position: Option<(f32, f32)>,
    last_persisted_size: (f32, f32),
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
}

impl Drop for UserCardPopout {
    fn drop(&mut self) {
        mark_user_card_popout_closed();
    }
}

impl UserCardPopout {
    fn new(state: UserCardState, cx: &mut Context<Self>) -> Self {
        let window_title = user_card_window_title(&state);
        let focus_handle = cx.focus_handle();
        let card = cx.new(|_cx| {
            let mut card = UserCard::new(state);
            card.hosted_in_window = true;
            card.host_focus = Some(focus_handle.clone());
            card
        });
        Self {
            card,
            focus_handle,
            window_title,
            last_persisted_position: crate::backend::settings::get_user_card_popout_window_position(
            ),
            last_persisted_size: crate::backend::settings::get_user_card_popout_window_size(),
            always_on_top: Default::default(),
        }
    }

    /// Esc closes the card. The user-card hotkeys (Quick Timeout 30s/10m,
    /// Quick Ban) act on this card's target while the card itself, not one of
    /// its text fields, has focus.
    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
            self.card.update(cx, |card, cx| {
                card.update_state(UserCardState::default());
                cx.notify();
            });
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if event.is_held || !self.focus_handle.is_focused(window) {
            return;
        }
        let context = HotkeyMatchContext {
            has_user_card: true,
            ..Default::default()
        };
        let Some(MatchedHotkey::Builtin(action)) = crate::backend::hotkeys::match_key_event(
            event,
            context,
            crate::components::ui::input::is_input_focused(window),
        ) else {
            return;
        };
        let timeout_seconds = match action {
            HotkeyAction::QuickTimeout30s => Some(30),
            HotkeyAction::QuickTimeout10m => Some(600),
            HotkeyAction::QuickBan => None,
            _ => return,
        };
        cx.stop_propagation();
        let Some((username, channel)) = self.card.read(cx).get_moderation_target() else {
            return;
        };
        if !crate::components::common::moderation_policy::recheck_moderation_target(
            &channel, &username,
        ) {
            return;
        }
        match timeout_seconds {
            Some(seconds) => {
                crate::backend::timeout_user(&channel, &username, seconds, None);
            }
            None => {
                crate::backend::ban_user(&channel, &username, None);
            }
        }
    }

    /// Apply a card state requested while the window was already open.
    fn apply_pending_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = PENDING_USER_CARD_STATE.lock_or_recover().take();
        let Some(state) = next else {
            return;
        };

        let title = user_card_window_title(&state);
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
        self.card.update(cx, |card, _| {
            card.hosted_in_window = true;
            card.update_state(state);
        });
        let focus_handle = self.focus_handle.clone();
        window.defer(cx, move |window, cx| window.focus(&focus_handle, cx));
        cx.notify();
    }

    fn persist_window_bounds(&mut self, window: &Window) {
        persist_window_bounds(
            window,
            &mut self.last_persisted_position,
            &mut self.last_persisted_size,
            crate::backend::settings::set_user_card_popout_window_position,
            crate::backend::settings::set_user_card_popout_window_size,
        );
    }
}

impl Render for UserCardPopout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        colors::begin_secondary_window_frame();
        self.always_on_top.sync(window);
        self.apply_pending_state(window, cx);

        if !self.card.read(cx).is_open() {
            let handle = window.window_handle();
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
            return div()
                .size_full()
                .bg(colors::bg_elevated())
                .into_any_element();
        }

        self.persist_window_bounds(window);
        ensure_global_tooltip_animation_waker(window.window_handle(), cx);

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .size_full()
            .bg(colors::bg_elevated())
            .text_color(colors::text_primary())
            .child(self.card.clone())
            .when_some(
                render_global_tooltip_overlay(
                    window.window_handle(),
                    window.viewport_size(),
                    window.mouse_position(),
                ),
                |s, tooltip| s.child(tooltip),
            )
            .into_any_element()
    }
}
