//! Quick Channel Switcher overlay (Ctrl+K)
//!
//! Provides fuzzy search across all channels/tabs with:
//! - Instant filtering as you type
//! - Arrow key navigation
//! - Enter to switch, Escape to dismiss
//! - Recently used channels ranked higher
//! - Live status indicators
//! - Recent mention badges
//! - A "Live now" group of followed channels that are live

use super::traits::{Openable, OverlayPointerIsolation};
use crate::backend::bridge::followed_live::{self, FollowedLiveStatus, FollowedLiveStream};
use crate::components::chat::chat_ui::{format_uptime, format_viewer_count};
use crate::components::common::count_label::capped_count_label;
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Escape, Input, InputEvent, InputState};
use crate::state::SplitId;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use echo_core::quick_switch::{QuickSwitchCandidate, rank_channels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::HashSet;
use std::sync::Arc;

const QUICK_SWITCHER_HISTORY_LIMIT: usize = 32;
const PLACEHOLDER: &str = "Type to search channels…";
const LIVE_ONLY_PLACEHOLDER: &str = "Search live followed channels…";
/// Live now rows listed at once; the rest wait for a narrower query.
const MAX_SWITCHER_LIVE_ROWS: usize = 50;

/// A single channel/tab item in the switcher
#[derive(Clone, Debug)]
pub struct QuickSwitchItem {
    pub channel: String,
    pub tab_index: usize,
    pub split_path: Vec<usize>,
    pub location_label: String,
    pub is_live: bool,
    /// Retained Activity mentions for this channel when the switcher opened.
    pub mention_count: usize,
}

/// What confirming a switcher row asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickSwitchSelection {
    /// Focus an open split.
    Open {
        tab_index: usize,
        split_path: Vec<usize>,
    },
    /// Open a followed live channel: in `fill_split` while that split is
    /// still empty, otherwise in a new tab (or where it is already open).
    FollowedLive {
        login: String,
        fill_split: Option<SplitId>,
    },
}

/// Which rows the switcher shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickSwitchMode {
    /// Open channels, then the Live now group (Ctrl+K).
    All,
    /// Only the Live now group (the split menu's "Live followed channels…").
    /// `fill_split` is the empty split the menu came from.
    LiveOnly { fill_split: Option<SplitId> },
}

impl QuickSwitchSelection {
    /// Hand the selection to the app's pending-work drain, for paths that
    /// have no handle on the app.
    pub fn push_pending(self) {
        match self {
            Self::Open {
                tab_index,
                split_path,
            } => crate::backend::push_pending_quick_switch(tab_index, split_path),
            Self::FollowedLive { login, fill_split } => {
                crate::backend::ui_actions::push_pending_followed_channel_open(login, fill_split);
            }
        }
    }
}

/// Quick channel switcher overlay
pub struct QuickSwitcher {
    pub is_open: bool,
    pub query: String,
    pub selected_index: usize,
    pub channels: Vec<QuickSwitchItem>,
    pub filtered: Vec<usize>,
    recent_channels: Vec<String>,
    mode: QuickSwitchMode,
    /// Logins open in any split when the switcher opened.
    open_logins: HashSet<String>,
    followed_status: FollowedLiveStatus,
    followed: Arc<Vec<FollowedLiveStream>>,
    /// Indices into `followed`, shown after the open channels.
    followed_filtered: Vec<usize>,
    /// Matches left out by [`MAX_SWITCHER_LIVE_ROWS`].
    followed_more: usize,
    followed_version: u64,
    /// Streamer mode hides the follow list.
    followed_hidden: bool,
    hide_viewer_stats: bool,
    results_scroll: ScrollHandle,
    /// Tracks the Live now group's children, which are not direct children of
    /// the scrolled list.
    followed_rows_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    focus_pending: bool,
    search_input: Option<Entity<InputState>>,
    _search_subscription: Option<Subscription>,
}

