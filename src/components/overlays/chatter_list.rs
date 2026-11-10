//! Chatter List overlay
//!
//! Shows all chatters in a channel, fetched via Helix API.
//! Features:
//! - Live count of chatters
//! - Search/filter functionality
//! - Scrollable list
//! - Click to mention username

use super::{
    ContextMenuState,
    traits::{Openable, OverlayPointerIsolation},
};
use crate::components::common::mod_buttons::{ModButtonStyle, render_mod_button};
use crate::components::overlays::text_tooltip_on_hover;
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Escape, Input, InputEvent, InputState};
use crate::icons::CLOSE_GLYPH;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;

const CHATTER_LIST_ROW_HEIGHT_PX: f32 = 54.0;

#[derive(Debug, Clone)]
pub struct Chatter {
    pub username: String,
    pub display_name: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatterListSortMode {
    #[default]
    Login,
    DisplayName,
}

#[derive(Debug, Clone, Default)]
pub struct ChatterListState {
    pub is_open: bool,
    pub channel: String,
    pub chatters: Arc<Vec<Chatter>>,
    pub total_count: usize,
    pub source: String,
    pub search_query: String,
    pub sort_mode: ChatterListSortMode,
    /// Whether the shared snapshot already uses the default login ordering.
    /// Twitch results establish this off the GPUI thread before publication.
    pub login_sorted: bool,
    pub is_loading: bool,
    pub error: Option<String>,
}

impl Openable for ChatterListState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

const PLACEHOLDER: &str = "Filter chatters…";

pub struct ChatterList {
    pub state: ChatterListState,
    /// Sorted, filtered indexes into the shared chatter snapshot. Rebuild only
    /// when rows, query, or sort mode changes—not on animation or parent renders.
    visible_indices: Arc<Vec<usize>>,
    focus_handle: FocusHandle,
    focus_pending: bool,
    search_input: Option<Entity<InputState>>,
    _search_subscription: Option<Subscription>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatterListSourceKind {
    Chatters,
    Moderators,
    Vips,
}

impl ChatterList {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            state: ChatterListState::default(),
            visible_indices: Arc::new(Vec::new()),
            focus_handle: cx.focus_handle(),
            focus_pending: false,
            search_input: None,
            _search_subscription: None,
        }
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
        if let InputEvent::Change = event {
            let query = input.read(cx).value().to_string();
            if query != self.state.search_query {
                self.set_search_query(query, cx);
            }
        }
    }

    pub fn open(&mut self, channel: String, cx: &mut Context<Self>) {
        let preserve_view_state =
            self.state.is_open && self.state.channel.eq_ignore_ascii_case(&channel);
        self.focus_pending |= !self.state.is_open;
        self.state.is_open = true;
        self.state.channel = channel;
        self.state.is_loading = true;
        if !preserve_view_state {
            let observed = crate::backend::get_recent_chatters(&self.state.channel, usize::MAX)
                .into_iter()
                .map(|(username, display_name, _)| Chatter {
                    username,
                    display_name,
                    user_id: String::new(),
                })
                .collect::<Vec<_>>();
            self.state.total_count = observed.len();
            self.state.chatters = Arc::new(observed);
            self.state.source = "observed".to_string();
            self.state.login_sorted = false;
        }
        if !preserve_view_state {
            self.state.search_query.clear();
            self.state.sort_mode = ChatterListSortMode::default();
        }
        self.state.error = None;
        self.rebuild_visible_indices();
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_loaded(
        &mut self,
        channel: String,
        chatters: Arc<Vec<Chatter>>,
        total_count: usize,
        source: String,
        login_sorted: bool,
        is_loading: bool,
        cx: &mut Context<Self>,
    ) {
        let preserve_view_state =
            self.state.is_open && self.state.channel.eq_ignore_ascii_case(&channel);
        self.focus_pending |= !self.state.is_open;
        self.state.is_open = true;
        self.state.channel = channel;
        self.state.total_count = total_count.max(chatters.len());
        self.state.source = source;
        self.state.chatters = chatters;
        self.state.login_sorted = login_sorted;
        if !preserve_view_state {
            self.state.search_query.clear();
            self.state.sort_mode = ChatterListSortMode::default();
        }
        self.state.is_loading = is_loading;
        self.state.error = None;
        self.rebuild_visible_indices();
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.state.is_open = false;
        cx.notify();
    }

    pub fn set_load_error(
        &mut self,
        channel: &str,
        source: Option<String>,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if !self.state.channel.eq_ignore_ascii_case(channel) {
            return;
        }

        self.state.is_open = true;
        self.state.is_loading = false;
        if self.state.source.is_empty()
            && let Some(source) = source.filter(|source| !source.is_empty())
        {
            self.state.source = source;
        }
        self.state.error = Some(error);
        cx.notify();
    }

    pub fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.state.search_query = query;
        self.rebuild_visible_indices();
        cx.notify();
    }

