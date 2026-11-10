//! GPUI-free per-split overlay state.
//!
//! Each split owns its emote picker and autocomplete state so typing or
//! opening a picker in one split never affects another.

/// State for a per-split overlay (emote picker, autocomplete, etc.)
#[derive(Clone, Debug, Default)]
pub struct SplitOverlayState {
    /// Whether the overlay is open
    pub is_open: bool,
}

impl SplitOverlayState {
    pub fn close(&mut self) {
        self.is_open = false;
    }
}

/// Per-split emote picker state
#[derive(Clone, Debug, Default)]
pub struct SplitEmotePickerState {
    pub overlay: SplitOverlayState,
    /// Current search query
    pub search_query: String,
    /// Pending emote to insert (set when user selects)
    pub pending_emote: Option<String>,
}

impl SplitEmotePickerState {
    pub fn close(&mut self) {
        self.overlay.close();
        self.search_query.clear();
        self.pending_emote = None;
    }
}

/// Per-split autocomplete state
#[derive(Clone, Debug, Default)]
pub struct SplitAutocompleteState {
    pub overlay: SplitOverlayState,
    /// Current trigger (@ for users, : for emotes)
    pub trigger: Option<char>,
    /// Current query text
    pub query: String,
    /// Selected suggestion index
    pub selected_index: usize,
}

impl SplitAutocompleteState {
    pub fn close(&mut self) {
        self.overlay.close();
        self.trigger = None;
        self.query.clear();
        self.selected_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emote_picker_state() {
        let mut state = SplitEmotePickerState::default();
        assert!(!state.overlay.is_open);

        state = SplitEmotePickerState {
            overlay: SplitOverlayState { is_open: true },
            ..SplitEmotePickerState::default()
        };
        assert!(state.overlay.is_open);

        state.close();
        assert!(!state.overlay.is_open);
    }

    #[test]
    fn test_autocomplete_state() {
        let mut state = SplitAutocompleteState::default();
        assert!(!state.overlay.is_open);

        state = SplitAutocompleteState {
            overlay: SplitOverlayState { is_open: true },
            trigger: Some('@'),
            ..SplitAutocompleteState::default()
        };
        assert!(state.overlay.is_open);
        assert_eq!(state.trigger, Some('@'));

        state.close();
        assert!(!state.overlay.is_open);
        assert_eq!(state.trigger, None);
    }
}
