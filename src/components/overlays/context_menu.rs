//! Context menu overlay for Echo GPUI
//!
//! Provides right-click context menus for:
//! - Messages (copy, reply, message-scoped moderation)
//! - Users (copy identity, mention, whisper, optional user-card access)
//! - Emotes (copy code, open image/page, copy image/page links, display rules)
//! - Links (open, copy)
#![allow(clippy::too_many_arguments)]

use super::traits::Openable;
use crate::backend;
use crate::components::chat::canvas_renderer::layout::{EmoteTooltipMeta, LayeredTooltipEmote};
use crate::components::ui::PressToClick;
use crate::icons::MenuMetrics;
use crate::state::SplitId;
use crate::theme::tokens::Spacing;
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels},
};
use crate::utils;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;

const CONTEXT_MENU_WIDTH: f32 = 208.0;
const CONTEXT_MENU_PANEL_OVERLAP: f32 = 1.0;
const CONTEXT_MENU_MARGIN: f32 = 8.0;
const CONTEXT_MENU_ACTION_HEIGHT: f32 = MenuMetrics::ITEM_HEIGHT;
const CONTEXT_MENU_DIVIDER_HEIGHT: f32 = MenuMetrics::DIVIDER_HEIGHT;
const CONTEXT_MENU_HEADER_HEIGHT: f32 = MenuMetrics::HEADER_HEIGHT;
const CONTEXT_MENU_PANEL_PADDING: f32 = MenuMetrics::PANEL_PADDING;

#[derive(Clone, Debug)]
struct EmoteContextLink {
    scale_label: String,
    url: String,
}

#[derive(Clone, Debug)]
pub struct EmoteContextTarget {
    name: String,
    provider_label: String,
    image_links: Vec<EmoteContextLink>,
    provider_page_url: Option<String>,
    /// Identity for display rules; `None` for emoji and unidentified emotes.
    rule: Option<EmoteRuleTarget>,
}

#[derive(Clone, Debug)]
struct EmoteRuleTarget {
    emote: echo_core::emote_preferences::EmoteRef,
    is_animated: bool,
}

#[derive(Clone, Debug)]
pub struct ComposerSelectionSnapshot {
    pub range: std::ops::Range<usize>,
    pub text: String,
}

/// A context menu item
#[derive(Clone)]
pub enum ContextMenuItem {
    Action {
        label: String,
        icon: Option<String>,
        is_destructive: bool,
        action: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    },
    Submenu {
        label: String,
        items: Vec<ContextMenuItem>,
    },
    Divider,
    Header {
        title: String,
        subtitle: Option<String>,
    },
}

impl ContextMenuItem {
    pub fn action(label: impl Into<String>, action: impl Fn() + Send + Sync + 'static) -> Self {
        Self::Action {
            label: label.into(),
            icon: None,
            is_destructive: false,
            action: Some(Arc::new(action)),
        }
    }

    pub fn destructive(
        label: impl Into<String>,
        action: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self::Action {
            label: label.into(),
            icon: None,
            is_destructive: true,
            action: Some(Arc::new(action)),
        }
    }

    pub fn divider() -> Self {
        Self::Divider
    }

    pub fn disabled(label: impl Into<String>) -> Self {
        Self::Action {
            label: label.into(),
            icon: None,
            is_destructive: false,
            action: None,
        }
    }

    pub fn submenu(label: impl Into<String>, items: Vec<ContextMenuItem>) -> Self {
        Self::Submenu {
            label: label.into(),
            items,
        }
    }

    pub fn header(title: impl Into<String>, subtitle: Option<String>) -> Self {
        Self::Header {
            title: title.into(),
            subtitle,
        }
    }

    fn is_actionable(&self) -> bool {
        match self {
            Self::Action { action, .. } => action.is_some(),
            Self::Submenu { items, .. } => items.iter().any(Self::is_actionable),
            Self::Divider | Self::Header { .. } => false,
        }
    }

    fn submenu_items(&self) -> Option<&[ContextMenuItem]> {
        match self {
            Self::Submenu { items, .. } => Some(items),
            _ => None,
        }
    }
}

/// Context menu state
#[derive(Clone, Debug, Default)]
pub struct ContextMenuState {
    pub is_open: bool,
    pub x: f32,
    pub y: f32,
    pub menu_type: ContextMenuType,
    /// Currently selected root item index (for keyboard navigation).
    pub selected_index: Option<usize>,
    /// Root item whose secondary pane is currently open.
    pub active_submenu: Option<usize>,
    /// Currently selected item in the open secondary pane.
    pub submenu_selected_index: Option<usize>,
}

/// Types of context menus
#[derive(Clone, Debug, Default)]
pub enum ContextMenuType {
    #[default]
    None,
    Message {
        message_id: String,
        content: String,
        sender: String,
        display_name: String,
        timestamp: i64,
        channel: String,
        source_split_id: Option<SplitId>,
        /// Twitch USERNOTICE and local system rows have stable IDs for
        /// deduplication, but Twitch does not allow replying to or moderating
        /// them as ordinary chat messages.
        allow_remote_actions: bool,
        is_whisper: bool,
        /// The row's highlight, which enables "Why is this highlighted?".
        highlight_type: Option<backend::HighlightType>,
    },
    Emote {
        emotes: Vec<EmoteContextTarget>,
        /// Channel the emote was seen in, for "Only in #channel" rules.
        rule_channel: String,
        /// The targets' display rules, read once when the menu opened.
        rules: Vec<backend::settings::EmoteDisplayRule>,
    },
    Link {
        url: String,
        message_id: String,
        embed_id: u32,
        resource_key: crate::backend::link_types::ResourceKey,
        preview_visible: bool,
    },
    Selection {
        text: String,
    },
    Composer {
        split_id: SplitId,
        selection: Option<ComposerSelectionSnapshot>,
        has_text: bool,
        can_undo: bool,
        can_redo: bool,
    },
    Chatter {
        username: String,
        display_name: String,
        user_id: Option<String>,
        channel: String,
        show_user_card: bool,
    },
    /// Several menus stacked in order and separated by dividers.
    Composite(Vec<ContextMenuType>),
}

impl Openable for ContextMenuState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

