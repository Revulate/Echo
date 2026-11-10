//! Standalone channel chat popup window.

use crate::backend;
use crate::backend::lock_utils::MutexExt;
use crate::components::chat::ChatView;
use crate::components::chat::chat_header_renderer::{
    ChannelHeaderLayout, render_channel_header_context,
};
use crate::components::chat::chat_settings_bar::{
    active_room_state_label_count, clear_chat_settings_tooltips,
};
use crate::components::overlays::{clear_global_tooltip, clear_global_tooltip_if_source};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Input, InputEvent, InputState};
use crate::state::{AppState, SplitId};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use crate::utils::persist_window_bounds;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

const INPUT_HEIGHT: f32 = 48.0;
const INPUT_CONTEXT_HEIGHT: f32 = 18.0;
const FIXTURE_POPUP_WIDTH: f32 = 720.0;
const FIXTURE_POPUP_HEIGHT: f32 = 640.0;
static NEXT_POPUP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct PopupHandleEntry {
    id: u64,
    handle: AnyWindowHandle,
}

static POPUP_HANDLES: LazyLock<Mutex<HashMap<String, PopupHandleEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
pub struct ChannelPopupHeaderSnapshot {
    pub width: f32,
    pub header_height: f32,
    pub expanded: bool,
    pub status: &'static str,
    pub title_present: bool,
    pub category_present: bool,
    pub roomstate_labels: usize,
    pub overlap_safe: bool,
}

static POPUP_HEADER_SNAPSHOTS: LazyLock<Mutex<HashMap<String, ChannelPopupHeaderSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn is_ui_fixture_popup(channel: &str) -> bool {
    crate::bench::ui_fixture::is_enabled()
        && matches!(
            channel,
            crate::bench::ui_fixture::OFFLINE_CHANNEL
                | crate::bench::ui_fixture::LIVE_CHANNEL
                | crate::bench::ui_fixture::HIGHLIGHT_LIVE_CHANNEL
        )
}

pub fn ui_fixture_popup_snapshot(channel: &str) -> Option<ChannelPopupHeaderSnapshot> {
    if !is_ui_fixture_popup(channel) {
        return None;
    }
    POPUP_HEADER_SNAPSHOTS
        .lock_or_recover()
        .get(channel)
        .cloned()
}

pub fn ui_fixture_popup_is_open(channel: &str) -> bool {
    is_ui_fixture_popup(channel) && POPUP_HANDLES.lock_or_recover().contains_key(channel)
}

pub fn close_ui_fixture_popup(channel: &str, cx: &mut App) -> bool {
    if !is_ui_fixture_popup(channel) {
        return false;
    }
    let handle = POPUP_HANDLES.lock_or_recover().get(channel).copied();
    let Some(handle) = handle else {
        return true;
    };
    handle
        .handle
        .update(cx, |_view, window, _cx| window.remove_window())
        .is_ok()
}

fn remove_popup_handle_if_current(channel: &str, popup_id: u64) -> bool {
    let mut handles = POPUP_HANDLES.lock_or_recover();
    if handles.get(channel).map(|entry| entry.id) != Some(popup_id) {
        return false;
    }
    handles.remove(channel);
    true
}

fn popup_header_tooltip_source(popup_id: u64) -> String {
    format!("popup-header-stream-info-{popup_id}")
}