    pub fn clear_search_query(&mut self, cx: &mut Context<Self>) {
        if !self.state.search_query.is_empty() {
            self.state.search_query.clear();
            self.rebuild_visible_indices();
            cx.notify();
        }
    }

    pub fn set_sort_mode(&mut self, sort_mode: ChatterListSortMode, cx: &mut Context<Self>) {
        if self.state.sort_mode != sort_mode {
            self.state.sort_mode = sort_mode;
            self.rebuild_visible_indices();
            cx.notify();
        }
    }

    pub(crate) fn rebuild_visible_indices(&mut self) {
        if self.state.search_query.is_empty()
            && self.state.sort_mode == ChatterListSortMode::Login
            && self.state.login_sorted
        {
            self.visible_indices = Arc::new((0..self.state.chatters.len()).collect());
            return;
        }

        let query = self.state.search_query.to_lowercase();
        let mut indices: Vec<usize> = if query.is_empty() {
            (0..self.state.chatters.len()).collect()
        } else {
            self.state
                .chatters
                .iter()
                .enumerate()
                .filter(|c| {
                    c.1.username.to_lowercase().contains(&query)
                        || c.1.display_name.to_lowercase().contains(&query)
                })
                .map(|(index, _)| index)
                .collect()
        };
        indices.sort_unstable_by(|left_index, right_index| {
            let left = &self.state.chatters[*left_index];
            let right = &self.state.chatters[*right_index];
            match self.state.sort_mode {
                ChatterListSortMode::Login => compare_chatter_text(&left.username, &right.username)
                    .then_with(|| compare_chatter_text(&left.display_name, &right.display_name))
                    .then_with(|| left.user_id.cmp(&right.user_id)),
                ChatterListSortMode::DisplayName => {
                    compare_chatter_text(&left.display_name, &right.display_name)
                        .then_with(|| compare_chatter_text(&left.username, &right.username))
                        .then_with(|| left.user_id.cmp(&right.user_id))
                }
            }
        });
        self.visible_indices = Arc::new(indices);
    }

    fn labels(&self) -> ChatterListLabels {
        chatter_list_labels(&self.state.source)
    }

    fn source_kind(&self) -> ChatterListSourceKind {
        chatter_list_source_kind(&self.state.source)
    }
}

