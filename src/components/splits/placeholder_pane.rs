//! Placeholder pane rendering for empty split panes.

use crate::EchoApp;
use crate::backend::bridge::followed_live::{self, FollowedLiveStatus, FollowedLiveStream};
use crate::components::chat::chat_pane::render_collapsed_header_capsule;
use crate::components::chat::chat_ui::format_viewer_count;
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Enter, Input, InputEvent, InputState};
use crate::managers::input_manager::{ChannelSuggestion, ChooserLiveRows, PlaceholderInput};
use crate::state::{SplitId, SplitNode};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::HashSet;
use std::rc::Rc;

impl EchoApp {
    fn channel_chooser_path(&self, split_id: SplitId, cx: &App) -> Option<Vec<usize>> {
        let tab = self.app_state.read(cx).active_tab();
        let path = tab.split_path_for_id(split_id)?;
        tab.layout
            .channel_at_path(&path)
            .filter(|channel| crate::state::is_placeholder(channel))?;
        Some(path)
    }

    pub(crate) fn ensure_channel_chooser(
        &mut self,
        split_id: SplitId,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        if let Some(chooser) = self
            .chat_input_renderer
            .read(cx)
            .input_manager
            .placeholder_inputs
            .get(&split_id)
        {
            return Some(chooser.input.clone());
        }
        let input = if let Some(window) = window {
            cx.new(|cx| InputState::new(window, cx).with_placeholder("Enter channel…"))
        } else {
            cx.with_window(cx.entity_id(), |window, app| {
                app.new(|cx| InputState::new(window, cx).with_placeholder("Enter channel…"))
            })?
        };
        let subscription = cx.subscribe(&input, move |this, _input, event, cx| {
            let Some(path) = this.channel_chooser_path(split_id, cx) else {
                return;
            };
            match event {
                InputEvent::PressEnter => this.submit_channel_chooser(split_id, cx),
                InputEvent::Change => {
                    this.chat_input_renderer.update(cx, |renderer, _| {
                        if let Some(chooser) =
                            renderer.input_manager.placeholder_inputs.get_mut(&split_id)
                        {
                            chooser.selected = None;
                        }
                    });
                    cx.notify();
                }
                InputEvent::Focus => {
                    followed_live::refresh_if_stale();
                    if this.split_layout.read(cx).get_active_split_path() != path {
                        this.set_active_split(path, cx);
                    }
                    this.chat_input_renderer.update(cx, |renderer, _| {
                        renderer.input_manager.focused_placeholder = Some(split_id);
                    });
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.chat_input_renderer.update(cx, |renderer, _| {
                        if renderer.input_manager.focused_placeholder == Some(split_id) {
                            renderer.input_manager.focused_placeholder = None;
                        }
                    });
                    cx.notify();
                }
            }
        });
        // Input is RenderOnce inside the app, so editor/caret notifies must dirty its host.
        let observer = cx.observe(&input, |_, _, cx| cx.notify());
        self.chat_input_renderer.update(cx, |renderer, _| {
            renderer.input_manager.placeholder_inputs.insert(
                split_id,
                PlaceholderInput {
                    input: input.clone(),
                    query: String::new(),
                    suggestions: Vec::new(),
                    selected: None,
                    scroll: ScrollHandle::new(),
                    live: ChooserLiveRows::default(),
                    live_scroll: ScrollHandle::new(),
                    _subscriptions: std::rc::Rc::new(vec![subscription, observer]),
                },
            );
        });
        Some(input)
    }

    pub(crate) fn submit_channel_chooser(&mut self, split_id: SplitId, cx: &mut Context<Self>) {
        let Some(path) = self.channel_chooser_path(split_id, cx) else {
            return;
        };
        let value = {
            let renderer = self.chat_input_renderer.read(cx);
            let Some(chooser) = renderer.input_manager.placeholder_inputs.get(&split_id) else {
                return;
            };
            let current = chooser.input.read(cx).value();
            chooser
                .selected
                .filter(|_| chooser.query == current)
                .and_then(|index| chooser.suggestions.get(index))
                .map(ChannelSuggestion::login)
                .unwrap_or(current)
                .to_string()
        };
        if crate::utils::channel_input::normalize_channel_input(&value).is_err() {
            cx.notify();
            return;
        }
        self.set_active_split(path.clone(), cx);
        self.set_channel_for_path(path, value, cx);
    }