impl ContextMenuState {
    pub fn open_message(
        x: f32,
        y: f32,
        message_id: String,
        content: String,
        sender: String,
        display_name: String,
        timestamp: i64,
        channel: String,
        source_split_id: Option<SplitId>,
        allow_remote_actions: bool,
        is_whisper: bool,
        highlight_type: Option<backend::HighlightType>,
    ) -> Self {
        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Message {
                message_id,
                content,
                sender,
                display_name,
                timestamp,
                channel,
                source_split_id,
                allow_remote_actions,
                is_whisper,
                highlight_type,
            },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    pub fn open_emote(
        x: f32,
        y: f32,
        name: String,
        url: String,
        meta: Option<EmoteTooltipMeta>,
        channel: String,
        rule_channel: String,
    ) -> Self {
        let emotes = vec![resolve_emote_context_target(
            &channel,
            name,
            url,
            meta.as_ref(),
        )];
        let rules = emote_rules_for_targets(&emotes);
        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Emote {
                emotes,
                rule_channel,
                rules,
            },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    pub fn open_layered_emote(
        x: f32,
        y: f32,
        emotes: Vec<LayeredTooltipEmote>,
        channel: String,
        rule_channel: String,
    ) -> Self {
        let mut targets = Vec::with_capacity(emotes.len());
        let mut seen = Vec::with_capacity(emotes.len());
        for emote in emotes {
            if seen
                .iter()
                .any(|(name, url): &(SharedString, SharedString)| {
                    name == &emote.name && url == &emote.url
                })
            {
                continue;
            }
            seen.push((emote.name.clone(), emote.url.clone()));
            targets.push(resolve_emote_context_target(
                &channel,
                emote.name.to_string(),
                emote.url.to_string(),
                emote.meta.as_deref(),
            ));
        }

        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Emote {
                rules: emote_rules_for_targets(&targets),
                emotes: targets,
                rule_channel,
            },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    pub fn open_link(x: f32, y: f32, url: String, message_id: String, embed_id: u32) -> Self {
        let resource_key = crate::backend::link_types::ResourceKey::for_url(&url);
        let preview_visible =
            crate::backend::bridge::is_embed_expanded_state(&message_id, embed_id, &url);
        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Link {
                url,
                message_id,
                embed_id,
                resource_key,
                preview_visible,
            },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    pub fn open_selection(x: f32, y: f32, text: String) -> Self {
        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Selection { text },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    pub fn open_composer(
        x: f32,
        y: f32,
        split_id: SplitId,
        selection: Option<ComposerSelectionSnapshot>,
        has_text: bool,
        can_undo: bool,
        can_redo: bool,
    ) -> Self {
        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Composer {
                split_id,
                selection,
                has_text,
                can_undo,
                can_redo,
            },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    pub fn open_chatter(
        x: f32,
        y: f32,
        username: String,
        display_name: String,
        user_id: Option<String>,
        channel: String,
        show_user_card: bool,
    ) -> Self {
        Self {
            is_open: true,
            x,
            y,
            menu_type: ContextMenuType::Chatter {
                username,
                display_name,
                user_id,
                channel,
                show_user_card,
            },
            selected_index: None,
            active_submenu: None,
            submenu_selected_index: None,
        }
    }

    /// Stack several menus into one, in the given order, at the first part's position.
    pub fn compose(parts: impl IntoIterator<Item = ContextMenuState>) -> Self {
        let mut parts = parts.into_iter();
        let Some(mut first) = parts.next() else {
            return Self::default();
        };
        let rest = parts.map(|part| part.menu_type).collect::<Vec<_>>();
        if !rest.is_empty() {
            let mut sections = Vec::with_capacity(rest.len() + 1);
            sections.push(std::mem::take(&mut first.menu_type));
            sections.extend(rest);
            first.menu_type = ContextMenuType::Composite(sections);
        }
        first
    }

    /// Close the context menu and clear state
    pub fn close(&mut self) {
        Openable::close(self);
        self.menu_type = ContextMenuType::None;
        self.selected_index = None;
        self.active_submenu = None;
        self.submenu_selected_index = None;
    }
}

/// Context menu overlay component
pub struct ContextMenu {
    pub state: ContextMenuState,
    /// Cached list of actionable item indices (for keyboard navigation)
    actionable_indices: Vec<usize>,
    /// Cached actionable indices for the currently open secondary pane.
    submenu_actionable_indices: Vec<usize>,
}

impl ContextMenu {
    pub fn new(state: ContextMenuState) -> Self {
        Self {
            state,
            actionable_indices: Vec::new(),
            submenu_actionable_indices: Vec::new(),
        }
    }

    /// Update state (available for external state management)
    pub fn update_state(&mut self, state: ContextMenuState) {
        self.state = state;
        self.actionable_indices.clear();
        self.submenu_actionable_indices.clear();
        self.update_actionable_indices();
    }

    /// Update the cached list of actionable item indices
    fn update_actionable_indices(&mut self) {
        let items = self.build_items();
        self.actionable_indices = items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| item.is_actionable().then_some(idx))
            .collect();

        let selected_still_valid = self
            .state
            .selected_index
            .is_some_and(|idx| self.actionable_indices.contains(&idx));
        if !selected_still_valid {
            self.state.selected_index = self.actionable_indices.first().copied();
        }

        if let Some(submenu_index) = self.state.active_submenu {
            self.update_submenu_actionable_indices(submenu_index);
        } else {
            self.submenu_actionable_indices.clear();
            self.state.submenu_selected_index = None;
        }
    }

    fn update_submenu_actionable_indices(&mut self, submenu_index: usize) {
        let items = self.build_items();
        let submenu_items = items
            .get(submenu_index)
            .and_then(ContextMenuItem::submenu_items);

        self.submenu_actionable_indices = submenu_items
            .map(|items| {
                items
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, item)| item.is_actionable().then_some(idx))
                    .collect()
            })
            .unwrap_or_default();

        if self.submenu_actionable_indices.is_empty() {
            self.state.active_submenu = None;
            self.state.submenu_selected_index = None;
            return;
        }

        let selected_still_valid = self
            .state
            .submenu_selected_index
            .is_some_and(|idx| self.submenu_actionable_indices.contains(&idx));
        if !selected_still_valid {
            self.state.submenu_selected_index = self.submenu_actionable_indices.first().copied();
        }
    }

    /// Select next actionable item
    pub fn select_next(&mut self, cx: &mut Context<Self>) {
        self.move_selection(1);
        cx.notify();
    }

    /// Select previous actionable item
    pub fn select_previous(&mut self, cx: &mut Context<Self>) {
        self.move_selection(-1);
        cx.notify();
    }

    /// Select the first (`last == false`) or last actionable item of the
    /// open level.
    pub fn select_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        if self.actionable_indices.is_empty() {
            self.update_actionable_indices();
        }
        let in_submenu = self.state.active_submenu.is_some();
        let indices = if in_submenu {
            &self.submenu_actionable_indices
        } else {
            &self.actionable_indices
        };
        let edge = if last {
            indices.last().copied()
        } else {
            indices.first().copied()
        };
        if let Some(index) = edge {
            if in_submenu {
                self.state.submenu_selected_index = Some(index);
            } else {
                self.state.selected_index = Some(index);
            }
            cx.notify();
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.actionable_indices.is_empty() {
            self.update_actionable_indices();
        }

        let (indices, selected) = if self.state.active_submenu.is_some() {
            (
                &self.submenu_actionable_indices,
                self.state.submenu_selected_index,
            )
        } else {
            (&self.actionable_indices, self.state.selected_index)
        };
        if indices.is_empty() {
            return;
        }

        let current_pos = selected.and_then(|idx| indices.iter().position(|&item| item == idx));
        let next_pos = match (current_pos, delta.is_negative()) {
            (Some(0), true) | (None, true) => indices.len() - 1,
            (Some(pos), true) => pos - 1,
            (Some(pos), false) => (pos + 1) % indices.len(),
            (None, false) => 0,
        };
        let next_index = indices[next_pos];

        if self.state.active_submenu.is_some() {
            self.state.submenu_selected_index = Some(next_index);
        } else {
            self.state.selected_index = Some(next_index);
        }
    }

    /// Open the submenu on the selected root item.
    pub fn open_selected_submenu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.state.selected_index else {
            return false;
        };
        if self
            .build_items()
            .get(index)
            .and_then(ContextMenuItem::submenu_items)
            .is_none()
        {
            return false;
        }

        self.activate_submenu(index);
        cx.notify();
        true
    }

    /// Close the active submenu while keeping the root menu open.
    pub fn close_submenu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.state.active_submenu.take().is_none() {
            return false;
        }

        self.state.submenu_selected_index = None;
        self.submenu_actionable_indices.clear();
        cx.notify();
        true
    }

    fn activate_submenu(&mut self, index: usize) {
        self.state.selected_index = Some(index);
        self.state.active_submenu = Some(index);
        self.state.submenu_selected_index = None;
        self.update_submenu_actionable_indices(index);
    }

    fn select_primary_action(&mut self, index: usize) {
        self.state.selected_index = Some(index);
        self.state.active_submenu = None;
        self.state.submenu_selected_index = None;
        self.submenu_actionable_indices.clear();
    }

    /// Confirm the current selection
    pub fn confirm_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let items = self.build_items();

        if let Some(submenu_index) = self.state.active_submenu {
            let action = items
                .get(submenu_index)
                .and_then(ContextMenuItem::submenu_items)
                .and_then(|items| {
                    self.state
                        .submenu_selected_index
                        .and_then(|idx| items.get(idx))
                })
                .and_then(|item| match item {
                    ContextMenuItem::Action {
                        action: Some(action),
                        ..
                    } => Some(action.clone()),
                    _ => None,
                });
            if let Some(action) = action {
                action();
                self.state.close();
                cx.notify();
                return true;
            }
            return false;
        }

        if let Some(idx) = self.state.selected_index {
            match items.get(idx) {
                Some(ContextMenuItem::Action {
                    action: Some(action),
                    ..
                }) => {
                    action();
                    self.state.close();
                    cx.notify();
                    return true;
                }
                Some(ContextMenuItem::Submenu { .. }) => {
                    self.activate_submenu(idx);
                    cx.notify();
                }
                _ => {}
            }
        }
        false
    }

