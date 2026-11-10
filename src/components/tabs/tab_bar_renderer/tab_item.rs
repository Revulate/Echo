//! A single tab: its theme styling, indicators, title or rename field, close
//! button, and the pointer handlers that switch, drag, close and open menus.

use super::{
    MINIMAL_TAB_HEIGHT_PX, MINIMAL_TAB_MAX_WIDTH_PX, MINIMAL_TAB_MIN_WIDTH_PX,
    NATURAL_TAB_MIN_WIDTH_PX, TAB_TITLE_FONT_SIZE_PX, TabBarActiveSplitChanged, TabBarRenderer,
    TabRenderModel,
};
use crate::app::TabCloseRequest;
use crate::backend;
use crate::components::overlays::{
    clear_global_tooltip_if_source_in_window, show_text_tooltip, text_tooltip_on_hover,
};
use crate::components::ui::input::{Input, InputState};
use crate::config;
use crate::icons::CLOSE_GLYPH;
use crate::managers::tab_manager::{
    TabContextMenu, TabDragState, TabDropTarget, TabDropZone, TabManager,
};
use crate::state::{TabHighlightState, TabState};
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// One tab as laid out by an overflow mode.
pub(super) struct TabItem<'a> {
    pub(super) idx: usize,
    pub(super) tab: &'a TabState,
    pub(super) model: &'a TabRenderModel,
    pub(super) is_active: bool,
    /// Width the overflow mode assigned; `None` lets the tab size itself.
    pub(super) assigned_width_px: Option<f32>,
}

/// The visual parameters of a tab shell for one appearance.
struct TabThemeStyle {
    active_bg: Hsla,
    inactive_bg: Hsla,
    hover_bg: Hsla,
    padding_x: f32,
    radius: f32,
    /// Round only the top corners, so the tab reads as attached to the strip.
    top_only_radius: bool,
    bordered: bool,
    /// Accent line along the active tab's top edge.
    active_border_top: bool,
    /// Accent line along the active tab's bottom edge.
    active_border_bottom: bool,
    /// The flat minimal appearance: a state line instead of browser chrome.
    minimal: bool,
}

impl TabThemeStyle {
    fn for_theme(theme: &backend::TabTheme) -> Self {
        match theme {
            backend::TabTheme::Chrome => Self {
                active_bg: colors::bg_plate(),
                inactive_bg: colors::bg_plate(),
                hover_bg: colors::bg_hover(),
                padding_x: 10.0,
                radius: 0.0,
                top_only_radius: false,
                bordered: false,
                active_border_top: false,
                active_border_bottom: true,
                minimal: false,
            },
            backend::TabTheme::Firefox => {
                let mut active_bg = colors::bg_elevated();
                active_bg.a = 0.95;
                Self {
                    active_bg,
                    inactive_bg: TabManager::tab_bg(false),
                    hover_bg: colors::bg_hover(),
                    padding_x: 12.0,
                    radius: 9.0,
                    top_only_radius: true,
                    bordered: true,
                    active_border_top: false,
                    active_border_bottom: false,
                    minimal: false,
                }
            }
            backend::TabTheme::Edge => {
                let mut inactive_bg = colors::bg_surface();
                inactive_bg.a = 0.9;
                Self {
                    active_bg: TabManager::tab_bg(true),
                    inactive_bg,
                    hover_bg: colors::bg_hover(),
                    padding_x: 12.0,
                    radius: 5.0,
                    top_only_radius: false,
                    bordered: true,
                    active_border_top: true,
                    active_border_bottom: false,
                    minimal: false,
                }
            }
            backend::TabTheme::Square => Self {
                active_bg: TabManager::tab_bg(true),
                inactive_bg: TabManager::tab_bg(false),
                hover_bg: colors::bg_elevated(),
                padding_x: 12.0,
                radius: 0.0,
                top_only_radius: false,
                bordered: true,
                active_border_top: false,
                active_border_bottom: true,
                minimal: false,
            },
            backend::TabTheme::Arc => {
                let mut hover_bg = colors::bg_hover();
                hover_bg.a = 0.75;
                Self {
                    active_bg: colors::bg_elevated(),
                    inactive_bg: TabManager::tab_bg(false),
                    hover_bg,
                    padding_x: 14.0,
                    radius: 999.0,
                    top_only_radius: false,
                    bordered: true,
                    active_border_top: false,
                    active_border_bottom: false,
                    minimal: false,
                }
            }
            backend::TabTheme::Minimal => Self {
                active_bg: colors::bg_elevated(),
                inactive_bg: colors::bg_plate(),
                hover_bg: colors::bg_hover(),
                padding_x: 4.0,
                radius: 0.0,
                top_only_radius: false,
                bordered: false,
                active_border_top: false,
                active_border_bottom: false,
                minimal: true,
            },
        }
    }