    /// Focus `login` where it is already open, leaving the chooser's split
    /// empty.
    fn switch_to_open_channel(&mut self, login: &str, cx: &mut Context<Self>) -> bool {
        let Some((tab_index, path, _)) =
            crate::app::find_open_tab_channel_target(self.app_state.read(cx), login)
        else {
            return false;
        };
        self.activate_tab_split(tab_index, path, cx);
        self.refocus_active_input(cx);
        true
    }

    /// Shift+Enter on a Live now row whose channel is open elsewhere: the
    /// keyboard path to that row's "Switch there".
    fn switch_to_selected_live_channel(
        &mut self,
        split_id: SplitId,
        cx: &mut Context<Self>,
    ) -> bool {
        let login = {
            let renderer = self.chat_input_renderer.read(cx);
            let Some(chooser) = renderer.input_manager.placeholder_inputs.get(&split_id) else {
                return false;
            };
            let current = chooser.input.read(cx).value();
            match chooser
                .selected
                .filter(|_| chooser.query == current)
                .and_then(|index| chooser.suggestions.get(index))
            {
                Some(ChannelSuggestion::Live(login)) => login.clone(),
                _ => return false,
            }
        };
        self.switch_to_open_channel(&login, cx)
    }

    /// Chooser keys handled before the input sees them: swallow held Enter,
    /// Escape restores a pending channel change or leaves the chooser, and
    /// plain Up/Down move the suggestion highlight. IME composition passes
    /// through untouched.
    fn handle_channel_chooser_key_down(
        &mut self,
        split_id: SplitId,
        input: &Entity<InputState>,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input.read(cx).marked_range().is_some() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        if key == "enter" && event.is_held {
            cx.stop_propagation();
        } else if key == "escape" {
            if self.restore_pending_channel_change(split_id, cx) {
                self.focus_input(window, cx);
            } else {
                self.chat_input_renderer.update(cx, |renderer, _| {
                    renderer.input_manager.focused_placeholder = None;
                });
                window.focus(&self.focus_handle(cx), cx);
            }
            cx.stop_propagation();
            cx.notify();
        } else if matches!(key, "up" | "down")
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
            && !modifiers.shift
        {
            self.chat_input_renderer.update(cx, |renderer, _| {
                if let Some(chooser) = renderer.input_manager.placeholder_inputs.get_mut(&split_id)
                {
                    step_chooser_selection(chooser, key == "up");
                }
            });
            cx.stop_propagation();
            cx.notify();
        }
    }
}

/// Move the chooser highlight one row, wrapping at either end, and scroll the
/// newly highlighted row into view.
fn step_chooser_selection(chooser: &mut PlaceholderInput, up: bool) {
    let count = chooser.suggestions.len();
    if count == 0 {
        return;
    }
    let index = match (chooser.selected, up) {
        (Some(index), true) => (index + count - 1) % count,
        (Some(index), false) => (index + 1) % count,
        (None, true) => count - 1,
        (None, false) => 0,
    };
    chooser.selected = Some(index);
    let recent = chooser
        .suggestions
        .iter()
        .take_while(|row| matches!(row, ChannelSuggestion::Recent(_)))
        .count();
    match index.checked_sub(recent) {
        None => chooser.scroll.scroll_to_item(index),
        // Live rows sit in the Live now group, below its header.
        Some(pos) => crate::components::overlays::scroll_grouped_row_into_view(
            &chooser.scroll,
            &chooser.live_scroll,
            pos + 1,
        ),
    }
}

fn local_channel_suggestions(
    app: &EchoApp,
    query: &str,
    skip: impl Fn(&str) -> bool,
    cx: &App,
) -> Vec<String> {
    let query = query
        .trim()
        .trim_start_matches(['#', '@'])
        .to_ascii_lowercase();
    let mut channels = crate::backend::settings::get_recent_channels();
    if !query.is_empty() {
        for tab in app.app_state.read(cx).tabs.iter() {
            channels.extend(tab.channels());
        }
    }
    let mut seen = HashSet::new();
    let mut suggestions: Vec<_> = channels
        .into_iter()
        .filter_map(|channel| {
            let login = crate::utils::channel_input::normalize_channel_input(&channel).ok()?;
            (crate::utils::channel_input::is_valid_twitch_login(&login)
                && login.contains(&query)
                && !skip(&login)
                && seen.insert(login.clone()))
            .then_some(login)
        })
        .collect();
    suggestions.sort_by_key(|login| !login.starts_with(&query));
    suggestions.truncate(8);
    suggestions
}

