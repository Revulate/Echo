//! Search modal overlay for searching messages across channels
//! Accessible via Ctrl+F keyboard shortcut

use super::traits::{Openable, block_overlay_pointer_passthrough};
use crate::EchoApp;
use crate::backend::{self, ChatMessage};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Input, InputEvent, InputState};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use crate::utils::{format_date_time, parse_user_color_opt};
use gpui::prelude::FluentBuilder;
use gpui::*;

const RESULTS_PER_PAGE: usize = 50;
pub const DOCKED_SEARCH_HEIGHT: f32 = 44.0;

/// Search modal state
#[derive(Clone, Default)]
pub struct SearchState {
    pub is_open: bool,
    pub query: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub channel_filter: Option<String>,
    pub results: Vec<ChatMessage>,
    pub current_page: usize,
    pub total_results: usize,
    pub selected_index: usize,
    pub error: Option<String>,
}

impl Openable for SearchState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

impl SearchState {
    pub fn total_pages(&self) -> usize {
        if self.total_results == 0 {
            1
        } else {
            self.total_results.div_ceil(RESULTS_PER_PAGE)
        }
    }
}

/// Pre-formatted text for one visible result row.
struct SearchResultRow {
    channel_label: SharedString,
    display_name: SharedString,
    timestamp: SharedString,
    preview: SharedString,
    user_color: Option<Hsla>,
    jump_channel: SharedString,
    message_id: SharedString,
}

impl SearchResultRow {
    fn from_message(msg: &ChatMessage) -> Self {
        Self {
            channel_label: SharedString::from(msg.channel.to_string()),
            display_name: SharedString::from(msg.display_name.clone()),
            timestamp: SharedString::from(format_date_time(msg.timestamp)),
            preview: SharedString::from(SearchModal::result_preview_text(msg)),
            user_color: msg.color.as_ref().and_then(|c| parse_user_color_opt(c)),
            jump_channel: SharedString::from(msg.channel.clone()),
            message_id: SharedString::from(msg.id.clone()),
        }
    }
}

/// Search modal component
pub struct SearchModal {
    pub state: SearchState,
    pub channels: Vec<String>,
    expanded: bool,
    docked_channel: Option<String>,
    docked_owner: Option<WeakEntity<EchoApp>>,
    query_input: Option<Entity<InputState>>,
    focus_query_on_open: bool,
    focus_return: Option<WeakFocusHandle>,
    subscriptions: Vec<Subscription>,
    /// Formatted rows for the visible page.
    page_rows: Vec<SearchResultRow>,
    /// `(results revision, page)` the cached rows were built from.
    page_rows_key: Option<(u64, usize)>,
    results_revision: u64,
}