    fn height(&self) -> f32 {
        if self.minimal {
            MINIMAL_TAB_HEIGHT_PX
        } else {
            config::TAB_BAR_HEIGHT_PX
        }
    }

    fn content_gap(&self) -> f32 {
        if self.minimal { 8.0 } else { 6.0 }
    }

    /// Corner radius, border and active accent lines of the tab shell.
    fn apply_shape(&self, tab: Stateful<Div>, is_active: bool) -> Stateful<Div> {
        let mut tab = if self.top_only_radius {
            tab.rounded_t(px(self.radius))
        } else {
            tab.rounded(px(self.radius))
        };
        if self.bordered {
            tab = tab.border_1().border_color(colors::border());
        }
        if is_active && self.active_border_top {
            tab = tab.border_t(px(3.0)).border_color(colors::text_secondary());
        }
        if is_active && self.active_border_bottom {
            tab = tab.border_b(px(3.0)).border_color(colors::text_secondary());
        }
        tab
    }
}

impl TabBarRenderer {
    /// Render a single tab; each tab has its own layout.
    pub(super) fn render_tab_item(
        &self,
        item: TabItem<'_>,
        tab_theme: &backend::TabTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let TabItem {
            idx,
            tab,
            model,
            is_active,
            assigned_width_px,
        } = item;
        let style = TabThemeStyle::for_theme(tab_theme);
        let minimal_style = style.minimal;

        let tab_highlight = if is_active {
            TabHighlightState::None
        } else {
            tab.highlight_state
        };
        let minimal_visual_state = Self::minimal_visual_state(tab, is_active);
        let bg = if is_active {
            style.active_bg
        } else {
            style.inactive_bg
        };
        let text_color = if is_active || minimal_visual_state != TabHighlightState::None {
            colors::text_primary()
        } else {
            colors::text_tertiary()
        };
        let unread_dot_color = match tab_highlight {
            TabHighlightState::Highlighted => colors::mention_border(),
            _ => colors::accent(),
        };
        let has_unread = (tab.has_unread || tab_highlight != TabHighlightState::None) && !is_active;
        let is_live = model.live.live_channel_count > 0;

        let is_renaming = self.tab_manager.rename_state.is_active
            && self.tab_manager.rename_state.tab_id == tab.id;
        let rename_input = is_renaming.then(|| self.rename_input.clone()).flatten();
        let rename_min_width = is_renaming.then(|| {
            assigned_width_px.unwrap_or_else(|| {
                self.measured_tab_width(tab, tab_theme, NATURAL_TAB_MIN_WIDTH_PX)
            })
        });

        let is_dragging = self.tab_manager.drag_state.is_dragging;
        let is_source = is_dragging && self.tab_manager.drag_state.source_idx == idx;
        let drop_zone = self
            .tab_manager
            .drag_state
            .drop_target
            .as_ref()
            .filter(|target| target.target_idx == idx)
            .map(|target| target.zone);

        let is_hovered = self.tab_manager.hovered_idx == Some(idx);
        let show_close_button =
            (is_hovered || (minimal_style && is_active)) && !is_renaming && !is_dragging;
        let minimal_line_color = if is_active {
            colors::text_secondary()
        } else {
            match minimal_visual_state {
                TabHighlightState::Highlighted => colors::mention_border(),
                TabHighlightState::NewMessage => colors::text_muted(),
                TabHighlightState::None if is_hovered => colors::border(),
                TabHighlightState::None => colors::border_subtle(),
            }
        };
        let constrain_title = assigned_width_px
            .is_some_and(|width| width + 0.5 < self.natural_tab_width(tab, tab_theme));

        let entity = cx.entity().clone();

        let mut tab_el = div()
            .id(("tab", idx))
            .role(Role::Tab)
            .aria_label(tab_accessible_name(model, has_unread, tab_highlight))
            .aria_description(tab_accessible_description(model, tab.is_locked))
            .aria_selected(is_active)
            .aria_position_in_set(model.position_in_set)
            .aria_size_of_set(model.size_of_set)
            .on_a11y_action(accesskit::Action::Click, {
                let tab_id = tab.id.clone();
                let click = cx.listener(move |this, _: &(), _window, cx| {
                    this.activate_tab_by_id(&tab_id, cx);
                });
                move |_, window, cx| click(&(), window, cx)
            })
            .group(SharedString::from(format!("tab-{}", idx)))
            .h(px(style.height()))
            .flex_shrink_0()
            .px(px(style.padding_x))
            .pt(px(Spacing::HAIRLINE))
            .flex()
            .items_center()
            .gap(px(style.content_gap()))
            .bg(bg)
            .relative()
            .when_some(assigned_width_px, |tab, width| tab.w(px(width)))
            .when(
                !is_renaming && !is_dragging && (!minimal_style || !is_active),
                |s| s.hover(|s| s.bg(style.hover_bg)),
            )
            .cursor_default()
            .opacity(if is_source { 0.5 } else { 1.0 })
            .when(drop_zone == Some(TabDropZone::SwapWith), |s| {
                s.border_2().border_color(colors::accent())
            })
            .when(minimal_style, |tab| {
                tab.min_w(px(MINIMAL_TAB_MIN_WIDTH_PX))
                    .max_w(px(MINIMAL_TAB_MAX_WIDTH_PX))
                    .overflow_hidden()
            })
            // The rename field has no intrinsic width, so an intrinsically sized
            // tab collapses around it; hold the tab at its pre-rename width.
            .when_some(rename_min_width, |tab, width| tab.min_w(px(width)))
            .map(|tab| style.apply_shape(tab, is_active));
        if minimal_style {
            tab_el = tab_el.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(if is_active { 3.0 } else { 2.0 }))
                    .bg(minimal_line_color),
            );
        }

        tab_el
            .on_hover(tab_tooltip_hover_handler(idx, model, entity.clone()))
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    this.track_tab_pointer(idx, event, cx);
                }),
            )
            .when(has_unread && !minimal_style, |s| {
                s.child(unread_dot(unread_dot_color))
            })
            .when(is_live, |s| {
                s.child(live_indicator(
                    idx,
                    is_active,
                    minimal_style,
                    model,
                    entity.clone(),
                ))
            })
            .when(!is_renaming, |el| {
                let tab_id = tab.id.clone();
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.press_tab(idx, &tab_id, event, window, cx);
                    }),
                )
            })
            .map(|el| Self::with_middle_click_close(el, &tab.id, cx))
            .on_mouse_down(MouseButton::Right, {
                let tab_id = tab.id.clone();
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.open_tab_context_menu(idx, &tab_id, event.position, cx);
                })
            })
            .when(tab.is_locked && !is_renaming, |el| {
                el.child(render_tab_lock_glyph(text_color))
            })
            .child(if is_renaming {
                let rename_input = rename_input.expect("active rename has an input state");
                Self::render_rename_field(idx, &rename_input, cx)
            } else {
                div()
                    .relative()
                    .text_size(px(TAB_TITLE_FONT_SIZE_PX))
                    .text_color(text_color)
                    .when(minimal_style, |title| {
                        title
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_center()
                    })
                    .when(!minimal_style && assigned_width_px.is_some(), |title| {
                        title.flex_1().whitespace_nowrap().text_center()
                    })
                    .when(constrain_title, |title| {
                        title.min_w_0().overflow_hidden().text_ellipsis()
                    })
                    .child(model.title.clone())
                    .child(title_clip_probe(
                        idx,
                        model.shaped_title_width,
                        entity.clone(),
                    ))
                    .into_any_element()
            })
            .when(
                show_close_button
                    || minimal_style
                    || assigned_width_px.is_none()
                    || constrain_title,
                |el| {
                    el.child(Self::render_close_slot(
                        idx,
                        &tab.id,
                        show_close_button,
                        minimal_style,
                        cx,
                    ))
                },
            )
            .children(drop_zone.and_then(drop_insert_marker))
            .child(hit_area_probe(idx, entity))
    }

    /// Switch to the tab with `tab_id` on an accessibility activation.
    fn activate_tab_by_id(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        self.commit_rename(cx);
        self.close_context_menu();
        self.close_overflow();
        let Some(idx) = self
            .app_state
            .read(cx)
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
        else {
            return;
        };
        self.activate_tab(idx, "accessible tab switch", cx);
        cx.notify();
    }

    /// Hover and drag tracking while the pointer moves over tab `idx`.
    fn track_tab_pointer(&mut self, idx: usize, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.tab_manager.hovered_idx != Some(idx) {
            self.tab_manager.hovered_idx = Some(idx);
            cx.notify();
        }
        if self.tab_manager.update_drag_position(
            event.position,
            event.pressed_button == Some(MouseButton::Left),
        ) {
            cx.notify();
        }
        if self.tab_manager.drag_state.is_dragging
            && update_drop_target(&mut self.tab_manager, idx, event.position)
        {
            cx.notify();
        }
    }

    /// Left press on a tab body: a double click starts renaming; otherwise the
    /// tab is activated (unless the press began on its close button) and a
    /// possible drag is armed.
    fn press_tab(
        &mut self,
        idx: usize,
        tab_id: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if self.is_renaming() {
            self.commit_rename(cx);
        }
        let close_button_press = self
            .tab_manager
            .is_close_button_press(tab_id, event.position);
        if !close_button_press {
            self.tab_manager.close_press = None;
        }
        if !close_button_press && self.tab_manager.check_tab_double_click(idx) {
            self.start_rename(tab_id.to_string());
            self.ensure_rename_input(window, cx);
            self.tab_manager.drag_state = TabDragState::default();
            cx.notify();
            return;
        }

        if !close_button_press {
            self.activate_tab(idx, "tab switch", cx);
        }

        let tab_order = self
            .app_state
            .read(cx)
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect();
        self.tab_manager
            .begin_drag(idx, tab_id.to_string(), tab_order, event.position);
        cx.notify();
    }

    fn open_tab_context_menu(
        &mut self,
        idx: usize,
        tab_id: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.tab_manager.context_menu = TabContextMenu {
            is_open: true,
            tab_idx: idx,
            tab_id: tab_id.to_string(),
            x: position.x.into(),
            y: position.y.into(),
        };
        cx.notify();
    }

    fn request_tab_close(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        self.cancel_tab_drag();
        cx.emit(TabCloseRequest {
            tab_id: tab_id.to_string(),
        });
        cx.notify();
    }

    /// A middle click that presses and releases on the same tab closes it.
    fn with_middle_click_close(
        tab: Stateful<Div>,
        tab_id: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let press_id = tab_id.to_string();
        let release_out_id = tab_id.to_string();
        let release_id = tab_id.to_string();
        tab.on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.tab_manager.arm_close_press(
                    press_id.clone(),
                    MouseButton::Middle,
                    event.position,
                );
            }),
        )
        .on_mouse_up_out(
            MouseButton::Middle,
            cx.listener(move |this, _event, _window, _cx| {
                this.tab_manager
                    .disarm_close_press(&release_out_id, MouseButton::Middle);
            }),
        )
        .on_mouse_up(
            MouseButton::Middle,
            cx.listener(move |this, event: &MouseUpEvent, _window, cx| {
                cx.stop_propagation();
                if this.tab_manager.take_completed_close(
                    &release_id,
                    MouseButton::Middle,
                    event.position,
                ) {
                    this.request_tab_close(&release_id, cx);
                }
            }),
        )
    }

    /// The fixed 16 px close slot, holding the close button when it is shown.
    fn render_close_slot(
        idx: usize,
        tab_id: &str,
        show_button: bool,
        minimal_style: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // Layout only ever sees this 16 px slot, hovered or not.
        // Taffy undersizes an intrinsically sized flex row when a
        // non-shrinking item has a negative margin, which made a
        // hovered tab collapse out from under the pointer, so the
        // larger close hit area is positioned absolutely instead.
        let slot = div().w(px(16.0)).h(px(16.0)).flex_shrink_0().relative();
        if !show_button {
            return slot;
        }
        let hover_bg = if minimal_style {
            colors::text_primary().opacity(0.16)
        } else {
            colors::error().opacity(0.15)
        };
        let hover_text = if minimal_style {
            colors::text_primary()
        } else {
            colors::signal_red()
        };
        let accessible_close_id = tab_id.to_string();
        let press_id = tab_id.to_string();
        let release_out_id = tab_id.to_string();
        let release_id = tab_id.to_string();
        slot.child(
            div()
                .id(("tab-close", idx))
                .role(Role::Button)
                .aria_label("Close tab")
                .on_a11y_action(accesskit::Action::Click, {
                    let click = cx.listener(move |this, _: &(), _window, cx| {
                        this.request_tab_close(&accessible_close_id, cx);
                    });
                    move |_, window, cx| click(&(), window, cx)
                })
                .on_hover(text_tooltip_on_hover(
                    format!("tab-close-{idx}"),
                    "Close tab".to_string(),
                ))
                // Extends into the existing gap and padding without changing layout.
                .absolute()
                .left(px(-4.0))
                .top(px(-4.0))
                .w(px(24.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(if minimal_style { px(0.0) } else { Radii::sm() })
                .text_size(px(FontSize::XS))
                .text_color(colors::text_muted())
                .hover(move |s| s.bg(hover_bg).text_color(hover_text))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _window, _cx| {
                        if this.tab_manager.drag_state.is_dragging {
                            return;
                        }
                        this.tab_manager.arm_close_press(
                            press_id.clone(),
                            MouseButton::Left,
                            event.position,
                        );
                    }),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(move |this, _event, _window, _cx| {
                        this.tab_manager
                            .disarm_close_press(&release_out_id, MouseButton::Left);
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseUpEvent, _window, cx| {
                        if this.tab_manager.take_completed_close(
                            &release_id,
                            MouseButton::Left,
                            event.position,
                        ) {
                            this.request_tab_close(&release_id, cx);
                        }
                    }),
                )
                .child(CLOSE_GLYPH),
        )
    }

    fn render_rename_field(
        idx: usize,
        rename_input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(("tab-rename-input", idx))
            .flex()
            .items_center()
            .flex_1()
            .min_w_0()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_rename();
                    cx.stop_propagation();
                    cx.emit(TabBarActiveSplitChanged);
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Input::new(rename_input)
                    .appearance(true)
                    .bordered(true)
                    .focus_bordered(true)
                    .accessibility_label("Rename tab"),
            )
            .into_any_element()
    }
}