const CHOOSER_LIVE_ROWS: usize = 8;

/// The chooser's Live now rows for the typed query.
fn chooser_live_rows(query: &str, streamer_mode: bool) -> ChooserLiveRows {
    // Fixture panes keep their recorded baselines, and an anonymous user
    // would otherwise carry a sign-in line in every empty split.
    if crate::bench::ui_fixture::is_enabled() {
        return ChooserLiveRows::default();
    }
    let snapshot = followed_live::snapshot();
    if snapshot.status == FollowedLiveStatus::SignedOut {
        return ChooserLiveRows::default();
    }
    let query = crate::utils::channel_input::normalize_channel_input(query)
        .unwrap_or_else(|_| query.trim().to_string());
    let mut matches = if streamer_mode {
        Vec::new()
    } else {
        echo_core::followed_live::matching_streams(&snapshot.streams, &query, |_| false)
    };
    let more = matches.len().saturating_sub(CHOOSER_LIVE_ROWS);
    matches.truncate(CHOOSER_LIVE_ROWS);
    let rows: Vec<FollowedLiveStream> = matches
        .into_iter()
        .map(|index| snapshot.streams[index].clone())
        .collect();
    let query_active = !query.is_empty();
    // While a channel name is being typed, a section with nothing matching
    // only gets in the way; a missing permission still says so.
    let notice = echo_core::followed_live::notice(
        snapshot.status,
        streamer_mode,
        snapshot.streams.len(),
        rows.len(),
        query_active,
    )
    .filter(|_| {
        streamer_mode || !query_active || snapshot.status == FollowedLiveStatus::NeedsPermission
    });
    ChooserLiveRows {
        key: None,
        rows,
        more,
        notice,
    }
}

fn live_now_header() -> Div {
    div()
        .flex_shrink_0()
        .px_3()
        .pt_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(colors::text_muted())
        .child("Live now")
}

/// A bare leaf of the pane's exact size, for a split that has nothing to show.
fn empty_pane(pane_width: f32, pane_height: f32) -> AnyElement {
    div()
        .w(px(pane_width))
        .h(px(pane_height))
        .flex_shrink_0()
        .into_any_element()
}

