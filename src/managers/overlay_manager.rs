//! Overlay Manager
//!
//! Handles:
//! - Modal lifecycle (settings, login, search, mod log)
//! - Overlay positioning (user card, context menu, autocomplete)
//! - Toast notifications
//! - Z-ordering (backdrop -> modals -> dropdowns -> context menus -> toasts)
//! - Dropdown state (account switcher and split settings)

use crate::components::auth::LoginModalState;
use crate::components::overlays::{
    Autocomplete, AutocompleteState, ChatterList, ChatterListState, ConfirmationModal, ContextMenu,
    ContextMenuState, EmotePicker, EmotePickerState, HotkeyCheatSheet, MetricsOverlay,
    ModLogViewer, QuickSwitcher, SearchModal, ThreadViewer, ThreadViewerState, ToastManager,
    UserCard, UserCardState, traits::Openable,
};
use crate::components::settings::{SettingsModalState, SettingsOpenRequest, SettingsSectionId};
use crate::components::tabs::TabBarRenderer;
use crate::components::ui::menu::MenuNav;
use crate::managers::split_layout_manager::PathKey;
use gpui::*;

/// Overlay layers for the escape stack.
/// Escape dismisses the most recently opened layer that is still open;
/// variant order carries no priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeLayer {
    /// Tab right-click menu and tab overflow dropdown
    TabMenu,
    /// Split ⋯ action menu
    SplitMenu,
    /// Top-bar account dropdown
    AccountMenu,
    /// Message context menu
    ContextMenu,
    /// Autocomplete dropdown (never pushed; its own key handling closes it)
    Autocomplete,
    /// User profile card
    UserCard,
    /// Emote picker
    EmotePicker,
    /// Quick switcher
    QuickSwitcher,
    /// Metrics overlay HUD
    Metrics,
    /// Chatter list
    ChatterList,
    /// Hotkey cheat sheet
    CheatSheet,
    /// Mod log viewer
    ModLog,
    /// Settings modal
    Settings,
}

/// Manager entity for overlay and modal state
pub struct OverlayManager {
    pub settings_modal: Entity<SettingsModalState>,
    pub login_modal: Entity<LoginModalState>,
    pub search_modal: Entity<SearchModal>,
    pub mod_log_viewer: Entity<ModLogViewer>,
    pub emote_picker: Entity<EmotePicker>,
    pub confirmation_modal: Entity<ConfirmationModal>,
    pub hotkey_cheat_sheet: Entity<HotkeyCheatSheet>,
    pub quick_switcher: Entity<QuickSwitcher>,

    pub context_menu: Entity<ContextMenu>,
    pub autocomplete: Entity<Autocomplete>,
    pub user_card: Entity<UserCard>,
    pub chatter_list: Entity<ChatterList>,
    pub thread_viewer: Entity<ThreadViewer>,
    pub toast_manager: Entity<ToastManager>,
    pub metrics_overlay: Entity<MetricsOverlay>,

    pub account_switcher_open: bool,

    pub open_settings_dropdown: Option<(PathKey, String)>,

    pub shown_reauth_toast: bool,

    /// Keyboard focus and highlight of the open chrome menu (tab, split or
    /// account menu). Only one of them can be open at a time.
    pub menu_focus: FocusHandle,
    pub menu_nav: MenuNav,
    tab_bar: Option<WeakEntity<TabBarRenderer>>,

    /// Escape stack for ordered overlay dismissal
    /// Push when overlay opens, pop when it closes or Escape is pressed
    escape_stack: Vec<EscapeLayer>,
    _emote_picker_subscription: Subscription,
}