impl Openable for QuickSwitcher {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

impl QuickSwitcher {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            is_open: false,
            query: String::new(),
            selected_index: 0,
            channels: Vec::new(),
            filtered: Vec::new(),
            recent_channels: Vec::new(),
            mode: QuickSwitchMode::All,
            open_logins: HashSet::new(),
            followed_status: FollowedLiveStatus::SignedOut,
            followed: Arc::default(),
            followed_filtered: Vec::new(),
            followed_more: 0,
            followed_version: 0,
            followed_hidden: false,
            hide_viewer_stats: false,
            results_scroll: ScrollHandle::new(),
            followed_rows_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            focus_pending: false,
            search_input: None,
            _search_subscription: None,
        }
    }

    /// Open the switcher. `channels` lists every open split: the full switcher
    /// shows them, and both modes use them to tell which live channels are
    /// already open.
    pub fn open_with_channels(
        &mut self,
        channels: Vec<QuickSwitchItem>,
        mode: QuickSwitchMode,
        cx: &mut Context<Self>,
    ) {
        self.open_logins = channels
            .iter()
            .map(|item| echo_core::channel_identity::normalize_channel_key(&item.channel))
            .collect();
        self.channels = channels;
        self.mode = mode;
        self.query.clear();
        self.selected_index = 0;
        followed_live::refresh_if_stale();
        self.read_followed();
        self.update_filter();
        self.results_scroll.scroll_to_item(0);
        self.open();
        self.focus_pending = true;
        cx.notify();
    }

    pub fn is_live_only(&self) -> bool {
        matches!(self.mode, QuickSwitchMode::LiveOnly { .. })
    }

    fn fill_split(&self) -> Option<SplitId> {
        match self.mode {
            QuickSwitchMode::LiveOnly { fill_split } => fill_split,
            QuickSwitchMode::All => None,
        }
    }

    /// The followed-live version this switcher last read; the app compares it
    /// to decide when to call [`Self::refresh_followed`].
    pub fn followed_version(&self) -> u64 {
        self.followed_version
    }

    /// Whether streamer mode or its viewer-count rule changed since the list
    /// was read, which needs a [`Self::refresh_followed`] as well.
    pub fn privacy_changed(&self) -> bool {
        self.followed_hidden != crate::backend::streamer_mode::is_streamer_mode_active_cached()
            || self.hide_viewer_stats != crate::backend::streamer_mode::should_hide_viewer_count()
    }

    /// Re-read the followed live list after it changed, keeping a selected
    /// followed row selected when it is still listed.
    pub fn refresh_followed(&mut self, cx: &mut Context<Self>) {
        if !self.is_open {
            return;
        }
        let selected_login = self
            .selected_index
            .checked_sub(self.filtered.len())
            .and_then(|pos| self.followed_filtered.get(pos))
            .map(|&index| self.followed[index].login.clone());
        self.read_followed();
        self.update_followed_filter();
        let reselected = selected_login.and_then(|login| {
            self.followed_filtered
                .iter()
                .position(|&index| self.followed[index].login == login)
        });
        self.selected_index = match reselected {
            Some(pos) => self.filtered.len() + pos,
            None => self.selected_index.min(self.row_count().saturating_sub(1)),
        };
        cx.notify();
    }

    fn read_followed(&mut self) {
        self.followed_version = followed_live::version();
        self.followed_hidden = crate::backend::streamer_mode::is_streamer_mode_active_cached();
        self.hide_viewer_stats = crate::backend::streamer_mode::should_hide_viewer_count();
        let snapshot = followed_live::snapshot();
        self.followed_status = snapshot.status;
        self.followed = snapshot.streams;
    }

    fn row_count(&self) -> usize {
        self.filtered.len() + self.followed_filtered.len()
    }

    /// Take keyboard focus right away instead of on the next render, so keys
    /// typed straight after opening reach the switcher.
    pub fn focus_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pending = false;
        let input = self.ensure_search_input(window, cx);
        let focus = input.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    }

    fn ensure_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.search_input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).with_placeholder(PLACEHOLDER));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            this.handle_search_event(&input, event, cx);
        });
        self.search_input = Some(input.clone());
        self._search_subscription = Some(subscription);
        input
    }

    /// Whether typed text already reaches the search box.
    pub fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window))
    }

    /// Focus the search box and type `text` into it.
    pub fn type_into_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.ensure_search_input(window, cx);
        let focus = input.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        input.update(cx, |input, cx| input.insert(text, window, cx));
    }

    /// Escape closes the overlay unless an IME composition is still open.
    fn escape_closes(&self, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_none_or(|input| input.read(cx).marked_range().is_none())
    }

    fn handle_search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = input.read(cx).value().to_string();
                if query != self.query {
                    self.set_query(query, cx);
                }
            }
            InputEvent::PressEnter => {
                if let Some(selection) = self.confirm_selection(cx) {
                    selection.push_pending();
                }
            }
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// Close the switcher
    pub fn close_switcher(&mut self, cx: &mut Context<Self>) {
        self.close();
        cx.notify();
    }

    /// Update the query string and re-filter
    pub fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        self.selected_index = 0;
        self.update_filter();
        self.results_scroll.scroll_to_item(0);
        cx.notify();
    }

    /// Update filtered indices based on current query
    fn update_filter(&mut self) {
        self.filtered.clear();
        self.update_followed_filter();

        if self.is_live_only() {
            return;
        }

        let candidates: Vec<QuickSwitchCandidate<'_>> = self
            .channels
            .iter()
            .map(|item| QuickSwitchCandidate {
                channel: &item.channel,
                mention_count: item.mention_count,
                is_live: item.is_live,
            })
            .collect();
        self.filtered.extend(rank_channels(
            &self.query,
            &candidates,
            &self.recent_channels,
        ));
    }

    /// Followed live rows for the query, at most [`MAX_SWITCHER_LIVE_ROWS`].
    /// The full switcher leaves out channels that are already open, since
    /// they are listed above.
    fn update_followed_filter(&mut self) {
        self.followed_more = 0;
        if self.followed_hidden {
            self.followed_filtered.clear();
            return;
        }
        let skip_open = !self.is_live_only();
        self.followed_filtered =
            echo_core::followed_live::matching_streams(&self.followed, &self.query, |login| {
                skip_open && self.open_logins.contains(login)
            });
        self.followed_more = self
            .followed_filtered
            .len()
            .saturating_sub(MAX_SWITCHER_LIVE_ROWS);
        self.followed_filtered.truncate(MAX_SWITCHER_LIVE_ROWS);
    }

    /// What Enter does with a followed row, shown on the selected row.
    fn open_hint(&self, login: &str) -> &'static str {
        if self.fill_split().is_some() {
            "Opens here"
        } else if self.open_logins.contains(login) {
            "Switches to it"
        } else {
            "Opens in a new tab"
        }
    }

    fn followed_notice(&self) -> Option<&'static str> {
        echo_core::followed_live::notice(
            self.followed_status,
            self.followed_hidden,
            self.followed.len(),
            self.followed_filtered.len(),
            !self.query.trim().is_empty(),
        )
    }

    /// Whether the Live now area renders at all. The full switcher stays quiet
    /// in streamer mode, when signed out and, while searching, when no
    /// followed channel matches.
    fn shows_followed_group(&self) -> bool {
        self.is_live_only()
            || (!self.followed_hidden
                && (!self.followed_filtered.is_empty()
                    || (self.query.trim().is_empty()
                        && self.followed_status != FollowedLiveStatus::SignedOut)))
    }

    fn touch_recent_channel(&mut self, channel: &str) {
        self.recent_channels
            .retain(|recent| !recent.eq_ignore_ascii_case(channel));
        self.recent_channels.insert(0, channel.to_string());
        if self.recent_channels.len() > QUICK_SWITCHER_HISTORY_LIMIT {
            self.recent_channels.truncate(QUICK_SWITCHER_HISTORY_LIMIT);
        }
    }

    /// Move selection up or down
    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let row_count = self.row_count();
        if row_count == 0 {
            return;
        }

        let current = self.selected_index as i32;
        let new_index = current + delta;
        let max_index = row_count as i32 - 1;

        self.selected_index = new_index.clamp(0, max_index) as usize;
        match self.selected_index.checked_sub(self.filtered.len()) {
            None => self.results_scroll.scroll_to_item(self.selected_index),
            // Followed rows sit in the Live now group, below its header.
            Some(pos) => scroll_grouped_row_into_view(
                &self.results_scroll,
                &self.followed_rows_scroll,
                pos + 1,
            ),
        }
        cx.notify();
    }

    /// Confirm the current selection and say what the app should do with it.
    pub fn confirm_selection(&mut self, cx: &mut Context<Self>) -> Option<QuickSwitchSelection> {
        let selection = if let Some(&channel_idx) = self.filtered.get(self.selected_index) {
            let item = &self.channels[channel_idx];
            let channel = item.channel.clone();
            let selection = QuickSwitchSelection::Open {
                tab_index: item.tab_index,
                split_path: item.split_path.clone(),
            };
            self.touch_recent_channel(&channel);
            selection
        } else {
            let &stream_idx = self
                .followed_filtered
                .get(self.selected_index - self.filtered.len())?;
            let login = self.followed[stream_idx].login.clone();
            self.touch_recent_channel(&login);
            QuickSwitchSelection::FollowedLive {
                login,
                fill_split: self.fill_split(),
            }
        };

        self.close_switcher(cx);
        Some(selection)
    }

    /// The Live now group: its header, then followed rows or the line
    /// explaining their absence. Streamer mode shows only that line.
    fn render_followed_group(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.shows_followed_group() {
            return None;
        }
        let mut elements = Vec::with_capacity(self.followed_filtered.len() + 3);
        if !self.followed_hidden {
            elements.push(
                div()
                    .w_full()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_muted())
                    .child("Live now")
                    .into_any_element(),
            );
        }
        let entity = cx.entity();
        elements.extend(
            self.followed_filtered
                .iter()
                .enumerate()
                .map(|(pos, &stream_idx)| {
                    let stream = &self.followed[stream_idx];
                    let display_idx = self.filtered.len() + pos;
                    let is_selected = display_idx == self.selected_index;
                    let summary = echo_core::followed_live::stream_summary(stream);
                    let stats = (!self.hide_viewer_stats).then(|| {
                        let viewers = format_viewer_count(stream.viewer_count);
                        if stream.started_at > 0 {
                            format!("{viewers} · {}", format_uptime(stream.started_at))
                        } else {
                            viewers
                        }
                    });
                    let open_hint = self.open_hint(&stream.login);
                    let aria_label = format!(
                        "{}, live{}{}, {}",
                        stream.display_name,
                        if summary.is_empty() {
                            String::new()
                        } else {
                            format!(", {summary}")
                        },
                        if self.hide_viewer_stats {
                            String::new()
                        } else {
                            format!(
                                ", {} viewer{}",
                                format_viewer_count(stream.viewer_count),
                                if stream.viewer_count == 1 { "" } else { "s" }
                            )
                        },
                        open_hint.to_lowercase(),
                    );
                    let login_differs = !stream.display_name.eq_ignore_ascii_case(&stream.login);

                    div()
                        .id(SharedString::from(format!("qs-live-{}", stream.login)))
                        .role(Role::ListBoxOption)
                        .aria_label(SharedString::from(aria_label))
                        .aria_selected(is_selected)
                        .aria_position_in_set(display_idx + 1)
                        .when(is_selected, |el| el.aria_active_descendant())
                        .w_full()
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(is_selected, |el| el.bg(colors::accent().opacity(0.15)))
                        .when(!is_selected, |el| el.hover(|el| el.bg(colors::bg_hover())))
                        .on_activate_without_tab_stop(cx.listener(
                            move |this, _event: &ClickEvent, _window, cx| {
                                this.selected_index = display_idx;
                                if let Some(selection) = this.confirm_selection(cx) {
                                    selection.push_pending();
                                }
                            },
                        ))
                        .on_a11y_action(AccessibleAction::Click, {
                            let entity = entity.clone();
                            move |_data, _window, app| {
                                entity.update(app, |this, cx| {
                                    this.selected_index = display_idx;
                                    if let Some(selection) = this.confirm_selection(cx) {
                                        selection.push_pending();
                                    }
                                });
                            }
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(8.0))
                                .h(px(8.0))
                                .rounded(Radii::full())
                                .bg(colors::signal_red()),
                        )
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
                                        .items_baseline()
                                        .gap_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .text_sm()
                                                .truncate()
                                                .text_color(if is_selected {
                                                    colors::accent_text()
                                                } else {
                                                    colors::text_primary()
                                                })
                                                .child(stream.display_name.clone()),
                                        )
                                        .when(login_differs, |el| {
                                            el.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .text_color(colors::text_muted())
                                                    .child(stream.login.clone()),
                                            )
                                        }),
                                )
                                .when(!summary.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(colors::text_muted())
                                            .child(summary),
                                    )
                                }),
                        )
                        .when(is_selected, |el| {
                            el.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .child(open_hint),
                            )
                        })
                        .children(stats.map(|stats| {
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(colors::text_secondary())
                                .child(stats)
                        }))
                        .into_any_element()
                }),
        );
        if let Some(more) = echo_core::followed_live::more_hint(self.followed_more) {
            elements.push(
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(more)
                    .into_any_element(),
            );
        }
        if self.followed_filtered.is_empty()
            && let Some(notice) = self.followed_notice()
        {
            elements.push(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child(notice)
                    .into_any_element(),
            );
        }
        Some(
            div()
                .id("qs-live-group")
                .role(Role::Group)
                .aria_label("Live now")
                .w_full()
                .flex()
                .flex_col()
                .track_scroll(&self.followed_rows_scroll)
                .when(!self.filtered.is_empty(), |el| {
                    el.mt_1().border_t_1().border_color(colors::border())
                })
                .children(elements)
                .into_any_element(),
        )
    }
}

