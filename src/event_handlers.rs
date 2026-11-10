//! Event handler registration for the main application.
//!
//! This module contains the logic for registering keyboard shortcuts,
//! mouse handlers, and other UI event handlers.

use crate::EchoApp;
use crate::app::apply_main_window_always_on_top;
use crate::backend;
use crate::backend::hotkeys::{HotkeyAction, HotkeyMatchContext, MatchedHotkey};
use crate::managers::frame_rate_controller;
use crate::state::{ActivitySection, AddSplitDirection};
use gpui::*;

/// A key press that produces text rather than a shortcut chord: no Ctrl, Alt
/// or Win, except AltGr-style layouts GPUI flags as preferring character input.
pub(crate) fn is_typed_text(event: &KeyDownEvent) -> bool {
    let modifiers = &event.keystroke.modifiers;
    event.keystroke.key_char.is_some()
        && (event.prefer_character_input
            || !(modifiers.control || modifiers.alt || modifiers.platform))
}

/// A dead key (for example `^` before `e`). Left unhandled, Windows composes
/// it with the next key into one character for the focused input.
fn is_dead_key(event: &KeyDownEvent) -> bool {
    let modifiers = &event.keystroke.modifiers;
    event.prefer_character_input && !(modifiers.control || modifiers.alt || modifiers.platform)
}

fn open_search_for_active_tab(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let app_state = app.app_state.read(cx);
    let active_tab = app_state.active_tab();
    let Some(current_channel) = active_tab.active_channel() else {
        return;
    };
    let _ = app_state;

    app.overlay_manager
        .read(cx)
        .search_modal
        .clone()
        .update(cx, |modal, _| {
            modal.open_docked(current_channel);
        });
    cx.notify();
}

fn open_search_for_all_tabs(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let mut channels: Vec<String> = app
        .app_state
        .read(cx)
        .all_active_channels()
        .into_iter()
        .collect();
    channels.sort();

    app.overlay_manager
        .read(cx)
        .search_modal
        .clone()
        .update(cx, |modal, _| {
            modal.open_expanded_with_scope(channels, None);
        });
    cx.notify();
}

fn dismiss_tab_transients(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let dismissed = app.tab_bar_renderer.update(cx, |renderer, _| {
        let was_open = renderer.is_context_menu_open() || renderer.is_overflow_open();
        renderer.close_context_menu();
        renderer.close_overflow();
        was_open
    });
    if dismissed {
        cx.notify();
    }
}

fn move_active_tab(app: &mut EchoApp, delta: isize, cx: &mut Context<EchoApp>) {
    let moved = app.app_state.update(cx, |state, _| {
        let from = state.active_tab_idx;
        let Some(to) = from.checked_add_signed(delta) else {
            return false;
        };
        if to >= state.tabs.len() {
            return false;
        }
        state.move_tab(from, to);
        true
    });
    if moved {
        app.save_tabs(cx);
        app.refocus_active_input(cx);
        cx.notify();
    }
}

fn execute_custom_hotkey_action(app: &mut EchoApp, command: &str, cx: &mut Context<EchoApp>) {
    let active_path = app.split_layout.read(cx).get_active_split_path().to_vec();
    let target = {
        let app_state = app.app_state.read(cx);
        let active_tab = app_state.active_tab();
        active_tab
            .layout
            .get_channel_at_path(&active_path)
            .zip(active_tab.layout.get_split_id_at_path(&active_path))
    };

    let Some((target_channel, split_id)) = target else {
        app.overlay_manager.update(cx, |manager, cx| {
            manager.push_toast(
                crate::components::overlays::Toast::warning("No active chat input")
                    .with_detail("Open a channel before running a custom hotkey."),
                cx,
            );
        });
        cx.notify();
        return;
    };

    let input_text = app
        .chat_input_renderer
        .read(cx)
        .input_manager
        .input_texts
        .get(&split_id)
        .cloned()
        .unwrap_or_default();
    let username = backend::settings::get_username().unwrap_or_default();
    let resolved = backend::hotkeys::resolve_hotkey_variables(
        command,
        &input_text,
        &target_channel,
        &username,
    );

    if resolved.trim().is_empty() {
        return;
    }

    backend::send_message_or_command_async(target_channel, resolved);
}

