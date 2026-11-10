//! The split action menu: its rows, their measured width, and the app-level
//! overlay that renders the open menu beside its split.

use super::*;

/// Width band the split action menu may ask for before the viewport clamps it.
///
/// The floor keeps a short menu from reading as a tooltip; the cap stops a
/// pathological label or rebound hotkey from producing a full-width panel.
const SPLIT_MENU_MIN_WIDTH_PX: f32 = 160.0;
const SPLIT_MENU_MAX_WIDTH_PX: f32 = 320.0;
const SPLIT_MENU_MAX_HEIGHT_PX: f32 = 420.0;
/// Label column below which the shortcut column is dropped instead of elided.
const SPLIT_MENU_MIN_LABEL_PX: f32 = 72.0;
/// The panel draws a 1px border on each side, inside its requested width.
const SPLIT_MENU_BORDER_PX: f32 = 2.0;

/// Split action menu entries, kept as data so one list drives both the width
/// measurement and the painted rows.
#[derive(Clone, Copy)]
enum SplitMenuAction {
    ToggleHeader,
    AddSplit,
    ChangeChannel,
    LiveFollowedChannels,
    OpenChatters,
    OpenChannelPopup,
    ModLog,
    CycleDeletedMode,
    OpenCustomPlayer,
    OpenStreamlink,
    CloseSplit,
}

/// Short name of a deleted-message mode in the split menu.
fn deleted_mode_label(mode: backend::settings::DeletedMessageMode) -> &'static str {
    use backend::settings::DeletedMessageMode;
    match mode {
        DeletedMessageMode::Disable => "Show",
        DeletedMessageMode::Hide => "Hide",
        DeletedMessageMode::Replace => "Click to reveal",
        DeletedMessageMode::Strikethrough => "Strikethrough",
        DeletedMessageMode::Dim => "Dimmed",
    }
}

/// The mode after `current` when cycling a split's deleted-message override:
/// follow the global setting, then each explicit mode, then back.
fn next_deleted_mode(
    current: Option<backend::settings::DeletedMessageMode>,
) -> Option<backend::settings::DeletedMessageMode> {
    use backend::settings::DeletedMessageMode;
    match current {
        None => Some(DeletedMessageMode::Dim),
        Some(DeletedMessageMode::Dim) => Some(DeletedMessageMode::Strikethrough),
        Some(DeletedMessageMode::Strikethrough) => Some(DeletedMessageMode::Replace),
        Some(DeletedMessageMode::Replace) => Some(DeletedMessageMode::Hide),
        Some(DeletedMessageMode::Hide) => Some(DeletedMessageMode::Disable),
        Some(DeletedMessageMode::Disable) => None,
    }
}

/// A split action menu row: an elidable label plus an optional trailing
/// shortcut.
struct SplitMenuRow {
    action: SplitMenuAction,
    label: SharedString,
    shortcut: Option<SharedString>,
}