    /// Check if context menu is open
    pub fn is_open(&self) -> bool {
        self.state.is_open
    }
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }

        super::clear_global_tooltip();

        if self.actionable_indices.is_empty() {
            self.update_actionable_indices();
        }

        let items = self.build_items();
        let submenu = self.state.active_submenu.and_then(|idx| {
            items
                .get(idx)
                .and_then(ContextMenuItem::submenu_items)
                .map(<[ContextMenuItem]>::to_vec)
        });
        let viewport = window.viewport_size();
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            CONTEXT_MENU_WIDTH,
            viewport_height,
            CONTEXT_MENU_MARGIN,
        );
        let panel_width = fit.width;
        let available_height = fit.height;
        let primary_height = estimated_menu_height(&items).min(available_height);
        let primary_left = crate::utils::clamp_overlay_axis(
            self.state.x,
            panel_width,
            viewport_width,
            fit.margin_x,
        );
        let primary_top = crate::utils::clamp_overlay_axis(
            self.state.y,
            primary_height,
            viewport_height,
            fit.margin_y,
        );
        let submenu_anchor_y = self
            .state
            .active_submenu
            .map(|index| primary_top + menu_item_offset(&items, index))
            .unwrap_or(primary_top);
        let selected_idx = self.state.selected_index;
        let submenu_selected_idx = self.state.submenu_selected_index;
        let primary_panel = div()
            .absolute()
            .left(px(primary_left))
            .top(px(primary_top))
            .child(render_menu_panel(
                items,
                selected_idx,
                MenuLevel::Primary,
                panel_width,
                available_height,
                cx,
            ));
        let submenu_panel = submenu.map(|items| {
            let open_right = primary_left + panel_width * 2.0 - CONTEXT_MENU_PANEL_OVERLAP
                + fit.margin_x
                <= viewport_width;
            let requested_left = if open_right {
                primary_left + panel_width - CONTEXT_MENU_PANEL_OVERLAP
            } else {
                primary_left - panel_width + CONTEXT_MENU_PANEL_OVERLAP
            };
            let left = crate::utils::clamp_overlay_axis(
                requested_left,
                panel_width,
                viewport_width,
                fit.margin_x,
            );
            let height = estimated_menu_height(&items).min(available_height);
            let top = crate::utils::clamp_overlay_axis(
                submenu_anchor_y,
                height,
                viewport_height,
                fit.margin_y,
            );

            div()
                .absolute()
                .left(px(left))
                .top(px(top))
                .child(render_menu_panel(
                    items,
                    submenu_selected_idx,
                    MenuLevel::Secondary,
                    panel_width,
                    available_height,
                    cx,
                ))
        });

        div()
            .id("context-menu")
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .child(primary_panel)
            .children(submenu_panel)
            .into_any_element()
    }
}

impl ContextMenu {
    fn build_items(&self) -> Vec<ContextMenuItem> {
        self.build_items_for(&self.state.menu_type)
    }

