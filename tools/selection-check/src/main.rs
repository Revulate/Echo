#![allow(dead_code, unused_variables)]

use std::sync::Arc;

extern crate self as gpui;

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SharedString(Arc<str>);

impl From<&str> for SharedString {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for SharedString {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

impl AsRef<str> for SharedString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

mod utils {
    use std::sync::{Mutex, OnceLock};

    static CLIPBOARD: OnceLock<Mutex<String>> = OnceLock::new();

    pub fn copy_text(text: &str) {
        *CLIPBOARD
            .get_or_init(|| Mutex::new(String::new()))
            .lock()
            .expect("clipboard lock") = text.to_string();
    }

    pub fn copied_text() -> String {
        CLIPBOARD
            .get_or_init(|| Mutex::new(String::new()))
            .lock()
            .expect("clipboard lock")
            .clone()
    }
}

mod ports {
    /// Stands in for `echo_chat_render::ports::copy_text`, whose real
    /// implementation is installed by the application.
    pub fn copy_text(text: &str) {
        crate::utils::copy_text(text);
    }
}

mod components;

use components::chat::canvas_renderer::eviction_cache::EvictionTrackingCache;
use components::chat::canvas_renderer::layout::{CanvasMessageLayout, LayoutCacheKey};
use components::chat::canvas_renderer::selection::{SelectionPoint, SelectionState};

fn main() {
    println!("kind,name,value,checksum");

    let container_width = 320.0;
    let mut layouts = EvictionTrackingCache::new(std::num::NonZeroUsize::new(8).expect("non-zero"));
    let msg1 = SharedString::from("m1");
    let msg2 = SharedString::from("m2");
    let msg3 = SharedString::from("m3");

    layouts.put(
        LayoutCacheKey::new(msg1.as_ref(), container_width, true),
        layout_with_text("A😊B\n中"),
    );
    layouts.put(
        LayoutCacheKey::new(msg2.as_ref(), container_width, true),
        layout_with_text("γειά σου"),
    );
    layouts.put(
        LayoutCacheKey::new(msg3.as_ref(), container_width, true),
        layout_with_text("tail"),
    );

    let order = vec![msg1.clone(), msg2.clone(), msg3.clone()];

    let mut selection = SelectionState::new();
    print_fixture(
        "selection_initial_has_selection",
        selection.has_selection().to_string(),
        "false",
    );
    selection.start_selection(point(&msg1, 0, 1));
    selection.extend_selection(point(&msg2, 1, 2));
    selection.end_drag();

    print_fixture(
        "selection_drag_ended",
        selection.is_dragging.to_string(),
        "false",
    );
    print_fixture(
        "selection_utf8_multimessage_text",
        selection.get_selected_text(&layouts, &order, container_width, true),
        "😊B\n中\nγε",
    );
    print_fixture(
        "selection_first_message_range",
        range_summary(&selection.get_ranges_for_message(0, "A😊B\n中".chars().count())),
        "1..5",
    );
    print_fixture(
        "selection_last_message_range",
        range_summary(&selection.get_ranges_for_message(1, "γειά σου".chars().count())),
        "0..2",
    );
    print_fixture(
        "selection_contains_boundary",
        format!(
            "{}|{}|{}",
            selection.contains_point(0, 1),
            selection.contains_point(1, 2),
            selection.contains_point(2, 0)
        ),
        "true|true|false",
    );

    selection.copy_to_clipboard(&layouts, &order, container_width, true);
    print_fixture(
        "selection_copy_to_clipboard",
        utils::copied_text(),
        "😊B\n中\nγε",
    );

    let mut reverse = SelectionState::new();
    reverse.start_selection(point(&msg2, 1, 99));
    reverse.extend_selection(point(&msg1, 0, 2));
    reverse.end_drag();
    print_fixture(
        "selection_reverse_drag_text",
        reverse.get_selected_text(&layouts, &order, container_width, true),
        "B\n中\nγειά σου",
    );
    print_fixture(
        "selection_reverse_normalized",
        normalized_summary(&reverse),
        "0:2->1:99",
    );

    let mut single = SelectionState::new();
    single.start_selection(point(&msg3, 2, 1));
    single.extend_selection(point(&msg3, 2, 3));
    single.end_drag();
    print_fixture(
        "selection_single_message_text",
        single.get_selected_text(&layouts, &order, container_width, true),
        "ai",
    );
    print_fixture(
        "selection_single_message_range",
        range_summary(&single.get_ranges_for_message(2, "tail".chars().count())),
        "1..3",
    );

    single.clear();
    print_fixture(
        "selection_clear_has_selection",
        single.has_selection().to_string(),
        "false",
    );

    let mut click_only = SelectionState::new();
    click_only.start_selection(point(&msg1, 0, 2));
    click_only.end_drag();
    print_fixture(
        "selection_click_only_does_not_claim_clipboard",
        click_only
            .copy_to_clipboard(&layouts, &order, container_width, true)
            .to_string(),
        "false",
    );

    let system1 = SharedString::from("system-1");
    let system2 = SharedString::from("system-2");
    let system1_text = "A moderation event happened\nwith a second detail line";
    let system2_text = "Another system row";
    layouts.put(
        LayoutCacheKey::new(system1.as_ref(), container_width, true),
        layout_with_text(system1_text),
    );
    layouts.put(
        LayoutCacheKey::new(system2.as_ref(), container_width, true),
        layout_with_text(system2_text),
    );
    let system_order = vec![system1.clone(), system2.clone()];
    let mut system_selection = SelectionState::new();
    system_selection.start_selection(point(&system1, 0, 0));
    system_selection.extend_selection(point(&system2, 1, system2_text.chars().count()));
    system_selection.end_drag();
    print_fixture(
        "selection_multiline_multirow_system_text",
        system_selection.get_selected_text(&layouts, &system_order, container_width, true),
        "A moderation event happened\nwith a second detail line\nAnother system row",
    );
    system_selection.copy_to_clipboard(&layouts, &system_order, container_width, true);
    print_fixture(
        "selection_multiline_multirow_system_clipboard",
        utils::copied_text(),
        "A moderation event happened\nwith a second detail line\nAnother system row",
    );
}

fn layout_with_text(text: &str) -> Arc<CanvasMessageLayout> {
    Arc::new(CanvasMessageLayout {
        source_text: text.to_string(),
        source_char_positions: (0..=text.chars().count()).map(|i| i as f32).collect(),
        segments: Vec::new(),
    })
}

fn point(message_id: &SharedString, message_index: usize, char_index: usize) -> SelectionPoint {
    SelectionPoint {
        message_id: message_id.clone(),
        message_index,
        char_index,
    }
}

fn range_summary(ranges: &[std::ops::Range<usize>]) -> String {
    ranges
        .iter()
        .map(|range| format!("{}..{}", range.start, range.end))
        .collect::<Vec<_>>()
        .join("|")
}

fn normalized_summary(selection: &SelectionState) -> String {
    let (start, end) = selection
        .get_normalized_range()
        .expect("normalized selection");
    format!(
        "{}:{}->{}:{}",
        start.message_index, start.char_index, end.message_index, end.char_index
    )
}

fn print_fixture(name: &str, actual: impl AsRef<str>, expected: &str) {
    let actual = actual.as_ref();
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