/// Exact leaf sizes prevent empty panes from squeezing their siblings.
pub fn render_placeholder_pane(
    app: &mut EchoApp,
    path: Vec<usize>,
    pane_width: f32,
    pane_height: f32,
    window: &mut Window,
    cx: &mut Context<EchoApp>,
) -> AnyElement {
    let pane_width = pane_width.max(0.0);
    let pane_height = pane_height.max(0.0);
    let Some(split_id) = app
        .app_state
        .read(cx)
        .active_tab()
        .layout
        .split_id_at_path(&path)
    else {
        return empty_pane(pane_width, pane_height);
    };
    let is_active_split = app.split_layout.read(cx).get_active_split_path() == path;
    let header_collapsed = matches!(
        app.app_state
            .read(cx)
            .active_tab()
            .layout
            .node_at_path(&path),
        Some(SplitNode::Leaf {
            header_collapsed: true,
            ..
        })
    );
    let created = !app
        .chat_input_renderer
        .read(cx)
        .input_manager
        .placeholder_inputs
        .contains_key(&split_id);
    if created {
        followed_live::refresh_if_stale();
    }
    let Some(input) = app.ensure_channel_chooser(split_id, Some(window), cx) else {
        return empty_pane(pane_width, pane_height);
    };
    let focus = input.read(cx).focus_handle().clone();
    if created
        && is_active_split
        && !app.overlay_manager.read(cx).text_overlay_open(cx)
        && !app
            .tab_bar_renderer
            .read(cx)
            .is_rename_input_focused(window, cx)
    {
        window.focus(&focus, cx);
    }
    let current = input.read(cx).value().to_string();
    let is_empty = current.is_empty();
    let helper = match crate::utils::channel_input::normalize_channel_input(&current) {
        Err(error) => Some((error.message().to_string(), colors::error())),
        Ok(login) if !login.is_empty() && login != current.trim() => {
            Some((format!("Joins #{login}"), colors::text_muted()))
        }
        Ok(_) => None,
    };
    let streamer_mode = crate::backend::streamer_mode::is_streamer_mode_active_cached();
    let live_key = (followed_live::version(), current.clone(), streamer_mode);
    let Some(live) = app.chat_input_renderer.update(cx, |renderer, _| {
        let chooser = renderer
            .input_manager
            .placeholder_inputs
            .get_mut(&split_id)?;
        if chooser.live.key.as_ref() != Some(&live_key) {
            chooser.live = chooser_live_rows(&current, streamer_mode);
            chooser.live.key = Some(live_key);
        }
        Some(chooser.live.clone())
    }) else {
        return empty_pane(pane_width, pane_height);
    };
    let hide_viewer_count = crate::backend::streamer_mode::should_hide_viewer_count();
    let more_hint = echo_core::followed_live::more_hint(live.more);
    // A live channel is listed once, under Live now.
    let live_logins: HashSet<&str> = live
        .rows
        .iter()
        .map(|stream| stream.login.as_str())
        .collect();
    let suggestions =
        local_channel_suggestions(app, &current, |login| live_logins.contains(login), cx);
    let rows: Vec<ChannelSuggestion> = suggestions
        .iter()
        .cloned()
        .map(ChannelSuggestion::Recent)
        .chain(
            live.rows
                .iter()
                .map(|stream| ChannelSuggestion::Live(stream.login.clone())),
        )
        .collect();
    let Some((selected, suggestions_scroll, live_scroll)) =
        app.chat_input_renderer.update(cx, |renderer, _| {
            let chooser = renderer
                .input_manager
                .placeholder_inputs
                .get_mut(&split_id)?;
            if chooser.query != current {
                chooser.selected = None;
                chooser.scroll.scroll_to_item(0);
            } else if chooser.suggestions != rows {
                // A refresh keeps the highlighted channel highlighted.
                chooser.selected = echo_core::followed_live::carry_selection(
                    &chooser.suggestions,
                    chooser.selected,
                    &rows,
                );
            }
            chooser.query.clone_from(&current);
            chooser.suggestions.clone_from(&rows);
            Some((
                chooser.selected,
                chooser.scroll.clone(),
                chooser.live_scroll.clone(),
            ))
        })
    else {
        return empty_pane(pane_width, pane_height);
    };
    let open_elsewhere: Vec<bool> = {
        let app_state = app.app_state.read(cx);
        live.rows
            .iter()
            .map(|stream| {
                crate::app::find_open_tab_channel_target(app_state, &stream.login).is_some()
            })
            .collect()
    };
    crate::bench::ui_fixture::publish_feedback_channel_chooser(
        crate::bench::ui_fixture::FeedbackChannelChooserSnapshot {
            split_id: split_id.as_u64(),
            input_focused: focus.is_focused(window),
            query_empty: is_empty,
            placeholder_present: is_empty,
        },
    );
    let path_for_watching = path.clone();
    let bounds_path_key = crate::managers::SplitLayoutManager::path_key(&path);
    let bounds_entity = cx.entity().clone();
    let keyboard_input = input.clone();
    let enter_input = input.clone();
    div()
        .id(("placeholder-pane", split_id.as_u64() as usize))
        .w(px(pane_width))
        .h(px(pane_height))
        .min_w_0()
        .min_h_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .relative()
        .overflow_hidden()
        .when(!header_collapsed, |pane| {
            pane.child({
                let drag_mode = app.split_layout.read(cx).is_drag_mode_active();
                let layout_locked = app.app_state.read(cx).active_tab().is_locked;
                let overlay_mgr = app.overlay_manager.clone();
                let app_entity = cx.entity().clone();
                app.chat_header_renderer.update(cx, |renderer, header_cx| {
                    renderer.render_fast(
                        "Add channel",
                        &path,
                        pane_width,
                        None,
                        true,
                        drag_mode,
                        false,
                        crate::backend::settings::get_compact_split_header(),
                        is_active_split,
                        layout_locked,
                        false,
                        None,
                        app_entity,
                        overlay_mgr,
                        header_cx,
                    )
                })
            })
        })
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .id(("channel-chooser-scroll", split_id.as_u64() as usize))
                .overflow_y_scroll()
                .child(
                    div()
                        .id(("channel-chooser", split_id.as_u64()))
                        .role(Role::Group)
                        .aria_label("Twitch channel chooser")
                        .aria_description("Type a channel, use Up and Down to choose a suggestion, then Enter to join.")
                        .w(px(360.0f32.min((pane_width - 32.0).max(120.0))))
                        // A fixed top, not vertical centering: the field must not
                        // move under the caret as the suggestion list filters.
                        .mt(px((pane_height * 0.2).clamp(12.0, 160.0)))
                        .flex_shrink_0()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child("Choose a channel for this split"),
                        )
                        .child(
                            div()
                                .id(("placeholder-input", split_id.as_u64() as usize))
                                .w_full()
                                .capture_key_down(cx.listener(
                                    move |this, event: &KeyDownEvent, window, cx| {
                                        this.handle_channel_chooser_key_down(
                                            split_id,
                                            &keyboard_input,
                                            event,
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                                .capture_action(cx.listener(move |this, action: &Enter, _window, cx| {
                                    if action.secondary
                                        && enter_input.read(cx).marked_range().is_none()
                                        && this.switch_to_selected_live_channel(split_id, cx)
                                    {
                                        cx.stop_propagation();
                                    }
                                }))
                                .child(Input::new(&input).accessibility_label("Twitch channel")),
                        )
                        .children(helper.map(|(text, color)| {
                            div()
                                .mt(px(-Spacing::XS_PLUS))
                                .text_xs()
                                .text_color(color)
                                .child(text)
                        }))
                        .when(!rows.is_empty(), |column| {
                            column.child(render_chooser_suggestions(
                                ChooserSuggestionList {
                                    split_id,
                                    path: &path,
                                    row_count: rows.len(),
                                    recent: suggestions,
                                    live: &live,
                                    open_elsewhere: &open_elsewhere,
                                    selected,
                                    hide_viewer_count,
                                    more_hint,
                                    pane_height,
                                    scroll: &suggestions_scroll,
                                    live_scroll: &live_scroll,
                                },
                                cx,
                            ))
                        })
                        .children(live.notice.filter(|_| live.rows.is_empty()).map(|notice| {
                            div()
                                .id(("channel-live-notice", split_id.as_u64() as usize))
                                .role(Role::Group)
                                .aria_label("Live now")
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .when(!streamer_mode, |section| section.child(live_now_header()))
                                .child(
                                    div()
                                        .px_3()
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .child(notice),
                                )
                        }))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .id(("placeholder-watching", split_id.as_u64() as usize))
                                        .px_3()
                                        .py_1p5()
                                        .border_1()
                                        .border_color(colors::border())
                                        .rounded(Radii::md())
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colors::bg_hover()))
                                        .on_activate(cx.listener(move |this, _, _, cx| {
                                            this.set_active_split(path_for_watching.clone(), cx);
                                            this.set_channel_for_path(
                                                path_for_watching.clone(),
                                                crate::backend::browser_watching::WATCHING_CHANNEL
                                                    .to_string(),
                                                cx,
                                            );
                                        }))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(colors::text_primary())
                                                .child("Watching"),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(("placeholder-join", split_id.as_u64() as usize))
                                        .px_3()
                                        .py_1p5()
                                        .bg(colors::accent())
                                        .rounded(Radii::md())
                                        .cursor_pointer()
                                        .hover(|s| s.opacity(0.9))
                                        .on_activate(cx.listener(move |this, _, _, cx| {
                                            this.submit_channel_chooser(split_id, cx)
                                        }))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(colors::text_on_accent_contrast())
                                                .child("Set channel"),
                                        ),
                                ),
                        ),
                ),
        )
        .when(header_collapsed, |pane| {
            let channel = app
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .channel_at_path(&path)
                .unwrap_or_default();
            pane.child(render_collapsed_header_capsule(
                &cx.entity(),
                &app.overlay_manager,
                &channel,
                &path,
                split_id,
                0.0,
                is_active_split,
                cx,
            ))
        })
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_entity.update(cx, |app, cx| {
                        app.split_layout.update(cx, |layout, _| {
                            layout.register_split_bounds(bounds_path_key.clone(), bounds)
                        });
                    });
                },
                |_, _: (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .into_any_element()
}