    fn build_items_for(&self, menu_type: &ContextMenuType) -> Vec<ContextMenuItem> {
        match menu_type {
            ContextMenuType::None => vec![],
            ContextMenuType::Message {
                message_id,
                content,
                sender,
                display_name,
                timestamp,
                channel,
                source_split_id,
                allow_remote_actions,
                is_whisper,
                highlight_type,
            } => self.build_message_menu(
                message_id,
                content,
                sender,
                display_name,
                *timestamp,
                channel,
                *source_split_id,
                *allow_remote_actions,
                *is_whisper,
                highlight_type.as_ref(),
            ),
            ContextMenuType::Emote {
                emotes,
                rule_channel,
                rules,
            } => self.build_emote_menu(emotes, rule_channel, rules),
            ContextMenuType::Link {
                url,
                message_id,
                embed_id,
                resource_key,
                preview_visible,
            } => self.build_link_menu(url, message_id, *embed_id, resource_key, *preview_visible),
            ContextMenuType::Selection { text } => self.build_selection_menu(text),
            ContextMenuType::Composer {
                split_id,
                selection,
                has_text,
                can_undo,
                can_redo,
            } => self.build_composer_menu(
                *split_id,
                selection.as_ref(),
                *has_text,
                *can_undo,
                *can_redo,
            ),
            ContextMenuType::Chatter {
                username,
                display_name,
                user_id,
                channel,
                show_user_card,
            } => self.build_chatter_menu(username, display_name, user_id, channel, *show_user_card),
            ContextMenuType::Composite(sections) => {
                let mut items = Vec::new();
                for section in sections {
                    let section_items = self.build_items_for(section);
                    if section_items.is_empty() {
                        continue;
                    }
                    if !items.is_empty() {
                        items.push(ContextMenuItem::divider());
                    }
                    items.extend(section_items);
                }
                items
            }
        }
    }

    fn build_selection_menu(&self, text: &str) -> Vec<ContextMenuItem> {
        let text = text.to_string();

        vec![ContextMenuItem::action("Copy selection", move || {
            utils::copy_text(&text);
        })]
    }

    fn build_composer_menu(
        &self,
        split_id: SplitId,
        selection: Option<&ComposerSelectionSnapshot>,
        has_text: bool,
        can_undo: bool,
        can_redo: bool,
    ) -> Vec<ContextMenuItem> {
        use crate::backend::ui_actions::{ComposerEditAction, push_pending_composer_edit};

        let mut items = Vec::with_capacity(9);
        items.push(if can_undo {
            ContextMenuItem::action("Undo", move || {
                push_pending_composer_edit(split_id, ComposerEditAction::Undo);
            })
        } else {
            ContextMenuItem::disabled("Undo")
        });
        items.push(if can_redo {
            ContextMenuItem::action("Redo", move || {
                push_pending_composer_edit(split_id, ComposerEditAction::Redo);
            })
        } else {
            ContextMenuItem::disabled("Redo")
        });
        items.push(ContextMenuItem::divider());

        if let Some(selection) = selection {
            let range = selection.range.clone();
            let expected_text = selection.text.clone();
            items.push(ContextMenuItem::action("Cut", move || {
                push_pending_composer_edit(
                    split_id,
                    ComposerEditAction::DeleteSelection {
                        range: range.clone(),
                        expected_text: expected_text.clone(),
                        copy: true,
                    },
                );
            }));
            let copied = selection.text.clone();
            items.push(ContextMenuItem::action("Copy", move || {
                utils::copy_text(&copied);
            }));
        } else {
            items.push(ContextMenuItem::disabled("Cut"));
            items.push(ContextMenuItem::disabled("Copy"));
        }
        items.push(ContextMenuItem::action("Paste", move || {
            push_pending_composer_edit(split_id, ComposerEditAction::PasteClipboard);
        }));
        items.push(if let Some(selection) = selection {
            let range = selection.range.clone();
            let expected_text = selection.text.clone();
            ContextMenuItem::action("Delete", move || {
                push_pending_composer_edit(
                    split_id,
                    ComposerEditAction::DeleteSelection {
                        range: range.clone(),
                        expected_text: expected_text.clone(),
                        copy: false,
                    },
                );
            })
        } else {
            ContextMenuItem::disabled("Delete")
        });
        items.push(ContextMenuItem::divider());
        items.push(if has_text {
            ContextMenuItem::action("Select all", move || {
                push_pending_composer_edit(split_id, ComposerEditAction::SelectAll);
            })
        } else {
            ContextMenuItem::disabled("Select all")
        });
        items
    }