impl OverlayManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings_modal = cx.new(SettingsModalState::new);
        let login_modal = cx.new(|_| LoginModalState::default());
        let search_modal = cx.new(|_| SearchModal::new(vec![]));
        let mod_log_viewer = cx.new(|_| ModLogViewer::new());
        let emote_picker = cx.new(|_| EmotePicker::new(EmotePickerState::default()));
        let emote_picker_subscription = cx.observe(&emote_picker, |manager, picker, cx| {
            if picker.read(cx).state.is_open {
                if !manager.escape_stack.contains(&EscapeLayer::EmotePicker) {
                    manager.push_escape_layer(EscapeLayer::EmotePicker);
                }
            } else {
                manager.remove_escape_layer(EscapeLayer::EmotePicker);
            }
        });
        let confirmation_modal = cx.new(ConfirmationModal::new);
        let hotkey_cheat_sheet = cx.new(|_| HotkeyCheatSheet::new());
        let quick_switcher = cx.new(QuickSwitcher::new);
        let context_menu = cx.new(|_| ContextMenu::new(ContextMenuState::default()));
        let autocomplete = cx.new(|_| Autocomplete::new(AutocompleteState::default()));
        let user_card = cx.new(|_| UserCard::new(UserCardState::default()));
        let chatter_list = cx.new(ChatterList::new);
        let thread_viewer = cx.new(|_| ThreadViewer::new(ThreadViewerState::default()));
        let toast_manager = cx.new(|_| ToastManager::new());
        let metrics_overlay = cx.new(|_| MetricsOverlay::new());

        Self {
            settings_modal,
            login_modal,
            search_modal,
            mod_log_viewer,
            emote_picker,
            confirmation_modal,
            hotkey_cheat_sheet,
            quick_switcher,
            context_menu,
            autocomplete,
            user_card,
            chatter_list,
            thread_viewer,
            toast_manager,
            metrics_overlay,
            account_switcher_open: false,
            open_settings_dropdown: None,
            shown_reauth_toast: false,
            menu_focus: cx.focus_handle(),
            menu_nav: MenuNav::default(),
            tab_bar: None,
            escape_stack: Vec::with_capacity(4),
            _emote_picker_subscription: emote_picker_subscription,
        }
    }

    pub fn open_login(&mut self, cx: &mut Context<Self>) {
        self.close_account_switcher();
        self.login_modal.update(cx, |state, _| {
            if !state.is_open {
                state.open();
            }
        });
        cx.notify();
    }

    /// Open settings on a page, or on the remembered location when `None`.
    pub fn open_settings(&mut self, section: Option<SettingsSectionId>, cx: &mut Context<Self>) {
        self.open_settings_at(SettingsOpenRequest::from_section(section), cx);
    }

    /// Open settings on an exact destination.
    ///
    /// The request carries a page *and* an anchor (plus any transferred query),
    /// so a caller can point at a subsection instead of dropping the user at the
    /// top of a long page.
    pub fn open_settings_at(&mut self, request: SettingsOpenRequest, cx: &mut Context<Self>) {
        use crate::backend::lock_utils::MutexExt;
        use crate::backend::settings::SETTINGS;

        let use_popout = SETTINGS.lock_or_recover().open_settings_as_popout;

        if use_popout {
            crate::components::settings::open_settings_popout(request, cx);
        } else {
            self.settings_modal.update(cx, |state, cx| {
                state.open_at(&request);
                cx.notify();
            });
            self.push_escape_layer(EscapeLayer::Settings);
        }
    }

    pub fn open_mod_log(&mut self, cx: &mut Context<Self>) {
        self.mod_log_viewer.update(cx, |viewer, _| {
            viewer.open();
        });
        self.push_escape_layer(EscapeLayer::ModLog);
    }

    pub fn close_mod_log(&mut self, cx: &mut Context<Self>) {
        self.mod_log_viewer.update(cx, |viewer, _| {
            viewer.close();
        });
        self.remove_escape_layer(EscapeLayer::ModLog);
    }

    pub fn toggle_mod_log(&mut self, cx: &mut Context<Self>) {
        let is_open = self.mod_log_viewer.read(cx).state.is_open;
        if is_open {
            self.close_mod_log(cx);
        } else {
            self.open_mod_log(cx);
        }
    }

    pub fn open_cheat_sheet(&mut self, cx: &mut Context<Self>) {
        self.hotkey_cheat_sheet.update(cx, |sheet, _| {
            sheet.open();
        });
        self.push_escape_layer(EscapeLayer::CheatSheet);
    }

    pub fn close_cheat_sheet(&mut self, cx: &mut Context<Self>) {
        self.hotkey_cheat_sheet.update(cx, |sheet, _| {
            sheet.close();
        });
        self.remove_escape_layer(EscapeLayer::CheatSheet);
    }

    pub fn open_quick_switcher(
        &mut self,
        channels: Vec<crate::components::overlays::QuickSwitchItem>,
        mode: crate::components::overlays::QuickSwitchMode,
        cx: &mut Context<Self>,
    ) {
        self.quick_switcher.update(cx, |qs, cx| {
            qs.open_with_channels(channels, mode, cx);
        });
        self.push_escape_layer(EscapeLayer::QuickSwitcher);
    }

    pub fn close_quick_switcher(&mut self, cx: &mut Context<Self>) {
        self.quick_switcher.update(cx, |qs, cx| {
            qs.close_switcher(cx);
        });
        self.remove_escape_layer(EscapeLayer::QuickSwitcher);
    }

    pub fn toggle_metrics(&mut self, cx: &mut Context<Self>) {
        self.metrics_overlay.update(cx, |overlay, _| {
            overlay.toggle();
        });
        let is_open = self.metrics_overlay.read(cx).is_open();
        crate::diagnostics::fps_profiler::set_dashboard_enabled(is_open);
        if is_open {
            self.push_escape_layer(EscapeLayer::Metrics);
        } else {
            self.remove_escape_layer(EscapeLayer::Metrics);
        }
    }

    pub fn close_emote_picker(&mut self, cx: &mut Context<Self>) {
        self.emote_picker.update(cx, |picker, cx| {
            picker.close(true, cx);
        });
        self.remove_escape_layer(EscapeLayer::EmotePicker);
    }

    pub fn open_chatter_list(&mut self, state: ChatterListState, cx: &mut Context<Self>) {
        self.chatter_list.update(cx, |list, cx| {
            list.open_loaded(state, cx);
        });
        self.push_escape_layer(EscapeLayer::ChatterList);
    }

    pub fn open_chatter_list_loading(&mut self, channel: String, cx: &mut Context<Self>) {
        self.chatter_list.update(cx, |list, cx| {
            list.open(channel, cx);
        });
        self.push_escape_layer(EscapeLayer::ChatterList);
    }

    pub fn close_chatter_list(&mut self, cx: &mut Context<Self>) {
        self.chatter_list.update(cx, |list, cx| {
            list.close(cx);
        });
        self.remove_escape_layer(EscapeLayer::ChatterList);
    }

    pub fn open_thread_viewer(
        &mut self,
        message_id: String,
        channel: String,
        x: f32,
        y: f32,
        cx: &mut Context<Self>,
    ) {
        self.thread_viewer.update(cx, |viewer, _| {
            viewer.open_thread(message_id, channel, x, y);
        });
    }

    pub fn close_thread_viewer(&mut self, cx: &mut Context<Self>) {
        self.thread_viewer.update(cx, |viewer, _| {
            viewer.state.close();
        });
    }

    pub fn open_context_menu(&mut self, state: ContextMenuState, cx: &mut Context<Self>) {
        self.context_menu.update(cx, |menu, _| {
            menu.update_state(state);
        });
        self.push_escape_layer(EscapeLayer::ContextMenu);
    }

    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu.update(cx, |menu, _| {
            menu.state.close();
        });
        self.remove_escape_layer(EscapeLayer::ContextMenu);
    }

    pub fn close_autocomplete(&mut self, cx: &mut Context<Self>) {
        self.autocomplete.update(cx, |ac, _| {
            ac.state.close();
        });
        self.remove_escape_layer(EscapeLayer::Autocomplete);
    }

    /// Show an info toast
    pub fn toast_info(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast_manager.update(cx, |m, cx| {
            m.info(message);
            cx.notify();
        });
    }

    /// Show a success toast
    pub fn toast_success(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast_manager.update(cx, |m, cx| {
            m.success(message);
            cx.notify();
        });
    }

    /// Show an error toast
    pub fn toast_error(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast_manager.update(cx, |m, cx| {
            m.error(message);
            cx.notify();
        });
    }

    /// Show a custom toast
    pub fn push_toast(&self, toast: crate::components::overlays::Toast, cx: &mut Context<Self>) {
        self.toast_manager.update(cx, |m, cx| {
            m.push(toast);
            cx.notify();
        });
    }

    pub fn toggle_account_switcher(&mut self) {
        self.account_switcher_open = !self.account_switcher_open;
        self.menu_nav.reset();
        if self.account_switcher_open {
            self.push_escape_layer(EscapeLayer::AccountMenu);
        } else {
            self.remove_escape_layer(EscapeLayer::AccountMenu);
        }
    }

    pub fn close_account_switcher(&mut self) {
        self.account_switcher_open = false;
        self.remove_escape_layer(EscapeLayer::AccountMenu);
    }

    pub fn toggle_split_settings(&mut self, path_key: PathKey, channel: String) {
        self.menu_nav.reset();
        if self.open_settings_dropdown.as_ref().map(|(p, _)| p) == Some(&path_key) {
            self.open_settings_dropdown = None;
            self.remove_escape_layer(EscapeLayer::SplitMenu);
        } else {
            self.open_settings_dropdown = Some((path_key, channel));
            self.push_escape_layer(EscapeLayer::SplitMenu);
        }
    }

    pub fn close_all_split_settings(&mut self) {
        self.open_settings_dropdown = None;
        self.remove_escape_layer(EscapeLayer::SplitMenu);
    }

    /// Let the escape stack close the tab bar's menus, which the tab bar owns.
    pub fn attach_tab_bar(&mut self, tab_bar: WeakEntity<TabBarRenderer>) {
        self.tab_bar = Some(tab_bar);
    }

    /// Track the tab bar's menus on the escape stack. The tab bar opens and
    /// closes them from its own listeners, so the app syncs the layer as it
    /// renders.
    pub fn sync_tab_menu_layer(&mut self, open: bool) {
        let tracked = self.escape_stack.contains(&EscapeLayer::TabMenu);
        if open && !tracked {
            self.menu_nav.reset();
            self.push_escape_layer(EscapeLayer::TabMenu);
        } else if !open && tracked {
            self.remove_escape_layer(EscapeLayer::TabMenu);
        }
    }

    fn tab_menu_open(&self, cx: &App) -> bool {
        self.tab_bar
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|tab_bar| {
                let tab_bar = tab_bar.read(cx);
                tab_bar.is_context_menu_open() || tab_bar.is_overflow_open()
            })
    }

    pub fn is_split_settings_open(&self, path_key: PathKey) -> bool {
        self.open_settings_dropdown
            .as_ref()
            .is_some_and(|(open, _)| *open == path_key)
    }

    /// Whether an in-window overlay that owns keyboard text input is open.
    /// Typed-text redirection and active-input refocus must leave these alone.
    pub fn text_overlay_open(&self, cx: &App) -> bool {
        self.settings_modal.read(cx).is_open
            || self.login_modal.read(cx).is_open
            || self.search_modal.read(cx).state.is_open
            || self.mod_log_viewer.read(cx).state.is_open
            || self.emote_picker.read(cx).state.is_open
            || self.confirmation_modal.read(cx).state.is_open
            || self.quick_switcher.read(cx).is_open
            || self.context_menu.read(cx).state.is_open
            || self.user_card.read(cx).is_open()
            || self.chatter_list.read(cx).state.is_open
            || self.thread_viewer.read(cx).is_open()
    }

    /// Push a layer onto the escape stack (call when overlay opens)
    pub fn push_escape_layer(&mut self, layer: EscapeLayer) {
        self.escape_stack.retain(|candidate| *candidate != layer);
        self.escape_stack.push(layer);
    }

    /// Remove a specific layer from the escape stack (call when overlay closes)
    pub fn remove_escape_layer(&mut self, layer: EscapeLayer) {
        self.escape_stack.retain(|&l| l != layer);
    }

    /// Dismiss the top-most overlay tracked in the escape stack.
    /// Returns true when an overlay was closed.
    pub fn dismiss_top_escape_layer(&mut self, cx: &mut Context<Self>) -> bool {
        let layer = loop {
            let Some(layer) = self.escape_stack.pop() else {
                return false;
            };

            if self.escape_layer_is_open(layer, cx) {
                break layer;
            }
        };

        match layer {
            EscapeLayer::TabMenu => {
                if let Some(tab_bar) = self.tab_bar.as_ref().and_then(WeakEntity::upgrade) {
                    tab_bar.update(cx, |tab_bar, cx| {
                        tab_bar.close_context_menu();
                        tab_bar.close_overflow();
                        cx.notify();
                    });
                }
            }
            EscapeLayer::SplitMenu => self.close_all_split_settings(),
            EscapeLayer::AccountMenu => self.close_account_switcher(),
            EscapeLayer::ContextMenu => {
                self.context_menu.update(cx, |menu, _| {
                    menu.state.close();
                });
            }
            EscapeLayer::Autocomplete => {
                self.autocomplete.update(cx, |ac, _| {
                    ac.state.close();
                });
            }
            EscapeLayer::UserCard => {
                self.user_card.update(cx, |card, _| {
                    card.state.close();
                });
            }
            EscapeLayer::EmotePicker => {
                self.emote_picker.update(cx, |picker, cx| {
                    picker.close(true, cx);
                });
            }
            EscapeLayer::QuickSwitcher => {
                self.quick_switcher.update(cx, |qs, _| {
                    qs.close();
                });
            }
            EscapeLayer::Metrics => {
                self.metrics_overlay.update(cx, |overlay, _| {
                    overlay.state.is_open = false;
                });
                crate::diagnostics::fps_profiler::set_dashboard_enabled(false);
            }
            EscapeLayer::ChatterList => {
                self.chatter_list.update(cx, |list, _| {
                    list.state.is_open = false;
                });
            }
            EscapeLayer::CheatSheet => {
                self.hotkey_cheat_sheet.update(cx, |sheet, _| {
                    sheet.close();
                });
            }
            EscapeLayer::ModLog => {
                self.mod_log_viewer.update(cx, |viewer, _| {
                    viewer.close();
                });
            }
            EscapeLayer::Settings => {
                self.settings_modal.update(cx, |state, cx| {
                    state.close();
                    cx.notify();
                });
            }
        }

        true
    }

    /// Components may close themselves from their backdrop or close button.
    /// Check live entity state so stale stack entries never consume Escape.
    fn escape_layer_is_open(&self, layer: EscapeLayer, cx: &App) -> bool {
        match layer {
            EscapeLayer::TabMenu => self.tab_menu_open(cx),
            EscapeLayer::SplitMenu => self.open_settings_dropdown.is_some(),
            EscapeLayer::AccountMenu => self.account_switcher_open,
            EscapeLayer::ContextMenu => self.context_menu.read(cx).state.is_open,
            EscapeLayer::Autocomplete => self.autocomplete.read(cx).state.is_open,
            EscapeLayer::UserCard => self.user_card.read(cx).is_open(),
            EscapeLayer::EmotePicker => self.emote_picker.read(cx).state.is_open,
            EscapeLayer::QuickSwitcher => self.quick_switcher.read(cx).is_open,
            EscapeLayer::Metrics => self.metrics_overlay.read(cx).is_open(),
            EscapeLayer::ChatterList => self.chatter_list.read(cx).state.is_open,
            EscapeLayer::CheatSheet => self.hotkey_cheat_sheet.read(cx).is_open(),
            EscapeLayer::ModLog => self.mod_log_viewer.read(cx).state.is_open,
            EscapeLayer::Settings => self.settings_modal.read(cx).is_open,
        }
    }

    /// Close overlays that belong to a specific channel (when channel is closed)
    pub fn cleanup_channel_overlays(&mut self, channel: &str, cx: &mut Context<Self>) {
        if self.autocomplete.read(cx).state.channel == channel {
            self.close_autocomplete(cx);
        }
        if self.emote_picker.read(cx).state.channel == channel {
            self.close_emote_picker(cx);
        }
        if self.chatter_list.read(cx).state.channel == channel {
            self.close_chatter_list(cx);
        }
        if self
            .thread_viewer
            .read(cx)
            .state
            .channel
            .as_deref()
            .is_some_and(|thread_channel| thread_channel == channel)
        {
            self.close_thread_viewer(cx);
        }
    }

    /// Get the current moderation target from the open user card (if any)
    /// Returns (username, channel) if a user card is open
    pub fn get_moderation_target(&self, cx: &App) -> Option<(String, String)> {
        self.user_card.read(cx).get_moderation_target()
    }
}