/// What the chooser's suggestion list shows: recent channels first, then the
/// Live now group, indexed together in display order.
struct ChooserSuggestionList<'a> {
    split_id: SplitId,
    path: &'a [usize],
    /// Recent and live rows together, for the list's set size.
    row_count: usize,
    recent: Vec<String>,
    live: &'a ChooserLiveRows,
    /// Per live row: the channel is already open in another split.
    open_elsewhere: &'a [bool],
    selected: Option<usize>,
    hide_viewer_count: bool,
    more_hint: Option<String>,
    pane_height: f32,
    scroll: &'a ScrollHandle,
    live_scroll: &'a ScrollHandle,
}

/// Set `login` as the channel of the chooser's split, if that split still
/// shows the chooser.
fn choose_channel_action(
    app: WeakEntity<EchoApp>,
    split_id: SplitId,
    path: Vec<usize>,
    login: String,
) -> Rc<dyn Fn(&mut App)> {
    Rc::new(move |cx: &mut App| {
        let _ = app.update(cx, |this, cx| {
            if this.channel_chooser_path(split_id, cx).is_some() {
                this.set_active_split(path.clone(), cx);
                this.set_channel_for_path(path.clone(), login.clone(), cx);
            }
        });
    })
}

fn render_chooser_suggestions(
    list: ChooserSuggestionList<'_>,
    cx: &Context<EchoApp>,
) -> Stateful<Div> {
    let recent_count = list.recent.len();
    div()
        .id(("channel-suggestions", list.split_id.as_u64() as usize))
        .role(Role::ListBox)
        .aria_label("Suggested Twitch channels")
        .aria_size_of_set(list.row_count)
        .max_h(px((list.pane_height - 190.0).clamp(48.0, 224.0)))
        .overflow_y_scroll()
        .track_scroll(list.scroll)
        .flex()
        .flex_col()
        .gap_0p5()
        .children(
            list.recent
                .iter()
                .enumerate()
                .map(|(index, login)| render_recent_suggestion(&list, index, login, cx)),
        )
        .when(!list.live.rows.is_empty(), |column| {
            column.child(
                div()
                    .id(("channel-live-group", list.split_id.as_u64() as usize))
                    .role(Role::Group)
                    .aria_label("Live now")
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .track_scroll(list.live_scroll)
                    .child(live_now_header())
                    .children(list.live.rows.iter().enumerate().map(|(pos, stream)| {
                        render_live_suggestion(&list, recent_count + pos, pos, stream, cx)
                    }))
                    .children(list.more_hint.clone().map(|more| {
                        div()
                            .px_3()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(more)
                    })),
            )
        })
}