    fn build_message_menu(
        &self,
        message_id: &str,
        content: &str,
        sender: &str,
        display_name: &str,
        timestamp: i64,
        channel: &str,
        source_split_id: Option<SplitId>,
        allow_remote_actions: bool,
        is_whisper: bool,
        highlight_type: Option<&backend::HighlightType>,
    ) -> Vec<ContextMenuItem> {
        let content = content.to_string();
        let sender = sender.to_string();
        let channel = channel.to_string();
        let message_id = message_id.to_string();
        let display_name = display_name.to_string();
        let copied_display_name = if display_name.trim().is_empty() {
            sender.clone()
        } else {
            display_name.clone()
        };
        let full_message = format!(
            "[{}] {}: {}",
            utils::format_time_24h_sec(timestamp),
            copied_display_name,
            content
        );

        let mut items = vec![
            ContextMenuItem::action("Copy message", {
                let content = content.clone();
                move || {
                    utils::copy_text(&content);
                }
            }),
            ContextMenuItem::action("Copy full message", move || {
                utils::copy_text(&full_message);
            }),
            ContextMenuItem::action("Insert message text", {
                let channel = channel.clone();
                let content = content.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::InsertMessageTextIntoComposer {
                        channel: channel.clone(),
                        source_split_id,
                        text: content.clone(),
                        quote: false,
                    });
                }
            }),
            ContextMenuItem::action("Quote into composer", {
                let channel = channel.clone();
                let content = content.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::InsertMessageTextIntoComposer {
                        channel: channel.clone(),
                        source_split_id,
                        text: content.clone(),
                        quote: true,
                    });
                }
            }),
        ];

        if is_whisper {
            items.push(ContextMenuItem::action("Reply to whisper", {
                let sender = sender.clone();
                let channel = channel.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::ComposeWhisper {
                        username: sender.clone(),
                        channel: Some(channel.clone()),
                    });
                }
            }));
        } else if allow_remote_actions {
            items.push(ContextMenuItem::action("Reply to message", {
                let channel = channel.clone();
                let message_id = message_id.clone();
                let sender = sender.clone();
                let display_name = display_name.clone();
                let content = content.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::Reply {
                        channel: channel.clone(),
                        source_split_id,
                        message_id: message_id.clone(),
                        sender: sender.clone(),
                        display_name: display_name.clone(),
                        content: content.clone(),
                    });
                }
            }));

            items.push(ContextMenuItem::action("View thread", {
                let channel = channel.clone();
                let message_id = message_id.clone();
                let x = self.state.x;
                let y = self.state.y;
                move || {
                    backend::push_ui_action(backend::UiAction::ViewThread {
                        message_id: message_id.clone(),
                        channel: channel.clone(),
                        x,
                        y,
                    });
                }
            }));

            if crate::components::common::moderation_policy::can_moderate_target(
                &channel,
                &sender,
                &[],
            ) {
                let moderation_items =
                    configured_message_mod_action_items(&sender, &channel, &message_id, &content);
                if !moderation_items.is_empty() {
                    items.push(ContextMenuItem::submenu("Moderate", moderation_items));
                }
            }
        }

        if let Some(highlight_type) = highlight_type.cloned() {
            items.push(ContextMenuItem::divider());
            items.push(ContextMenuItem::action(
                "Why is this highlighted?",
                move || {
                    backend::push_ui_action(backend::UiAction::ExplainHighlight {
                        highlight_type: highlight_type.clone(),
                    });
                },
            ));
        }

        items
    }

    fn build_emote_menu(
        &self,
        emotes: &[EmoteContextTarget],
        rule_channel: &str,
        rules: &[backend::settings::EmoteDisplayRule],
    ) -> Vec<ContextMenuItem> {
        if emotes.is_empty() {
            return Vec::new();
        }

        let copied_emotes = emotes
            .iter()
            .map(|emote| emote.name.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let copy_label = if emotes.len() == 1 {
            "Copy emote"
        } else {
            "Copy emote names"
        };
        let mut items = vec![ContextMenuItem::action(copy_label, move || {
            utils::copy_text(&copied_emotes);
        })];

        let open_items = build_emote_link_items(emotes, EmoteLinkAction::Open);
        if !open_items.is_empty() {
            items.push(ContextMenuItem::submenu("Open", open_items));
        }

        let copy_items = build_emote_link_items(emotes, EmoteLinkAction::Copy);
        if !copy_items.is_empty() {
            items.push(ContextMenuItem::submenu("Copy links", copy_items));
        }

        let rule_items = build_emote_rule_items(emotes, rule_channel, rules);
        if !rule_items.is_empty() {
            items.push(ContextMenuItem::divider());
            items.extend(rule_items);
        }

        items
    }

    fn build_link_menu(
        &self,
        url: &str,
        message_id: &str,
        embed_id: u32,
        resource_key: &crate::backend::link_types::ResourceKey,
        preview_visible: bool,
    ) -> Vec<ContextMenuItem> {
        let url = url.to_string();
        let message_id = message_id.to_string();
        let resource_key = resource_key.clone();

        let mut items = vec![
            ContextMenuItem::action("Open link", {
                let url = url.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
                        url: url.clone(),
                        mode: backend::ui_actions::LinkOpenMode::Configured,
                    });
                }
            }),
            ContextMenuItem::submenu(
                "Open with",
                vec![
                    ContextMenuItem::action("Normal window", {
                        let url = url.clone();
                        move || {
                            backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
                                url: url.clone(),
                                mode: backend::ui_actions::LinkOpenMode::Normal,
                            });
                        }
                    }),
                    ContextMenuItem::action("Incognito window", {
                        let url = url.clone();
                        move || {
                            backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
                                url: url.clone(),
                                mode: backend::ui_actions::LinkOpenMode::Incognito,
                            });
                        }
                    }),
                ],
            ),
            ContextMenuItem::divider(),
            ContextMenuItem::action("Copy link", {
                let url = url.clone();
                move || {
                    utils::copy_text(&url);
                }
            }),
        ];

        if embed_id == crate::backend::upload_history::UPLOAD_LINK_NO_PREVIEW
            || crate::backend::upload_history::is_known_upload_deletion_url(&url)
        {
            return items;
        }

        let preview_item = if preview_visible {
            ContextMenuItem::action("Hide preview", {
                let message_id = message_id.clone();
                let resource_key = resource_key.clone();
                move || {
                    backend::bridge::hide_embed_preview_occurrence(
                        crate::backend::embeds::state::PreviewOccurrence {
                            message_id: message_id.clone(),
                            embed_id,
                            resource_key: resource_key.clone(),
                        },
                    )
                }
            })
        } else {
            ContextMenuItem::action("Show preview", {
                let url = url.clone();
                let message_id = message_id.clone();
                let resource_key = resource_key.clone();
                move || {
                    backend::bridge::show_embed_preview_occurrence(
                        url.clone(),
                        crate::backend::embeds::state::PreviewOccurrence {
                            message_id: message_id.clone(),
                            embed_id,
                            resource_key: resource_key.clone(),
                        },
                    )
                }
            })
        };
        if !matches!(
            backend::settings::get_embed_display_mode(),
            backend::settings::EmbedDisplayMode::None
        ) {
            items.insert(0, preview_item);
        }
        items
    }

    fn build_chatter_menu(
        &self,
        username: &str,
        display_name: &str,
        user_id: &Option<String>,
        channel: &str,
        show_user_card: bool,
    ) -> Vec<ContextMenuItem> {
        let username = username.to_string();
        let display_name = display_name.to_string();
        let channel = channel.to_string();

        let mut extra_copy_items = Vec::with_capacity(2);
        if !display_name.eq_ignore_ascii_case(&username) {
            extra_copy_items.push(ContextMenuItem::action("Copy display name", {
                let display_name = display_name.clone();
                move || {
                    utils::copy_text(&display_name);
                }
            }));
        }

        if let Some(user_id) = user_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            let user_id = user_id.to_string();
            extra_copy_items.push(ContextMenuItem::action("Copy user ID", move || {
                utils::copy_text(&user_id);
            }));
        }

        let mut items = vec![
            ContextMenuItem::header(
                display_name.clone(),
                (!display_name.eq_ignore_ascii_case(&username)).then(|| format!("@{username}")),
            ),
            ContextMenuItem::divider(),
            ContextMenuItem::action("Copy username", {
                let username = username.clone();
                move || {
                    utils::copy_text(&username);
                }
            }),
            ContextMenuItem::action("Mention user", {
                let username = username.clone();
                let channel = channel.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::InsertMention {
                        username: username.clone(),
                        channel: channel.clone(),
                        source_split_id: None,
                    });
                }
            }),
            ContextMenuItem::action("Whisper", {
                let username = username.clone();
                let channel = channel.clone();
                move || {
                    backend::push_ui_action(backend::UiAction::ComposeWhisper {
                        username: username.clone(),
                        channel: Some(channel.clone()),
                    });
                }
            }),
        ];

        if show_user_card {
            items.push(ContextMenuItem::action("View user card", {
                let username = username.clone();
                let display_name = display_name.clone();
                let user_id = user_id.clone();
                let channel = channel.clone();
                let x = self.state.x;
                let y = self.state.y;
                move || {
                    backend::push_ui_action(backend::UiAction::ShowUserCard {
                        username: username.clone(),
                        display_name: display_name.clone(),
                        user_id: user_id.clone(),
                        channel: channel.clone(),
                        color: None,
                        badges: Vec::new(),
                        x,
                        y,
                    });
                }
            }));
        }

        if !extra_copy_items.is_empty() {
            items.push(ContextMenuItem::submenu(
                "More copy options",
                extra_copy_items,
            ));
        }

        items
    }
}

