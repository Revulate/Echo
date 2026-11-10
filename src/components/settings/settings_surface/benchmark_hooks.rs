//! Automation hooks the UI fixture reads and drives the settings surface through.

use super::*;

impl SettingsSurface {
    #[doc(hidden)]
    pub fn benchmark_focus_search(&mut self) {
        self.search_active = true;
        self.developer_override = Some(false);
        self.search.sync(&self.query, false);
        if self.location.is_developer_only() {
            let location = restore_location(&navigation_prefs(), false);
            self.go_to(location, false);
        }
        self.resync_selection();
    }

    #[doc(hidden)]
    pub fn benchmark_focus_region(&self, window: &Window, cx: &App) -> &'static str {
        if self.search_is_focused(window, cx) {
            "search"
        } else if self.tree_focus.is_focused(window) {
            "navigation"
        } else if self.content_focus.contains_focused(window, cx) {
            "content"
        } else {
            "outside"
        }
    }

    #[doc(hidden)]
    pub fn benchmark_content_bounds(&self) -> Option<Bounds<Pixels>> {
        self.page_scroll
            .get(&self.location.section)
            .map(|scroll| scroll.handle().bounds())
    }

    #[doc(hidden)]
    pub fn benchmark_set_query(&mut self, query: &str) {
        self.set_query(query);
    }

    /// Run the production hidden-developer recovery without writing the user's
    /// developer-mode preference.
    #[doc(hidden)]
    pub fn benchmark_reveal_advanced(&mut self) {
        self.reveal_hidden_developer();
    }

    #[doc(hidden)]
    pub fn benchmark_developer_revealed(&self) -> bool {
        self.developer_override == Some(true)
    }

    #[doc(hidden)]
    pub fn benchmark_result_count(&self) -> usize {
        self.search.model().ordered_ids().len()
    }

    /// Press a key on the tree through the *production* key handler.
    ///
    /// The harness synthesizes the keystroke rather than the outcome, so the
    /// acceptance run exercises the same `handle_tree_key` the `on_key_down`
    /// listener calls — a regression in the key model fails the run instead of
    /// being papered over by a parallel test-only navigation path.
    #[doc(hidden)]
    pub fn benchmark_press_tree_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let event = KeyDownEvent {
            keystroke: Keystroke {
                modifiers: Modifiers::default(),
                key: key.to_string(),
                key_char: (key.chars().count() == 1).then(|| key.to_string()),
            },
            is_held: false,
            prefer_character_input: false,
        };
        self.handle_tree_key(&event, window, cx).handled
    }

    /// Current destination as the slug pair that is persisted and logged.
    #[doc(hidden)]
    pub fn benchmark_location_slugs(&self) -> (&'static str, Option<&'static str>) {
        (
            section_slug(self.location.section),
            self.location
                .subsection
                .and_then(|id| {
                    crate::components::settings::settings_navigation::subsection_descriptor(
                        self.location.section,
                        id,
                    )
                })
                .map(|descriptor| descriptor.slug),
        )
    }

    /// Visible tree rows flattened to `level:label` for log assertions.
    #[doc(hidden)]
    pub fn benchmark_tree_labels(&self) -> Vec<String> {
        self.visible_rows()
            .iter()
            .map(|row| format!("{}:{}", row.level, row.label))
            .collect()
    }

    /// Label of the row the tree keyboard model currently focuses.
    #[doc(hidden)]
    pub fn benchmark_focused_label(&self) -> Option<&'static str> {
        let focused = self.focused;
        self.visible_rows()
            .into_iter()
            .find(|row| row.node == focused)
            .map(|row| row.label)
    }

    /// Search-only secondary line on the row for `location`, e.g.
    /// `"Streamlink path · +3"`.
    #[doc(hidden)]
    pub fn benchmark_secondary_for(&self, location: SettingsLocation) -> Option<String> {
        let target = node_for_location(location);
        self.visible_rows()
            .into_iter()
            .find(|row| row.node == target)
            .and_then(|row| row.secondary)
    }

    /// Live-region text the status node would settle on, without waiting out the
    /// announcement delay.
    #[doc(hidden)]
    pub fn benchmark_status_announcement(&self) -> Option<String> {
        self.search.model().status_announcement()
    }

    /// `(settings, subsections, pages)` matching only behind the disclosure.
    #[doc(hidden)]
    pub fn benchmark_hidden_developer_counts(&self) -> (usize, usize, usize) {
        let counts = self.search.model().hidden_developer_counts();
        (counts.settings, counts.subsections, counts.pages)
    }

    /// Block index the current page's scroll-spy reports as active.
    ///
    /// This is the value a laid-out frame settles on, so comparing it with
    /// [`Self::benchmark_target_block`] after a render proves the anchor jump
    /// reached the block it addressed rather than merely updating the label.
    #[doc(hidden)]
    pub fn benchmark_active_block(&self) -> usize {
        self.page_scroll
            .get(&self.location.section)
            .map_or(0, SettingsPageScroll::active)
    }

    /// Block index the current destination *should* occupy, taken from the
    /// descriptor the tree and breadcrumb index against.
    #[doc(hidden)]
    pub fn benchmark_target_block(&self) -> usize {
        let Some(subsection) = self.location.subsection else {
            return 0;
        };
        section_descriptor(self.location.section)
            .and_then(|section| {
                section
                    .subsections
                    .iter()
                    .position(|descriptor| descriptor.id == subsection)
            })
            .unwrap_or(usize::MAX)
    }

    /// Whether an anchor jump is still waiting for a laid-out frame.
    #[doc(hidden)]
    pub fn benchmark_anchor_pending(&self) -> bool {
        self.pending_anchor.is_some()
    }

    #[doc(hidden)]
    pub fn benchmark_disclosure_expanded(&self, id: &str) -> bool {
        self.page_ui
            .get(&self.location.section)
            .and_then(|ui| ui.disclosures.get(id))
            .is_some_and(SettingsDisclosureState::is_expanded)
    }
}