pub fn open_channel_popup(channel: String, app_state: Entity<AppState>, cx: &mut App) -> bool {
    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }

    app_state.update(cx, |state, cx| {
        state.chat_backend.update(cx, |backend, cx| {
            backend.get_or_create_channel(&channel, cx);
        });
    });
    let fixture_popup = is_ui_fixture_popup(&channel);
    if !fixture_popup {
        backend::connect_channel(&channel);
    }

    let existing_popup = POPUP_HANDLES.lock_or_recover().get(&channel).copied();
    if let Some(existing_popup) = existing_popup {
        if existing_popup
            .handle
            .update(cx, |_, window, cx| {
                cx.activate(true);
                window.activate_window();
            })
            .is_ok()
        {
            if !fixture_popup {
                backend::settings::set_chat_popup_window_open(&channel, true);
            }
            return true;
        }

        if remove_popup_handle_if_current(&channel, existing_popup.id)
            && !fixture_popup
            && !backend::is_shutdown_started()
        {
            backend::settings::set_chat_popup_window_open(&channel, false);
        }
    }

    let (popup_width, popup_height) = if fixture_popup {
        (FIXTURE_POPUP_WIDTH, FIXTURE_POPUP_HEIGHT)
    } else {
        backend::settings::get_chat_popup_window_size_for_channel(&channel)
    };
    let saved_position = (!fixture_popup)
        .then(|| backend::settings::get_chat_popup_window_position_for_channel(&channel))
        .flatten();
    let (bounds, display_id) = crate::utils::popup_window_placement(
        saved_position,
        size(px(popup_width), px(popup_height)),
        cx,
    );
    let window_bounds = if !fixture_popup
        && backend::settings::get_chat_popup_window_maximized_for_channel(&channel)
    {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    let title = format!("Echo - #{channel}");
    let popup_id = NEXT_POPUP_ID.fetch_add(1, Ordering::Relaxed);
    let failed_channel = channel.clone();

    match crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: false,
                ..Default::default()
            }),
            window_bounds: Some(window_bounds),
            focus: true,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(px(360.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            crate::components::chat::canvas_renderer::initialize_font_metrics(window, cx);
            let window_handle = window.window_handle();
            let window_id = window_handle.window_id();
            POPUP_HANDLES.lock_or_recover().insert(
                channel.clone(),
                PopupHandleEntry {
                    id: popup_id,
                    handle: window_handle,
                },
            );
            if !fixture_popup {
                backend::settings::set_chat_popup_window_open(&channel, true);
            }

            let close_channel = channel.clone();
            let close_app_state = app_state.clone();
            let close_header_tooltip_source = popup_header_tooltip_source(popup_id);
            let window_closed_subscription = cx.on_window_closed(move |cx, closed_window_id| {
                if closed_window_id != window_id {
                    return;
                }
                clear_global_tooltip_if_source(&close_header_tooltip_source);
                clear_chat_settings_tooltips(popup_id);
                if !remove_popup_handle_if_current(&close_channel, popup_id) {
                    return;
                }

                if is_ui_fixture_popup(&close_channel) {
                    POPUP_HEADER_SNAPSHOTS
                        .lock_or_recover()
                        .remove(&close_channel);
                } else if !backend::is_shutdown_started() {
                    backend::settings::set_chat_popup_window_open(&close_channel, false);
                    close_app_state.update(cx, |state, cx| {
                        state.cleanup_orphaned_states(cx);
                        state.rebalance_message_retention(cx);
                    });
                }
            });
            cx.new(|cx| {
                ChannelPopup::new(
                    channel.clone(),
                    popup_id,
                    app_state.clone(),
                    window_closed_subscription,
                    window,
                    cx,
                )
            })
        },
    ) {
        Ok(_) => true,
        Err(error) => {
            if !fixture_popup && !backend::is_shutdown_started() {
                backend::settings::set_chat_popup_window_open(&failed_channel, false);
            }
            crate::log_ui!(
                warn,
                "Failed to open channel popup for '{}': {}",
                failed_channel,
                error
            );
            false
        }
    }
}

/// Recreate native channel popups that were still open at graceful shutdown.
/// Failed restores clear their intent so every later launch does not repeat an
/// unrecoverable platform-window failure.
pub fn restore_open_channel_popups(app_state: Entity<AppState>, cx: &mut App) {
    for channel in backend::settings::get_open_chat_popup_channels() {
        open_channel_popup(channel, app_state.clone(), cx);
    }
}