/// Register all event handlers for the application.
///
/// This includes keyboard shortcuts, mouse handlers, and modifier tracking.
pub fn register_event_handlers(
    _app: &mut EchoApp,
    element: Stateful<Div>,
    _window: &mut Window,
    cx: &mut Context<EchoApp>,
) -> Stateful<Div> {
    element
        .can_drop({
            let app = cx.weak_entity();
            move |dropped, window, cx| {
                dropped.downcast_ref::<ExternalPaths>().is_some()
                    && backend::image_uploader::ImageFileSource::Dropped
                        .upload_enabled(&backend::settings::get_image_uploader_config())
                    && app.upgrade().is_some_and(|app| {
                        app.read(cx)
                            .file_drop_target_at(window.mouse_position(), cx)
                            .is_some()
                    })
            }
        })
        .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
            let source = backend::image_uploader::ImageFileSource::Dropped;
            if !source.upload_enabled(&backend::settings::get_image_uploader_config()) {
                return;
            }

            let Some((channel, split_id)) = this.file_drop_target_at(window.mouse_position(), cx)
            else {
                return;
            };
            this.confirm_or_upload_image_files(
                channel,
                split_id,
                paths.paths().to_vec(),
                source,
                cx,
            );
            cx.notify();
        }))
        .on_action(
            cx.listener(|this, _: &crate::app::actions::OpenSettings, _window, cx| {
                this.open_settings(None, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &crate::app::actions::OpenSearch, _window, cx| {
                open_search_for_active_tab(this, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &crate::app::actions::CloseTab, _window, cx| {
                dismiss_tab_transients(this, cx);
                this.close_current_tab(cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &crate::app::actions::NextTab, _window, cx| {
                dismiss_tab_transients(this, cx);
                this.next_tab(cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &crate::app::actions::PrevTab, _window, cx| {
                dismiss_tab_transients(this, cx);
                this.prev_tab(cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &crate::app::actions::FocusInput, window, cx| {
                this.focus_input(window, cx);
            }),
        )
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            frame_rate_controller::record_interaction();

            let key = &event.keystroke.key;
            let ctrl = event.keystroke.modifiers.control;
            let alt = event.keystroke.modifiers.alt;

            let mode_changed = this
                .split_layout
                .update(cx, |layout, _| layout.update_modifiers(ctrl, alt));
            let pause_changed = this.sync_chat_pause_modifiers(
                ctrl,
                alt,
                event.keystroke.modifiers.shift,
                event.keystroke.modifiers.platform,
                window.is_window_active(),
                cx,
            );
            if mode_changed || pause_changed {
                cx.notify();
            }

            if this
                .tab_bar_renderer
                .read(cx)
                .is_rename_input_focused(window, cx)
            {
                if key == "escape" {
                    this.tab_bar_renderer.update(cx, |r, _| r.cancel_rename());
                    this.refocus_active_input(cx);
                    cx.notify();
                }
                return;
            }

            let confirmation_open = this
                .overlay_manager
                .read(cx)
                .confirmation_modal
                .read(cx)
                .state
                .is_open;
            if confirmation_open {
                match key.as_str() {
                    "escape" => {
                        this.overlay_manager
                            .read(cx)
                            .confirmation_modal
                            .clone()
                            .update(cx, |modal, cx| {
                                modal.cancel(cx);
                            });
                        cx.notify();
                        return;
                    }
                    "enter" => {
                        this.overlay_manager
                            .read(cx)
                            .confirmation_modal
                            .clone()
                            .update(cx, |modal, cx| {
                                modal.execute_focused(cx);
                            });
                        cx.notify();
                        return;
                    }
                    "left" | "right" | "tab" => {
                        this.overlay_manager
                            .read(cx)
                            .confirmation_modal
                            .clone()
                            .update(cx, |modal, cx| {
                                modal.toggle_focus(cx);
                            });
                        cx.notify();
                        return;
                    }
                    _ => {}
                }
            }

            let login_modal = this.overlay_manager.read(cx).login_modal.clone();
            if key == "escape" && login_modal.read(cx).is_open {
                login_modal.update(cx, |state, _| state.close());
                cx.notify();
                return;
            }

            let search_modal = this.overlay_manager.read(cx).search_modal.clone();
            let search_open = search_modal.read(cx).state.is_open;
            if search_open {
                let search_hotkey_context = HotkeyMatchContext {
                    allow_global: false,
                    has_active_split: false,
                    has_search_modal: true,
                    has_mentions_sidebar: false,
                    has_whispers_sidebar: false,
                    has_thread_viewer: false,
                    has_user_card: false,
                };
                if let Some(MatchedHotkey::Builtin(action)) = backend::hotkeys::match_key_event(
                    event,
                    search_hotkey_context,
                    crate::components::ui::input::is_input_focused(window),
                ) {
                    this.execute_hotkey_action(action, window, cx);
                    return;
                }

                if key == "escape" {
                    search_modal.update(cx, |modal, cx| {
                        modal.close_and_restore_focus(window, cx);
                        cx.notify();
                    });
                    cx.notify();
                    return;
                }
                return;
            }

            let chatter_list = this.overlay_manager.read(cx).chatter_list.clone();
            let chatter_list_open =
                chatter_list.read(cx).state.is_open && !this.composer_owns_focus(window, cx);
            if chatter_list_open {
                let shortcut = (ctrl || event.keystroke.modifiers.platform) && !alt;
                let search_focused = chatter_list.read(cx).search_focused(window, cx);

                if shortcut && key.eq_ignore_ascii_case("v") && !search_focused {
                    let pasted = crate::utils::read_text(cx).unwrap_or_default();
                    chatter_list.update(cx, |list, cx| {
                        list.type_into_search(&pasted, window, cx);
                    });
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }

                if shortcut && key.eq_ignore_ascii_case("u") {
                    chatter_list.update(cx, |list, cx| {
                        list.clear_search_query(cx);
                    });
                    cx.notify();
                    return;
                }

                match key.as_str() {
                    "escape" => {
                        this.overlay_manager.update(cx, |mgr, cx| {
                            mgr.close_chatter_list(cx);
                        });
                        this.refocus_active_input(cx);
                        cx.notify();
                        return;
                    }
                    _ => {
                        // Typing elsewhere in the list moves it into the search box.
                        if !search_focused
                            && is_typed_text(event)
                            && let Some(text) = event.keystroke.key_char.clone()
                        {
                            chatter_list.update(cx, |list, cx| {
                                list.type_into_search(&text, window, cx);
                            });
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                    }
                }

                return;
            }

            let context_menu_open = this
                .overlay_manager
                .read(cx)
                .context_menu
                .read(cx)
                .is_open();
            if context_menu_open {
                match key.as_str() {
                    "escape" => {
                        this.overlay_manager.update(cx, |mgr, cx| {
                            mgr.close_context_menu(cx);
                        });
                        cx.notify();
                        return;
                    }
                    "up" | "k" => {
                        this.overlay_manager.read(cx).context_menu.clone().update(
                            cx,
                            |menu, cx| {
                                menu.select_previous(cx);
                            },
                        );
                        cx.notify();
                        return;
                    }
                    "down" | "j" => {
                        this.overlay_manager.read(cx).context_menu.clone().update(
                            cx,
                            |menu, cx| {
                                menu.select_next(cx);
                            },
                        );
                        cx.notify();
                        return;
                    }
                    "right" | "l" => {
                        this.overlay_manager.read(cx).context_menu.clone().update(
                            cx,
                            |menu, cx| {
                                menu.open_selected_submenu(cx);
                            },
                        );
                        cx.notify();
                        return;
                    }
                    "left" | "h" => {
                        this.overlay_manager.read(cx).context_menu.clone().update(
                            cx,
                            |menu, cx| {
                                menu.close_submenu(cx);
                            },
                        );
                        cx.notify();
                        return;
                    }
                    "home" | "end" => {
                        let last = key == "end";
                        this.overlay_manager.read(cx).context_menu.clone().update(
                            cx,
                            |menu, cx| {
                                menu.select_edge(last, cx);
                            },
                        );
                        cx.notify();
                        return;
                    }
                    "enter" | "space" => {
                        let confirmed = this
                            .overlay_manager
                            .read(cx)
                            .context_menu
                            .clone()
                            .update(cx, |menu, cx| menu.confirm_selection(cx));
                        if confirmed {
                            this.overlay_manager.update(cx, |mgr, cx| {
                                mgr.close_context_menu(cx);
                            });
                        }
                        cx.notify();
                        return;
                    }
                    _ => {}
                }
                return;
            }

            let quick_switcher_open = this
                .overlay_manager
                .read(cx)
                .quick_switcher
                .read(cx)
                .is_open;
            if quick_switcher_open {
                match key.as_str() {
                    "escape" => {
                        this.overlay_manager.update(cx, |mgr, cx| {
                            mgr.close_quick_switcher(cx);
                        });
                        this.refocus_active_input(cx);
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    "up" => {
                        this.overlay_manager.read(cx).quick_switcher.clone().update(
                            cx,
                            |qs, cx| {
                                qs.move_selection(-1, cx);
                            },
                        );
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    "down" => {
                        this.overlay_manager.read(cx).quick_switcher.clone().update(
                            cx,
                            |qs, cx| {
                                qs.move_selection(1, cx);
                            },
                        );
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    "enter" => {
                        let selection = this
                            .overlay_manager
                            .read(cx)
                            .quick_switcher
                            .clone()
                            .update(cx, |qs, cx| qs.confirm_selection(cx));
                        if let Some(selection) = selection {
                            this.overlay_manager.update(cx, |mgr, cx| {
                                mgr.close_quick_switcher(cx);
                            });
                            match selection {
                                crate::components::overlays::QuickSwitchSelection::Open {
                                    tab_index,
                                    split_path,
                                } => {
                                    this.activate_tab_split(tab_index, split_path, cx);
                                    this.refocus_active_input(cx);
                                }
                                crate::components::overlays::QuickSwitchSelection::FollowedLive {
                                    login,
                                    fill_split,
                                } => this.open_followed_channel(login, fill_split, cx),
                            }
                        }
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    _ if is_typed_text(event) => {
                        // Typed text belongs to the search box: let it through
                        // when focused there, otherwise move it there.
                        let quick_switcher = this.overlay_manager.read(cx).quick_switcher.clone();
                        if !quick_switcher.read(cx).search_focused(window, cx)
                            && let Some(text) = event.keystroke.key_char.clone()
                        {
                            quick_switcher.update(cx, |qs, cx| {
                                qs.type_into_search(&text, window, cx);
                            });
                            cx.notify();
                            cx.stop_propagation();
                        }
                        return;
                    }
                    _ => {}
                }
            }

            if key == "escape" && !this.overlay_manager.read(cx).text_overlay_open(cx) {
                let path = this.split_layout.read(cx).get_active_split_path().to_vec();
                let split_id = this
                    .app_state
                    .read(cx)
                    .active_tab()
                    .layout
                    .get_split_id_at_path(&path);
                if let Some(split_id) = split_id
                    && this
                        .chat_input_renderer
                        .read(cx)
                        .multiline_pastes
                        .contains_key(&split_id)
                {
                    this.resolve_multiline_paste(
                        split_id,
                        None,
                        crate::components::chat::chat_input_renderer::MultilinePasteAction::Cancel,
                        cx,
                    );
                    cx.stop_propagation();
                    return;
                }
            }

            if backend::hotkeys::is_capturing()
                && let Some(binding) = backend::hotkeys::binding_from_event(event)
            {
                if event.keystroke.key == "escape" {
                    crate::components::settings::clear_capture_rejection();
                    crate::components::settings::cancel_all_settings_hotkey_captures();
                    if backend::hotkeys::is_capturing() {
                        backend::hotkeys::cancel_capture();
                    }
                } else if !crate::components::settings::reject_plain_typing_capture(
                    &binding,
                    is_typed_text(event),
                ) {
                    crate::components::settings::complete_settings_hotkey_capture(binding);
                }
                cx.notify();
                return;
            }

            let chat_input_focused = if backend::settings::get_unified_input_mode() {
                this.input_focus.is_focused(window)
            } else {
                let active_path = this.split_layout.read(cx).get_active_split_path().to_vec();
                let active_split_id = this
                    .app_state
                    .read(cx)
                    .active_tab()
                    .layout
                    .get_split_id_at_path(&active_path);
                active_split_id
                    .and_then(|split_id| this.split_panes.get(&split_id))
                    .is_some_and(|pane| pane.read(cx).focus_handle.is_focused(window))
            };
            if chat_input_focused && is_typed_text(event) {
                let copied_chat_selection =
                    backend::hotkeys::get_binding(HotkeyAction::CopyMessage).is_some_and(
                        |binding| backend::hotkeys::binding_matches_event(&binding, event),
                    ) && this.copy_selection(cx);
                if copied_chat_selection {
                    cx.stop_propagation();
                    return;
                }
                return;
            }
            if !chat_input_focused
                && is_typed_text(event)
                && let Some(text) = event.keystroke.key_char.as_deref()
            {
                if is_dead_key(event) {
                    if this.redirect_typed_text_to_active_input(None, window, cx) {
                        return;
                    }
                } else if this.redirect_typed_text_to_active_input(Some(text), window, cx) {
                    cx.stop_propagation();
                    return;
                }
            }

            let (has_mentions_sidebar, has_whispers_sidebar) =
                this.app_state.read(cx).activity_hotkey_scope();
            let hotkey_context = HotkeyMatchContext {
                allow_global: true,
                has_active_split: get_active_channel(this, cx).is_some(),
                has_search_modal: false,
                has_mentions_sidebar,
                has_whispers_sidebar,
                has_thread_viewer: this
                    .overlay_manager
                    .read(cx)
                    .thread_viewer
                    .read(cx)
                    .is_open(),
                has_user_card: this.overlay_manager.read(cx).user_card.read(cx).is_open(),
            };
            if let Some(action) = backend::hotkeys::match_key_event(
                event,
                hotkey_context,
                chat_input_focused || crate::components::ui::input::is_input_focused(window),
            ) {
                match action {
                    MatchedHotkey::Builtin(
                        HotkeyAction::SendMessage | HotkeyAction::SendMessageKeepInput,
                    ) if event.is_held => {}
                    MatchedHotkey::Builtin(action) => {
                        this.execute_hotkey_action(action, window, cx)
                    }
                    MatchedHotkey::CustomCommand(action) => {
                        execute_custom_hotkey_action(this, &action.command, cx);
                    }
                }
                return;
            }

            if key == "escape" {
                let was_dragging = this.split_layout.read(cx).is_dragging_pane();
                if was_dragging {
                    this.split_layout
                        .update(cx, |layout, _| layout.cancel_pane_drag());
                    cx.notify();
                }
                let dismissed_overlay = this
                    .overlay_manager
                    .update(cx, |mgr, cx| mgr.dismiss_top_escape_layer(cx));
                if dismissed_overlay {
                    cx.notify();
                    return;
                }
                let thread_viewer_open = this
                    .overlay_manager
                    .read(cx)
                    .thread_viewer
                    .read(cx)
                    .is_open();
                if thread_viewer_open {
                    this.overlay_manager.update(cx, |mgr, cx| {
                        mgr.close_thread_viewer(cx);
                    });
                    this.refocus_active_input(cx);
                    cx.notify();
                }
            }
        }))
        .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
            let ctrl = event.keystroke.modifiers.control;
            let alt = event.keystroke.modifiers.alt;

            let mode_changed = this.split_layout.update(cx, |layout, _| {
                let mut changed = layout.update_modifiers(ctrl, alt);
                if !ctrl && layout.pane_drag_needs_ctrl() {
                    layout.cancel_pane_drag();
                }
                if layout.recompute_hovered_split() && layout.is_add_split_mode_active() {
                    changed = true;
                }
                changed
            });

            let pause_changed = this.sync_chat_pause_modifiers(
                ctrl,
                alt,
                event.keystroke.modifiers.shift,
                event.keystroke.modifiers.platform,
                window.is_window_active(),
                cx,
            );
            if mode_changed || pause_changed {
                cx.notify();
            }
        }))
        .on_modifiers_changed(
            cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                let ctrl = event.modifiers.control;
                let alt = event.modifiers.alt;

                let mode_changed = this.split_layout.update(cx, |layout, _| {
                    let mut changed = layout.update_modifiers(ctrl, alt);
                    if !ctrl && layout.pane_drag_needs_ctrl() {
                        layout.cancel_pane_drag();
                    }
                    if layout.recompute_hovered_split() && layout.is_add_split_mode_active() {
                        changed = true;
                    }
                    changed
                });

                let pause_changed = this.sync_chat_pause_modifiers(
                    ctrl,
                    alt,
                    event.modifiers.shift,
                    event.modifiers.platform,
                    window.is_window_active(),
                    cx,
                );
                if mode_changed || pause_changed {
                    cx.notify();
                }
            }),
        )
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
            this.cancel_stale_pointer_drags(cx);
            if event.pressed_button != Some(MouseButton::Left) {
                this.cancel_pointer_drags(cx);
            }
            let modifiers = window.modifiers();
            let ctrl = modifiers.control;
            let alt = modifiers.alt;
            let tooltip_moved = crate::components::overlays::update_global_tooltip_position(
                window.window_handle(),
                event.position,
            );

            // Divider drags are tracked by the window-level listener from
            // `EchoApp::render_drag_tracker`, which also sees moves over
            // the occluding hit band that never reach this handler.
            let (mode_changed, is_pane_dragging) = this.split_layout.update(cx, |layout, _| {
                layout.update_cursor_position(event.position);
                let mut changed = layout.update_modifiers(ctrl, alt);
                if !ctrl && layout.pane_drag_needs_ctrl() {
                    layout.cancel_pane_drag();
                }
                if event.pressed_button == Some(MouseButton::Left)
                    && layout.promote_pending_pane_drag(event.position)
                {
                    // The promoting move already reached the chat underneath;
                    // drop any tooltip it raised before the drag shield paints.
                    crate::components::overlays::clear_global_tooltip();
                    changed = true;
                }
                if layout.recompute_hovered_split() && layout.is_add_split_mode_active() {
                    changed = true;
                }
                (changed, layout.is_dragging_pane())
            });

            let pause_changed = this.sync_chat_pause_modifiers(
                ctrl,
                alt,
                modifiers.shift,
                modifiers.platform,
                window.is_window_active(),
                cx,
            );
            if mode_changed || pause_changed || tooltip_moved {
                cx.notify();
            }

            if is_pane_dragging {
                frame_rate_controller::record_interaction();

                this.split_layout.update(cx, |layout, _| {
                    layout.update_pane_drop_target(event.position);
                });
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                this.finish_pointer_drags(event.position, cx);
            }),
        )
}

/// Dispatch a hotkey action to the appropriate handler
///
/// This function is the central dispatcher for all hotkey actions.
/// It was extracted from EchoApp to reduce app.rs LOC.
pub fn dispatch_hotkey_action(
    app: &mut EchoApp,
    action: HotkeyAction,
    window: &mut Window,
    cx: &mut Context<EchoApp>,
) {
    if matches!(
        action,
        HotkeyAction::NewTab
            | HotkeyAction::NewSplit
            | HotkeyAction::NextTab
            | HotkeyAction::PrevTab
            | HotkeyAction::CloseSplit
            | HotkeyAction::CloseTab
            | HotkeyAction::SelectTab1
            | HotkeyAction::SelectTab2
            | HotkeyAction::SelectTab3
            | HotkeyAction::SelectTab4
            | HotkeyAction::SelectTab5
            | HotkeyAction::SelectTab6
            | HotkeyAction::SelectTab7
            | HotkeyAction::SelectTab8
            | HotkeyAction::SelectTab9
            | HotkeyAction::ToggleTabBarVisibility
            | HotkeyAction::ToggleLiveTabsOnly
            | HotkeyAction::ChangeChannel
            | HotkeyAction::QuickSwitcher
            | HotkeyAction::ReopenClosedSplit
            | HotkeyAction::ReopenClosedTab
    ) {
        dismiss_tab_transients(app, cx);
    }

    match action {
        HotkeyAction::ToggleSettings => app.open_settings(None, cx),
        HotkeyAction::ToggleAccountSwitcher => {
            app.overlay_manager.update(cx, |mgr, _cx| {
                mgr.toggle_account_switcher();
            });
            cx.notify();
        }
        HotkeyAction::ToggleNotifications => {
            app.app_state.update(cx, |state, cx| {
                state.toggle_activity_section(
                    ActivitySection::Events,
                    backend::ui_actions::NotificationFilter::All,
                    cx,
                );
            });
        }
        HotkeyAction::ToggleSearch => open_search_for_active_tab(app, cx),
        HotkeyAction::ToggleGlobalSearch => open_search_for_all_tabs(app, cx),
        HotkeyAction::ToggleWhispers => {
            app.app_state.update(cx, |state, cx| {
                state.toggle_activity_section(
                    ActivitySection::Whispers,
                    backend::ui_actions::NotificationFilter::All,
                    cx,
                );
            });
        }
        HotkeyAction::ToggleMentions => {
            app.app_state.update(cx, |state, cx| {
                state.toggle_activity_section(
                    ActivitySection::Mentions,
                    backend::ui_actions::NotificationFilter::All,
                    cx,
                );
            });
        }
        HotkeyAction::NewTab => app.create_new_tab(cx),
        HotkeyAction::NewSplit => {
            app.create_placeholder_split_adjacent_to_active(AddSplitDirection::Right, cx);
        }
        HotkeyAction::NextTab => app.next_tab(cx),
        HotkeyAction::PrevTab => app.prev_tab(cx),
        HotkeyAction::CloseSplit => app.close_active_split(cx),
        HotkeyAction::CloseTab => app.close_current_tab(cx),
        HotkeyAction::MoveTabLeft => {
            dismiss_tab_transients(app, cx);
            move_active_tab(app, -1, cx);
        }
        HotkeyAction::MoveTabRight => {
            dismiss_tab_transients(app, cx);
            move_active_tab(app, 1, cx);
        }
        HotkeyAction::OpenTabMenu => {
            app.tab_bar_renderer
                .update(cx, |renderer, cx| renderer.open_active_tab_menu(window, cx));
            cx.notify();
        }
        HotkeyAction::RenameTab => {
            dismiss_tab_transients(app, cx);
            app.tab_bar_renderer
                .update(cx, |renderer, cx| renderer.rename_active_tab(window, cx));
            cx.notify();
        }
        HotkeyAction::SelectTab1 => app.select_tab_by_number(1, cx),
        HotkeyAction::SelectTab2 => app.select_tab_by_number(2, cx),
        HotkeyAction::SelectTab3 => app.select_tab_by_number(3, cx),
        HotkeyAction::SelectTab4 => app.select_tab_by_number(4, cx),
        HotkeyAction::SelectTab5 => app.select_tab_by_number(5, cx),
        HotkeyAction::SelectTab6 => app.select_tab_by_number(6, cx),
        HotkeyAction::SelectTab7 => app.select_tab_by_number(7, cx),
        HotkeyAction::SelectTab8 => app.select_tab_by_number(8, cx),
        HotkeyAction::SelectTab9 => app.select_tab_by_number(9, cx),
        HotkeyAction::PopupActiveSplit => {
            app.popup_active_split(cx);
        }
        HotkeyAction::PopupActiveTab => {
            app.popup_active_tab(cx);
        }
        HotkeyAction::ZoomIn => app.zoom_in(cx),
        HotkeyAction::ZoomOut => app.zoom_out(cx),
        HotkeyAction::ResetZoom => app.reset_zoom(cx),
        HotkeyAction::ToggleTabBarVisibility => app.toggle_tab_bar_visibility(cx),
        HotkeyAction::ToggleLiveTabsOnly => app.toggle_live_tabs_only(cx),
        HotkeyAction::ToggleSidebar => {
            app.app_state.update(cx, |state, cx| {
                state.toggle_activity_panel(cx);
            });
        }

        HotkeyAction::ChangeChannel => {
            if !app.start_change_active_split_channel(cx) {
                warn_no_active_channel(app, cx, "Change channel");
            }
        }
        HotkeyAction::FocusInput => app.focus_input(window, cx),
        HotkeyAction::SendMessage => app.send_message(cx),
        HotkeyAction::SendMessageKeepInput => app.send_message_keep_input(cx),
        HotkeyAction::OpenEmotePicker => {
            let split_path = app.split_layout.read(cx).get_active_split_path().to_vec();
            app.toggle_emote_picker(&split_path, window, cx);
        }
        HotkeyAction::ClearChat => {
            if let Some(ch) = get_active_channel(app, cx) {
                backend::clear_chat(&ch);
                cx.notify();
            }
        }
        HotkeyAction::CopyMessage => {
            app.copy_selection(cx);
        }
        HotkeyAction::CreateClip => {
            create_clip_for_active_channel(app, cx);
        }
        HotkeyAction::OpenChannelInBrowser => {
            open_channel_in_browser_for_active_channel(app, cx);
        }
        HotkeyAction::OpenChannelInStreamlink => {
            open_streamlink_for_active_channel(app, cx);
        }
        HotkeyAction::OpenChannelInCustomPlayer => {
            open_custom_player_for_active_channel(app, cx);
        }
        HotkeyAction::OpenModViewInBrowser => {
            open_mod_view_in_browser_for_active_channel(app, cx);
        }
        HotkeyAction::OpenViewerList => {
            open_viewer_list_for_active_channel(app, cx);
        }
        HotkeyAction::ScrollUp => {
            app.scroll_active_chat_by_page(-1, cx);
        }
        HotkeyAction::ScrollDown => {
            app.scroll_active_chat_by_page(1, cx);
        }
        HotkeyAction::ScrollToTop => {
            app.scroll_active_chat_to_top(cx);
        }
        HotkeyAction::ScrollToBottom => {
            app.scroll_active_chat_to_bottom(cx);
        }
        HotkeyAction::ReconnectActiveChannel => {
            app.reconnect_active_channel(cx);
        }
        HotkeyAction::EnableHighlightSounds => {
            set_highlight_sounds_for_active_channel(app, false, cx);
        }
        HotkeyAction::DisableHighlightSounds => {
            set_highlight_sounds_for_active_channel(app, true, cx);
        }
        HotkeyAction::ToggleHighlightSounds => {
            toggle_highlight_sounds_for_active_channel(app, cx);
        }
        HotkeyAction::OpenSubscriptionPage => {
            open_subscription_page_for_active_channel(app, cx);
        }
        HotkeyAction::FocusSplitLeft => {
            app.focus_adjacent_split(AddSplitDirection::Left, cx);
        }
        HotkeyAction::FocusSplitRight => {
            app.focus_adjacent_split(AddSplitDirection::Right, cx);
        }
        HotkeyAction::FocusSplitUp => {
            app.focus_adjacent_split(AddSplitDirection::Top, cx);
        }
        HotkeyAction::FocusSplitDown => {
            app.focus_adjacent_split(AddSplitDirection::Bottom, cx);
        }
        HotkeyAction::SearchPreviousResult => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    if modal.is_docked_open() {
                        modal.select_previous_result_and_jump();
                    } else {
                        modal.select_previous_result();
                    }
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchNextResult => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    if modal.is_docked_open() {
                        modal.select_next_result_and_jump();
                    } else {
                        modal.select_next_result();
                    }
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchOpenSelectedResult => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    if modal.is_docked_open() {
                        modal.submit_query(cx);
                    } else {
                        modal.submit_or_open_selected(window, cx);
                    }
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchPreviousPage => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    modal.select_previous_page();
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchNextPage => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    modal.select_next_page();
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchFirstResult => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    modal.select_first_result();
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchLastResult => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    modal.select_last_result();
                    cx.notify();
                });
            cx.notify();
        }
        HotkeyAction::SearchClearQuery => {
            app.overlay_manager
                .read(cx)
                .search_modal
                .clone()
                .update(cx, |modal, cx| {
                    modal.clear_query(window, cx);
                    cx.notify();
                });
            cx.notify();
        }

        HotkeyAction::ToggleModMode => {
            app.toggle_active_split_mod_mode(cx);
            cx.notify();
        }
        HotkeyAction::ToggleModLog => {
            app.overlay_manager.update(cx, |mgr, cx| {
                mgr.toggle_mod_log(cx);
            });
            cx.notify();
        }
        HotkeyAction::QuickTimeout30s => {
            if let Some((username, channel)) =
                app.overlay_manager.read(cx).get_moderation_target(cx)
            {
                backend::timeout_user(&channel, &username, 30, None);
                app.overlay_manager.update(cx, |mgr, cx| {
                    mgr.toast_manager.update(cx, |m, cx| {
                        m.success(format!("Timed out {} for 30s", username));
                        cx.notify();
                    });
                });
            }
        }
        HotkeyAction::QuickTimeout10m => {
            if let Some((username, channel)) =
                app.overlay_manager.read(cx).get_moderation_target(cx)
            {
                backend::timeout_user(&channel, &username, 600, None);
                app.overlay_manager.update(cx, |mgr, cx| {
                    mgr.toast_manager.update(cx, |m, cx| {
                        m.success(format!("Timed out {} for 10m", username));
                        cx.notify();
                    });
                });
            }
        }
        HotkeyAction::QuickBan => {
            if let Some((username, channel)) =
                app.overlay_manager.read(cx).get_moderation_target(cx)
            {
                backend::ban_user(&channel, &username, None);
                app.overlay_manager.update(cx, |mgr, cx| {
                    mgr.toast_manager.update(cx, |m, cx| {
                        m.success(format!("Banned {}", username));
                        cx.notify();
                    });
                });
            }
        }

        HotkeyAction::ClearMentions => {
            backend::clear_global_mentions();
            app.app_state.update(cx, |state, cx| {
                state.refresh_sidebar_caches();
                cx.notify();
            });
            cx.notify();
        }
        HotkeyAction::MarkWhispersRead => {
            backend::mark_all_whisper_threads_read();
            app.app_state.update(cx, |state, cx| {
                state.refresh_sidebar_caches();
                cx.notify();
            });
            cx.notify();
        }
        HotkeyAction::CloseThreadViewer => {
            app.overlay_manager.update(cx, |mgr, cx| {
                mgr.close_thread_viewer(cx);
            });
            app.refocus_active_input(cx);
            cx.notify();
        }
        HotkeyAction::ReloadEmotes => {
            if let Some(ch) = get_active_channel(app, cx) {
                backend::bridge::reload_channel_emotes(&ch);
                app.overlay_manager.update(cx, |mgr, cx| {
                    mgr.toast_manager.update(cx, |m, cx| {
                        m.info("Reloading emotes…");
                        cx.notify();
                    });
                });
                cx.notify();
            }
        }
        HotkeyAction::ToggleFullscreen => {
            window.toggle_fullscreen();
        }
        HotkeyAction::OpenDebugPopup => {
            let context_label = get_active_channel(app, cx)
                .map(|channel| channel.to_string())
                .unwrap_or_else(|| "Active split".to_string());
            cx.spawn(async move |_this, cx| {
                cx.update(|cx| {
                    crate::components::debug_popup::toggle_debug_popup(context_label, cx);
                });
            })
            .detach();
        }
        HotkeyAction::ToggleCheatSheet => {
            app.overlay_manager.update(cx, |mgr, cx| {
                let is_open = mgr.hotkey_cheat_sheet.read(cx).is_open();
                if is_open {
                    mgr.close_cheat_sheet(cx);
                } else {
                    mgr.open_cheat_sheet(cx);
                }
            });
            cx.notify();
        }
        HotkeyAction::ToggleMetrics => {
            app.overlay_manager.update(cx, |mgr, cx| {
                mgr.toggle_metrics(cx);
            });
            cx.notify();
        }
        HotkeyAction::ToggleStreamerOverlay => {
            let Some(channel) = get_active_channel(app, cx) else {
                warn_no_active_channel(app, cx, "Toggle overlay window");
                return;
            };

            cx.spawn(async move |_this, cx| {
                cx.update(|cx| {
                    crate::components::overlay::toggle_overlay_window(channel, cx);
                });
            })
            .detach();
        }
        HotkeyAction::ReopenClosedSplit => {
            if !app.reopen_last_closed_split(cx) {
                warn_no_closed_split(app, cx);
            }
        }
        HotkeyAction::ReopenClosedTab => {
            if !app.reopen_closed_tab(cx) {
                app.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(
                        crate::components::overlays::Toast::warning("No closed tab")
                            .with_detail("Close a tab before using Reopen Closed Tab."),
                        cx,
                    );
                });
                cx.notify();
            }
        }
        HotkeyAction::ToggleLocalR9K => {
            let similarity = backend::similarity::get_similarity_settings();
            let enable_hide = !(similarity.enabled && similarity.hide_similar);

            if enable_hide {
                if !similarity.enabled {
                    backend::similarity::set_similarity_enabled(true);
                }
                backend::similarity::set_similarity_hide_similar(true);
            } else {
                backend::similarity::set_similarity_hide_similar(false);
            }

            backend::settings::broadcast_settings_event(
                backend::settings::SettingsEvent::CosmeticSettingsChanged,
            );
            let status = if enable_hide { "enabled" } else { "disabled" };
            app.overlay_manager.update(cx, |mgr, cx| {
                mgr.toast_manager.update(cx, |toasts, cx| {
                    toasts.success(format!("Local R9K {status}"));
                    cx.notify();
                });
            });
            cx.notify();
        }
        HotkeyAction::ToggleTransparency => {
            let enabled = backend::settings::get_window_transparent();
            let new_state = !enabled;
            backend::settings::set_window_transparent(new_state);
            app.sync_main_window_appearance(window);
            app.overlay_manager.update(cx, |mgr, cx| {
                mgr.toast_manager.update(cx, |m, cx| {
                    let status = if new_state { "enabled" } else { "disabled" };
                    m.info(format!("Window Transparency {}", status));
                    cx.notify();
                });
            });
            cx.notify();
        }
        HotkeyAction::ToggleAlwaysOnTop => {
            let enabled = backend::settings::get_always_on_top();
            let new_state = !enabled;
            backend::settings::set_always_on_top(new_state);
            let applied_live = apply_main_window_always_on_top(window, new_state);
            crate::components::ui::refresh_windows_for_always_on_top(cx);
            app.overlay_manager.update(cx, |mgr, cx| {
                mgr.toast_manager.update(cx, |m, cx| {
                    let status = if new_state { "enabled" } else { "disabled" };
                    let message = if applied_live {
                        format!("Always on Top {}", status)
                    } else {
                        format!("Always on Top {} (applies when Echo is reopened)", status)
                    };
                    m.info(message);
                    cx.notify();
                });
            });
            cx.notify();
        }
        HotkeyAction::QuickSwitcher => {
            let switcher = app.overlay_manager.read(cx).quick_switcher.clone();
            let (is_open, live_only) = {
                let switcher = switcher.read(cx);
                (switcher.is_open, switcher.is_live_only())
            };
            if is_open && !live_only {
                app.overlay_manager
                    .update(cx, |mgr, cx| mgr.close_quick_switcher(cx));
            } else {
                // From the live-only switcher, Ctrl+K widens to every channel.
                let channels = quick_switch_items(app, cx);
                app.overlay_manager.update(cx, |mgr, cx| {
                    mgr.open_quick_switcher(
                        channels,
                        crate::components::overlays::QuickSwitchMode::All,
                        cx,
                    );
                });
                switcher.update(cx, |qs, cx| qs.focus_now(window, cx));
            }
            cx.notify();
        }
    }
}

