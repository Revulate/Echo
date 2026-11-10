//! Automation hooks the UI fixture drives the settings modal through; they reuse the production controllers and never persist.

use super::*;

impl SettingsModalState {
    /// Focus the production search controller without synthesizing key events.
    #[doc(hidden)]
    pub fn benchmark_focus_search(&mut self) {
        self.reconcile_selected_section();
        self.surface.benchmark_focus_search();
        self.pull_selected_section();
    }

    /// Replace the search query through the same bounded text handler used by
    /// typing and paste in the real settings surface.
    #[doc(hidden)]
    pub fn benchmark_set_search_query(&mut self, query: &str) {
        self.reconcile_selected_section();
        self.surface.benchmark_set_query(query);
        self.pull_selected_section();
    }

    /// Reveal developer navigation for one automation run without changing the
    /// user's persisted developer-mode preference.
    #[doc(hidden)]
    pub fn benchmark_reveal_advanced(&mut self) {
        self.reconcile_selected_section();
        self.surface.benchmark_reveal_advanced();
        self.pull_selected_section();
    }

    /// Navigate exactly the way the tree keyboard model does, so the acceptance
    /// run traverses the production key handler instead of a shortcut.
    #[doc(hidden)]
    pub fn benchmark_press_tree_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.reconcile_selected_section();
        let handled = self.surface.benchmark_press_tree_key(key, window, cx);
        self.pull_selected_section();
        handled
    }

    #[doc(hidden)]
    pub fn benchmark_press_surface_key(
        &mut self,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let event = KeyDownEvent {
            keystroke: Keystroke {
                key: key.to_string(),
                key_char: None,
                modifiers: Modifiers {
                    shift,
                    ..Default::default()
                },
            },
            is_held: false,
            prefer_character_input: false,
        };
        crate::components::settings::settings_surface::handle_shell_key(self, &event, window, cx);
    }

    #[doc(hidden)]
    pub fn benchmark_focus_region(&self, window: &Window, cx: &App) -> &'static str {
        self.surface.benchmark_focus_region(window, cx)
    }

    #[doc(hidden)]
    pub fn benchmark_content_bounds(&self) -> Option<Bounds<Pixels>> {
        self.surface.benchmark_content_bounds()
    }

    #[doc(hidden)]
    pub fn benchmark_focus_search_region(&mut self, window: &mut Window, cx: &mut App) {
        self.reconcile_selected_section();
        self.surface.focus_search(window, cx);
    }

    /// Open on an explicit destination expressed as a slug, current or legacy.
    #[doc(hidden)]
    pub fn benchmark_open_slug(&mut self, slug: &str) {
        self.open_at(&SettingsOpenRequest::from_slug(slug));
    }

    /// Navigation state the acceptance checkpoints assert against.
    #[doc(hidden)]
    pub fn benchmark_navigation_snapshot(&self) -> SettingsNavigationSnapshot {
        let (page, anchor) = self.surface.benchmark_location_slugs();
        let (hidden_settings, hidden_subsections, hidden_pages) =
            self.surface.benchmark_hidden_developer_counts();
        let transfer = self.surface.open_request();
        SettingsNavigationSnapshot {
            page,
            anchor,
            focused_row: self.surface.benchmark_focused_label(),
            tree_rows: self.surface.benchmark_tree_labels(),
            status: self.surface.benchmark_status_announcement(),
            hidden_settings,
            hidden_subsections,
            hidden_pages,
            transfer_query: transfer.query.clone(),
            transfer_matches_location: transfer.location == Some(self.surface.location()),
            active_block: self.surface.benchmark_active_block(),
            target_block: self.surface.benchmark_target_block(),
            anchor_pending: self.surface.benchmark_anchor_pending(),
        }
    }

    /// Whether the current page has mounted the requested disclosure body.
    #[doc(hidden)]
    pub fn benchmark_disclosure_expanded(&self, id: &str) -> bool {
        self.surface.benchmark_disclosure_expanded(id)
    }

    /// Secondary text on the tree row for the current destination.
    #[doc(hidden)]
    pub fn benchmark_secondary_text(&self) -> Option<String> {
        self.surface
            .benchmark_secondary_for(self.surface.location())
    }

    /// Whether `request` would open the other surface on exactly the destination
    /// this one is showing.
    ///
    /// This is what "modal and popout behave identically" reduces to for a
    /// transfer: the payload survives the handoff *and* resolves back to the
    /// same page and anchor under the same disclosure state.
    #[doc(hidden)]
    pub fn benchmark_request_lands_here(&self, request: &SettingsOpenRequest) -> bool {
        request.resolve(self.surface.developer_visible()) == self.surface.location()
    }

    #[doc(hidden)]
    pub fn benchmark_snapshot(&self) -> SettingsAutomationSnapshot {
        let visible_result_count = self.surface.benchmark_result_count();
        SettingsAutomationSnapshot {
            is_open: self.is_open,
            search_focused: self.surface.search_active(),
            query: self.surface.query().to_string(),
            selected_destination: self.selected_label(),
            visible_result_count,
            no_results: visible_result_count == 0,
            advanced_revealed: self.surface.benchmark_developer_revealed(),
            developer_visible: self.surface.developer_visible(),
        }
    }

    /// Automation teardown. Deliberately does not persist: the harness asserts
    /// that driving the whole settings surface leaves preferences untouched.
    #[doc(hidden)]
    pub fn benchmark_close_and_cleanup(&mut self) {
        self.close_silent();
    }
}