#[derive(Clone, Copy)]
enum EmoteLinkAction {
    Open,
    Copy,
}

fn resolve_emote_context_target(
    channel: &str,
    name: String,
    rendered_url: String,
    meta: Option<&EmoteTooltipMeta>,
) -> EmoteContextTarget {
    let cached_emote = backend::emotes::get_emote_fast_case_sensitive(channel, &name)
        .or_else(|| backend::emotes::get_emote_fast_lowercased(channel, &name));
    let rule = meta.and_then(|meta| emote_rule_target(&name, meta));

    let Some(emote) = cached_emote else {
        return EmoteContextTarget {
            rule,
            name,
            provider_label: meta
                .map(|meta| meta.provider_label.to_string())
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Emote".to_string()),
            image_links: (!rendered_url.trim().is_empty())
                .then(|| EmoteContextLink {
                    scale_label: String::new(),
                    url: rendered_url,
                })
                .into_iter()
                .collect(),
            provider_page_url: None,
        };
    };

    let mut image_links = Vec::with_capacity(3);
    for (scale_label, url) in [
        ("1×", emote.url_1x.as_deref()),
        ("2×", emote.url_2x.as_deref()),
        ("4×", emote.url_4x.as_deref()),
    ] {
        let Some(url) = url.map(str::trim).filter(|url| !url.is_empty()) else {
            continue;
        };
        if image_links
            .iter()
            .any(|link: &EmoteContextLink| link.url == url)
        {
            continue;
        }
        image_links.push(EmoteContextLink {
            scale_label: scale_label.to_string(),
            url: url.to_string(),
        });
    }

    if image_links.is_empty() {
        let fallback_url = emote.url.trim();
        if !fallback_url.is_empty() {
            image_links.push(EmoteContextLink {
                scale_label: String::new(),
                url: fallback_url.to_string(),
            });
        } else if !rendered_url.trim().is_empty() {
            image_links.push(EmoteContextLink {
                scale_label: String::new(),
                url: rendered_url,
            });
        }
    }

    let provider_label = match emote.provider {
        backend::emotes::EmoteProvider::Twitch => "Twitch",
        backend::emotes::EmoteProvider::SevenTV => "7TV",
        backend::emotes::EmoteProvider::BTTV => "BTTV",
        backend::emotes::EmoteProvider::FFZ => "FFZ",
        backend::emotes::EmoteProvider::Emoji => "Emoji",
    }
    .to_string();
    let provider_page_url = match emote.provider {
        backend::emotes::EmoteProvider::SevenTV => {
            Some(format!("https://7tv.app/emotes/{}", emote.id))
        }
        backend::emotes::EmoteProvider::BTTV => {
            Some(format!("https://betterttv.com/emotes/{}", emote.id))
        }
        backend::emotes::EmoteProvider::FFZ => Some(format!(
            "https://www.frankerfacez.com/emoticon/{}-{}",
            emote.id,
            urlencoding::encode(emote.name.as_ref())
        )),
        backend::emotes::EmoteProvider::Twitch | backend::emotes::EmoteProvider::Emoji => None,
    };

    EmoteContextTarget {
        name,
        provider_label,
        image_links,
        provider_page_url,
        rule,
    }
}

/// The emote a display rule should name: the rendered emote's own provider and
/// id, never a by-name lookup, which misses tag-only Twitch emotes and can pick
/// another provider's emote of the same name. Emoji carry no emote metadata and
/// get no rules: they have no emote identity, and how they draw is already the
/// Emoji style setting.
fn emote_rule_target(name: &str, meta: &EmoteTooltipMeta) -> Option<EmoteRuleTarget> {
    use echo_core::emote_display_rules::provider_from_key;

    provider_from_key(meta.provider_key)
        .filter(|_| !meta.emote_id.is_empty())
        .map(|_| EmoteRuleTarget {
            emote: echo_core::emote_preferences::EmoteRef::new(
                &meta.emote_id,
                meta.provider_key,
                name,
            ),
            is_animated: meta.is_animated,
        })
}

/// Display-rule entries for the targeted emotes: "Show as text" and, for
/// animated emotes, "Use still image", each Everywhere or only in this channel,
/// plus "Show image again" / "Animate again" while a rule applies here. A
/// stack gets one submenu per emote with the same entries flattened.
fn build_emote_rule_items(
    emotes: &[EmoteContextTarget],
    rule_channel: &str,
    rules: &[backend::settings::EmoteDisplayRule],
) -> Vec<ContextMenuItem> {
    let targets: Vec<(&str, &EmoteRuleTarget)> = emotes
        .iter()
        .filter_map(|emote| emote.rule.as_ref().map(|rule| (emote.name.as_str(), rule)))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }
    let channel = rule_scope_channel(rule_channel);

    if let [(name, target)] = targets.as_slice()
        && emotes.len() == 1
    {
        return emote_rule_items(name, target, rules, channel.as_deref(), false);
    }
    targets
        .into_iter()
        .filter_map(|(name, target)| {
            let items = emote_rule_items(name, target, rules, channel.as_deref(), true);
            (!items.is_empty()).then(|| ContextMenuItem::submenu(name, items))
        })
        .collect()
}

/// The targets' rules, copied once when the menu opens so building the menu
/// on every render never touches the settings lock.
fn emote_rules_for_targets(
    targets: &[EmoteContextTarget],
) -> Vec<backend::settings::EmoteDisplayRule> {
    if targets.iter().all(|target| target.rule.is_none()) {
        return Vec::new();
    }
    let mut rules = backend::settings::get_emote_display_rules();
    rules.retain(|rule| {
        targets
            .iter()
            .filter_map(|target| target.rule.as_ref())
            .any(|target| {
                target.emote.id == rule.emote.id && target.emote.provider == rule.emote.provider
            })
    });
    rules
}

/// The login an "Only in #channel" rule may name, or `None` for the Whispers
/// leaf, the Watching sentinel and any other value that is not a Twitch login.
fn rule_scope_channel(channel: &str) -> Option<String> {
    let login = channel.trim().trim_start_matches('#');
    let is_login = login
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && login
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    (is_login
        && !echo_core::channel_identity::is_whispers_channel(login)
        && !echo_core::channel_identity::is_watching_channel(login))
    .then(|| login.to_ascii_lowercase())
}