/// Every open split as a quick-switcher row, with its retained mention count.
pub(crate) fn quick_switch_items(
    app: &EchoApp,
    cx: &App,
) -> Vec<crate::components::overlays::QuickSwitchItem> {
    let app_state = app.app_state.read(cx);
    let mention_counts = backend::get_global_mentions(100).iter().fold(
        std::collections::HashMap::<String, usize>::new(),
        |mut counts, (channel, _)| {
            let channel = channel.trim().trim_start_matches('#').to_lowercase();
            if !channel.is_empty() {
                *counts.entry(channel).or_default() += 1;
            }
            counts
        },
    );
    app_state
        .tabs
        .iter()
        .enumerate()
        .flat_map(|(tab_index, tab)| {
            let mention_counts = &mention_counts;
            tab.layout
                .collect_channel_path_keys(&[])
                .into_iter()
                .filter_map(move |key| {
                    let (channel, path_str) = key.split_once(':')?;
                    if crate::state::is_placeholder(channel) {
                        return None;
                    }

                    let split_path = if path_str.is_empty() {
                        Vec::new()
                    } else {
                        path_str
                            .split('-')
                            .map(|part| part.parse::<usize>().ok())
                            .collect::<Option<Vec<_>>>()?
                    };

                    let channel = channel.to_string();
                    let is_live = app_state
                        .stream_info
                        .get(&channel)
                        .and_then(|info| info.as_ref())
                        .is_some_and(|info| info.is_live);
                    let mention_count = mention_counts
                        .get(channel.trim_start_matches('#'))
                        .copied()
                        .unwrap_or_default();
                    let location_label = if split_path.is_empty() {
                        format!("Tab {}", tab_index + 1)
                    } else {
                        let path_label = split_path
                            .iter()
                            .map(|idx| (idx + 1).to_string())
                            .collect::<Vec<_>>()
                            .join(".");
                        format!("Tab {} • Split {}", tab_index + 1, path_label)
                    };

                    Some(crate::components::overlays::QuickSwitchItem {
                        channel,
                        tab_index,
                        split_path,
                        location_label,
                        is_live,
                        mention_count,
                    })
                })
        })
        .collect()
}

