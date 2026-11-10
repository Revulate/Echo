#![allow(dead_code, unused_variables)]

#[macro_export]
macro_rules! log_ui {
    ($($arg:tt)*) => {{}};
}

#[macro_export]
macro_rules! log_persist {
    ($($arg:tt)*) => {{}};
}

mod state;

#[path = "../../../src/utils/atomic_file.rs"]
mod atomic_file;

mod utils {
    pub(crate) use crate::atomic_file::{
        backup_generation_paths, json_error_summary, preserve_unloadable_file,
        quarantine_corrupt_file, replace_file, rotate_known_good_backups,
    };
}

// Stub for src/state/tab_state.rs's crate::backend::browser_watching references;
// mirrors the real semantics minus the runtime current-channel lookup.
mod backend {
    pub mod reliability {
        pub(crate) fn note_storage_recovery(
            _what: &'static str,
            _primary: &std::path::Path,
            _recovered_from: Option<&std::path::Path>,
            _preserved: &[std::path::PathBuf],
            _unpreserved: &[std::path::PathBuf],
        ) {
        }
    }

    pub mod whispers {
        pub fn is_whispers_channel(channel: &str) -> bool {
            channel
                .trim()
                .trim_start_matches('#')
                .eq_ignore_ascii_case("whispers")
        }
    }

    pub mod browser_watching {
        pub const WATCHING_CHANNEL: &str = "__echo_watching__";

        pub fn is_watching_channel(channel: &str) -> bool {
            channel == WATCHING_CHANNEL
        }

        pub fn current_channel() -> Option<String> {
            None
        }

        pub fn resolve_layout_channel(channel: &str) -> Option<String> {
            if is_watching_channel(channel) {
                None
            } else {
                Some(channel.to_string())
            }
        }
    }
}

mod benchmark_profile {
    use std::path::PathBuf;

    pub fn config_root() -> Option<PathBuf> {
        dirs::config_dir()
    }
}

#[path = "../../../src/config.rs"]
mod config;

use config::TabDescriptor;
use state::workspace_state::{AddSplitDirection, is_placeholder};
use state::{SplitNode, SplitOrientation, TabState, TabVisibility};

fn main() {
    println!("kind,name,value,checksum");

    let layout = SplitNode::split(
        SplitOrientation::Horizontal,
        vec![
            SplitNode::leaf("alpha".to_string()),
            SplitNode::split(
                SplitOrientation::Vertical,
                vec![
                    SplitNode::leaf("bravo".to_string()),
                    SplitNode::leaf("charlie".to_string()),
                ],
                vec![0.4, 0.6],
            ),
        ],
        vec![0.45, 0.55],
    );

    let mut tab = TabState::with_layout(layout);
    tab.id = "tab-layout-smoke".to_string();
    tab.custom_title = Some("Operations".to_string());
    tab.is_selected = true;
    tab.highlights_enabled = false;
    tab.is_locked = true;
    tab.visibility = TabVisibility::OnlyLive;
    tab.mark_unread_on_new = true;

    assert!(tab.set_active_split_path(vec![1, 1]));
    assert!(!tab.set_active_split_path(vec![9]));

    let descriptor = TabDescriptor::from(&tab);
    print_fixture(
        "layout_selected_tab_persisted",
        descriptor.selected.to_string(),
        "true",
    );
    print_fixture(
        "layout_active_split_persisted",
        path_summary(&descriptor.active_split_path),
        "1-1",
    );
    print_fixture(
        "layout_active_channel_persisted",
        descriptor
            .layout
            .get_channel_at_path(&descriptor.active_split_path)
            .unwrap_or_default(),
        "charlie",
    );
    print_fixture(
        "layout_visibility_persisted",
        format!("{:?}", descriptor.visibility),
        "OnlyLive",
    );
    print_fixture(
        "layout_mark_unread_persisted",
        descriptor.mark_unread_on_new.to_string(),
        "true",
    );

    let restored = TabState::from(descriptor.clone());
    print_fixture(
        "layout_active_split_restored",
        path_summary(&restored.active_split_path),
        "1-1",
    );
    print_fixture(
        "layout_restored_title",
        restored.display_title(),
        "Operations",
    );

    let mut invalid_descriptor = descriptor;
    invalid_descriptor.active_split_path = vec![7, 7];
    let restored_invalid = TabState::from(invalid_descriptor);
    print_fixture(
        "layout_invalid_split_fallback",
        path_summary(&restored_invalid.valid_active_split_path()),
        "0",
    );

    let json = serde_json::to_string(&vec![TabDescriptor::from(&tab)]).expect("serialize tabs");
    let decoded: Vec<TabDescriptor> = serde_json::from_str(&json).expect("deserialize tabs");
    let decoded_first = decoded.first().expect("decoded tab");
    print_fixture(
        "layout_json_selected_roundtrip",
        decoded_first.selected.to_string(),
        "true",
    );
    print_fixture(
        "layout_json_active_split_roundtrip",
        path_summary(&decoded_first.active_split_path),
        "1-1",
    );

    run_split_operation_fixtures();
}

fn run_split_operation_fixtures() {
    let mut inserted = SplitNode::leaf("alpha".to_string());
    assert!(inserted.insert_relative(&[], AddSplitDirection::Right));
    print_fixture(
        "split_insert_right_orientation",
        split_orientation_summary(&inserted),
        "Vertical",
    );
    print_fixture(
        "split_insert_right_channels",
        channel_summary(&inserted),
        "alpha|<empty>",
    );
    assert!(inserted.replace_channel_at_path(&[1], "bravo".to_string()));
    print_fixture(
        "split_replace_placeholder_channel",
        channel_summary(&inserted),
        "alpha|bravo",
    );
    assert!(inserted.adjust_ratio(&[], 0, 0.2));
    print_fixture("split_adjust_ratio", ratio_summary(&inserted), "0.70|0.30");

    let mut tab = TabState::with_layout(SplitNode::split(
        SplitOrientation::Horizontal,
        vec![
            SplitNode::leaf("alpha".to_string()),
            SplitNode::leaf("bravo".to_string()),
        ],
        vec![0.5, 0.5],
    ));
    assert!(tab.swap_splits(&[0], &[1]));
    print_fixture(
        "split_swap_channels",
        channel_summary(&tab.layout),
        "bravo|alpha",
    );
    tab.close_pane_by_path(&[0]);
    print_fixture(
        "split_close_collapses_single_child",
        channel_summary(&tab.layout),
        "alpha",
    );
}

fn path_summary(path: &[usize]) -> String {
    if path.is_empty() {
        return "<root>".to_string();
    }

    path.iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join("-")
}

fn split_orientation_summary(node: &SplitNode) -> &'static str {
    match node {
        SplitNode::Leaf { .. } => "Leaf",
        SplitNode::Split { orientation, .. } => match orientation {
            SplitOrientation::Vertical => "Vertical",
            SplitOrientation::Horizontal => "Horizontal",
        },
    }
}

fn channel_summary(node: &SplitNode) -> String {
    node.channels()
        .into_iter()
        .map(|channel| {
            if is_placeholder(&channel) {
                "<empty>".to_string()
            } else {
                channel
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn ratio_summary(node: &SplitNode) -> String {
    let SplitNode::Split { ratios, .. } = node else {
        return "<leaf>".to_string();
    };

    ratios
        .iter()
        .map(|ratio| format!("{ratio:.2}"))
        .collect::<Vec<_>>()
        .join("|")
}

fn print_fixture(name: &str, actual: impl AsRef<str>, expected: &str) {
    let actual = actual.as_ref();
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