fn emote_rule_items(
    name: &str,
    target: &EmoteRuleTarget,
    rules: &[backend::settings::EmoteDisplayRule],
    channel: Option<&str>,
    flat: bool,
) -> Vec<ContextMenuItem> {
    use backend::settings::{EmoteDisplayFlag, EmoteRuleChange, EmoteRuleScope};
    use echo_core::emote_display_rules::{rule_flags_at, scopes_applying};

    let mut items = Vec::new();
    for flag in [EmoteDisplayFlag::ShowAsText, EmoteDisplayFlag::StillImage] {
        if flag == EmoteDisplayFlag::StillImage && !target.is_animated {
            continue;
        }
        let (verb, applied, restore, restored) = match flag {
            EmoteDisplayFlag::ShowAsText => (
                "Show as text",
                "now shows as text",
                "Show image again",
                "shows as an image again",
            ),
            EmoteDisplayFlag::StillImage => (
                "Use still image",
                "now shows as a still image",
                "Animate again",
                "animates again",
            ),
        };

        let mut offers = Vec::with_capacity(2);
        if !rule_flags_at(rules, &target.emote, &EmoteRuleScope::Everywhere).get(flag) {
            offers.push(EmoteRuleScope::Everywhere);
            if let Some(channel) = channel
                && !rule_flags_at(rules, &target.emote, &EmoteRuleScope::channel(channel)).get(flag)
            {
                offers.push(EmoteRuleScope::channel(channel));
            }
        }
        let offer_items: Vec<_> = offers
            .into_iter()
            .map(|scope| {
                let place = scope_phrase(std::slice::from_ref(&scope));
                let label = match (&scope, flat) {
                    (EmoteRuleScope::Everywhere, false) => "Everywhere".to_string(),
                    (EmoteRuleScope::Channel(login), false) => format!("Only in #{login}"),
                    (EmoteRuleScope::Everywhere, true) => format!("{verb} everywhere"),
                    (EmoteRuleScope::Channel(login), true) => format!("{verb} only in #{login}"),
                };
                let change = EmoteRuleChange {
                    emote: target.emote.clone(),
                    scope,
                    flag,
                    value: true,
                };
                ContextMenuItem::action(
                    label,
                    emote_rule_action(vec![change], format!("\"{name}\" {applied} {place}")),
                )
            })
            .collect();
        if flat {
            items.extend(offer_items);
        } else if !offer_items.is_empty() {
            items.push(ContextMenuItem::submenu(verb, offer_items));
        }

        let covering = scopes_applying(rules, &target.emote, channel.unwrap_or_default(), flag);
        if !covering.is_empty() {
            let message = format!("\"{name}\" {restored} {}", scope_phrase(&covering));
            let changes = covering
                .into_iter()
                .map(|scope| EmoteRuleChange {
                    emote: target.emote.clone(),
                    scope,
                    flag,
                    value: false,
                })
                .collect();
            items.push(ContextMenuItem::action(
                restore,
                emote_rule_action(changes, message),
            ));
        }
    }
    items
}

/// "everywhere", "in #xqc", or both joined.
fn scope_phrase(scopes: &[backend::settings::EmoteRuleScope]) -> String {
    scopes
        .iter()
        .map(|scope| match scope {
            backend::settings::EmoteRuleScope::Everywhere => "everywhere".to_string(),
            backend::settings::EmoteRuleScope::Channel(login) => format!("in #{login}"),
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Apply rule changes and offer their undo in a toast, which the UI thread
/// shows when it drains the action queue.
fn emote_rule_action(
    changes: Vec<backend::settings::EmoteRuleChange>,
    message: String,
) -> impl Fn() + Send + Sync + 'static {
    move || {
        let undo = backend::settings::apply_emote_display_rule_changes(&changes);
        if !undo.is_empty() {
            backend::push_ui_action(backend::UiAction::ShowEmoteDisplayRuleToast {
                message: message.clone(),
                undo,
            });
        }
    }
}

fn build_emote_link_items(
    emotes: &[EmoteContextTarget],
    action: EmoteLinkAction,
) -> Vec<ContextMenuItem> {
    let mut items = Vec::new();
    let include_emote_name = emotes.len() > 1;

    for emote in emotes {
        let mut group = Vec::new();
        for image in &emote.image_links {
            let image_label = if image.scale_label.is_empty() {
                "Image".to_string()
            } else {
                format!("{} Image", image.scale_label)
            };
            let label = emote_link_label(
                &emote.name,
                &match action {
                    EmoteLinkAction::Open => image_label,
                    EmoteLinkAction::Copy => format!("{image_label} Link"),
                },
                include_emote_name,
            );
            group.push(emote_url_action(label, image.url.clone(), action));
        }

        if let Some(page_url) = emote.provider_page_url.as_ref() {
            if !group.is_empty() {
                group.push(ContextMenuItem::divider());
            }
            let page_label = match action {
                EmoteLinkAction::Open => format!("{} Emote Page", emote.provider_label),
                EmoteLinkAction::Copy => format!("{} Emote Page Link", emote.provider_label),
            };
            group.push(emote_url_action(
                emote_link_label(&emote.name, &page_label, include_emote_name),
                page_url.clone(),
                action,
            ));
        }

        if group.is_empty() {
            continue;
        }
        if !items.is_empty() {
            items.push(ContextMenuItem::divider());
        }
        items.extend(group);
    }

    items
}

fn emote_link_label(emote_name: &str, action_label: &str, include_emote_name: bool) -> String {
    if include_emote_name {
        format!("{emote_name} · {action_label}")
    } else {
        action_label.to_string()
    }
}

fn emote_url_action(label: String, url: String, action: EmoteLinkAction) -> ContextMenuItem {
    match action {
        EmoteLinkAction::Open => ContextMenuItem::action(label, move || {
            queue_confirm_open_link(url.clone());
        }),
        EmoteLinkAction::Copy => ContextMenuItem::action(label, move || {
            utils::copy_text(&url);
        }),
    }
}

fn queue_confirm_open_link(url: String) {
    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
        url,
        mode: backend::ui_actions::LinkOpenMode::Configured,
    });
}

fn configured_message_mod_action_items(
    username: &str,
    channel: &str,
    message_id: &str,
    content: &str,
) -> Vec<ContextMenuItem> {
    let buttons = backend::settings::get_mod_buttons();
    let mut items = Vec::with_capacity(buttons.len());

    for button in buttons {
        let raw_command = button.command.trim();
        if button.label.trim().is_empty()
            || raw_command.is_empty()
            || !button.requires_message_context()
        {
            continue;
        }

        let label = if raw_command
            .split_whitespace()
            .next()
            .is_some_and(|command| command.eq_ignore_ascii_case("/delete"))
        {
            "Delete message".to_string()
        } else {
            button.label.clone()
        };
        let command = button.resolve_for_context(username, message_id, content, channel);
        let channel = channel.to_string();
        let username = username.to_string();
        let action = move || {
            if crate::components::common::moderation_policy::recheck_moderation_target(
                &channel, &username,
            ) {
                backend::send_message_or_command_async(channel.clone(), command.clone());
            }
        };

        if button.is_destructive {
            items.push(ContextMenuItem::destructive(label, action));
        } else {
            items.push(ContextMenuItem::action(label, action));
        }
    }

    items
}