/// Retarget an active drag from the pointer position over tab `idx`.
/// Returns whether the drop target changed.
fn update_drop_target(manager: &mut TabManager, idx: usize, position: Point<Pixels>) -> bool {
    let Some(bounds) = manager.hit_areas.get(&idx).cloned() else {
        return false;
    };
    let source_idx = manager.drag_state.source_idx;
    let zone = TabManager::calculate_tab_drop_zone(position, bounds);
    // Dropping a tab onto itself or into the gap it already occupies is a no-op.
    let is_noop = match zone {
        TabDropZone::InsertBefore => idx == source_idx || idx == source_idx + 1,
        TabDropZone::InsertAfter => idx == source_idx || idx + 1 == source_idx,
        TabDropZone::SwapWith => idx == source_idx,
    };
    if is_noop {
        return manager.drag_state.drop_target.take().is_some();
    }
    let unchanged = manager
        .drag_state
        .drop_target
        .as_ref()
        .is_some_and(|target| target.target_idx == idx && target.zone == zone);
    if unchanged {
        return false;
    }
    manager.drag_state.drop_target = Some(TabDropTarget {
        target_idx: idx,
        zone,
    });
    true
}

/// Screen readers announce the name first and often skip the description,
/// so live and unread state ride on the name ("#xqc, live, unread").
fn tab_accessible_name(
    model: &TabRenderModel,
    has_unread: bool,
    tab_highlight: TabHighlightState,
) -> String {
    let mut name = model.title.to_string();
    match model.live.live_channel_count {
        0 => {}
        1 => name.push_str(", live"),
        count => name.push_str(&format!(", {count} live channels")),
    }
    if has_unread {
        name.push_str(if tab_highlight == TabHighlightState::Highlighted {
            ", unread mentions"
        } else {
            ", unread"
        });
    }
    name
}