fn render_recent_suggestion(
    list: &ChooserSuggestionList<'_>,
    index: usize,
    login: &str,
    cx: &Context<EchoApp>,
) -> Stateful<Div> {
    let label = login.to_string();
    let activate = choose_channel_action(
        cx.weak_entity(),
        list.split_id,
        list.path.to_vec(),
        login.to_string(),
    );
    let pointer_activate = activate.clone();
    let is_selected = list.selected == Some(index);
    div()
        .id(SharedString::from(format!("channel-suggestion-{login}")))
        .role(Role::ListBoxOption)
        .aria_label(label.clone())
        .aria_selected(is_selected)
        .aria_position_in_set(index + 1)
        .flex_shrink_0()
        .px_3()
        .py_1()
        .rounded(Radii::md())
        .text_sm()
        .text_color(colors::text_secondary())
        .cursor_pointer()
        .when(is_selected, |row| {
            row.bg(colors::bg_hover())
                .text_color(colors::text_primary())
        })
        .hover(|row| row.bg(colors::bg_hover()))
        .on_activate_without_tab_stop(move |_, _, cx| pointer_activate(cx))
        .on_a11y_action(accesskit::Action::Click, move |_, _, cx| activate(cx))
        .child(label)
}

/// Accessible name of a Live now row: name, "live", category and viewers.
fn live_suggestion_label(stream: &FollowedLiveStream, viewers: Option<&String>) -> String {
    match (stream.category.trim(), viewers) {
        ("", None) => format!("{}, live", stream.display_name),
        ("", Some(viewers)) => {
            format!("{}, live, {viewers} viewers", stream.display_name)
        }
        (category, None) => {
            format!("{}, live, {category}", stream.display_name)
        }
        (category, Some(viewers)) => format!(
            "{}, live, {category}, {viewers} viewers",
            stream.display_name
        ),
    }
}