impl SearchModal {
    pub fn new(channels: Vec<String>) -> Self {
        Self {
            state: SearchState::default(),
            channels,
            expanded: false,
            docked_channel: None,
            docked_owner: None,
            query_input: None,
            focus_query_on_open: false,
            focus_return: None,
            subscriptions: Vec::new(),
            page_rows: Vec::new(),
            page_rows_key: None,
            results_revision: 0,
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.state.is_open && self.expanded
    }

    pub fn is_docked_open(&self) -> bool {
        self.state.is_open && !self.expanded
    }

    pub fn open_docked(&mut self, channel: String) {
        self.set_docked_channel(channel);
        self.expanded = false;
        self.state.open();
        self.focus_query_on_open = true;
    }

    pub fn open_expanded_with_scope(
        &mut self,
        channels: Vec<String>,
        default_filter: Option<String>,
    ) {
        let normalized_channels = normalized_channel_scope(&channels);
        self.set_channels(channels);
        let next_filter = default_filter
            .filter(|channel| normalized_channels.contains(&channel.to_ascii_lowercase()));
        if self.state.channel_filter != next_filter {
            self.state.channel_filter = next_filter;
            self.clear_results_for_query_edit();
        }
        self.docked_channel = None;
        self.docked_owner = None;
        self.expanded = true;
        self.state.open();
        self.focus_query_on_open = true;
    }

    fn set_docked_channel(&mut self, channel: String) {
        let unchanged = self
            .docked_channel
            .as_ref()
            .is_some_and(|current| current.eq_ignore_ascii_case(&channel));
        if unchanged {
            return;
        }

        self.set_channels(vec![channel.clone()]);
        let next_filter = Some(channel.clone());
        if self.state.channel_filter != next_filter {
            self.state.channel_filter = next_filter;
            self.clear_results_for_query_edit();
        }
        self.docked_channel = Some(channel);
    }

    fn expand(&mut self) {
        if self.is_docked_open() {
            self.expanded = true;
            self.focus_query_on_open = true;
        }
    }

    fn collapse(&mut self) {
        if self.is_expanded() && self.docked_channel.is_some() {
            self.expanded = false;
            self.focus_query_on_open = true;
        }
    }

    fn notify_docked_owner(&self, cx: &mut App) {
        if let Some(owner) = self.docked_owner.as_ref().and_then(WeakEntity::upgrade) {
            owner.update(cx, |_app, cx| cx.notify());
        }
    }

    fn ensure_query_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query_input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).with_placeholder("Type to search…"));
            let subscription =
                cx.subscribe(&input, |this, _input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.clear_results_for_query_edit();
                        this.notify_docked_owner(cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter => {
                        this.submit_query(cx);
                        this.notify_docked_owner(cx);
                        cx.notify();
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                });
            self.query_input = Some(input);
            self.subscriptions.push(subscription);
        }

        if self.focus_query_on_open {
            self.focus_query_on_open = false;
            if let Some(input) = self.query_input.as_ref() {
                let focus_handle = input.read(cx).focus_handle().clone();
                self.focus_return = window
                    .focused(cx)
                    .filter(|focused| focused != &focus_handle)
                    .map(|focused| focused.downgrade());
                window.focus(&focus_handle, cx);
            }
        }
    }

    pub(crate) fn close_and_restore_focus(&mut self, window: &mut Window, cx: &mut App) {
        self.state.close();
        self.expanded = false;
        self.docked_channel = None;
        match self
            .focus_return
            .take()
            .and_then(|focus_handle| focus_handle.upgrade())
        {
            Some(focus_handle) => {
                window.focus(&focus_handle, cx);
            }
            _ => {
                window.blur(cx);
            }
        }
    }

    fn query_value_owned(&self, cx: &App) -> String {
        self.query_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn query_edit_state(&self, cx: &App) -> (bool, bool) {
        self.query_input
            .as_ref()
            .map(|input| {
                let input = input.read(cx);
                let value = input.value();
                (value.trim().is_empty(), !value.is_empty())
            })
            .unwrap_or((true, false))
    }

    pub fn set_channels(&mut self, channels: Vec<String>) {
        let previous_scope = normalized_channel_scope(&self.channels);
        let normalized_channels = normalized_channel_scope(&channels);
        let filter_stale =
            self.state.channel_filter.as_ref().is_some_and(|channel| {
                !normalized_channels.contains(&channel.to_ascii_lowercase())
            });
        let scope_changed = previous_scope != normalized_channels;
        self.channels = channels;
        if filter_stale {
            self.state.channel_filter = None;
            self.clear_results_for_query_edit();
        } else if scope_changed && self.state.channel_filter.is_none() {
            self.clear_results_for_query_edit();
        }
    }

    /// Execute search across all messages
    pub fn execute_search(&mut self, cx: &App) {
        let query_input = self.query_value_owned(cx);
        let query = query_input.trim();
        if query.is_empty() {
            self.mark_results_changed();
            self.state.results.clear();
            self.state.query.clear();
            self.state.total_results = 0;
            self.state.current_page = 0;
            self.state.selected_index = 0;
            self.state.error = None;
            backend::search::clear_active_search_markers();
            return;
        }

        self.state.query = query.to_string();
        self.state.current_page = 0;
        self.state.selected_index = 0;
        self.state.error = None;

        let compiled_query = match if self.state.case_sensitive {
            backend::search::compile_search_query_with_case(
                &self.state.query,
                self.state.use_regex,
                true,
            )
        } else {
            backend::search::compile_search_query(&self.state.query, self.state.use_regex)
        } {
            Ok(query) => query,
            Err(error) => {
                self.mark_results_changed();
                self.state.results.clear();
                self.state.total_results = 0;
                self.state.error = Some(error.to_string());
                backend::search::clear_active_search_markers();
                return;
            }
        };

        let mut all_results: Vec<ChatMessage> = Vec::new();

        let channels_to_search: Vec<String> = match &self.state.channel_filter {
            Some(ch) => vec![ch.clone()],
            None => self.channels.clone(),
        };

        for channel in channels_to_search {
            let messages = backend::get_messages(&channel, 2000);

            for msg in messages {
                let searchable = backend::search::ResolvedSearchMessage::new(&msg);
                if compiled_query.matches(&searchable) {
                    all_results.push(msg);
                }
            }
        }

        all_results.sort_by_key(|b| std::cmp::Reverse(b.timestamp));

        self.state.total_results = all_results.len();
        backend::search::set_active_search_markers(&all_results);
        self.mark_results_changed();
        self.state.results = all_results;
    }

    pub fn submit_query(&mut self, cx: &App) {
        if self.is_docked_open()
            && self.search_results_match_query(cx)
            && !self.state.results.is_empty()
        {
            self.state.selected_index = (self.state.selected_index + 1) % self.state.results.len();
            self.sync_page_to_selection();
            self.jump_to_selected_result();
            return;
        }

        self.execute_search(cx);
        if self.is_docked_open() {
            self.jump_to_selected_result();
        }
    }

    pub fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.query_input.clone()
            && !input.read(cx).value().is_empty()
        {
            input.update(cx, |input, cx| input.set_value("", window, cx));
            return;
        }

        if !self.state.query.is_empty()
            || !self.state.results.is_empty()
            || self.state.error.is_some()
        {
            self.clear_results_for_query_edit();
        }
    }

    pub fn submit_or_open_selected(&mut self, window: &mut Window, cx: &mut App) {
        if self.search_results_match_query(cx) && !self.state.results.is_empty() {
            self.open_selected_result(window, cx);
        } else {
            self.execute_search(cx);
        }
    }

    pub fn select_next_result(&mut self) {
        self.move_selection(1);
    }

    pub fn select_previous_result(&mut self) {
        self.move_selection(-1);
    }

    pub fn select_next_result_and_jump(&mut self) {
        if !self.state.results.is_empty() {
            self.state.selected_index = (self.state.selected_index + 1) % self.state.results.len();
            self.sync_page_to_selection();
        }
        self.jump_to_selected_result();
    }

    pub fn select_previous_result_and_jump(&mut self) {
        if !self.state.results.is_empty() {
            self.state.selected_index = if self.state.selected_index == 0 {
                self.state.results.len() - 1
            } else {
                self.state.selected_index - 1
            };
            self.sync_page_to_selection();
        }
        self.jump_to_selected_result();
    }

    pub fn select_next_page(&mut self) {
        self.move_selection(RESULTS_PER_PAGE as isize);
    }

    pub fn select_previous_page(&mut self) {
        self.move_selection(-(RESULTS_PER_PAGE as isize));
    }

    pub fn select_first_result(&mut self) {
        if !self.state.results.is_empty() {
            self.state.selected_index = 0;
            self.state.current_page = 0;
        }
    }

    pub fn select_last_result(&mut self) {
        if !self.state.results.is_empty() {
            self.state.selected_index = self.state.results.len() - 1;
            self.sync_page_to_selection();
        }
    }

    fn set_use_regex(&mut self, use_regex: bool) {
        if self.state.use_regex != use_regex {
            self.state.use_regex = use_regex;
            self.clear_results_for_query_edit();
        }
    }

    fn set_case_sensitive(&mut self, case_sensitive: bool) {
        if self.state.case_sensitive != case_sensitive {
            self.state.case_sensitive = case_sensitive;
            self.clear_results_for_query_edit();
        }
    }

    fn set_channel_filter(&mut self, channel_filter: Option<String>) {
        if self.state.channel_filter != channel_filter {
            self.state.channel_filter = channel_filter;
            self.clear_results_for_query_edit();
        }
    }

    fn clear_results_for_query_edit(&mut self) {
        self.mark_results_changed();
        self.state.query.clear();
        self.state.results.clear();
        self.state.current_page = 0;
        self.state.total_results = 0;
        self.state.selected_index = 0;
        self.state.error = None;
        backend::search::clear_active_search_markers();
    }

    fn search_results_match_query(&self, cx: &App) -> bool {
        !self.state.query.is_empty()
            && self
                .query_input
                .as_ref()
                .is_some_and(|input| self.state.query == input.read(cx).value().trim())
    }

    fn move_selection(&mut self, delta: isize) {
        if self.state.results.is_empty() {
            return;
        }

        let max = self.state.results.len() as isize - 1;
        let next = (self.state.selected_index as isize + delta).clamp(0, max) as usize;
        self.state.selected_index = next;
        self.sync_page_to_selection();
    }

    fn select_page(&mut self, page: usize) {
        if self.state.results.is_empty() {
            self.state.current_page = page.min(self.state.total_pages().saturating_sub(1));
            return;
        }

        let page = page.min(self.state.total_pages().saturating_sub(1));
        self.state.current_page = page;
        self.state.selected_index =
            (page * RESULTS_PER_PAGE).min(self.state.results.len().saturating_sub(1));
    }

    fn sync_page_to_selection(&mut self) {
        self.state.current_page = self.state.selected_index / RESULTS_PER_PAGE;
    }

    fn open_selected_result(&mut self, window: &mut Window, cx: &mut App) -> bool {
        let Some(msg) = self.state.results.get(self.state.selected_index) else {
            return false;
        };

        backend::push_pending_search_jump(msg.channel.to_string(), msg.id.to_string());
        self.close_and_restore_focus(window, cx);
        true
    }

    fn jump_to_selected_result(&self) -> bool {
        let Some(msg) = self.state.results.get(self.state.selected_index) else {
            return false;
        };

        backend::push_pending_search_jump(msg.channel.to_string(), msg.id.to_string());
        true
    }

    fn get_page_results(&self) -> &[ChatMessage] {
        let start = self.state.current_page * RESULTS_PER_PAGE;
        let end = (start + RESULTS_PER_PAGE).min(self.state.results.len());

        if start >= self.state.results.len() {
            &[]
        } else {
            &self.state.results[start..end]
        }
    }

    fn result_preview_text(msg: &ChatMessage) -> String {
        backend::search::resolved_search_text(msg)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| msg.content.to_string())
    }

    pub fn render_docked(
        &mut self,
        channel: &str,
        pane_width: f32,
        owner: WeakEntity<EchoApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.is_docked_open() {
            return div().into_any_element();
        }

        self.set_docked_channel(channel.to_string());
        self.docked_owner = Some(owner);
        self.ensure_query_input(window, cx);

        let query_input = self.query_input.clone();
        let (_, has_query) = self.query_edit_state(cx);
        let has_results = !self.state.results.is_empty();
        let status = if self.state.error.is_some() {
            Some("Invalid query".to_string())
        } else if !self.state.query.is_empty() {
            Some(if has_results {
                format!(
                    "{} / {}",
                    self.state.selected_index.saturating_add(1),
                    self.state.total_results
                )
            } else {
                "0 / 0".to_string()
            })
        } else {
            None
        };

        div()
            .id("docked-message-search")
            .role(Role::Group)
            .aria_label("Search messages in this channel")
            .w_full()
            .h(px(DOCKED_SEARCH_HEIGHT))
            .flex_shrink_0()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_1()
            .bg(colors::bg_elevated())
            .border_b_1()
            .border_color(colors::border())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(pane_width >= 680.0, |row| {
                row.child(
                    div()
                        .max_w(px(120.0))
                        .mr_1()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .overflow_hidden()
                        .child(channel.to_string()),
                )
            })
            .child(
                div()
                    .id("docked-search-query-input")
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(if let Some(input) = query_input.as_ref() {
                        Input::new(input)
                            .accessibility_label("Search messages")
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .when(has_query, |row| {
                row.child(
                    div()
                        .id("docked-search-clear")
                        .px_2()
                        .py_1()
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .hover(|style| style.bg(colors::bg_hover()))
                        .on_activate(cx.listener(|this, _, window, cx| {
                            this.clear_query(window, cx);
                            this.notify_docked_owner(cx);
                            cx.notify();
                        }))
                        .child("Clear"),
                )
            })
            .child(
                div()
                    .id("docked-search-submit")
                    .px_3()
                    .py_1()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .bg(colors::accent())
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .hover(|style| {
                        let mut hover = colors::accent();
                        hover.l = (hover.l + 0.1).min(1.0);
                        style.bg(hover)
                    })
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.submit_query(cx);
                        this.notify_docked_owner(cx);
                        cx.notify();
                    }))
                    .child(if pane_width >= 400.0 {
                        "Search"
                    } else {
                        "Find"
                    }),
            )
            .when_some(
                (pane_width >= 460.0).then_some(status).flatten(),
                |row, status| {
                    row.child(
                        div()
                            .min_w(px(38.0))
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(status),
                    )
                },
            )
            .when(has_results && pane_width >= 500.0, |row| {
                row.child(
                    div()
                        .id("docked-search-previous")
                        .px_2()
                        .py_1()
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .hover(|style| style.bg(colors::bg_hover()))
                        .on_activate(cx.listener(|this, _, _window, cx| {
                            this.select_previous_result_and_jump();
                            this.notify_docked_owner(cx);
                            cx.notify();
                        }))
                        .child("↑"),
                )
                .child(
                    div()
                        .id("docked-search-next")
                        .px_2()
                        .py_1()
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .hover(|style| style.bg(colors::bg_hover()))
                        .on_activate(cx.listener(|this, _, _window, cx| {
                            this.select_next_result_and_jump();
                            this.notify_docked_owner(cx);
                            cx.notify();
                        }))
                        .child("↓"),
                )
            })
            .child(
                div()
                    .id("docked-search-expand")
                    .px_2()
                    .py_1()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .hover(|style| style.bg(colors::bg_hover()))
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.expand();
                        this.notify_docked_owner(cx);
                        cx.notify();
                    }))
                    .child(if pane_width >= 620.0 {
                        "Expand"
                    } else {
                        "Full"
                    }),
            )
            .child(
                div()
                    .id("docked-search-close")
                    .role(Role::Button)
                    .aria_label("Close search")
                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                        "docked-search-close".to_string(),
                        "Close search".to_string(),
                    ))
                    .px_2()
                    .py_1()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .text_sm()
                    .text_color(colors::text_muted())
                    .hover(|style| style.bg(colors::bg_hover()))
                    .on_activate(cx.listener(|this, _, window, cx| {
                        this.close_and_restore_focus(window, cx);
                        this.notify_docked_owner(cx);
                        cx.notify();
                    }))
                    .child(crate::icons::CLOSE_GLYPH),
            )
            .into_any_element()
    }

    /// Invalidate the cached page rows. Must be called before every write to
    /// `state.results`.
    fn mark_results_changed(&mut self) {
        self.results_revision = self.results_revision.wrapping_add(1);
    }

    fn ensure_page_rows(&mut self) {
        let key = (self.results_revision, self.state.current_page);
        if self.page_rows_key == Some(key) {
            return;
        }

        let rows: Vec<SearchResultRow> = self
            .get_page_results()
            .iter()
            .map(SearchResultRow::from_message)
            .collect();
        self.page_rows = rows;
        self.page_rows_key = Some(key);
    }
}