impl Render for ChatterList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }
        let search_input = self.ensure_search_input(window, cx);
        if search_input.read(cx).value() != self.state.search_query {
            let query = self.state.search_query.clone();
            search_input.update(cx, |input, cx| input.set_value(query, window, cx));
        }
        if std::mem::take(&mut self.focus_pending) {
            let focus = search_input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }

        let visible_indices = self.visible_indices.clone();
        let chatters = self.state.chatters.clone();
        let count = visible_indices.len();
        let total = self.state.total_count;
        let channel_for_mentions = self.state.channel.clone();
        let search_query = self.state.search_query.clone();
        let sort_mode = self.state.sort_mode;
        let labels = self.labels();
        let source_kind = self.source_kind();
        let has_search_query = !search_query.is_empty();
        let hide_chatter_details = crate::backend::streamer_mode::should_hide_user_card_details();
        let source_label = if self.state.source.is_empty() {
            labels.title.to_string()
        } else {
            self.state.source.clone()
        };
        let status_text = if self.state.is_loading {
            if total > 0 {
                format!(
                    "Refreshing {}… keeping {} of {} {} visible",
                    source_label.to_lowercase(),
                    count,
                    total,
                    if has_search_query {
                        labels.matching_plural
                    } else {
                        labels.plural
                    }
                )
            } else {
                format!("Loading {}…", source_label.to_lowercase())
            }
        } else if let Some(ref err) = self.state.error {
            if total > 0 {
                format!(
                    "Couldn't refresh: {}. Keeping {} of {} {}",
                    err,
                    count,
                    total,
                    if has_search_query {
                        labels.matching_plural
                    } else {
                        labels.plural
                    }
                )
            } else {
                format!("Couldn't load chatters: {err}")
            }
        } else if self.state.source.is_empty() {
            if has_search_query {
                format!("Showing {} of {} matching {}", count, total, labels.plural)
            } else {
                format!("Showing {} of {} {}", count, total, labels.plural)
            }
        } else {
            let noun = if has_search_query {
                labels.matching_plural
            } else {
                labels.plural
            };
            format!(
                "Showing {} of {} {} from {}",
                count, total, noun, source_label
            )
        };
        let refresh_channel = self.state.channel.clone();
        let refresh_source = self.state.source.clone();
        let viewport = window.viewport_size();
        let viewport_height: f32 = viewport.height.into();
        let panel_fit = crate::utils::fit_overlay_to_viewport(viewport, 360.0, 620.0, 8.0);
        let panel_width = panel_fit.width;
        let panel_height = panel_fit.height;
        let panel_top = crate::utils::clamp_overlay_axis(
            40.0,
            panel_height,
            viewport_height,
            panel_fit.margin_y,
        );

        div()
            .id("chatter-list")
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(|this, _: &Escape, _window, cx| {
                if this.escape_closes(cx) {
                    this.close(cx);
                    crate::backend::ui_actions::push_pending_refocus_active_input();
                    cx.stop_propagation();
                }
            }))
            .absolute()
            .right(px(panel_fit.margin_x))
            .top(px(panel_top))
            .w(px(panel_width))
            .h(px(panel_height))
            .bg(colors::bg_elevated())
            .rounded_md()
            .border_1()
            .border_color(colors::border())
            .shadow_sm()
            .flex()
            .flex_col()
            .overflow_hidden()
            .block_overlay_pointer_passthrough()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors::border_subtle())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .child(format!("{} ({})", labels.title, total)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded(Radii::full())
                                    .bg(colors::bg_surface())
                                    .border_1()
                                    .border_color(colors::border_subtle())
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .child(source_label.clone()),
                            )
                            .child(
                                div()
                                    .id("refresh-chatter-list")
                                    .px_2()
                                    .py_0p5()
                                    .rounded(Radii::sm())
                                    .bg(colors::bg_surface())
                                    .border_1()
                                    .border_color(colors::border())
                                    .text_xs()
                                    .text_color(if self.state.is_loading {
                                        colors::text_muted()
                                    } else {
                                        colors::text_secondary()
                                    })
                                    .when(!self.state.is_loading, |button| {
                                        let refresh_channel = refresh_channel.clone();
                                        let refresh_source = refresh_source.clone();
                                        button
                                            .cursor_pointer()
                                            .hover(|s| s.bg(colors::bg_hover()))
                                            .on_click(cx.listener(move |_this, _, _window, _cx| {
                                                crate::backend::push_ui_action(
                                                    crate::backend::UiAction::RefreshChatterList {
                                                        channel: refresh_channel.clone(),
                                                        source: refresh_source.clone(),
                                                    },
                                                );
                                            }))
                                    })
                                    .child(if self.state.is_loading {
                                        "Refreshing"
                                    } else {
                                        "Refresh"
                                    }),
                            )
                            .child(
                                div()
                                    .id("close-chatter-list")
                                    .role(Role::Button)
                                    .aria_label("Close chatter list")
                                    .on_hover(text_tooltip_on_hover(
                                        "close-chatter-list".to_string(),
                                        "Close".to_string(),
                                    ))
                                    .size(px(Spacing::XXL))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(Radii::sm())
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(colors::hover_surface()))
                                    .child(CLOSE_GLYPH)
                                    .on_activate(cx.listener(|this, _, _window, cx| {
                                        this.close(cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors::border_subtle())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .px_2()
                            .py_1()
                            .rounded(Radii::sm())
                            .bg(colors::bg_surface())
                            .border_1()
                            .border_color(colors::border())
                            .child(
                                div().flex_1().min_w_0().text_xs().child(
                                    Input::new(&search_input)
                                        .appearance(false)
                                        .accessibility_label("Filter chatters")
                                        .accessibility_search(true),
                                ),
                            )
                            .when(has_search_query, |bar| {
                                bar.child(
                                    div()
                                        .id("chatter-list-clear-filter")
                                        .flex_shrink_0()
                                        .px_2()
                                        .py_0p5()
                                        .rounded(Radii::sm())
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colors::bg_hover()))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.clear_search_query(cx);
                                        }))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors::text_secondary())
                                                .child("Clear"),
                                        ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(colors::border_subtle())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child("Sort"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(sort_button(
                                "Login",
                                sort_mode == ChatterListSortMode::Login,
                                cx.listener(|this, _, _window, cx| {
                                    this.set_sort_mode(ChatterListSortMode::Login, cx);
                                }),
                            ))
                            .child(sort_button(
                                "Display",
                                sort_mode == ChatterListSortMode::DisplayName,
                                cx.listener(|this, _, _window, cx| {
                                    this.set_sort_mode(ChatterListSortMode::DisplayName, cx);
                                }),
                            )),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(colors::border_subtle())
                    .text_xs()
                    .text_color(colors::text_secondary())
                    .child(status_text),
            )
            .child(if count == 0 {
                div()
                    .id("chatter-list-items-empty")
                    .flex_1()
                    .px_3()
                    .py_3()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(if search_query.is_empty() {
                        labels.empty
                    } else {
                        labels.no_matches
                    })
                    .into_any_element()
            } else {
                uniform_list(
                    ElementId::Name("chatter-list-items".into()),
                    count,
                    move |range, _window, _cx| {
                        range
                            .map(|visible_index| {
                                let chatter_index = visible_indices[visible_index];
                                render_chatter_row(
                                    visible_index,
                                    &chatters[chatter_index],
                                    &channel_for_mentions,
                                    source_kind,
                                    hide_chatter_details,
                                )
                            })
                            .collect()
                    },
                )
                .flex_1()
                .w_full()
                .into_any_element()
            })
            .into_any_element()
    }
}