/// Width the split action menu needs, measured rather than guessed.
///
/// Returns the intrinsic width — widest `label + gap + shortcut` row plus the
/// shared [`MenuMetrics`] inset and the panel border — and the width the
/// shortcut column occupies, so a squeezed panel can drop that column instead
/// of clipping it. Shaping through the same `shape_line` path the tab bar uses
/// means the number matches what actually gets painted.
fn measure_split_menu_width(rows: &[SplitMenuRow], window: &Window) -> (f32, f32) {
    let font = window.text_style().font();
    let measure = |text: &SharedString, font_size: f32| -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let runs = [TextRun {
            len: text.len(),
            font: font.clone(),
            color: colors::text_primary(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        window
            .text_system()
            .shape_line(text.clone(), px(font_size), &runs, None)
            .width
            .as_f32()
    };

    let mut widest_row: f32 = 0.0;
    let mut widest_shortcut: f32 = 0.0;
    for row in rows {
        let label = measure(&row.label, MenuMetrics::ITEM_TEXT);
        let shortcut = row
            .shortcut
            .as_ref()
            .map(|shortcut| measure(shortcut, MenuMetrics::SECONDARY_TEXT))
            .unwrap_or(0.0);
        widest_shortcut = widest_shortcut.max(shortcut);
        widest_row = widest_row.max(if shortcut > 0.0 {
            label + MenuMetrics::ITEM_GAP + shortcut
        } else {
            label
        });
    }

    let shortcut_column = if widest_shortcut > 0.0 {
        MenuMetrics::ITEM_GAP + widest_shortcut
    } else {
        0.0
    };
    let insets = MenuMetrics::ITEM_PADDING_X * 2.0 + SPLIT_MENU_BORDER_PX;
    (widest_row.ceil() + insets, shortcut_column)
}

impl EchoApp {
    /// Render the open split settings menu as an app-level overlay, positioned
    /// from the split hit areas so it follows resizes.
    pub(super) fn render_split_menu(
        &mut self,
        chrome_height: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some((path_key, channel)) =
            self.overlay_manager.read(cx).open_settings_dropdown.clone()
        else {
            return div().into_any_element();
        };

        crate::components::overlays::clear_global_tooltip();

        let bounds = self.split_layout.read(cx).hit_area_for(&path_key);
        let Some(bounds) = bounds else {
            self.overlay_manager
                .update(cx, |overlays, _| overlays.close_all_split_settings());
            return div().into_any_element();
        };

        let compact_split_header = backend::settings::get_compact_split_header();
        let header_height = main_split_header_height(compact_split_header);
        let header_collapsed = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .header_collapsed_at_path(path_key.as_slice())
            .unwrap_or(false);

        let custom_player_path = crate::backend::settings::get_use_custom_player()
            .then(|| {
                crate::backend::settings::get_custom_player_path()
                    .filter(|path| !path.trim().is_empty())
            })
            .flatten();
        let shortcut_for = |action: crate::backend::hotkeys::HotkeyAction| {
            crate::backend::hotkeys::get_binding(action)
                .map(|binding| SharedString::from(binding.to_display()))
        };

        let deleted_mode_override = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .deleted_mode_at_path(path_key.as_slice());
        let is_whispers = backend::whispers::is_whispers_channel(&channel);
        let mut rows: Vec<SplitMenuRow> = vec![
            SplitMenuRow {
                action: SplitMenuAction::ToggleHeader,
                label: if header_collapsed {
                    "Show header".into()
                } else {
                    "Hide header".into()
                },
                shortcut: None,
            },
            SplitMenuRow {
                action: SplitMenuAction::AddSplit,
                label: "Add split to right".into(),
                shortcut: shortcut_for(crate::backend::hotkeys::HotkeyAction::NewSplit),
            },
            SplitMenuRow {
                action: SplitMenuAction::ChangeChannel,
                label: "Change channel".into(),
                shortcut: shortcut_for(crate::backend::hotkeys::HotkeyAction::ChangeChannel),
            },
            SplitMenuRow {
                action: SplitMenuAction::LiveFollowedChannels,
                label: "Live followed channels…".into(),
                shortcut: None,
            },
        ];
        if !is_whispers {
            rows.extend([
                SplitMenuRow {
                    action: SplitMenuAction::OpenChatters,
                    label: "Open chatters".into(),
                    shortcut: None,
                },
                SplitMenuRow {
                    action: SplitMenuAction::OpenChannelPopup,
                    label: "Open channel popup".into(),
                    shortcut: None,
                },
                SplitMenuRow {
                    action: SplitMenuAction::ModLog,
                    label: "Mod Log".into(),
                    shortcut: None,
                },
                SplitMenuRow {
                    action: SplitMenuAction::CycleDeletedMode,
                    label: match deleted_mode_override {
                        Some(mode) => format!("Deleted: {}", deleted_mode_label(mode)).into(),
                        None => format!(
                            "Deleted: Default ({})",
                            deleted_mode_label(backend::settings::get_deleted_message_mode())
                        )
                        .into(),
                    },
                    shortcut: None,
                },
            ]);
        }
        if !is_whispers && custom_player_path.is_some() {
            rows.push(SplitMenuRow {
                action: SplitMenuAction::OpenCustomPlayer,
                label: "Open with Custom Player".into(),
                shortcut: None,
            });
        }
        if !is_whispers {
            rows.push(SplitMenuRow {
                action: SplitMenuAction::OpenStreamlink,
                label: "Open with Streamlink".into(),
                shortcut: None,
            });
        }
        rows.push(SplitMenuRow {
            action: SplitMenuAction::CloseSplit,
            label: "Close split".into(),
            shortcut: shortcut_for(crate::backend::hotkeys::HotkeyAction::CloseSplit),
        });

        let viewport = window.viewport_size();
        let (intrinsic_width, shortcut_column) = measure_split_menu_width(&rows, window);
        let dropdown_fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            intrinsic_width.clamp(SPLIT_MENU_MIN_WIDTH_PX, SPLIT_MENU_MAX_WIDTH_PX),
            SPLIT_MENU_MAX_HEIGHT_PX,
            8.0,
        );
        let dropdown_width = dropdown_fit.width;
        let show_shortcuts = shortcut_column <= 0.0
            || dropdown_width
                >= MenuMetrics::ITEM_PADDING_X * 2.0
                    + SPLIT_MENU_BORDER_PX
                    + SPLIT_MENU_MIN_LABEL_PX
                    + shortcut_column;
        let horizontal_inset = if header_collapsed { 8.0 } else { 28.0 };
        let vertical_offset = if header_collapsed {
            40.0
        } else {
            header_height + 4.0
        };
        let dropdown_x: f32 =
            (bounds.origin.x + bounds.size.width - px(dropdown_width + horizontal_inset)).into();
        let dropdown_y: f32 = (bounds.origin.y + px(vertical_offset)).into();

        let clamped_x = crate::utils::clamp_overlay_axis(
            dropdown_x,
            dropdown_width,
            f32::from(viewport.width),
            dropdown_fit.margin_x,
        );
        let clamped_y = crate::utils::clamp_overlay_axis(
            dropdown_y.max(chrome_height + dropdown_fit.margin_y),
            dropdown_fit.height,
            f32::from(viewport.height),
            dropdown_fit.margin_y,
        );

        div()
            .id("settings-dropdown-overlay")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.overlay_manager
                        .update(cx, |o, _| o.close_all_split_settings());
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .child({
                let menu = self.chrome_menu_host(cx);
                let nav = menu.nav.clone();
                let app = cx.entity();
                let actions: Vec<MenuAction> = rows
                    .iter()
                    .map(|row| {
                        let path = path_key.to_vec();
                        let ch = channel.clone();
                        let player_path = custom_player_path.clone();
                        let action = row.action;
                        let app = app.clone();
                        Rc::new(move |window: &mut Window, cx: &mut App| {
                            app.update(cx, |this, cx| {
                                match action {
                                    SplitMenuAction::ToggleHeader => {
                                        this.set_split_header_collapsed(
                                            path.clone(),
                                            !header_collapsed,
                                            cx,
                                        );
                                    }
                                    SplitMenuAction::AddSplit => {
                                        this.add_empty_split(
                                            path.clone(),
                                            AddSplitDirection::Right,
                                            cx,
                                        );
                                    }
                                    SplitMenuAction::ChangeChannel => {
                                        this.set_active_split(path.clone(), cx);
                                        let _ = this.start_change_active_split_channel(cx);
                                    }
                                    SplitMenuAction::LiveFollowedChannels => {
                                        this.overlay_manager
                                            .update(cx, |o, _| o.close_all_split_settings());
                                        let source = this
                                            .app_state
                                            .read(cx)
                                            .active_tab()
                                            .layout
                                            .split_id_at_path(&path);
                                        this.open_live_followed_switcher(source, window, cx);
                                        return;
                                    }
                                    SplitMenuAction::OpenChatters => {
                                        backend::open_chatter_list_async(ch.clone());
                                    }
                                    SplitMenuAction::OpenChannelPopup => {
                                        crate::components::chat::open_channel_popup(
                                            ch.clone(),
                                            this.app_state.clone(),
                                            cx,
                                        );
                                    }
                                    SplitMenuAction::ModLog => this.open_mod_log(cx),
                                    SplitMenuAction::CycleDeletedMode => {
                                        this.set_split_deleted_mode(
                                            path.clone(),
                                            next_deleted_mode(deleted_mode_override),
                                            cx,
                                        );
                                        // Stay open so the label shows the new mode.
                                        return;
                                    }
                                    SplitMenuAction::OpenCustomPlayer => {
                                        if let Some(player_path) = player_path.clone() {
                                            backend::commands::launch_stream_player(
                                                &ch,
                                                backend::commands::StreamPlayer::Custom(
                                                    player_path,
                                                ),
                                            );
                                        }
                                    }
                                    SplitMenuAction::OpenStreamlink => {
                                        backend::commands::launch_stream_player(
                                            &ch,
                                            backend::commands::StreamPlayer::Streamlink,
                                        );
                                    }
                                    SplitMenuAction::CloseSplit => {
                                        this.overlay_manager
                                            .update(cx, |o, _| o.close_all_split_settings());
                                        this.close_pane(path.clone(), cx);
                                        cx.notify();
                                        return;
                                    }
                                }
                                this.overlay_manager
                                    .update(cx, |o, _| o.close_all_split_settings());
                                cx.notify();
                            });
                        }) as MenuAction
                    })
                    .collect();
                menu_panel()
                    .id("split-settings-dropdown-panel")
                    .absolute()
                    .left(px(clamped_x))
                    .top(px(clamped_y))
                    .w(px(dropdown_width))
                    .max_h(px(dropdown_fit.height))
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .menu_keyboard(&menu.focus, &nav, actions.clone(), menu.close_handler())
                    .children(rows.into_iter().zip(actions).enumerate().map(
                        |(row_index, (row, action))| {
                            menu_item(("split-menu-item", row_index), row_index, &nav, action)
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(MenuMetrics::ITEM_TEXT))
                                        .text_color(colors::text_primary())
                                        .child(row.label),
                                )
                                .when_some(
                                    row.shortcut.filter(|_| show_shortcuts),
                                    |item, shortcut| {
                                        item.child(
                                            div()
                                                .flex_shrink_0()
                                                .whitespace_nowrap()
                                                .text_size(px(MenuMetrics::SECONDARY_TEXT))
                                                .text_color(colors::text_muted())
                                                .child(shortcut),
                                        )
                                    },
                                )
                        },
                    ))
            })
            .into_any_element()
    }
}