/// Get the active channel from the current split (helper for hotkey dispatch)
fn get_active_channel(app: &EchoApp, cx: &Context<EchoApp>) -> Option<String> {
    let active_path = app.split_layout.read(cx).get_active_split_path();
    app.app_state
        .read(cx)
        .active_tab()
        .layout
        .get_channel_at_path(active_path)
}

fn get_normalized_active_channel(app: &EchoApp, cx: &Context<EchoApp>) -> Option<String> {
    get_active_channel(app, cx).and_then(|channel| normalize_twitch_channel(&channel))
}

fn normalize_twitch_channel(channel: &str) -> Option<String> {
    let channel = channel.trim().trim_start_matches('#').to_lowercase();
    (!channel.is_empty()).then_some(channel)
}

fn warn_no_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>, action: &'static str) {
    app.overlay_manager.update(cx, |manager, cx| {
        manager.push_toast(
            crate::components::overlays::Toast::warning("No active channel")
                .with_detail(format!("Open a Twitch channel before using {action}.")),
            cx,
        );
    });
    cx.notify();
}

fn warn_no_closed_split(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    app.overlay_manager.update(cx, |manager, cx| {
        manager.push_toast(
            crate::components::overlays::Toast::warning("No closed split")
                .with_detail("Close a split before using Reopen closed split."),
            cx,
        );
    });
    cx.notify();
}