fn normalized_channel_scope(channels: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = channels
        .iter()
        .map(|channel| channel.to_ascii_lowercase())
        .collect();
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

impl Render for SearchModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_expanded() {
            return div().into_any_element();
        }

        super::clear_global_tooltip();
        self.ensure_query_input(window, cx);
        self.ensure_page_rows();

        let current_page = self.state.current_page;
        let total_pages = self.state.total_pages();
        let use_regex = self.state.use_regex;
        let case_sensitive = self.state.case_sensitive;
        let page_start = current_page * RESULTS_PER_PAGE;
        let selected_index = self.state.selected_index;
        let (query_empty, has_query) = self.query_edit_state(cx);
        let search_has_run = !self.state.query.is_empty();
        let error = self.state.error.clone();
        let modal_fit =
            crate::utils::fit_overlay_to_viewport(window.viewport_size(), 700.0, 600.0, 12.0);
        let filters = Self::render_filters(
            use_regex,
            case_sensitive,
            self.state.channel_filter.as_deref(),
            &self.channels,
            cx,
        );
        let results = Self::render_results(
            &self.page_rows,
            query_empty,
            search_has_run,
            error,
            page_start,
            selected_index,
            cx,
        );

        block_overlay_pointer_passthrough(
            div()
                .id("search-modal-overlay")
                .absolute()
                .inset_0()
                .bg(colors::backdrop())
                .flex()
                .items_center()
                .justify_center(),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _event: &MouseDownEvent, window, cx| {
                this.close_and_restore_focus(window, cx);
                cx.notify();
                cx.stop_propagation();
            }),
        )
        .child(
            block_overlay_pointer_passthrough(
                div()
                    .id("search-modal")
                    .role(Role::Dialog)
                    .aria_label("Search messages")
                    .w(px(modal_fit.width))
                    .max_h(px(modal_fit.height))
                    .bg(colors::bg_elevated())
                    .rounded(Radii::lg())
                    .border_1()
                    .border_color(colors::border())
                    .shadow_xl()
                    .overflow_hidden()
                    .overflow_y_scroll(),
            )
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            })
            .child(self.render_header(cx))
            .child(self.render_search_bar(has_query, cx))
            .child(filters)
            .child(results)
            .child(self.render_pagination(current_page, total_pages, cx)),
        )
        .into_any_element()
    }
}