fn render_chatter_row(
    visible_index: usize,
    chatter: &Chatter,
    channel: &str,
    source_kind: ChatterListSourceKind,
    hide_chatter_details: bool,
) -> AnyElement {
    let username = chatter.username.clone();
    let display_name = chatter.display_name.clone();
    let channel = channel.to_string();
    let can_moderate_target =
        crate::components::common::moderation_policy::can_moderate_target(&channel, &username, &[]);
    let can_manage_role_target =
        crate::components::common::moderation_policy::can_manage_role_target(&channel, &username);
    let primary_role_action = can_manage_role_target
        .then(|| chatter_list_role_action(source_kind, &channel, &username))
        .flatten();
    let secondary_role_action = can_manage_role_target
        .then(|| chatter_list_secondary_role_action(source_kind, &channel, &username))
        .flatten();
    let visible_display_name = if hide_chatter_details {
        format!("Hidden chatter {}", visible_index + 1)
    } else {
        display_name.clone()
    };

    div()
        .id(SharedString::from(format!("chatter-{}", chatter.username)))
        .h(px(CHATTER_LIST_ROW_HEIGHT_PX))
        .flex_shrink_0()
        .overflow_hidden()
        .px_3()
        .py_0p5()
        .flex()
        .items_start()
        .justify_between()
        .gap_2()
        .text_xs()
        .text_color(colors::text_primary())
        .when(!hide_chatter_details, |row| {
            row.cursor_pointer().hover(|s| s.bg(colors::bg_hover()))
        })
        .when(!hide_chatter_details, |row| {
            let mention_username = username.clone();
            let mention_channel = channel.clone();
            let menu_username = username.clone();
            let menu_display_name = display_name.clone();
            let menu_user_id =
                (!chatter.user_id.trim().is_empty()).then(|| chatter.user_id.clone());
            let menu_channel = channel.clone();
            row.on_click(move |_, _window, _cx| {
                crate::backend::push_ui_action(crate::backend::UiAction::InsertMention {
                    username: mention_username.clone(),
                    channel: mention_channel.clone(),
                    source_split_id: None,
                });
            })
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowContextMenu(
                        ContextMenuState::open_chatter(
                            event.position.x.into(),
                            event.position.y.into(),
                            menu_username.clone(),
                            menu_display_name.clone(),
                            menu_user_id.clone(),
                            menu_channel.clone(),
                            true,
                        ),
                    ));
                },
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
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
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(visible_display_name),
                        )
                        .when(
                            !hide_chatter_details
                                && !display_name.eq_ignore_ascii_case(&chatter.username),
                            |row| {
                                row.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(colors::text_muted())
                                        .child(format!("@{}", chatter.username)),
                                )
                            },
                        ),
                )
                .when(!hide_chatter_details && can_moderate_target, |column| {
                    let timeout_channel = channel.clone();
                    let timeout_username = username.clone();
                    let timeout_long_channel = channel.clone();
                    let timeout_long_username = username.clone();
                    let ban_channel = channel.clone();
                    let ban_username = username.clone();
                    column.child(
                        div()
                            .w_full()
                            .flex()
                            .gap_1()
                            .child(render_mod_button(
                                "30s",
                                ModButtonStyle::Warning,
                                move |_event, _window, _cx| {
                                    if crate::components::common::moderation_policy::recheck_moderation_target(&timeout_channel, &timeout_username) {
                                        crate::backend::bridge::timeout_user(
                                            &timeout_channel,
                                            &timeout_username,
                                            30,
                                            None,
                                        );
                                    }
                                },
                            ))
                            .child(render_mod_button(
                                "10m",
                                ModButtonStyle::Warning,
                                move |_event, _window, _cx| {
                                    if crate::components::common::moderation_policy::recheck_moderation_target(&timeout_long_channel, &timeout_long_username) {
                                        crate::backend::bridge::timeout_user(
                                            &timeout_long_channel,
                                            &timeout_long_username,
                                            600,
                                            None,
                                        );
                                    }
                                },
                            ))
                            .child(render_mod_button(
                                "Ban",
                                ModButtonStyle::Danger,
                                move |_event, _window, _cx| {
                                    if crate::components::common::moderation_policy::recheck_moderation_target(&ban_channel, &ban_username) {
                                        crate::backend::bridge::ban_user(
                                            &ban_channel,
                                            &ban_username,
                                            None,
                                        );
                                    }
                                },
                            ))
                            .when_some(primary_role_action, |row, role_action| {
                                row.child(render_role_action_button(role_action))
                            })
                            .when_some(secondary_role_action, |row, role_action| {
                                row.child(render_role_action_button(role_action))
                            }),
                    )
                }),
        )
        .into_any_element()
}