fn set_highlight_sounds_for_active_channel(
    app: &mut EchoApp,
    muted: bool,
    cx: &mut Context<EchoApp>,
) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "channel notification hotkeys");
        return;
    };

    crate::backend::sounds::SOUND_STORE.set_channel_muted(&channel, muted);
    let status = if muted { "muted" } else { "unmuted" };
    app.overlay_manager.update(cx, |manager, cx| {
        manager.toast_manager.update(cx, |toasts, cx| {
            toasts.success(format!(
                "Sounds and desktop notifications {status} for #{channel}"
            ));
            cx.notify();
        });
    });
    cx.notify();
}

fn toggle_highlight_sounds_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "channel notification hotkeys");
        return;
    };

    let muted = crate::backend::sounds::SOUND_STORE.toggle_channel_muted(&channel);
    let status = if muted { "muted" } else { "unmuted" };
    app.overlay_manager.update(cx, |manager, cx| {
        manager.toast_manager.update(cx, |toasts, cx| {
            toasts.success(format!(
                "Sounds and desktop notifications {status} for #{channel}"
            ));
            cx.notify();
        });
    });
    cx.notify();
}

fn open_subscription_page_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Open subscription page");
        return;
    };

    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
        url: format!("https://www.twitch.tv/subs/{channel}"),
        mode: backend::ui_actions::LinkOpenMode::Configured,
    });
    cx.notify();
}