fn tab_accessible_description(model: &TabRenderModel, is_locked: bool) -> String {
    [
        (model.live.live_channel_count > 0).then(|| model.live_tooltip.to_string()),
        is_locked.then(|| "Layout locked".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(". ")
}

/// Show the tab's tooltip (clipped title and/or channel list) while hovered.
fn tab_tooltip_hover_handler(
    idx: usize,
    model: &TabRenderModel,
    entity: Entity<TabBarRenderer>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let tooltip = model.channel_tooltip.clone();
    let tooltip_source = model.channel_tooltip_source.clone();
    let title = model.title.clone();
    move |hovered, window, app| {
        if *hovered {
            if let Some(tooltip) = entity.read(app).tab_tooltip(idx, &title, tooltip.as_ref()) {
                show_text_tooltip(
                    window.window_handle(),
                    tooltip_source.to_string(),
                    tooltip,
                    window.mouse_position(),
                );
            }
        } else {
            clear_global_tooltip_if_source_in_window(window.window_handle(), &tooltip_source);
        }
    }
}

fn unread_dot(color: Hsla) -> Div {
    div().w(px(6.0)).h(px(6.0)).rounded(Radii::full()).bg(color)
}

/// The live dot. Its tooltip replaces the tab's own while hovered and hands
/// back to the tab's tooltip when the pointer leaves the dot but stays on the
/// tab.
fn live_indicator(
    idx: usize,
    is_active: bool,
    minimal_style: bool,
    model: &TabRenderModel,
    entity: Entity<TabBarRenderer>,
) -> Stateful<Div> {
    let indicator_color = if minimal_style {
        colors::signal_red()
    } else {
        colors::signal_green()
    };
    let tooltip_source = model.live_tooltip_source.clone();
    let tooltip_text = model.live_tooltip.clone();
    let channel_tooltip_source = model.channel_tooltip_source.clone();
    let channel_tooltip = model.channel_tooltip.clone();
    let tooltip_title = model.title.clone();
    div()
        .id(("live-indicator", idx))
        .w(px(if minimal_style { 4.0 } else { 6.0 }))
        .h(px(if minimal_style { 4.0 } else { 6.0 }))
        .rounded(Radii::full())
        .bg(indicator_color)
        .when(minimal_style, |indicator| {
            indicator
                .absolute()
                .top(px(if is_active { 4.0 } else { 5.0 }))
                .right(px(3.0))
        })
        .on_hover(move |hovered, window, app| {
            if *hovered {
                clear_global_tooltip_if_source_in_window(
                    window.window_handle(),
                    &channel_tooltip_source,
                );
                show_text_tooltip(
                    window.window_handle(),
                    tooltip_source.to_string(),
                    tooltip_text.to_string(),
                    window.mouse_position(),
                );
            } else {
                clear_global_tooltip_if_source_in_window(window.window_handle(), &tooltip_source);
                let renderer = entity.read(app);
                let position = window.mouse_position();
                let over_tab = renderer
                    .tab_bounds(idx)
                    .is_some_and(|bounds| bounds.contains(&position))
                    && renderer
                        .tab_manager
                        .scroll_viewport_bounds
                        .as_ref()
                        .is_none_or(|bounds| bounds.contains(&position));
                if let Some(channel_tooltip) = over_tab
                    .then(|| renderer.tab_tooltip(idx, &tooltip_title, channel_tooltip.as_ref()))
                    .flatten()
                {
                    show_text_tooltip(
                        window.window_handle(),
                        channel_tooltip_source.to_string(),
                        channel_tooltip.to_string(),
                        window.mouse_position(),
                    );
                }
            }
        })
}

/// The insertion bar drawn on the side of the drop target a dragged tab will
/// land on. A swap is shown by the target's border instead.
fn drop_insert_marker(zone: TabDropZone) -> Option<Div> {
    let marker = div()
        .absolute()
        .top(px(6.0))
        .bottom(px(6.0))
        .w(px(3.0))
        .rounded(Radii::xs())
        .bg(colors::accent());
    match zone {
        TabDropZone::InsertBefore => Some(marker.left(px(-2.0))),
        TabDropZone::InsertAfter => Some(marker.right(px(-2.0))),
        TabDropZone::SwapWith => None,
    }
}

/// Records whether the painted title is clipped, which decides whether the
/// tab's tooltip repeats the full title.
fn title_clip_probe(
    idx: usize,
    title_width: f32,
    entity: Entity<TabBarRenderer>,
) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let visible = bounds.intersect(&window.content_mask().bounds);
            entity.update(cx, |renderer, _| {
                if title_width > f32::from(visible.size.width) + 0.5 {
                    renderer.tab_manager.clipped_titles.insert(idx);
                } else {
                    renderer.tab_manager.clipped_titles.remove(&idx);
                }
            });
        },
        |_, _: (), _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Records the tab's laid-out bounds for hover, drag and menu placement.
fn hit_area_probe(idx: usize, entity: Entity<TabBarRenderer>) -> impl IntoElement {
    canvas(
        move |bounds, _window, cx| {
            entity.update(cx, |renderer, _| {
                renderer.tab_manager.hit_areas.insert(idx, bounds);
            });
        },
        |_bounds, _: (), _window, _cx| {},
    )
    .absolute()
    .inset_0()
}

/// A small padlock drawn from boxes, marking a tab whose split layout is locked.
fn render_tab_lock_glyph(color: Hsla) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(8.0))
        .h(px(10.0))
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w(px(6.0))
                .h(px(5.0))
                .border_1()
                .border_b_0()
                .border_color(color)
                .rounded_t(Radii::sm()),
        )
        .child(div().w(px(8.0)).h(px(5.0)).rounded(Radii::xs()).bg(color))
}