fn estimated_menu_height(items: &[ContextMenuItem]) -> f32 {
    CONTEXT_MENU_PANEL_PADDING + items.iter().map(menu_item_height).sum::<f32>()
}

fn menu_item_offset(items: &[ContextMenuItem], index: usize) -> f32 {
    CONTEXT_MENU_PANEL_PADDING / 2.0 + items.iter().take(index).map(menu_item_height).sum::<f32>()
}

fn menu_item_height(item: &ContextMenuItem) -> f32 {
    match item {
        ContextMenuItem::Action { .. } | ContextMenuItem::Submenu { .. } => {
            CONTEXT_MENU_ACTION_HEIGHT
        }
        ContextMenuItem::Divider => CONTEXT_MENU_DIVIDER_HEIGHT,
        ContextMenuItem::Header { .. } => CONTEXT_MENU_HEADER_HEIGHT,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuLevel {
    Primary,
    Secondary,
}

fn render_menu_panel(
    items: Vec<ContextMenuItem>,
    selected_idx: Option<usize>,
    level: MenuLevel,
    panel_width: f32,
    max_height: f32,
    cx: &mut Context<ContextMenu>,
) -> AnyElement {
    let panel_id = match level {
        MenuLevel::Primary => "context-primary-panel",
        MenuLevel::Secondary => "context-secondary-panel",
    };

    div()
        .id(panel_id)
        .w(px(panel_width))
        .max_h(px(max_height))
        .bg(colors::bg_elevated())
        .rounded(Radii::md())
        .border_1()
        .border_color(colors::border())
        .shadow_lg()
        .py(px(MenuMetrics::PANEL_PADDING / 2.0))
        .overflow_y_scroll()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .children(
            items
                .into_iter()
                .enumerate()
                .map(|(idx, item)| render_menu_item(item, idx, selected_idx, level, cx)),
        )
        .into_any_element()
}

fn render_menu_item(
    item: ContextMenuItem,
    idx: usize,
    selected_idx: Option<usize>,
    level: MenuLevel,
    cx: &mut Context<ContextMenu>,
) -> AnyElement {
    let is_selected = selected_idx == Some(idx);
    let item_id = match level {
        MenuLevel::Primary => ("context-primary-item", idx),
        MenuLevel::Secondary => ("context-secondary-item", idx),
    };

    match item {
        ContextMenuItem::Action {
            label,
            icon,
            is_destructive,
            action,
        } => {
            let enabled = action.is_some();
            let text_color = if !enabled {
                colors::text_muted()
            } else if is_destructive {
                colors::error()
            } else {
                colors::text_primary()
            };

            let action_item_id = ElementId::Name(SharedString::from(format!(
                "{}-{}-{}",
                item_id.0, item_id.1, label
            )));
            let row = div()
                .id(action_item_id)
                .w_full()
                .h(px(MenuMetrics::ITEM_HEIGHT))
                .px(px(MenuMetrics::ITEM_PADDING_X))
                .cursor_default()
                .when(is_selected && enabled, |s| s.bg(colors::bg_hover()))
                .when(enabled, |row| row.hover(|s| s.bg(colors::bg_hover())));
            let row = if enabled {
                row.on_press_click(cx.listener(move |this, _, _window, cx| {
                    if let Some(ref action) = action {
                        action();
                    }
                    this.state.close();
                    backend::push_ui_action(backend::UiAction::CloseContextMenu);
                    cx.notify();
                }))
                .on_mouse_move(cx.listener(move |this, _, _window, cx| {
                    let changed = match level {
                        MenuLevel::Primary => {
                            let changed = this.state.selected_index != Some(idx)
                                || this.state.active_submenu.is_some();
                            if changed {
                                this.select_primary_action(idx);
                            }
                            changed
                        }
                        MenuLevel::Secondary => {
                            let changed = this.state.submenu_selected_index != Some(idx);
                            if changed {
                                this.state.submenu_selected_index = Some(idx);
                            }
                            changed
                        }
                    };
                    if changed {
                        cx.notify();
                    }
                }))
            } else {
                row
            };
            row.flex()
                .items_center()
                .gap(px(MenuMetrics::ITEM_GAP))
                .when_some(icon.clone(), |el, icon_str| {
                    el.child(
                        div()
                            .text_size(px(MenuMetrics::ITEM_TEXT))
                            .text_color(colors::text_muted())
                            .child(icon_str),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_size(px(MenuMetrics::ITEM_TEXT))
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(text_color)
                        .font_weight(if is_selected {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .child(label),
                )
                .into_any_element()
        }
        ContextMenuItem::Submenu { label, items: _ } => div()
            .id(item_id)
            .w_full()
            .h(px(MenuMetrics::ITEM_HEIGHT))
            .px(px(MenuMetrics::ITEM_PADDING_X))
            .cursor_default()
            .when(is_selected, |s| s.bg(colors::bg_hover()))
            .hover(|s| s.bg(colors::bg_hover()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _window, cx| {
                    if level == MenuLevel::Primary {
                        this.activate_submenu(idx);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, _, _window, cx| {
                if level == MenuLevel::Primary
                    && (this.state.active_submenu != Some(idx)
                        || this.state.selected_index != Some(idx))
                {
                    this.activate_submenu(idx);
                    cx.notify();
                }
            }))
            .flex()
            .items_center()
            .gap(px(MenuMetrics::ITEM_GAP))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_size(px(MenuMetrics::ITEM_TEXT))
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(colors::text_primary())
                    .font_weight(if is_selected {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(MenuMetrics::ITEM_TEXT))
                    .text_color(colors::text_muted())
                    .child("›"),
            )
            .into_any_element(),
        ContextMenuItem::Divider => div()
            .w_full()
            .h(px(1.0))
            .my(px((MenuMetrics::DIVIDER_HEIGHT - 1.0) / 2.0))
            .bg(colors::border())
            .into_any_element(),
        ContextMenuItem::Header { title, subtitle } => div()
            .w_full()
            .h(px(MenuMetrics::HEADER_HEIGHT))
            .px(px(MenuMetrics::ITEM_PADDING_X))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(Spacing::HAIRLINE))
            .child(
                div()
                    .w_full()
                    .text_size(px(MenuMetrics::ITEM_TEXT))
                    .line_height(px(15.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(title),
            )
            .children(subtitle.map(|subtitle| {
                div()
                    .w_full()
                    .text_size(px(MenuMetrics::SECONDARY_TEXT))
                    .line_height(px(13.0))
                    .text_color(colors::text_muted())
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(subtitle)
            }))
            .into_any_element(),
    }
}
