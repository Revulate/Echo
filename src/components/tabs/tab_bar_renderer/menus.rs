//! The tab bar's pop-up menus: the per-tab context menu, the Dropdown-mode
//! overflow list, and the context-menu actions they dispatch.

use super::{
    DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX, TAB_CONTEXT_MENU_MAX_HEIGHT_PX, TabBarActiveSplitChanged,
    TabBarRenderer, tab_context_menu_preferred_height,
};
use crate::backend;
use crate::components::overlays::clear_global_tooltip;
use crate::components::overlays::traits::OverlayPointerIsolation;
use crate::components::ui::menu::{
    MenuAction, MenuHost, MenuKeyboard, menu_divider, menu_header, menu_item, menu_panel,
};
use crate::config;
use crate::icons::{MenuMetrics, modal_backdrop_style};
use crate::managers::tab_manager::{
    TAB_CONTEXT_MENU_ITEMS, TabManager, TabMenuAction, TabMenuItem,
};
use crate::state::{AppState, TabState};
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::rc::Rc;

impl TabBarRenderer {
    /// Render the tab's right-click context menu overlay.
    pub fn render_context_menu_overlay(
        &mut self,
        app_state: Entity<AppState>,
        menu: &MenuHost,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.is_context_menu_open() {
            return div().into_any_element();
        }

        clear_global_tooltip();

        let (_, tab_id_ref, x, y) = self.context_menu_info();
        let tab_id = tab_id_ref.to_string();

        let target = {
            let state = app_state.read(cx);
            state.tabs.iter().enumerate().find_map(|(idx, tab)| {
                (tab.id == tab_id).then(|| {
                    (
                        idx,
                        tab.is_locked,
                        tab.mark_unread_on_new,
                        tab.valid_active_split_path(),
                        tab.channel_targets(),
                    )
                })
            })
        };
        let Some((idx, is_locked, mark_unread, active_split_path, channel_targets)) = target else {
            self.close_context_menu();
            cx.notify();
            return div().into_any_element();
        };
        self.tab_manager.context_menu.tab_idx = idx;

        let viewport = window.viewport_size();
        let menu_fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            220.0,
            tab_context_menu_preferred_height(channel_targets.len())
                .min(TAB_CONTEXT_MENU_MAX_HEIGHT_PX),
            8.0,
        );
        let x = crate::utils::clamp_overlay_axis(
            x,
            menu_fit.width,
            f32::from(viewport.width),
            menu_fit.margin_x,
        );
        let y = crate::utils::clamp_overlay_axis(
            y,
            menu_fit.height,
            f32::from(viewport.height),
            menu_fit.margin_y,
        );