pub struct ChannelPopup {
    channel: String,
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
    popup_id: u64,
    app_state: Entity<AppState>,
    chat_view: Entity<ChatView>,
    input: Option<Entity<InputState>>,
    pending_clear_input: bool,
    last_persisted_position: Option<(f32, f32)>,
    last_persisted_size: (f32, f32),
    last_persisted_maximized: bool,
    eligibility_waker_generation: u64,
    eligibility_waker_task: Option<Task<()>>,
    eligibility_visibility_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl ChannelPopup {
    fn new(
        channel: String,
        popup_id: u64,
        app_state: Entity<AppState>,
        window_closed_subscription: Subscription,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fixture_popup = is_ui_fixture_popup(&channel);
        let last_persisted_size = if fixture_popup {
            (FIXTURE_POPUP_WIDTH, FIXTURE_POPUP_HEIGHT)
        } else {
            backend::settings::get_chat_popup_window_size_for_channel(&channel)
        };
        let last_persisted_position = if fixture_popup {
            None
        } else {
            backend::settings::get_chat_popup_window_position_for_channel(&channel)
        };
        let last_persisted_maximized = !fixture_popup
            && backend::settings::get_chat_popup_window_maximized_for_channel(&channel);
        let chat_backend = app_state.read(cx).chat_backend.clone();
        let chat_state = chat_backend
            .read(cx)
            .get_channel(&channel)
            .unwrap_or_else(|| {
                chat_backend.update(cx, |backend, cx| {
                    backend.get_or_create_channel(&channel, cx)
                })
            });
        let chat_view = cx.new(|cx| {
            ChatView::new(
                channel.clone(),
                SplitId::new(),
                app_state.clone(),
                chat_state.clone(),
                last_persisted_size.0,
                cx,
            )
        });
        let app_state_subscription = cx.observe(&app_state, |_this, _state, cx| {
            cx.notify();
        });
        let chat_state_subscription = cx.observe(&chat_state, |_this, _state, cx| {
            cx.notify();
        });
        let window_activation_subscription =
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    clear_global_tooltip();
                }
                let modifiers = window.modifiers();
                if this.sync_chat_pause_modifiers(modifiers, window.is_window_active(), cx) {
                    cx.notify();
                }
            });
        Self {
            channel,
            always_on_top: Default::default(),
            popup_id,
            app_state,
            chat_view,
            input: None,
            pending_clear_input: false,
            last_persisted_position,
            last_persisted_size,
            last_persisted_maximized,
            eligibility_waker_generation: 0,
            eligibility_waker_task: None,
            eligibility_visibility_subscription: None,
            _subscriptions: vec![
                window_closed_subscription,
                app_state_subscription,
                chat_state_subscription,
                window_activation_subscription,
            ],
        }
    }

    fn ensure_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
            return;
        }

        let input = cx.new(|cx| InputState::new(window, cx).with_placeholder("Send a message…"));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
            if matches!(event, InputEvent::PressEnter) {
                let message = input.read(cx).value().trim().to_string();
                if !message.is_empty() && this.try_send_message(message, cx) {
                    this.pending_clear_input = true;
                    cx.notify();
                }
            }
        });

        let focus_handle = input.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        self.input = Some(input);
        self._subscriptions.push(subscription);
    }

    fn clear_input_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_clear_input {
            return;
        }
        self.pending_clear_input = false;
        if let Some(input) = self.input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    fn send_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        let message = input.read(cx).value().trim().to_string();
        if message.is_empty() {
            return;
        }

        if self.try_send_message(message, cx) {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    fn try_send_message(&mut self, message: String, cx: &mut Context<Self>) -> bool {
        if Self::sends_chat_payload(&message)
            && let Some(error) = popup_send_readiness_error(&self.channel)
        {
            push_popup_send_blocked_toast(&self.channel, &error);
            cx.notify();
            return false;
        }

        backend::send_message_or_command_async(self.channel.clone(), message);
        true
    }

    fn sends_chat_payload(message: &str) -> bool {
        !message.starts_with('/')
            || message
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/me "))
            || !backend::settings::get_slash_commands_enabled()
    }

    fn release_eligibility_waker(&mut self) {
        self.eligibility_waker_generation = self.eligibility_waker_generation.wrapping_add(1);
        self.eligibility_waker_task.take();
    }

    fn ensure_eligibility_visibility_observer(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.eligibility_visibility_subscription.is_some() {
            return;
        }

        let popup = cx.weak_entity();
        self.eligibility_visibility_subscription = Some(window.observe_window_visibility(
            move |visibility, window, cx| {
                let _ = popup.update(cx, |popup, cx| {
                    if visibility.is_visible() {
                        popup.ensure_eligibility_waker(window, cx);
                        cx.notify();
                    } else {
                        popup.release_eligibility_waker();
                    }
                });
            },
        ));
    }

    fn ensure_eligibility_waker(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !window.is_visible() {
            if self.eligibility_waker_task.is_some() {
                self.release_eligibility_waker();
            }
            return;
        }
        let Some(delay) = backend::irc::room_state::current_send_eligibility(&self.channel)
            .and_then(|status| status.next_change)
        else {
            if self.eligibility_waker_task.is_some() {
                self.release_eligibility_waker();
            }
            return;
        };
        if self.eligibility_waker_task.is_some() {
            return;
        }

        self.eligibility_waker_generation = self.eligibility_waker_generation.wrapping_add(1);
        let generation = self.eligibility_waker_generation;
        let popup = cx.weak_entity();
        let task = cx.spawn_in(window, async move |_, cx| {
            cx.background_executor().timer(delay).await;
            let _ = cx.update(|window, cx| {
                let _ = popup.update(cx, |popup, cx| {
                    if popup.eligibility_waker_generation != generation {
                        return;
                    }
                    popup.eligibility_waker_task.take();
                    if window.is_visible() {
                        cx.notify();
                    }
                });
            });
        });
        self.eligibility_waker_task = Some(task);
    }

    fn persist_window_bounds(&mut self, window: &Window) {
        if is_ui_fixture_popup(&self.channel) {
            return;
        }
        let maximized = window.is_maximized();
        if self.last_persisted_maximized != maximized {
            backend::settings::set_chat_popup_window_maximized_for_channel(
                &self.channel,
                maximized,
            );
            self.last_persisted_maximized = maximized;
        }

        if maximized {
            return;
        }

        let position_channel = self.channel.clone();
        let size_channel = self.channel.clone();
        persist_window_bounds(
            window,
            &mut self.last_persisted_position,
            &mut self.last_persisted_size,
            move |x, y| {
                backend::settings::set_chat_popup_window_position_for_channel(
                    &position_channel,
                    x,
                    y,
                )
            },
            move |width, height| {
                backend::settings::set_chat_popup_window_size_for_channel(
                    &size_channel,
                    width,
                    height,
                )
            },
        );
    }

    fn sync_chat_pause_modifiers(
        &self,
        modifiers: Modifiers,
        window_active: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let held = if window_active {
            modifiers
        } else {
            Modifiers::default()
        };
        let canvas_list = self.chat_view.read(cx).canvas_list.clone();
        canvas_list.update(cx, |list, cx| {
            let changed = list.set_chat_pause_modifiers(held);
            if changed {
                crate::backend::metrics::record_ui_notify_source(
                    "channel_popup.chat_pause.modifier",
                );
                cx.notify();
            }
            changed
        })
    }
}