/// Scroll `viewport` so child `ix` of a group inside it is fully visible.
/// `rows` tracks the group's children, whose bounds come from the last frame;
/// moving a highlight through a list does not move its rows.
pub fn scroll_grouped_row_into_view(viewport: &ScrollHandle, rows: &ScrollHandle, ix: usize) {
    let Some(row) = rows.bounds_for_item(ix) else {
        return;
    };
    let view = viewport.bounds();
    let mut offset = viewport.offset();
    if row.top() < view.top() {
        offset.y += view.top() - row.top();
    } else if row.bottom() > view.bottom() {
        offset.y -= row.bottom() - view.bottom();
    } else {
        return;
    }
    viewport.set_offset(offset);
}

impl Render for QuickSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open {
            return div().into_any_element();
        }

        super::clear_global_tooltip();
        let search_input = self.ensure_search_input(window, cx);
        if search_input.read(cx).value() != self.query {
            let query = self.query.clone();
            search_input.update(cx, |input, cx| input.set_value(query, window, cx));
        }
        let placeholder = if self.is_live_only() {
            LIVE_ONLY_PLACEHOLDER
        } else {
            PLACEHOLDER
        };
        if search_input.read(cx).placeholder() != placeholder {
            search_input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        let followed_group = self.render_followed_group(cx);
        let show_no_channels = self.filtered.is_empty() && followed_group.is_none();
        if std::mem::take(&mut self.focus_pending) {
            let focus = search_input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        let modal_fit =
            crate::utils::fit_overlay_to_viewport(window.viewport_size(), 500.0, 400.0, 12.0);
        let entity = cx.entity();

        div()
            .id("quick-switcher")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors::backdrop())
            .block_overlay_pointer_passthrough()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.close_switcher(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("quick-switcher-dialog")
                    .track_focus(&self.focus_handle)
                    .role(Role::Dialog)
                    .aria_label(if self.is_live_only() {
                        "Live followed channels"
                    } else {
                        "Quick channel switcher"
                    })
                    .w(px(modal_fit.width))
                    .max_h(px(modal_fit.height))
                    .bg(colors::bg_elevated())
                    .border_1()
                    .border_color(colors::border())
                    .rounded_md()
                    .shadow_lg()
                    .overflow_hidden()
                    .block_overlay_pointer_passthrough()
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation();
                    })
                    .capture_action(cx.listener(|this, _: &Escape, _window, cx| {
                        if this.escape_closes(cx) {
                            this.close_switcher(cx);
                            crate::backend::ui_actions::push_pending_refocus_active_input();
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .id("quick-switcher-query")
                            .w_full()
                            .p_3()
                            .border_b_1()
                            .border_color(colors::border())
                            .child(
                                div().w_full().text_sm().child(
                                    Input::new(&search_input)
                                        .accessibility_label(if self.is_live_only() {
                                            "Search live followed channels"
                                        } else {
                                            "Search channels"
                                        })
                                        .accessibility_search(true),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .id("quick-switcher-results")
                            .role(Role::ListBox)
                            .aria_label("Matching channels")
                            .aria_size_of_set(self.row_count())
                            .w_full()
                            .max_h(px(320.0))
                            .overflow_y_scroll()
                            .track_scroll(&self.results_scroll)
                            .children(if show_no_channels {
                                vec![
                                    div()
                                        .w_full()
                                        .p_4()
                                        .text_sm()
                                        .text_color(colors::text_secondary())
                                        .child("No channels found")
                                        .into_any_element(),
                                ]
                            } else {
                                self.filtered
                                    .iter()
                                    .enumerate()
                                    .map(|(display_idx, &channel_idx)| {
                                        let item = &self.channels[channel_idx];
                                        let is_selected = display_idx == self.selected_index;
                                        let split_path_id = item
                                            .split_path
                                            .iter()
                                            .map(usize::to_string)
                                            .collect::<Vec<_>>()
                                            .join("-");

                                        div()
                                            .id(SharedString::from(format!(
                                                "qs-item-{}-{}-{}",
                                                item.tab_index, split_path_id, item.channel
                                            )))
                                            .role(Role::ListBoxOption)
                                            .aria_label(SharedString::from(format!(
                                                "{} in {}{}{}",
                                                item.channel,
                                                item.location_label,
                                                if item.is_live { ", live" } else { "" },
                                                if item.mention_count == 0 {
                                                    String::new()
                                                } else {
                                                    format!(
                                                        ", {} retained mention{}",
                                                        item.mention_count,
                                                        if item.mention_count == 1 {
                                                            ""
                                                        } else {
                                                            "s"
                                                        }
                                                    )
                                                },
                                            )))
                                            .aria_selected(is_selected)
                                            .aria_position_in_set(display_idx + 1)
                                            .when(is_selected, |el| el.aria_active_descendant())
                                            .w_full()
                                            .px_3()
                                            .py_2()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .when(is_selected, |el| {
                                                el.bg(colors::accent().opacity(0.15))
                                            })
                                            .when(!is_selected, |el| {
                                                el.hover(|el| el.bg(colors::bg_hover()))
                                            })
                                            .on_activate_without_tab_stop(cx.listener(
                                                move |this, _event: &ClickEvent, _window, cx| {
                                                    this.selected_index = display_idx;
                                                    if let Some(selection) =
                                                        this.confirm_selection(cx)
                                                    {
                                                        selection.push_pending();
                                                    }
                                                },
                                            ))
                                            .on_a11y_action(AccessibleAction::Click, {
                                                let entity = entity.clone();
                                                move |_data, _window, app| {
                                                    entity.update(app, |this, cx| {
                                                        this.selected_index = display_idx;
                                                        if let Some(selection) =
                                                            this.confirm_selection(cx)
                                                        {
                                                            selection.push_pending();
                                                        }
                                                    });
                                                }
                                            })
                                            .child(
                                                div()
                                                    .w(px(8.0))
                                                    .h(px(8.0))
                                                    .rounded(Radii::full())
                                                    .when(item.is_live, |el| {
                                                        el.bg(colors::signal_red())
                                                    })
                                                    .when(!item.is_live, |el| {
                                                        el.bg(colors::text_secondary().opacity(0.3))
                                                    }),
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
                                                            .text_color(if is_selected {
                                                                colors::accent_text()
                                                            } else {
                                                                colors::text_primary()
                                                            })
                                                            .child(item.channel.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(colors::text_muted())
                                                            .child(item.location_label.clone()),
                                                    ),
                                            )
                                            .when(item.mention_count > 0, |el| {
                                                el.child(
                                                    div()
                                                        .px_1p5()
                                                        .py_0p5()
                                                        .rounded_full()
                                                        .bg(colors::mention_border().opacity(0.2))
                                                        .text_xs()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(colors::mention_border())
                                                        .child(capped_count_label(
                                                            item.mention_count,
                                                        )),
                                                )
                                            })
                                            .into_any_element()
                                    })
                                    .collect()
                            })
                            .children(followed_group),
                    ),
            )
            .into_any_element()
    }
}