        let tab_bar = cx.entity();
        let nav = &menu.nav;
        let mut actions: Vec<MenuAction> = Vec::new();
        let mut menu_items: Vec<AnyElement> = Vec::new();
        for item in TAB_CONTEXT_MENU_ITEMS {
            let &TabMenuItem::Action {
                label,
                action,
                is_destructive,
            } = item
            else {
                menu_items.push(menu_divider().into_any_element());
                continue;
            };
            let tab_id = tab_id.clone();
            let app_state = app_state.clone();
            let tab_bar = tab_bar.clone();
            let checked =
                TabManager::get_toggle_state(action, is_locked, mark_unread) == Some(true);
            let run: MenuAction = Rc::new(move |window: &mut Window, cx: &mut App| {
                tab_bar.update(cx, |this, cx| {
                    this.handle_tab_action(action, &tab_id, app_state.clone(), window, cx);
                    this.close_context_menu();
                    cx.notify();
                });
            });
            let index = actions.len();
            actions.push(run.clone());
            menu_items.push(
                menu_item(action.id(), index, nav, run)
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(MenuMetrics::ITEM_TEXT))
                            .text_color(if is_destructive {
                                colors::signal_red()
                            } else {
                                colors::text_primary()
                            })
                            .child(label),
                    )
                    .when(checked, |row| {
                        row.child(
                            div()
                                .text_size(px(MenuMetrics::ITEM_TEXT))
                                .text_color(colors::signal_green())
                                .child("✓"),
                        )
                    })
                    .into_any_element(),
            );
        }

        if channel_targets.len() > 1 {
            menu_items.push(menu_divider().into_any_element());
            menu_items.push(
                menu_header()
                    .id(("tab-channel-heading", idx))
                    .child("Switch to split")
                    .into_any_element(),
            );

            for (target_idx, (path, label)) in channel_targets.into_iter().enumerate() {
                let is_active_target = path == active_split_path;
                let app_state = app_state.clone();
                let tab_bar = tab_bar.clone();
                let tab_id = tab_id.clone();
                let run: MenuAction = Rc::new(move |_window: &mut Window, cx: &mut App| {
                    tab_bar.update(cx, |this, cx| {
                        let mut changed = false;
                        app_state.update(cx, |state, cx| {
                            let Some(idx) = state.tabs.iter().position(|tab| tab.id == tab_id)
                            else {
                                return;
                            };
                            changed = state.active_tab_idx != idx;
                            state.switch_tab(idx, cx);
                            changed |= state.tabs_mut()[idx].set_active_split_path(path.clone());
                        });
                        if changed {
                            this.publish_tab_activation("switch split from tab menu", cx);
                        }
                        this.close_context_menu();
                        cx.notify();
                    });
                });
                let index = actions.len();
                actions.push(run.clone());
                menu_items.push(
                    menu_item(
                        SharedString::from(format!("tab-channel-target-{idx}-{target_idx}")),
                        index,
                        nav,
                        run,
                    )
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(MenuMetrics::ITEM_TEXT))
                            .text_color(colors::text_primary())
                            .child(label),
                    )
                    .when(is_active_target, |item| {
                        item.child(
                            div()
                                .text_size(px(MenuMetrics::ITEM_TEXT))
                                .text_color(colors::signal_green())
                                .child("✓"),
                        )
                    })
                    .into_any_element(),
                );
            }
        }

        modal_backdrop_style()
            .id("tab-context-menu-overlay")
            .block_overlay_pointer_passthrough()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    this.close_context_menu();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                menu_panel()
                    .id("tab-context-menu")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(menu_fit.width))
                    .max_h(px(menu_fit.height))
                    .overflow_y_scroll()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .menu_keyboard(&menu.focus, &menu.nav, actions, menu.close_handler())
                    .children(menu_items),
            )
            .into_any_element()
    }

    /// Render tab overflow dropdown overlay for dropdown mode
    pub fn render_overflow_dropdown_overlay(
        &mut self,
        app_state: Entity<AppState>,
        menu: &MenuHost,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.is_overflow_open() {
            return div().into_any_element();
        }

        clear_global_tooltip();

        if Self::effective_overflow_mode() != backend::TabOverflowMode::Dropdown {
            self.close_overflow();
            return div().into_any_element();
        }

        if app_state.read(cx).should_hide_tab_bar() {
            self.close_overflow();
            return div().into_any_element();
        }

        let tab_bar_width = self.bar_width();

        let tab_theme = Self::effective_tab_theme();
        let metrics = Self::tab_layout_metrics(&tab_theme, &backend::TabOverflowMode::Dropdown);
        let overflow_button_width = DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX;

        let overflow_rows: Vec<(
            usize,
            SharedString,
            bool,
            Hsla,
            crate::state::TabHighlightState,
        )> = {
            let state = app_state.read(cx);
            let active_idx = state.active_tab_idx;
            let tabs = state
                .visible_tab_indices()
                .into_iter()
                .filter_map(|idx| state.tabs.get(idx).map(|tab| (idx, tab)))
                .collect::<Vec<_>>();
            self.sync_tab_models(&tabs, &state.stream_info, window);
            let visible_positions = self
                .calculate_dropdown_layout(
                    &tabs,
                    active_idx,
                    &tab_theme,
                    tab_bar_width,
                    metrics,
                    overflow_button_width,
                )
                .visible_positions;

            let overflow_tabs: Vec<(usize, &TabState)> = tabs
                .iter()
                .enumerate()
                .filter(|(position, _)| !visible_positions.contains(position))
                .map(|(_, tab)| *tab)
                .collect();

            overflow_tabs
                .into_iter()
                .map(|(idx, tab)| {
                    let title = self
                        .tab_models
                        .get(&tab.id)
                        .map(|model| model.title.clone())
                        .unwrap_or_else(|| SharedString::from(tab.display_title()));
                    let is_active = idx == active_idx;
                    let tab_highlight = if is_active {
                        crate::state::TabHighlightState::None
                    } else {
                        tab.highlight_state
                    };
                    let visual_state = match tab_highlight {
                        crate::state::TabHighlightState::None if !is_active && tab.has_unread => {
                            crate::state::TabHighlightState::NewMessage
                        }
                        state => state,
                    };
                    let text_color =
                        if is_active || visual_state != crate::state::TabHighlightState::None {
                            colors::text_primary()
                        } else {
                            colors::text_tertiary()
                        };
                    (idx, title, is_active, text_color, visual_state)
                })
                .collect()
        };

        if overflow_rows.is_empty() {
            self.close_overflow();
            return div().into_any_element();
        }

        let viewport = window.viewport_size();
        let dropdown_fit = crate::utils::fit_overlay_to_viewport(viewport, 180.0, 400.0, 8.0);
        let dropdown_width = dropdown_fit.width;
        let (x, y) = if let Some(bounds) = self.overflow_button_bounds() {
            let bx: f32 = bounds.origin.x.into();
            let by: f32 = bounds.origin.y.into();
            let height: f32 = bounds.size.height.into();
            let constrained_x = crate::utils::clamp_overlay_axis(
                bx,
                dropdown_width,
                f32::from(viewport.width),
                dropdown_fit.margin_x,
            );
            (constrained_x, by + height)
        } else {
            (
                0.0,
                backend::settings::titlebar_height_px() + config::TAB_BAR_HEIGHT_PX,
            )
        };

        let tab_bar = cx.entity();
        let mut actions: Vec<MenuAction> = Vec::with_capacity(overflow_rows.len());
        let overflow_items: Vec<_> = overflow_rows
            .into_iter()
            .enumerate()
            .map(
                |(position, (idx, title, is_active, text_color, visual_state))| {
                    let tab_bar = tab_bar.clone();
                    let run: MenuAction = Rc::new(move |_window: &mut Window, cx: &mut App| {
                        tab_bar.update(cx, |this, cx| {
                            this.activate_tab(idx, "overflow switch", cx);
                            this.close_overflow();
                            cx.notify();
                        });
                    });
                    actions.push(run.clone());
                    let state_color = if is_active {
                        colors::text_secondary()
                    } else {
                        match visual_state {
                            crate::state::TabHighlightState::Highlighted => {
                                colors::mention_border()
                            }
                            crate::state::TabHighlightState::NewMessage => colors::signal_yellow(),
                            crate::state::TabHighlightState::None => colors::border_subtle(),
                        }
                    };

                    menu_item(("overflow-tab", idx), position, &menu.nav, run)
                        .border_l(px(if is_active { 3.0 } else { 2.0 }))
                        .border_color(state_color)
                        .when(is_active && !menu.nav.is_highlighted(position), |s| {
                            s.bg(colors::bg_plate())
                        })
                        .child(
                            div()
                                .text_size(px(MenuMetrics::ITEM_TEXT))
                                .text_color(text_color)
                                .child(title),
                        )
                        .into_any_element()
                },
            )
            .collect();

        let y = crate::utils::clamp_overlay_axis(
            y,
            dropdown_fit.height,
            f32::from(viewport.height),
            dropdown_fit.margin_y,
        );

        modal_backdrop_style()
            .id("tab-overflow-dropdown-overlay")
            .block_overlay_pointer_passthrough()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    this.close_overflow();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                menu_panel()
                    .id("tab-overflow-dropdown")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(dropdown_width))
                    .max_h(px(dropdown_fit.height))
                    .overflow_y_scroll()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .menu_keyboard(&menu.focus, &menu.nav, actions, menu.close_handler())
                    .children(overflow_items),
            )
            .into_any_element()
    }

    /// Handle a tab context menu action.
    pub fn handle_tab_action(
        &mut self,
        action: TabMenuAction,
        tab_id: &str,
        app_state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = app_state
            .read(cx)
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
        else {
            self.cancel_tab_drag();
            return;
        };
        self.cancel_tab_drag();

        match action {
            TabMenuAction::Rename => {
                self.start_rename(tab_id.to_string());
                self.ensure_rename_input(window, cx);
                cx.notify();
            }
            TabMenuAction::Duplicate => {
                app_state.update(cx, |state, _cx| {
                    state.duplicate_tab(idx);
                });
            }
            TabMenuAction::Close => {
                cx.emit(crate::app::TabCloseRequest {
                    tab_id: tab_id.to_string(),
                });
                return;
            }
            TabMenuAction::CloseOthers | TabMenuAction::CloseRight => {
                let to_right = action == TabMenuAction::CloseRight;
                let tab_count = app_state.read(cx).tabs.len();
                let count = if to_right {
                    tab_count.saturating_sub(idx + 1)
                } else {
                    tab_count.saturating_sub(1)
                };
                if count > 0 {
                    crate::backend::push_ui_action(crate::backend::UiAction::ConfirmCloseTabs {
                        tab_id: tab_id.to_string(),
                        to_right,
                        count,
                    });
                }
            }
            TabMenuAction::ReopenClosed => {
                let reopened = app_state.update(cx, |state, cx| state.reopen_closed_tabs(cx));
                if reopened == 0 {
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                        message: "No closed tab".to_string(),
                        detail: Some("Close a tab before using Reopen Closed Tab.".to_string()),
                    });
                }
            }
            TabMenuAction::ToggleUnread => {
                app_state.update(cx, |state, _cx| {
                    if idx < state.tabs.len() {
                        let current = state.tabs[idx].mark_unread_on_new;
                        state.tabs_mut()[idx].mark_unread_on_new = !current;
                    }
                });
            }
            TabMenuAction::ToggleLock => {
                app_state.update(cx, |state, _cx| {
                    if idx < state.tabs.len() {
                        let current = state.tabs[idx].is_locked;
                        state.tabs_mut()[idx].is_locked = !current;
                    }
                });
            }
            TabMenuAction::SortAlpha => {
                app_state.update(cx, |state, _cx| {
                    state.sort_tabs_alphabetically();
                });
            }
        }
        if action == TabMenuAction::ReopenClosed {
            cx.emit(TabBarActiveSplitChanged);
        }
        let state = app_state.read(cx);
        Self::save_tabs_if_allowed(state, "context action");
    }
}