fn open_streamlink_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Open channel in Streamlink");
        return;
    };

    let Some(launched_quality) = backend::commands::launch_stream_player(
        &channel,
        backend::commands::StreamPlayer::Streamlink,
    ) else {
        return;
    };

    app.overlay_manager.update(cx, |manager, cx| {
        manager.push_toast(
            crate::components::overlays::Toast::success(format!(
                "Opening #{channel} with Streamlink"
            ))
            .with_detail(format!("Quality: {launched_quality}")),
            cx,
        );
    });
    cx.notify();
}

fn open_custom_player_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Open channel in custom player");
        return;
    };
    let Some(player_path) = backend::settings::get_use_custom_player()
        .then(backend::settings::get_custom_player_path)
        .flatten()
        .filter(|path| !path.trim().is_empty())
    else {
        app.overlay_manager.update(cx, |manager, cx| {
            manager.push_toast(
                crate::components::overlays::Toast::warning("Custom player is not configured")
                    .with_detail("Set a custom player executable in General > Players."),
                cx,
            );
        });
        return;
    };

    if backend::commands::launch_stream_player(
        &channel,
        backend::commands::StreamPlayer::Custom(player_path),
    )
    .is_none()
    {
        return;
    }

    app.overlay_manager.update(cx, |manager, cx| {
        manager.push_toast(
            crate::components::overlays::Toast::success(format!(
                "Opening #{channel} with custom player"
            )),
            cx,
        );
    });
    cx.notify();
}

fn create_clip_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Create clip");
        return;
    };

    backend::send_message_or_command_async(channel, "/clip".to_string());
}

fn open_channel_in_browser_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Open channel in browser");
        return;
    };

    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
        url: format!("https://www.twitch.tv/{channel}"),
        mode: backend::ui_actions::LinkOpenMode::Configured,
    });
    cx.notify();
}

fn open_mod_view_in_browser_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Open moderator view in browser");
        return;
    };

    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
        url: format!("https://www.twitch.tv/moderator/{channel}"),
        mode: backend::ui_actions::LinkOpenMode::Configured,
    });
    cx.notify();
}

fn open_viewer_list_for_active_channel(app: &mut EchoApp, cx: &mut Context<EchoApp>) {
    let Some(channel) = get_normalized_active_channel(app, cx) else {
        warn_no_active_channel(app, cx, "Open viewer list");
        return;
    };

    backend::open_chatter_list_async(channel);
    cx.notify();
}