/// One Live now row at display `index` (live row `pos`), plus its pointer-only
/// "Switch there" button when the channel is open in another split.
fn render_live_suggestion(
    list: &ChooserSuggestionList<'_>,
    index: usize,
    pos: usize,
    stream: &FollowedLiveStream,
    cx: &Context<EchoApp>,
) -> Div {
    let is_selected = list.selected == Some(index);
    let open_elsewhere = list.open_elsewhere[pos];
    let login = stream.login.clone();
    let viewers = (!list.hide_viewer_count).then(|| format_viewer_count(stream.viewer_count));
    let label = live_suggestion_label(stream, viewers.as_ref());
    let open_here = choose_channel_action(
        cx.weak_entity(),
        list.split_id,
        list.path.to_vec(),
        login.clone(),
    );
    let pointer_open_here = open_here.clone();
    let switch_app = cx.weak_entity();
    let switch_login = login.clone();
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("channel-live-{login}")))
                .role(Role::ListBoxOption)
                .aria_label(label)
                .when(open_elsewhere, |row| {
                    row.aria_description(
                        "Already open in another split. Shift+Enter switches there.",
                    )
                })
                .aria_selected(is_selected)
                .aria_position_in_set(index + 1)
                .flex_1()
                .min_w_0()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .rounded(Radii::md())
                .cursor_pointer()
                .when(is_selected, |row| row.bg(colors::bg_hover()))
                .hover(|row| row.bg(colors::bg_hover()))
                .on_activate_without_tab_stop(move |_, _, cx| pointer_open_here(cx))
                .on_a11y_action(accesskit::Action::Click, move |_, _, cx| open_here(cx))
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded(Radii::full())
                        .bg(colors::signal_red()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .text_color(if is_selected {
                                    colors::text_primary()
                                } else {
                                    colors::text_secondary()
                                })
                                .child(stream.display_name.clone()),
                        )
                        .when(!stream.category.trim().is_empty(), |text| {
                            text.child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(colors::text_muted())
                                    .child(stream.category.trim().to_string()),
                            )
                        }),
                )
                .children(viewers.map(|viewers| {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(viewers)
                })),
        )
        .when(open_elsewhere, |row| {
            // Pointer only: Shift+Enter is the keyboard path, so the buttons
            // do not add a tab stop per row.
            row.child(
                div()
                    .id(SharedString::from(format!("channel-live-switch-{login}")))
                    .role(Role::Button)
                    .aria_label(format!(
                        "Switch to {} where it is already open",
                        stream.display_name
                    ))
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .border_1()
                    .border_color(colors::border())
                    .rounded(Radii::md())
                    .text_xs()
                    .text_color(colors::text_secondary())
                    .cursor_pointer()
                    .hover(|button| button.bg(colors::bg_hover()))
                    .on_activate_without_tab_stop(move |_, _, cx| {
                        let _ = switch_app.update(cx, |this, cx| {
                            this.switch_to_open_channel(&switch_login, cx);
                        });
                    })
                    .child("Switch there"),
            )
        })
}