impl SearchModal {
    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let can_collapse = self.docked_channel.is_some();
        let entity = cx.entity();

        div()
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(colors::border())
            .child(
                div()
                    .id("search-modal-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label("Search messages")
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Search Messages"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(can_collapse, |actions| {
                        actions.child(
                            div()
                                .id("search-collapse-btn")
                                .role(Role::Button)
                                .aria_label("Collapse search to this channel")
                                .cursor_pointer()
                                .px_2()
                                .py_1()
                                .rounded(Radii::sm())
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .hover(|s| s.bg(colors::bg_hover()))
                                .on_activate(cx.listener(|this, _, _window, cx| {
                                    this.collapse();
                                    this.notify_docked_owner(cx);
                                    cx.notify();
                                }))
                                .on_a11y_action(AccessibleAction::Click, {
                                    let entity = entity.clone();
                                    move |_data, _window, app| {
                                        entity.update(app, |this, cx| {
                                            this.collapse();
                                            this.notify_docked_owner(cx);
                                            cx.notify();
                                        });
                                    }
                                })
                                .child("Collapse"),
                        )
                    })
                    .child(
                        div()
                            .id("search-close-btn")
                            .role(Role::Button)
                            .aria_label("Close search")
                            .cursor_pointer()
                            .px_2()
                            .py_1()
                            .rounded(Radii::sm())
                            .hover(|s| s.bg(colors::bg_hover()))
                            .on_activate(cx.listener(|this, _, window, cx| {
                                this.close_and_restore_focus(window, cx);
                                cx.notify();
                            }))
                            .on_a11y_action(AccessibleAction::Click, move |_data, window, app| {
                                entity.update(app, |this, cx| {
                                    this.close_and_restore_focus(window, cx);
                                    cx.notify();
                                });
                            })
                            .child(
                                div()
                                    .text_color(colors::text_muted())
                                    .child(crate::icons::CLOSE_GLYPH),
                            ),
                    ),
            )
    }

    fn render_search_bar(
        &mut self,
        has_query: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let query_input = self.query_input.clone();
        let entity = cx.entity();

        div()
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .gap_2()
            .child(div().id("search-query-input").flex_1().text_sm().child(
                if let Some(input) = query_input.as_ref() {
                    Input::new(input)
                        .accessibility_label("Search messages")
                        .into_any_element()
                } else {
                    div().into_any_element()
                },
            ))
            .when(has_query, |el| {
                el.child(
                    div()
                        .id("search-clear-btn")
                        .role(Role::Button)
                        .aria_label("Clear search query")
                        .px_3()
                        .py_2()
                        .bg(colors::bg_surface())
                        .border_1()
                        .border_color(colors::border())
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .hover(|s| s.bg(colors::bg_hover()))
                        .on_activate(cx.listener(|this, _, window, cx| {
                            this.clear_query(window, cx);
                            cx.notify();
                        }))
                        .on_a11y_action(AccessibleAction::Click, {
                            let entity = entity.clone();
                            move |_data, window, app| {
                                entity.update(app, |this, cx| {
                                    this.clear_query(window, cx);
                                    cx.notify();
                                });
                            }
                        })
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_secondary())
                                .child("Clear"),
                        ),
                )
            })
            .child(
                div()
                    .id("search-btn")
                    .role(Role::Button)
                    .aria_label("Search messages")
                    .px_4()
                    .py_2()
                    .bg(colors::accent())
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .hover(|s| {
                        let mut hover = colors::accent();
                        hover.l = (hover.l + 0.1).min(1.0);
                        s.bg(hover)
                    })
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.submit_query(cx);
                        cx.notify();
                    }))
                    .on_a11y_action(AccessibleAction::Click, move |_data, _window, app| {
                        entity.update(app, |this, cx| {
                            this.submit_query(cx);
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .child("Search"),
                    ),
            )
    }

    fn render_filters(
        use_regex: bool,
        case_sensitive: bool,
        channel_filter: Option<&str>,
        channels: &[String],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .w_full()
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .gap_4()
            .border_b_1()
            .border_color(colors::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("regex-toggle")
                            .role(Role::Switch)
                            .aria_label("Use regular expression")
                            .aria_toggled(if use_regex {
                                Toggled::True
                            } else {
                                Toggled::False
                            })
                            .w(px(36.0))
                            .h(px(20.0))
                            .rounded(Radii::xl())
                            .cursor_pointer()
                            .bg(if use_regex {
                                colors::accent()
                            } else {
                                colors::bg_surface()
                            })
                            .border_1()
                            .border_color(colors::border())
                            .hover(|s| s.border_color(colors::accent()))
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .rounded(Radii::lg())
                                    .bg(colors::text_primary())
                                    .ml(if use_regex { px(18.0) } else { px(2.0) }),
                            )
                            .on_activate(cx.listener(|this, _, _window, cx| {
                                this.set_use_regex(!this.state.use_regex);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child("Regex"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("case-sensitive-toggle")
                            .role(Role::Switch)
                            .aria_label("Match case")
                            .aria_toggled(if case_sensitive {
                                Toggled::True
                            } else {
                                Toggled::False
                            })
                            .w(px(36.0))
                            .h(px(20.0))
                            .rounded(Radii::xl())
                            .cursor_pointer()
                            .bg(if case_sensitive {
                                colors::accent()
                            } else {
                                colors::bg_surface()
                            })
                            .border_1()
                            .border_color(colors::border())
                            .hover(|s| s.border_color(colors::accent()))
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .rounded(Radii::lg())
                                    .bg(colors::text_primary())
                                    .ml(if case_sensitive { px(18.0) } else { px(2.0) }),
                            )
                            .on_activate(cx.listener(|this, _, _window, cx| {
                                this.set_case_sensitive(!this.state.case_sensitive);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child("Case"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child("Channel:"),
                    )
                    .child(Self::render_channel_dropdown(channel_filter, channels, cx)),
            )
    }

    fn render_channel_dropdown(
        current: Option<&str>,
        channels: &[String],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("search-channel-filter")
            .role(Role::TabList)
            .aria_label("Channel filter")
            .aria_orientation(Orientation::Horizontal)
            .flex()
            .gap_1()
            .child(
                div()
                    .id("channel-all")
                    .role(Role::Tab)
                    .aria_label("All channels")
                    .aria_selected(current.is_none())
                    .px_2()
                    .py_1()
                    .text_xs()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .bg(if current.is_none() {
                        colors::accent()
                    } else {
                        colors::bg_surface()
                    })
                    .text_color(colors::text_primary())
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.set_channel_filter(None);
                        cx.notify();
                    }))
                    .child("All"),
            )
            .children(channels.iter().map(|ch| {
                let ch_clone = ch.clone();
                let is_selected = current == Some(ch.as_str());

                div()
                    .id(SharedString::from(format!("channel-{}", ch)))
                    .role(Role::Tab)
                    .aria_label(SharedString::from(ch.to_string()))
                    .aria_selected(is_selected)
                    .px_2()
                    .py_1()
                    .text_xs()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .bg(if is_selected {
                        colors::accent()
                    } else {
                        colors::bg_surface()
                    })
                    .text_color(colors::text_primary())
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate(cx.listener(move |this, _, _window, cx| {
                        this.set_channel_filter(Some(ch_clone.clone()));
                        cx.notify();
                    }))
                    .child(ch.to_string())
            }))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_results(
        rows: &[SearchResultRow],
        query_empty: bool,
        search_has_run: bool,
        error: Option<String>,
        page_start: usize,
        selected_index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("search-results")
            .role(Role::ListBox)
            .aria_label("Search results")
            .aria_size_of_set(rows.len())
            .w_full()
            .flex_1()
            .min_h(px(300.0))
            .max_h(px(400.0))
            .overflow_y_scroll()
            .child(if rows.is_empty() {
                div()
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(colors::text_muted())
                            .child(error.unwrap_or_else(|| {
                                if query_empty {
                                    "Enter a search query".to_string()
                                } else if !search_has_run {
                                    "Press Enter to search".to_string()
                                } else {
                                    "No results found".to_string()
                                }
                            })),
                    )
                    .into_any_element()
            } else {
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .children(rows.iter().enumerate().map(|(idx, row)| {
                        let global_idx = page_start + idx;
                        Self::render_result_item(
                            idx,
                            global_idx,
                            row,
                            global_idx == selected_index,
                            cx,
                        )
                    }))
                    .into_any_element()
            })
    }

    fn render_result_item(
        idx: usize,
        global_idx: usize,
        row: &SearchResultRow,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let channel = row.jump_channel.clone();
        let message_id = row.message_id.clone();

        div()
            .id(("search-result", idx))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(format!(
                "{} in {} at {}: {}",
                row.display_name, row.channel_label, row.timestamp, row.preview
            )))
            .aria_selected(is_selected)
            .aria_position_in_set(idx + 1)
            .w_full()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(colors::border())
            .cursor_pointer()
            .when(is_selected, |s| s.bg(colors::accent_transparent(0.15)))
            .hover(|s| s.bg(colors::bg_hover()))
            .on_activate_without_tab_stop(cx.listener(move |this, _, window, cx| {
                backend::push_pending_search_jump(channel.to_string(), message_id.to_string());
                this.state.selected_index = global_idx;
                this.close_and_restore_focus(window, cx);
                cx.notify();
            }))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(row.channel_label.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(row.user_color.unwrap_or_else(colors::text_primary))
                            .child(row.display_name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(row.timestamp.clone()),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child(row.preview.clone()),
            )
    }

    fn render_pagination(
        &mut self,
        current_page: usize,
        total_pages: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(colors::border())
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_muted())
                    .child(format!(
                        "{} results, page {} of {}",
                        self.state.total_results,
                        current_page + 1,
                        total_pages
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("page-prev")
                            .px_3()
                            .py_1()
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .bg(colors::bg_surface())
                            .text_sm()
                            .text_color(if current_page > 0 {
                                colors::text_primary()
                            } else {
                                colors::text_muted()
                            })
                            .hover(|s| {
                                if current_page > 0 {
                                    s.bg(colors::bg_hover())
                                } else {
                                    s
                                }
                            })
                            .on_activate(cx.listener(|this, _, _window, cx| {
                                if this.state.current_page > 0 {
                                    this.select_page(this.state.current_page - 1);
                                    cx.notify();
                                }
                            }))
                            .child("← Prev"),
                    )
                    .child(
                        div()
                            .id("page-next")
                            .px_3()
                            .py_1()
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .bg(colors::bg_surface())
                            .text_sm()
                            .text_color(if current_page + 1 < total_pages {
                                colors::text_primary()
                            } else {
                                colors::text_muted()
                            })
                            .hover(|s| {
                                if current_page + 1 < total_pages {
                                    s.bg(colors::bg_hover())
                                } else {
                                    s
                                }
                            })
                            .on_activate(cx.listener(move |this, _, _window, cx| {
                                if this.state.current_page + 1 < total_pages {
                                    this.select_page(this.state.current_page + 1);
                                    cx.notify();
                                }
                            }))
                            .child("Next →"),
                    ),
            )
    }
}
