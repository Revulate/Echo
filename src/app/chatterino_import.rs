//! Applying a Chatterino import to the running app, and the first-run card
//! that offers one.
//!
//! Nothing imported takes effect until both writes succeed: the request is
//! staged on the UI thread, the backups and the settings.json and tabs.json
//! writes run on the background executor, and only after both succeed do the
//! live settings (and their compiled matchers) and the tab list change.

use super::EchoApp;
use crate::backend;
use crate::backend::bridge::chatterino_import::{
    self as import, ImportBackups, ImportError, ImportOutcome, StagedImport, TabImportMode,
};
use crate::components::settings::{
    SettingsLocation, SettingsOpenRequest, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::ui::{ButtonExt, button_hover};
use crate::state::TabState;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use echo_core::chatterino_import::layout_channel_key;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::HashSet;
use std::sync::Arc;

const WELCOME_CARD_WIDTH_PX: f32 = 340.0;

impl EchoApp {
    /// Stage a queued import and start its writes. Returns whether anything
    /// on screen changed.
    pub(super) fn process_pending_chatterino_import(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(request) = import::take_pending_apply() else {
            return false;
        };
        let current_tabs = self.app_state.read(cx).tabs_for_persistence();
        let mut staged = import::stage(&request, &current_tabs);
        let visibility = self.app_state.read(cx).tab_visibility_mode();
        for tab in &mut staged.tabs {
            tab.visibility = visibility.clone();
        }
        // With only Echo's empty starting tab open nothing can be lost, so
        // the imported tabs take its place instead of trailing behind it.
        let replaces_starter = staged.tab_mode == TabImportMode::Append
            && !staged.tabs.is_empty()
            && current_tabs
                .iter()
                .all(|tab| layout_channel_key(&tab.layout).is_empty());
        if replaces_starter {
            staged.tab_mode = TabImportMode::Replace;
        }
        if staged.tab_mode == TabImportMode::Replace
            && !staged.tabs.is_empty()
            && !replaces_starter
            && !request.replace_confirmed
        {
            let tab_count = staged.tabs.len();
            import::ask_to_replace(request, tab_count);
            return true;
        }
        if staged.is_empty() {
            self.finish_chatterino_import(staged, false, Ok(ImportBackups::default()), cx);
            return true;
        }

        let staged_settings = (!staged.collections.is_empty())
            .then(|| backend::settings::settings_with_collections(&staged.collections));
        let staged_tabs =
            (!staged.tabs.is_empty()).then(|| Arc::new(persisted_tab_list(&current_tabs, &staged)));
        let plan = request.plan.clone();
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn(async move { import::write_import(&plan, staged_settings, staged_tabs) })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.finish_chatterino_import(staged, replaces_starter, written, cx);
            });
        })
        .detach();
        true
    }

    /// Apply a staged import whose writes succeeded, then report either way.
    fn finish_chatterino_import(
        &mut self,
        mut staged: StagedImport,
        replaces_starter: bool,
        written: Result<ImportBackups, ImportError>,
        cx: &mut Context<Self>,
    ) {
        let result: Result<ImportOutcome, ImportError> = match written {
            Ok(backups) => {
                let collections = std::mem::take(&mut staged.collections);
                if !collections.is_empty() {
                    backend::settings::append_collections(collections);
                }
                let tabs = std::mem::take(&mut staged.tabs);
                if !tabs.is_empty() {
                    self.add_imported_tabs(
                        tabs,
                        staged.selected_tab,
                        staged.tab_mode,
                        replaces_starter,
                        cx,
                    );
                }
                staged.outcome.backup_files = backups.file_names();
                Ok(staged.outcome)
            }
            Err(error) => Err(error),
        };
        match &result {
            Ok(outcome) => {
                crate::log_persist!(
                    info,
                    imported = outcome.total_imported(),
                    skipped = outcome.skipped.len(),
                    "Imported a Chatterino setup"
                );
                self.chatterino_welcome = None;
                backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                    message: "Chatterino import finished".to_string(),
                    detail: Some(outcome.summary()),
                });
            }
            Err(error) => {
                let message = error.message();
                crate::log_persist!(warn, "Chatterino import did not run: {}", message);
                backend::push_ui_action(backend::UiAction::ShowWarningToast {
                    message: "Chatterino import didn't run".to_string(),
                    detail: Some(message),
                });
            }
        }
        import::finish_import(&result);
        // The result line lives in Settings, which may be a popout window.
        cx.refresh_windows();
    }

    /// Put the imported tabs into the live tab list with startup-style
    /// connection tiers. Replacing closes the old tabs as one reopenable
    /// batch; `quiet_close` skips the Undo toast for Echo's blank starting tab.
    fn add_imported_tabs(
        &mut self,
        tabs: Vec<TabState>,
        selected: Option<usize>,
        mode: TabImportMode,
        quiet_close: bool,
        cx: &mut Context<Self>,
    ) {
        let (old_ids, mut released) = {
            let state = self.app_state.read(cx);
            let ids: HashSet<String> = state.tabs.iter().map(|tab| tab.id.clone()).collect();
            let released = match mode {
                TabImportMode::Append => Vec::new(),
                TabImportMode::Replace => {
                    let closing: Vec<&TabState> = state.tabs.iter().collect();
                    super::released_channels(state, &closing)
                }
            };
            (ids, released)
        };
        let target_id = match mode {
            TabImportMode::Append => None,
            TabImportMode::Replace => tabs.get(selected.unwrap_or(0)).map(|tab| tab.id.clone()),
        };

        self.app_state.update(cx, |state, cx| {
            state.add_tabs_with_startup_priority(tabs, target_id.as_deref());
            if mode == TabImportMode::Replace {
                // Select first so closing the old tabs activates nothing else.
                if let Some(idx) = target_id
                    .as_deref()
                    .and_then(|id| state.tabs.iter().position(|tab| tab.id == id))
                {
                    state.switch_tab(idx, cx);
                }
                state.close_tabs_with_ids(&old_ids);
            }
        });
        if quiet_close {
            self.seen_closed_tab_generation = self.app_state.read(cx).closed_tab_generation();
        }

        released.sort_unstable();
        released.dedup();
        for channel in released {
            self.cleanup_channel_caches(&channel, cx);
        }
        self.save_tabs(cx);
        self.refocus_active_input(cx);
        cx.notify();
    }

    /// The dismissible card shown on a first run that found Chatterino.
    pub(super) fn render_chatterino_welcome(
        &self,
        top: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.chatterino_welcome.as_ref()?;
        let card = div()
            .id("chatterino-welcome")
            .role(Role::Group)
            .aria_label("Import from Chatterino")
            .absolute()
            .top(px(top + Spacing::MD))
            .right(px(Spacing::MD))
            .w(px(WELCOME_CARD_WIDTH_PX))
            .max_w_full()
            .p(px(Spacing::MD))
            .flex()
            .flex_col()
            .gap(px(Spacing::SM))
            .rounded(Radii::md())
            .border_1()
            .border_color(colors::border())
            .bg(colors::bg_surface())
            .shadow_lg()
            // Clicks on the card must not reach the chat underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Found your Chatterino setup."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(colors::text_secondary())
                    .child("Bring over your tabs, highlights and more?"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(import::NOT_IMPORTED_NOTE),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(Spacing::SM))
                    .child(
                        welcome_button("chatterino-welcome-not-now", "Not now", false)
                            .on_accessible_activate(cx.listener(
                                |this, _: &ClickEvent, _window, cx| {
                                    this.chatterino_welcome = None;
                                    backend::settings::set_chatterino_import_prompt_dismissed(true);
                                    cx.notify();
                                },
                            )),
                    )
                    .child(
                        welcome_button("chatterino-welcome-review", "Review import", true)
                            .on_accessible_activate(cx.listener(
                                |this, _: &ClickEvent, _window, cx| {
                                    this.chatterino_welcome = None;
                                    let request = SettingsOpenRequest {
                                        location: Some(SettingsLocation::anchor(
                                            SettingsSectionId::Advanced,
                                            SettingsSubsectionId::Configuration,
                                        )),
                                        setting: Some(import::SETTINGS_TARGET_ID.to_string()),
                                        ..SettingsOpenRequest::default()
                                    };
                                    this.overlay_manager.update(cx, |overlays, cx| {
                                        overlays.open_settings_at(request, cx);
                                    });
                                    cx.notify();
                                },
                            )),
                    ),
            );
        Some(card.into_any_element())
    }
}

fn welcome_button(id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .px(px(Spacing::SM))
        .py(px(Spacing::XS))
        .rounded(Radii::sm())
        .cursor_pointer()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .when(primary, |button| {
            button
                .text_color(colors::accent_text())
                .bg(colors::accent().opacity(0.12))
                .hover(|style| style.bg(colors::accent().opacity(0.2)))
        })
        .when(!primary, |button| {
            button
                .text_color(colors::text_secondary())
                .border_1()
                .border_color(colors::border())
                .hover(button_hover)
        })
        .child(label)
}

/// tabs.json as it will be once the import lands.
fn persisted_tab_list(current: &[TabState], staged: &StagedImport) -> Vec<TabState> {
    match staged.tab_mode {
        TabImportMode::Append => current.iter().chain(&staged.tabs).cloned().collect(),
        TabImportMode::Replace => {
            let selected = staged.selected_tab.unwrap_or(0);
            let mut tabs = staged.tabs.clone();
            for (idx, tab) in tabs.iter_mut().enumerate() {
                tab.is_selected = idx == selected;
            }
            tabs
        }
    }
}