/// Render a special Watching leaf before the browser has selected a Twitch channel.
pub fn render_watching_pane(
    app: &mut EchoApp,
    path: Vec<usize>,
    pane_width: f32,
    pane_height: f32,
    _window: &Window,
    cx: &mut Context<EchoApp>,
) -> AnyElement {
    let pane_width = pane_width.max(0.0);
    let pane_height = pane_height.max(0.0);
    let is_active_split = app.split_layout.read(cx).get_active_split_path() == path;
    let (split_id, header_collapsed) = match app
        .app_state
        .read(cx)
        .active_tab()
        .layout
        .node_at_path(&path)
    {
        Some(SplitNode::Leaf {
            split_id,
            header_collapsed,
            ..
        }) => (*split_id, *header_collapsed),
        _ => (None, false),
    };
    let drag_mode = app.split_layout.read(cx).is_drag_mode_active();
    let layout_locked = app.app_state.read(cx).active_tab().is_locked;
    let bounds_path_key = crate::managers::SplitLayoutManager::path_key(&path);
    let bounds_entity = cx.entity().clone();
    let click_path = path.clone();
    let app_entity = cx.entity().clone();
    let overlay_manager = app.overlay_manager.clone();
    let integration_error = crate::backend::browser_watching::integration_error();
    let watching_status = crate::backend::browser_watching::status();
    let (heading, detail) = match watching_status.media_session {
        crate::backend::browser_watching::MediaSessionStatus::Starting => (
            "Looking for an active Twitch stream".to_string(),
            "Play a Twitch stream in a Chromium-based browser. Echo follows its active-media metadata on a best-effort basis."
                .to_string(),
        ),
        crate::backend::browser_watching::MediaSessionStatus::Waiting => (
            "Waiting for an active Twitch stream".to_string(),
            "Start or unpause a Twitch stream in Edge, Chrome, Brave, Vivaldi, or another Chromium-based browser."
                .to_string(),
        ),
        crate::backend::browser_watching::MediaSessionStatus::Ambiguous {
            candidate_count,
        } => (
            "Choose the stream to follow".to_string(),
            format!(
                "Windows found {candidate_count} active Twitch media sessions. Activate one browser tab or pause the others."
            ),
        ),
        crate::backend::browser_watching::MediaSessionStatus::Unavailable => (
            "Active-media detection is unavailable".to_string(),
            "The Chatterino Native Host extension can still select the Twitch channel for this split."
                .to_string(),
        ),
        crate::backend::browser_watching::MediaSessionStatus::Unsupported => (
            "Waiting for the Twitch browser channel".to_string(),
            "Open or switch channels on twitch.tv with the Chatterino Native Host extension enabled."
                .to_string(),
        ),
        crate::backend::browser_watching::MediaSessionStatus::Active {
            channel,
            source_app,
            paused,
        } => (
            format!("Following {channel}"),
            format!(
                "Detected through {source_app}{}.",
                if paused { " (paused)" } else { "" }
            ),
        ),
    };

    div()
        .id(("watching-pane", bounds_path_key.stable_hash()))
        .w(px(pane_width))
        .h(px(pane_height))
        .min_w_0()
        .min_h_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .relative()
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event, _window, cx| {
                this.set_active_split(click_path.clone(), cx);
            }),
        )
        .when(!header_collapsed, |pane| {
            pane.child(app.chat_header_renderer.update(cx, |renderer, header_cx| {
                renderer.render_fast(
                    "Watching",
                    &path,
                    pane_width,
                    None,
                    false,
                    drag_mode,
                    false,
                    crate::backend::settings::get_compact_split_header(),
                    is_active_split,
                    layout_locked,
                    false,
                    None,
                    app_entity,
                    overlay_manager,
                    header_cx,
                )
            }))
        })
        .child(
            div()
                .flex_1()
                .w_full()
                .px_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .max_w(px(440.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child(heading),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(colors::text_secondary())
                                .child(detail),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_center()
                                .text_color(colors::text_muted())
                                .child(
                                    "Keep the Twitch player itself unmuted; you can still mute the browser or Windows output.",
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_center()
                                .text_color(colors::text_muted())
                                .child(
                                    "The Chatterino Native Host extension takes priority when installed. Turn off Replace Twitch chat when you only want Watching sync.",
                                ),
                        )
                        .children(integration_error.map(|error| {
                            div()
                                .mt_2()
                                .text_xs()
                                .text_center()
                                .text_color(colors::warning())
                                .child(format!("Extension setup: {error}"))
                        })),
                ),
        )
        .when(header_collapsed, |pane| {
            pane.children(split_id.map(|split_id| {
                render_collapsed_header_capsule(
                    &cx.entity(),
                    &app.overlay_manager,
                    crate::backend::browser_watching::WATCHING_CHANNEL,
                    &path,
                    split_id,
                    0.0,
                    is_active_split,
                    cx,
                )
            }))
        })
        .child(
            canvas(
                move |bounds, _window, cx| {
                    bounds_entity.update(cx, |app, cx| {
                        app.split_layout.update(cx, |layout, _| {
                            layout.register_split_bounds(bounds_path_key.clone(), bounds);
                        });
                    });
                },
                |_bounds, _: (), _window, _cx| {},
            )
            .absolute()
            .inset_0(),
        )
        .into_any_element()
}