fn sort_button(
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("chatter-list-sort-{label}")))
        .px_2()
        .py_0p5()
        .rounded(Radii::sm())
        .cursor_pointer()
        .bg(if active {
            colors::accent().opacity(0.16)
        } else {
            colors::bg_surface()
        })
        .border_1()
        .border_color(if active {
            colors::accent().opacity(0.35)
        } else {
            colors::border()
        })
        .hover(|s| s.bg(colors::bg_hover()))
        .on_click(on_click)
        .child(
            div()
                .text_xs()
                .text_color(if active {
                    colors::accent_text()
                } else {
                    colors::text_secondary()
                })
                .child(label),
        )
}

fn compare_chatter_text(a: &str, b: &str) -> std::cmp::Ordering {
    cmp_ascii_case_insensitive(a, b).then_with(|| a.cmp(b))
}

fn cmp_ascii_case_insensitive(a: &str, b: &str) -> std::cmp::Ordering {
    let mut a_bytes = a.bytes();
    let mut b_bytes = b.bytes();

    loop {
        match (a_bytes.next(), b_bytes.next()) {
            (Some(a), Some(b)) => {
                let cmp = a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (None, None) => return std::cmp::Ordering::Equal,
        }
    }
}

#[derive(Clone)]
struct RoleAction {
    label: &'static str,
    style: ModButtonStyle,
    on_click: std::sync::Arc<dyn Fn() + Send + Sync>,
}

fn render_role_action_button(action: RoleAction) -> impl IntoElement {
    let handler = action.on_click.clone();
    render_mod_button(action.label, action.style, move |_event, _window, _cx| {
        handler();
    })
}

fn chatter_list_role_action(
    source_kind: ChatterListSourceKind,
    channel: &str,
    username: &str,
) -> Option<RoleAction> {
    let channel = channel.to_string();
    let username = username.to_string();
    match source_kind {
        ChatterListSourceKind::Chatters => Some(RoleAction {
            label: "Mod",
            style: ModButtonStyle::Success,
            on_click: std::sync::Arc::new(move || {
                if crate::components::common::moderation_policy::recheck_role_management_target(
                    &channel, &username,
                ) {
                    crate::backend::bridge::mod_user(&channel, &username);
                }
            }),
        }),
        ChatterListSourceKind::Moderators => Some(RoleAction {
            label: "Unmod",
            style: ModButtonStyle::Success,
            on_click: std::sync::Arc::new(move || {
                if crate::components::common::moderation_policy::recheck_role_management_target(
                    &channel, &username,
                ) {
                    crate::backend::bridge::unmod_chatter_list_user(&channel, &username);
                }
            }),
        }),
        ChatterListSourceKind::Vips => Some(RoleAction {
            label: "Unvip",
            style: ModButtonStyle::Success,
            on_click: std::sync::Arc::new(move || {
                if crate::components::common::moderation_policy::recheck_role_management_target(
                    &channel, &username,
                ) {
                    crate::backend::bridge::unvip_chatter_list_user(&channel, &username);
                }
            }),
        }),
    }
}

fn chatter_list_secondary_role_action(
    source_kind: ChatterListSourceKind,
    channel: &str,
    username: &str,
) -> Option<RoleAction> {
    if source_kind != ChatterListSourceKind::Chatters {
        return None;
    }

    let channel = channel.to_string();
    let username = username.to_string();
    Some(RoleAction {
        label: "VIP",
        style: ModButtonStyle::Success,
        on_click: std::sync::Arc::new(move || {
            if crate::components::common::moderation_policy::recheck_role_management_target(
                &channel, &username,
            ) {
                crate::backend::bridge::vip_user(&channel, &username);
            }
        }),
    })
}

#[derive(Clone, Copy)]
struct ChatterListLabels {
    title: &'static str,
    plural: &'static str,
    matching_plural: &'static str,
    empty: &'static str,
    no_matches: &'static str,
}

fn chatter_list_labels(source: &str) -> ChatterListLabels {
    match chatter_list_source_kind(source) {
        ChatterListSourceKind::Moderators => ChatterListLabels {
            title: "Moderators",
            plural: "moderators",
            matching_plural: "matching moderators",
            empty: "No moderators loaded",
            no_matches: "No matching moderators",
        },
        ChatterListSourceKind::Vips => ChatterListLabels {
            title: "VIPs",
            plural: "VIPs",
            matching_plural: "matching VIPs",
            empty: "No VIPs loaded",
            no_matches: "No matching VIPs",
        },
        ChatterListSourceKind::Chatters => ChatterListLabels {
            title: "Chatters",
            plural: "chatters",
            matching_plural: "matching chatters",
            empty: "No chatters loaded",
            no_matches: "No matching chatters",
        },
    }
}

fn chatter_list_source_kind(source: &str) -> ChatterListSourceKind {
    if source.eq_ignore_ascii_case("Twitch moderators") {
        ChatterListSourceKind::Moderators
    } else if source.eq_ignore_ascii_case("Twitch VIPs") {
        ChatterListSourceKind::Vips
    } else {
        ChatterListSourceKind::Chatters
    }
}