impl Render for ChannelPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        colors::begin_secondary_window_frame();
        self.always_on_top.sync(window);
        self.ensure_input(window, cx);
        self.ensure_eligibility_visibility_observer(window, cx);
        self.ensure_eligibility_waker(window, cx);
        self.clear_input_if_needed(window, cx);
        self.persist_window_bounds(window);
        let _ = self.sync_chat_pause_modifiers(window.modifiers(), window.is_window_active(), cx);

        let viewport = window.viewport_size();
        let width: f32 = viewport.width.into();
        let height: f32 = viewport.height.into();
        let content_width = width.max(240.0);
        let input = self.input.clone();
        let send_block_reason = popup_send_readiness_error(&self.channel);
        let input_context_height = if send_block_reason.is_some() {
            INPUT_CONTEXT_HEIGHT
        } else {
            0.0
        };
        let input_area_height = INPUT_HEIGHT + input_context_height;
        let send_blocked = send_block_reason.is_some()
            && input
                .as_ref()
                .is_some_and(|input| Self::sends_chat_payload(input.read(cx).value().trim()));
        let can_clear_restriction = matches!(
            backend::irc::room_state::current_send_eligibility(&self.channel)
                .map(|status| status.block),
            Some(
                backend::ui_actions::composer_send::EligibilityBlock::Banned
                    | backend::ui_actions::composer_send::EligibilityBlock::TimedOut(_)
            )
        );
        let stream_info = self
            .app_state
            .read(cx)
            .stream_info
            .get(&self.channel)
            .cloned()
            .flatten();
        let room_state = backend::irc::room_state::get_room_state(&self.channel);
        let header_layout = ChannelHeaderLayout::for_popup_width(width, room_state.as_ref());
        let header_height = header_layout.height();
        let message_height = (height - header_height - input_area_height).max(120.0);

        if is_ui_fixture_popup(&self.channel) {
            let title_present = stream_info
                .as_ref()
                .is_some_and(|info| !info.title.trim().is_empty());
            let category_present = stream_info.as_ref().is_some_and(|info| {
                info.game_name
                    .as_deref()
                    .is_some_and(|category| !category.trim().is_empty())
            });
            let roomstate_labels = active_room_state_label_count(room_state.as_ref());
            let snapshot = ChannelPopupHeaderSnapshot {
                width,
                header_height,
                expanded: header_layout.is_expanded(),
                status: if stream_info.as_ref().is_some_and(|info| info.is_live) {
                    "LIVE"
                } else {
                    "OFFLINE"
                },
                title_present,
                category_present,
                roomstate_labels,
                overlap_safe: (!header_layout.is_expanded() || header_height >= 68.0)
                    && message_height + header_height + input_area_height <= height + 0.5,
            };
            POPUP_HEADER_SNAPSHOTS
                .lock_or_recover()
                .insert(self.channel.clone(), snapshot);
        }

        div()
            .id("channel-popup")
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(colors::bg_void())
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    if this.sync_chat_pause_modifiers(
                        event.modifiers,
                        window.is_window_active(),
                        cx,
                    ) {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .id("channel-popup-header")
                    .h(px(header_height))
                    .flex_shrink_0()
                    .px_3()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(colors::border())
                    .bg(colors::bg_surface())
                    .child(render_channel_header_context(
                        &self.channel,
                        width,
                        stream_info.as_ref(),
                        room_state.as_ref(),
                        false,
                        header_layout,
                        popup_header_tooltip_source(self.popup_id),
                        self.popup_id,
                    )),
            )
            .child(
                div()
                    .id("channel-popup-messages")
                    .h(px(message_height))
                    .min_h_0()
                    .min_w_0()
                    .child({
                        self.chat_view.update(cx, |view, cx| {
                            view.update_render_params(
                                content_width,
                                message_height,
                                true,
                                crate::backend::settings::should_show_message_timestamps(
                                    stream_info.as_ref().is_some_and(|info| info.is_live),
                                ),
                                true,
                                cx,
                            );
                        });
                        self.chat_view.clone().into_any_element()
                    }),
            )
            .child(
                div()
                    .id("channel-popup-input-area")
                    .h(px(input_area_height))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(colors::border())
                    .bg(colors::bg_surface())
                    .child(
                        div()
                            .id("channel-popup-input-row")
                            .h(px(INPUT_HEIGHT))
                            .flex_shrink_0()
                            .p_2()
                            .gap_2()
                            .flex()
                            .items_center()
                            .when_some(input, |row, input| {
                                row.child(div().flex_1().min_w_0().child(Input::new(&input)))
                            })
                            .child(
                                div()
                                    .id("channel-popup-send")
                                    .px_3()
                                    .py_1()
                                    .rounded(Radii::md())
                                    .bg(if send_blocked {
                                        colors::bg_elevated()
                                    } else {
                                        colors::accent()
                                    })
                                    .text_color(if send_blocked {
                                        colors::text_muted()
                                    } else {
                                        colors::text_on_accent_contrast()
                                    })
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Send")
                                    .on_activate(cx.listener(|this, _event, window, cx| {
                                        this.send_input(window, cx);
                                    })),
                            ),
                    )
                    .when_some(send_block_reason, |area, reason| {
                        area.child(
                            div()
                                .id("channel-popup-input-context")
                                .h(px(INPUT_CONTEXT_HEIGHT))
                                .flex_shrink_0()
                                .px_2()
                                .gap_2()
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .truncate()
                                        .child(reason),
                                )
                                .when(can_clear_restriction, |row| {
                                    let channel = self.channel.clone();
                                    row.child(
                                        div()
                                            .id("channel-popup-try-again")
                                            .flex_shrink_0()
                                            .px_1()
                                            .text_xs()
                                            .text_color(colors::text_muted())
                                            .cursor_pointer()
                                            .hover(|button| {
                                                button.text_color(colors::text_primary())
                                            })
                                            .child("Try again")
                                            .on_activate_without_tab_stop(cx.listener(
                                                move |_this, _, _window, cx| {
                                                    backend::irc::room_state::clear_current_send_restriction(&channel);
                                                    cx.notify();
                                                },
                                            )),
                                    )
                                }),
                        )
                    })
            )
    }
}

fn push_popup_send_blocked_toast(channel: &str, error: &str) {
    let channel_label = channel.trim_start_matches('#').to_string();

    backend::push_ui_action(backend::UiAction::ShowWarningToast {
        message: "Message not sent".to_string(),
        detail: Some(format!("{channel_label}: {}", error.trim())),
    });
}

fn popup_send_readiness_error(channel: &str) -> Option<String> {
    backend::irc::ensure_authenticated_send_ready()
        .err()
        .map(String::from)
        .or_else(|| backend::irc::ensure_channel_send_ready(channel).err())
}
