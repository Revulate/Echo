//! Contract: tab and split-layout persistence. `TabDescriptor` round trips keep the
//! selected tab, active split path, title, lock, visibility and highlight flags,
//! and echo-core's split operations (insert, ratio adjust, swap, close) keep the
//! tree valid.

use check_support::{FIXTURE_HEADER, assert_fixture};
pub(crate) use check_support::{category_log as log_persist, category_log as log_ui};

mod state;

#[path = "../../../src/utils/atomic_file.rs"]
#[allow(dead_code)] // `config.rs` needs these to compile; this check writes no files
mod atomic_file;

mod utils {
    pub(crate) use crate::atomic_file::{
        backup_generation_paths, json_error_summary, preserve_unloadable_file,
        quarantine_corrupt_file, replace_file, rotate_known_good_backups,
    };
}

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
}

mod bench {
    pub mod ui_fixture {
        pub fn should_suppress_persistence() -> bool {
            false
        }
    }
}

mod storage_roots {
    use std::path::PathBuf;

    pub fn config_root() -> Option<PathBuf> {
        dirs::config_dir()
    }
}

// This check drives only the `TabDescriptor` conversions in `config.rs`, not its
// layout constants or tabs-file I/O.
#[path = "../../../src/config.rs"]
#[allow(dead_code)]
mod config;

use config::TabDescriptor;
use state::workspace_state::{AddSplitDirection, is_placeholder};
use state::{SplitNode, SplitOrientation, TabState, TabVisibility};

fn main() {
    println!("{FIXTURE_HEADER}");

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
    assert_fixture(
        "layout_selected_tab_persisted",
        descriptor.selected.to_string(),
        "true",
    );
    assert_fixture(
        "layout_active_split_persisted",
        path_summary(&descriptor.active_split_path),
        "1-1",
    );
    assert_fixture(
        "layout_active_channel_persisted",
        descriptor
            .layout
            .channel_at_path(&descriptor.active_split_path)
            .unwrap_or_default(),
        "charlie",
    );
    assert_fixture(
        "layout_visibility_persisted",
        format!("{:?}", descriptor.visibility),
        "OnlyLive",
    );
    assert_fixture(
        "layout_mark_unread_persisted",
        descriptor.mark_unread_on_new.to_string(),
        "true",
    );

    let restored = TabState::from(descriptor.clone());
    assert_fixture(
        "layout_active_split_restored",
        path_summary(&restored.active_split_path),
        "1-1",
    );
    assert_fixture(
        "layout_restored_title",
        restored.display_title(),
        "Operations",
    );

    let mut invalid_descriptor = descriptor;
    invalid_descriptor.active_split_path = vec![7, 7];
    let restored_invalid = TabState::from(invalid_descriptor);
    assert_fixture(
        "layout_invalid_split_fallback",
        path_summary(&restored_invalid.valid_active_split_path()),
        "0",
    );

    let json = serde_json::to_string(&vec![TabDescriptor::from(&tab)]).expect("serialize tabs");
    let decoded: Vec<TabDescriptor> = serde_json::from_str(&json).expect("deserialize tabs");
    let decoded_first = decoded.first().expect("decoded tab");
    assert_fixture(
        "layout_json_selected_roundtrip",
        decoded_first.selected.to_string(),
        "true",
    );
    assert_fixture(
        "layout_json_active_split_roundtrip",
        path_summary(&decoded_first.active_split_path),
        "1-1",
    );

    run_split_operation_fixtures();
}

fn run_split_operation_fixtures() {
    let mut inserted = SplitNode::leaf("alpha".to_string());
    assert!(inserted.insert_relative(&[], AddSplitDirection::Right));
    assert_fixture(
        "split_insert_right_orientation",
        split_orientation_summary(&inserted),
        "Vertical",
    );
    assert_fixture(
        "split_insert_right_channels",
        channel_summary(&inserted),
        "alpha|<empty>",
    );
    assert!(inserted.replace_channel_at_path(&[1], "bravo".to_string()));
    assert_fixture(
        "split_replace_placeholder_channel",
        channel_summary(&inserted),
        "alpha|bravo",
    );
    assert!(inserted.adjust_ratio(&[], 0, 0.2, 1.0, &[0.0, 0.0]) != 0.0);
    assert_fixture("split_adjust_ratio", ratio_summary(&inserted), "0.70|0.30");

    let mut tab = TabState::with_layout(SplitNode::split(
        SplitOrientation::Horizontal,
        vec![
            SplitNode::leaf("alpha".to_string()),
            SplitNode::leaf("bravo".to_string()),
        ],
        vec![0.5, 0.5],
    ));
    assert!(tab.swap_splits(&[0], &[1]));
    assert_fixture(
        "split_swap_channels",
        channel_summary(&tab.layout),
        "bravo|alpha",
    );
    tab.close_pane_by_path(&[0]);
    assert_fixture(
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
