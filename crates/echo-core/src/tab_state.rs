//! Tab state for the tab system
//!
//! Each tab contains its own complete split layout, enabling configurations like:
//! - Tab 1 "Watching": xQc + forsen side-by-side
//! - Tab 2 "Me": user's own channel
//! - Tab 3 "S": different split configuration

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use uuid::Uuid;

use crate::SplitId;
use crate::workspace::{
    AddSplitDirection, SplitNode, effective_ratios, is_placeholder, new_placeholder,
};

#[derive(Clone, Copy)]
struct LeafBounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl LeafBounds {
    fn width(self) -> f32 {
        self.right - self.left
    }

    fn height(self) -> f32 {
        self.bottom - self.top
    }

    fn center_x(self) -> f32 {
        self.left + self.width() * 0.5
    }

    fn center_y(self) -> f32 {
        self.top + self.height() * 0.5
    }

    fn vertical_overlap(self, other: Self) -> f32 {
        (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0.0)
    }

    fn horizontal_overlap(self, other: Self) -> f32 {
        (self.right.min(other.right) - self.left.max(other.left)).max(0.0)
    }

    fn from_rect([left, top, right, bottom]: [f32; 4]) -> Option<Self> {
        let bounds = Self {
            left,
            top,
            right,
            bottom,
        };
        (left.is_finite()
            && top.is_finite()
            && right.is_finite()
            && bottom.is_finite()
            && bounds.width().is_finite()
            && bounds.height().is_finite()
            && bounds.width() > 0.0
            && bounds.height() > 0.0)
            .then_some(bounds)
    }
}

/// Unique identifier for a tab
pub type TabId = String;

/// Per-tab highlight state.
///
/// Driven by activity in the tab's channels while it is not the active tab and
/// cleared when the tab is visited. Variant order encodes severity (None <
/// NewMessage < Highlighted) so [`TabHighlightState::escalate`] never lowers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum TabHighlightState {
    /// No pending activity.
    #[default]
    None,
    /// A new message arrived (neutral "new message" indicator).
    NewMessage,
    /// A mention/highlight arrived (colored indicator, takes priority).
    Highlighted,
}

impl TabHighlightState {
    /// Raise to a stronger state without ever lowering it. Returns whether it changed.
    pub fn escalate(&mut self, to: TabHighlightState) -> bool {
        if to > *self {
            *self = to;
            true
        } else {
            false
        }
    }
}

/// Tab visibility filter modes
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TabVisibility {
    /// Show all tabs regardless of stream status
    #[default]
    ShowAll,
    /// Only show tabs where at least one channel is live
    OnlyLive,
    /// Hide the tab bar entirely
    Hidden,
}

/// A tab containing a complete split layout
///
/// Each tab owns its own independent split layout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabState {
    /// Unique identifier for this tab
    pub id: TabId,

    /// Custom title set by user (if None, derives from first channel or "New tab")
    pub custom_title: Option<String>,

    /// The split layout tree for this tab - each tab owns its own layout
    /// Wrapped in Arc for O(1) clone during render (avoids deep clone every frame)
    pub layout: Arc<SplitNode>,

    /// Whether this tab is currently selected/active
    #[serde(default)]
    pub is_selected: bool,

    /// Active split path within this tab.
    #[serde(default)]
    pub active_split_path: Vec<usize>,

    /// Whether to show highlight indicators (mentions, etc.) on this tab
    #[serde(default = "crate::default_true")]
    pub highlights_enabled: bool,

    /// Whether the layout is locked (prevents accidental split changes)
    #[serde(default)]
    pub is_locked: bool,

    /// Visibility mode for this tab in filtered views
    #[serde(default)]
    pub visibility: TabVisibility,

    /// Whether this tab's channel(s) are live
    #[serde(default)]
    pub is_live: bool,

    /// Whether this tab has unread messages (shown as indicator)
    #[serde(default, skip_serializing)]
    pub has_unread: bool,

    /// Highlight state shown while the tab is inactive.
    #[serde(default, skip_serializing)]
    pub highlight_state: TabHighlightState,

    /// Whether to mark as unread on new messages (user preference per tab)
    #[serde(default)]
    pub mark_unread_on_new: bool,
}

impl TabState {
    /// Create a new tab with a single placeholder pane
    pub fn new() -> Self {
        Self::with_layout(SplitNode::leaf(new_placeholder()))
    }

    /// Create a new tab with an existing layout (for migration or duplication)
    pub fn with_layout(layout: SplitNode) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            custom_title: None,
            layout: Arc::new(layout),
            is_selected: false,
            active_split_path: Vec::new(),
            highlights_enabled: true,
            is_locked: false,
            visibility: TabVisibility::default(),
            is_live: false,
            has_unread: false,
            highlight_state: TabHighlightState::None,
            mark_unread_on_new: false,
        }
    }

    /// Get the display title for this tab
    ///
    /// Returns custom_title if set, otherwise derives from channels:
    /// - Empty/placeholder only: "New tab"
    /// - Single channel: "channelname"
    /// - Multiple channels: "first +N"
    pub fn display_title(&self) -> String {
        if let Some(ref title) = self.custom_title {
            return title.clone();
        }

        // Preserve special-channel identity in the tab label. `channels()`
        // resolves Watching to its current Twitch login for lifecycle work,
        // but the user-facing tab should continue to say Watching.
        let channels = self
            .layout
            .channels()
            .into_iter()
            .filter(|channel| !is_placeholder(channel))
            .collect::<Vec<_>>();

        if channels.is_empty() {
            "New tab".to_string()
        } else if channels.len() == 1 {
            if crate::channel_identity::is_watching_channel(&channels[0]) {
                "Watching".to_string()
            } else if crate::channel_identity::is_whispers_channel(&channels[0]) {
                "Whispers".to_string()
            } else {
                channels[0].clone()
            }
        } else {
            let first = if crate::channel_identity::is_watching_channel(&channels[0]) {
                "Watching".to_string()
            } else if crate::channel_identity::is_whispers_channel(&channels[0]) {
                "Whispers".to_string()
            } else {
                channels[0].clone()
            };
            format!("{} +{}", first, channels.len() - 1)
        }
    }

    /// Get all unique concrete Twitch channels in this tab's layout.
    ///
    /// A Watching leaf resolves to the browser-selected channel while its
    /// persisted layout value remains the stable Watching sentinel.
    pub fn channels(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.layout
            .channels()
            .into_iter()
            .filter(|channel| !is_placeholder(channel))
            .filter_map(|channel| crate::channel_identity::resolve_layout_channel(&channel))
            .filter(|channel| seen.insert(channel.clone()))
            .collect()
    }

    /// Return every non-placeholder channel leaf in depth-first layout order.
    ///
    /// Unlike [`Self::channels`], this keeps duplicate leaves and their exact
    /// split paths. The paths let tab-level affordances focus the intended
    /// split without guessing from a channel name, while the labels preserve
    /// the user-facing identity of virtual Watching and Whispers leaves.
    pub fn channel_targets(&self) -> Vec<(Vec<usize>, String)> {
        fn walk(node: &SplitNode, path: &mut Vec<usize>, targets: &mut Vec<(Vec<usize>, String)>) {
            match node {
                SplitNode::Leaf { channel, .. } => {
                    if is_placeholder(channel) {
                        return;
                    }

                    let label = if crate::channel_identity::is_watching_channel(channel) {
                        "Watching".to_string()
                    } else if crate::channel_identity::is_whispers_channel(channel) {
                        "Whispers".to_string()
                    } else {
                        channel.clone()
                    };
                    targets.push((path.clone(), label));
                }
                SplitNode::Split { children, .. } => {
                    for (idx, child) in children.iter().enumerate() {
                        path.push(idx);
                        walk(child, path, targets);
                        path.pop();
                    }
                }
            }
        }

        let mut targets = Vec::new();
        walk(&self.layout, &mut Vec::new(), &mut targets);
        targets
    }

    /// Change key for [`Self::channels`] that avoids building the list.
    ///
    /// Hashes the same resolved leaves in layout order without the dedup set,
    /// the intermediate Vec, or the per-channel String clones, so a poller can
    /// tell "nothing to do" from "rebuild the list". Duplicate leaves are part
    /// of the key, so it can move while `channels()` output does not; it never
    /// stays equal across a real change. Watching resolves through the browser
    /// selection, which moves without a layout edit, so it is hashed resolved.
    pub fn channels_change_key(&self) -> u64 {
        fn hash_node(node: &SplitNode, hasher: &mut DefaultHasher) {
            match node {
                SplitNode::Leaf { channel, .. } => {
                    if is_placeholder(channel) {
                        return;
                    }
                    if crate::channel_identity::is_watching_channel(channel) {
                        if let Some(resolved) = crate::channel_identity::current_watching_channel()
                        {
                            resolved.hash(hasher);
                        }
                    } else {
                        channel.hash(hasher);
                    }
                }
                SplitNode::Split { children, .. } => {
                    for child in children {
                        hash_node(child, hasher);
                    }
                }
            }
        }

        let mut hasher = DefaultHasher::new();
        hash_node(&self.layout, &mut hasher);
        hasher.finish()
    }

    /// Get the channel for this tab's active split, falling back to the first real channel.
    /// Used for autocomplete and input context
    pub fn active_channel(&self) -> Option<String> {
        let active_path = self.valid_active_split_path();
        if let Some(channel) = self.layout.channel_ref_at_path(&active_path)
            && !is_placeholder(channel)
            && let Some(channel) = crate::channel_identity::resolve_layout_channel(channel)
        {
            return Some(channel);
        }

        fn find_first_channel(node: &crate::workspace::SplitNode) -> Option<String> {
            match node {
                crate::workspace::SplitNode::Leaf { channel, .. } => {
                    if is_placeholder(channel) {
                        None
                    } else {
                        crate::channel_identity::resolve_layout_channel(channel)
                    }
                }
                crate::workspace::SplitNode::Split { children, .. } => {
                    children.iter().find_map(find_first_channel)
                }
            }
        }

        find_first_channel(&self.layout)
    }

    /// Duplicate this tab with a new ID
    ///
    /// The duplicate will not be selected and will have "(copy)" appended to title
    pub fn duplicate(&self) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            custom_title: self.custom_title.clone().map(|t| format!("{} (copy)", t)),
            layout: Arc::new(self.layout.with_fresh_split_ids()),
            is_selected: false,
            active_split_path: self.valid_active_split_path(),
            highlights_enabled: self.highlights_enabled,
            is_locked: false, // Don't copy lock status
            visibility: self.visibility.clone(),
            is_live: self.is_live,
            has_unread: false,
            highlight_state: TabHighlightState::None,
            mark_unread_on_new: self.mark_unread_on_new,
        }
    }

    /// Set a custom title for this tab
    pub fn set_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        if title.trim().is_empty() {
            self.custom_title = None;
        } else {
            self.custom_title = Some(title);
        }
    }

    /// Return the persisted active split path if it still resolves, otherwise the first leaf.
    pub fn valid_active_split_path(&self) -> Vec<usize> {
        if self
            .layout
            .channel_at_path(&self.active_split_path)
            .is_some()
        {
            return self.active_split_path.clone();
        }

        self.layout.first_leaf_path().unwrap_or_default()
    }

    /// Persist a new active split path when it resolves in this tab.
    pub fn set_active_split_path(&mut self, path: Vec<usize>) -> bool {
        if self.layout.channel_at_path(&path).is_none() {
            return false;
        }
        if self.active_split_path == path {
            return false;
        }
        self.active_split_path = path;
        true
    }

    pub fn insert_placeholder_split_adjacent_to_active(
        &mut self,
        direction: crate::workspace::AddSplitDirection,
    ) -> Option<Vec<usize>> {
        let target_path = self.valid_active_split_path();
        self.insert_placeholder_split_adjacent_to_path(&target_path, direction)
    }

    pub fn insert_placeholder_split_adjacent_to_path(
        &mut self,
        target_path: &[usize],
        direction: crate::workspace::AddSplitDirection,
    ) -> Option<Vec<usize>> {
        let placeholder_child_idx = match direction {
            crate::workspace::AddSplitDirection::Top
            | crate::workspace::AddSplitDirection::Left => 0,
            crate::workspace::AddSplitDirection::Bottom
            | crate::workspace::AddSplitDirection::Right => 1,
        };

        if !self.insert_split_relative(target_path, direction) {
            return None;
        }

        let mut placeholder_path = target_path.to_vec();
        placeholder_path.push(placeholder_child_idx);
        self.active_split_path = placeholder_path.clone();

        // A perpendicular split can complete an exact 2x2 block. Rotate it now,
        // while both band dividers still agree, so each visual row owns its own
        // divider instead of sharing one that spans the whole block.
        let placeholder_id = self.layout.split_id_at_path(&placeholder_path);
        if self.normalize_completed_grid_block(target_path)
            && let Some(rotated) = placeholder_id.and_then(|id| self.split_path_for_id(id))
        {
            placeholder_path = rotated;
            self.active_split_path = placeholder_path.clone();
        }

        Some(placeholder_path)
    }

    pub fn split_path_for_id(&self, target: SplitId) -> Option<Vec<usize>> {
        Self::find_path_by_split_id(&self.layout, target)
    }

    pub fn reopen_anchor_for_path(
        &self,
        path: &[usize],
    ) -> Option<(SplitId, crate::workspace::AddSplitDirection)> {
        let (&child_idx, parent_path) = path.split_last()?;
        let parent = self.layout.node_at_path(parent_path)?;

        let SplitNode::Split { children, .. } = parent else {
            return None;
        };

        if child_idx > 0 {
            let anchor_split_id = children.get(child_idx - 1)?.rightmost_leaf_split_id()?;
            return Some((anchor_split_id, crate::workspace::AddSplitDirection::Right));
        }

        let anchor_split_id = children.get(child_idx + 1)?.leftmost_leaf_split_id()?;
        Some((anchor_split_id, crate::workspace::AddSplitDirection::Left))
    }

    pub fn adjacent_split_path(&self, direction: AddSplitDirection) -> Option<Vec<usize>> {
        fn walk(
            node: &SplitNode,
            bounds: LeafBounds,
            path: &mut Vec<usize>,
            leaves: &mut Vec<(Vec<usize>, LeafBounds)>,
        ) {
            match node {
                SplitNode::Leaf { .. } => leaves.push((path.clone(), bounds)),
                SplitNode::Split {
                    orientation,
                    children,
                    ratios,
                    ..
                } => {
                    let ratios = effective_ratios(ratios, children.len());
                    let mut cursor_x = bounds.left;
                    let mut cursor_y = bounds.top;

                    for (idx, (child, ratio)) in children.iter().zip(ratios).enumerate() {
                        let child_bounds = match orientation {
                            crate::workspace::SplitOrientation::Vertical => {
                                let width = bounds.width() * ratio;
                                let child_bounds = LeafBounds {
                                    left: cursor_x,
                                    top: bounds.top,
                                    right: cursor_x + width,
                                    bottom: bounds.bottom,
                                };
                                cursor_x += width;
                                child_bounds
                            }
                            crate::workspace::SplitOrientation::Horizontal => {
                                let height = bounds.height() * ratio;
                                let child_bounds = LeafBounds {
                                    left: bounds.left,
                                    top: cursor_y,
                                    right: bounds.right,
                                    bottom: cursor_y + height,
                                };
                                cursor_y += height;
                                child_bounds
                            }
                        };

                        path.push(idx);
                        walk(child, child_bounds, path, leaves);
                        path.pop();
                    }
                }
            }
        }

        let mut leaves = Vec::new();
        walk(
            &self.layout,
            LeafBounds {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
            &mut Vec::new(),
            &mut leaves,
        );

        self.adjacent_split_path_in_bounds(
            direction,
            leaves.into_iter().map(|(path, bounds)| {
                (path, [bounds.left, bounds.top, bounds.right, bounds.bottom])
            }),
        )
    }

    /// Finds the nearest leaf in `direction` from the active split using rendered bounds.
    ///
    /// Bounds are logical `[left, top, right, bottom]` coordinates. Invalid rectangles and
    /// paths that no longer identify a leaf in this layout are ignored.
    pub fn adjacent_split_path_in_bounds(
        &self,
        direction: AddSplitDirection,
        bounds: impl IntoIterator<Item = (Vec<usize>, [f32; 4])>,
    ) -> Option<Vec<usize>> {
        let active_path = self.valid_active_split_path();
        let leaves: Vec<_> = bounds
            .into_iter()
            .filter_map(|(path, rect)| {
                (self.layout.channel_ref_at_path(&path).is_some())
                    .then(|| LeafBounds::from_rect(rect))
                    .flatten()
                    .map(|bounds| (path, bounds))
            })
            .collect();

        let current_bounds = leaves
            .iter()
            .find_map(|(path, bounds)| (path == &active_path).then_some(*bounds))?;

        leaves
            .into_iter()
            .filter(|(path, _)| path != &active_path)
            .filter_map(|(path, bounds)| {
                let (primary_gap, perpendicular_distance, overlaps_axis) = match direction {
                    AddSplitDirection::Left => {
                        if bounds.right > current_bounds.left {
                            return None;
                        }
                        (
                            current_bounds.left - bounds.right,
                            (bounds.center_y() - current_bounds.center_y()).abs(),
                            bounds.vertical_overlap(current_bounds) > 0.0,
                        )
                    }
                    AddSplitDirection::Right => {
                        if bounds.left < current_bounds.right {
                            return None;
                        }
                        (
                            bounds.left - current_bounds.right,
                            (bounds.center_y() - current_bounds.center_y()).abs(),
                            bounds.vertical_overlap(current_bounds) > 0.0,
                        )
                    }
                    AddSplitDirection::Top => {
                        if bounds.bottom > current_bounds.top {
                            return None;
                        }
                        (
                            current_bounds.top - bounds.bottom,
                            (bounds.center_x() - current_bounds.center_x()).abs(),
                            bounds.horizontal_overlap(current_bounds) > 0.0,
                        )
                    }
                    AddSplitDirection::Bottom => {
                        if bounds.top < current_bounds.bottom {
                            return None;
                        }
                        (
                            bounds.top - current_bounds.bottom,
                            (bounds.center_x() - current_bounds.center_x()).abs(),
                            bounds.horizontal_overlap(current_bounds) > 0.0,
                        )
                    }
                };

                Some((path, primary_gap, perpendicular_distance, overlaps_axis))
            })
            .min_by(|a, b| {
                let overlap_order = b.3.cmp(&a.3);
                if overlap_order != Ordering::Equal {
                    return overlap_order;
                }

                a.1.partial_cmp(&b.1)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal))
                    .then_with(|| a.0.cmp(&b.0))
            })
            .map(|(path, _, _, _)| path)
    }

    // =========================================================================
    // Layout Manipulation (delegates to SplitNode methods)
    // =========================================================================

    /// Close pane at a specific path
    ///
    /// Closing the tab's only split resets it to an empty channel prompt rather
    /// than doing nothing.
    pub fn close_pane_by_path(&mut self, path: &[usize]) {
        if self.is_locked {
            return;
        }

        if path.is_empty() {
            if matches!(self.layout.as_ref(), SplitNode::Leaf { .. }) {
                self.layout = Arc::new(SplitNode::leaf(new_placeholder()));
                self.active_split_path = Vec::new();
            }
            return;
        }

        let active_path = self.valid_active_split_path();
        let active_split_id = self.layout.split_id_at_path(&active_path);
        let closed_split_id = self.layout.split_id_at_path(path);
        let replacement_split_id = (active_split_id == closed_split_id)
            .then(|| self.close_replacement_split_id(path))
            .flatten();

        // Clone inner SplitNode (not Arc) for remove operation
        if let Some(new_layout) = (*self.layout).clone().remove_at_path(path) {
            self.layout = Arc::new(new_layout);
            self.active_split_path = if active_split_id == closed_split_id {
                replacement_split_id.and_then(|id| self.split_path_for_id(id))
            } else {
                active_split_id.and_then(|id| self.split_path_for_id(id))
            }
            .unwrap_or_else(|| self.valid_active_split_path());
        }
    }

    /// Replace channel at path (for placeholder input)
    pub fn set_channel_at_path(&mut self, path: &[usize], channel: String) -> bool {
        let changed = Arc::make_mut(&mut self.layout).replace_channel_at_path(path, channel);
        if changed {
            self.active_split_path = path.to_vec();
        }
        changed
    }

    /// Persist the channel-header visibility for one leaf without changing its
    /// channel, split id, active-path ownership, or neighbouring panes.
    pub fn set_header_collapsed_at_path(&mut self, path: &[usize], collapsed: bool) -> bool {
        Arc::make_mut(&mut self.layout).set_header_collapsed_at_path(path, collapsed)
    }

    /// Persist a pane's deleted-message override (`None` follows the global
    /// setting) without touching its channel or neighbours.
    pub fn set_deleted_mode_at_path(
        &mut self,
        path: &[usize],
        mode: Option<crate::message::DeletedMessageMode>,
    ) -> bool {
        Arc::make_mut(&mut self.layout).set_deleted_mode_at_path(path, mode)
    }

    /// [`SplitNode::adjust_ratio`] on this tab's layout; a locked tab applies
    /// nothing and returns `0.0`.
    pub fn adjust_ratio(
        &mut self,
        path: &[usize],
        child_idx: usize,
        delta: f32,
        extent: f32,
        minimums: &[f32],
    ) -> f32 {
        if self.is_locked {
            return 0.0;
        }
        Arc::make_mut(&mut self.layout).adjust_ratio(path, child_idx, delta, extent, minimums)
    }

    /// Give every child of the split at `path` an equal share.
    pub fn reset_ratios(&mut self, path: &[usize]) -> bool {
        if self.is_locked {
            return false;
        }
        Arc::make_mut(&mut self.layout).reset_ratios(path)
    }

    /// Rotate the 2x2 block at `path` between column-major and row-major.
    ///
    /// Every leaf keeps its `SplitId`, so the active split is re-resolved by id
    /// rather than by its old path: rotation swaps the two path components
    /// inside the block.
    pub fn transpose_grid_block(&mut self, path: &[usize]) -> bool {
        if self.is_locked {
            return false;
        }

        let active_id = self.layout.split_id_at_path(&self.active_split_path);
        if !Arc::make_mut(&mut self.layout).transpose_2x2_at_path(path) {
            return false;
        }

        self.active_split_path = active_id
            .and_then(|id| self.split_path_for_id(id))
            .unwrap_or_else(|| self.valid_active_split_path());
        true
    }

    /// Rotate the block a just-inserted split completed, if it completed one.
    ///
    /// `inserted_path` is the path that was wrapped by the insertion, so the
    /// block that could have been completed is its parent. Only column-major
    /// blocks rotate here: a row-major block already gives each visual row its
    /// own divider, and re-rotating it would just move the shared divider back
    /// to the other axis.
    fn normalize_completed_grid_block(&mut self, inserted_path: &[usize]) -> bool {
        let Some((_, block_path)) = inserted_path.split_last() else {
            return false;
        };
        let block_path = block_path.to_vec();

        let rotatable = self
            .layout
            .node_at_path(&block_path)
            .is_some_and(|node| node.is_column_major_2x2() && node.can_transpose_2x2());
        if !rotatable {
            return false;
        }

        self.transpose_grid_block(&block_path)
    }

    /// Swap two splits (for center drop zone)
    pub fn swap_splits(&mut self, path_a: &[usize], path_b: &[usize]) -> bool {
        if self.is_locked {
            return false;
        }
        // Swap the whole leaves, not just their channel strings: split-local
        // state (draft text, reply target, chat view) is keyed by SplitId, so
        // moving only the name would leave that state on the old position.
        let node_a = match self.layout.node_at_path(path_a) {
            Some(node @ SplitNode::Leaf { .. }) => node.clone(),
            _ => return false,
        };
        let node_b = match self.layout.node_at_path(path_b) {
            Some(node @ SplitNode::Leaf { .. }) => node.clone(),
            _ => return false,
        };

        let layout = Arc::make_mut(&mut self.layout);
        let success_a = layout.set_node_at_path(path_a, node_b);
        let success_b = layout.set_node_at_path(path_b, node_a);

        if success_a && success_b {
            if self.active_split_path == path_a {
                self.active_split_path = path_b.to_vec();
            } else if self.active_split_path == path_b {
                self.active_split_path = path_a.to_vec();
            }
        }

        success_a && success_b
    }

    /// Insert a new split relative to a target path
    fn insert_split_relative(
        &mut self,
        path: &[usize],
        direction: crate::workspace::AddSplitDirection,
    ) -> bool {
        if self.is_locked {
            return false;
        }
        Arc::make_mut(&mut self.layout).insert_relative(path, direction)
    }

    /// Move a split from source to target with the given drop zone
    pub fn move_split(
        &mut self,
        source_path: &[usize],
        target_path: &[usize],
        zone: crate::workspace::DropZone,
    ) -> bool {
        use crate::workspace::{AddSplitDirection, DropZone};

        if self.is_locked {
            return false;
        }

        let active_split_id = self
            .layout
            .split_id_at_path(&self.valid_active_split_path());

        // For center zone, swap instead
        if zone == DropZone::Center {
            return self.swap_splits(source_path, target_path);
        }

        // Resolve source/target using stable split IDs so path shifts during
        // mutation do not cause us to move/overwrite the wrong pane.
        let source_split_id = match self.layout.split_id_at_path(source_path) {
            Some(id) => id,
            None => return false,
        };
        let target_split_id = match self.layout.split_id_at_path(target_path) {
            Some(id) => id,
            None => return false,
        };
        if source_split_id == target_split_id {
            return false;
        }

        // Capture the source node before removal so its stable SplitId and
        // split-local state keys survive drag/drop moves.
        let Some(source_node) = self.layout.node_at_path(source_path).cloned() else {
            return false;
        };
        if !matches!(source_node, SplitNode::Leaf { .. }) {
            return false;
        };

        // First, insert a new split at the target position
        let direction = match zone {
            DropZone::Top => AddSplitDirection::Top,
            DropZone::Bottom => AddSplitDirection::Bottom,
            DropZone::Left => AddSplitDirection::Left,
            DropZone::Right => AddSplitDirection::Right,
            DropZone::Center => unreachable!(),
        };

        // Remove source and collapse it first so target path can be resolved
        // against the post-removal tree.
        let mut compacted = match self.layout.as_ref().clone().remove_at_path(source_path) {
            Some(new_layout) => new_layout,
            None => return false,
        };

        // Resolve target path again after source removal.
        let resolved_target_path = match Self::find_path_by_split_id(&compacted, target_split_id) {
            Some(path) => path,
            None => return false,
        };

        if !compacted.insert_node_relative(&resolved_target_path, direction, source_node) {
            return false;
        }

        self.layout = Arc::new(compacted);
        self.active_split_path = active_split_id
            .and_then(|id| self.split_path_for_id(id))
            .unwrap_or_else(|| self.valid_active_split_path());
        // A drop can complete a 2x2 block just as an add-split can.
        self.normalize_completed_grid_block(&resolved_target_path);
        true
    }

    /// Move a split to an outer edge of the whole layout, spanning it fully.
    pub fn move_split_to_edge(
        &mut self,
        source_path: &[usize],
        zone: crate::workspace::DropZone,
    ) -> bool {
        use crate::workspace::{AddSplitDirection, DropZone};

        if self.is_locked {
            return false;
        }
        let direction = match zone {
            DropZone::Top => AddSplitDirection::Top,
            DropZone::Bottom => AddSplitDirection::Bottom,
            DropZone::Left => AddSplitDirection::Left,
            DropZone::Right => AddSplitDirection::Right,
            DropZone::Center => return false,
        };
        let Some(source_node) = self.layout.node_at_path(source_path).cloned() else {
            return false;
        };
        if !matches!(source_node, SplitNode::Leaf { .. }) {
            return false;
        }
        let active_split_id = self
            .layout
            .split_id_at_path(&self.valid_active_split_path());
        let Some(mut compacted) = self.layout.as_ref().clone().remove_at_path(source_path) else {
            return false;
        };
        compacted.insert_node_at_edge(direction, source_node);

        self.layout = Arc::new(compacted);
        self.active_split_path = active_split_id
            .and_then(|id| self.split_path_for_id(id))
            .unwrap_or_else(|| self.valid_active_split_path());
        true
    }

    fn close_replacement_split_id(&self, path: &[usize]) -> Option<SplitId> {
        let (&child_idx, parent_path) = path.split_last()?;
        let SplitNode::Split { children, .. } = self.layout.node_at_path(parent_path)? else {
            return None;
        };
        if child_idx >= children.len() || children.len() < 2 {
            return None;
        }

        let replacement_idx = if child_idx + 1 < children.len() {
            child_idx + 1
        } else {
            child_idx - 1
        };
        children[replacement_idx].leftmost_leaf_split_id()
    }

    fn find_path_by_split_id(root: &SplitNode, target: SplitId) -> Option<Vec<usize>> {
        fn walk(node: &SplitNode, target: SplitId, path: &mut Vec<usize>) -> bool {
            if node.split_id() == Some(target) {
                return true;
            }

            match node {
                SplitNode::Leaf { .. } => false,
                SplitNode::Split { children, .. } => {
                    for (idx, child) in children.iter().enumerate() {
                        path.push(idx);
                        if walk(child, target, path) {
                            return true;
                        }
                        path.pop();
                    }
                    false
                }
            }
        }

        let mut path = Vec::new();
        if walk(root, target, &mut path) {
            Some(path)
        } else {
            None
        }
    }
}

impl Default for TabState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{DropZone, SplitOrientation};

    #[test]
    fn test_new_tab() {
        let tab = TabState::new();
        assert!(tab.channels().is_empty());
        assert_eq!(tab.display_title(), "New tab");
        assert!(!tab.is_selected);
        assert!(tab.highlights_enabled);
    }

    #[test]
    fn test_tab_with_channel() {
        let tab = TabState::with_layout(SplitNode::leaf("xqc".to_string()));
        assert_eq!(tab.channels(), vec!["xqc"]);
        assert_eq!(tab.display_title(), "xqc");
    }

    #[test]
    fn test_tab_with_custom_title() {
        let mut tab = TabState::with_layout(SplitNode::leaf("xqc".to_string()));
        tab.set_title("Watching");
        assert_eq!(tab.display_title(), "Watching");
    }

    #[test]
    fn test_tab_multiple_channels() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("xqc".into()),
                SplitNode::leaf("forsen".into()),
            ],
            vec![0.5, 0.5],
        );
        let tab = TabState::with_layout(layout);
        assert_eq!(tab.channels().len(), 2);
        assert_eq!(tab.display_title(), "xqc +1");
    }

    #[test]
    fn test_adjacent_split_uses_effective_ratios_for_malformed_layout() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".into()),
                SplitNode::leaf("b".into()),
                SplitNode::leaf("c".into()),
            ],
            vec![f32::NAN, 1.0, f32::INFINITY],
        );
        let mut tab = TabState::with_layout(layout);
        tab.active_split_path = vec![0];

        assert_eq!(
            tab.adjacent_split_path(crate::workspace::AddSplitDirection::Right),
            Some(vec![1])
        );
    }

    #[test]
    fn test_adjacent_split_uses_painted_bounds_when_available() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::split(
                    SplitOrientation::Horizontal,
                    vec![
                        SplitNode::leaf("top-left".into()),
                        SplitNode::leaf("bottom-left".into()),
                    ],
                    vec![0.9, 0.1],
                ),
                SplitNode::split(
                    SplitOrientation::Horizontal,
                    vec![
                        SplitNode::leaf("top-right".into()),
                        SplitNode::leaf("bottom-right".into()),
                    ],
                    vec![0.1, 0.9],
                ),
            ],
            vec![0.5, 0.5],
        );
        let mut tab = TabState::with_layout(layout);
        tab.active_split_path = vec![0, 0];

        assert_eq!(
            tab.adjacent_split_path(AddSplitDirection::Right),
            Some(vec![1, 1])
        );
        assert_eq!(
            tab.adjacent_split_path_in_bounds(
                AddSplitDirection::Right,
                [
                    (vec![0, 0], [0.0, 0.0, 50.0, 50.0]),
                    (vec![0, 1], [0.0, 50.0, 50.0, 100.0]),
                    (vec![1, 0], [50.0, 0.0, 100.0, 50.0]),
                    (vec![1, 1], [50.0, 50.0, 100.0, 100.0]),
                ],
            ),
            Some(vec![1, 0])
        );
    }

    #[test]
    fn test_tab_duplicate() {
        let mut original = TabState::with_layout(SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("xqc".to_string()),
                SplitNode::leaf("forsen".to_string()),
            ],
            vec![0.5, 0.5],
        ));
        original.set_title("Original");
        original.is_locked = true;

        let copy = original.duplicate();
        assert_ne!(copy.id, original.id);
        assert_eq!(copy.custom_title, Some("Original (copy)".to_string()));
        assert!(!copy.is_locked); // Lock not copied
        assert!(!copy.is_selected);
        assert!(
            original
                .layout
                .collect_split_ids()
                .iter()
                .all(|id| !copy.layout.collect_split_ids().contains(id))
        );
    }

    #[test]
    fn closing_an_inactive_pane_preserves_the_active_split() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            vec![1.0 / 3.0; 3],
        );
        let mut tab = TabState::with_layout(layout);
        tab.set_active_split_path(vec![2]);
        let active_id = tab.layout.split_id_at_path(&[2]);

        tab.close_pane_by_path(&[0]);

        assert_eq!(tab.active_split_path, vec![1]);
        assert_eq!(
            tab.layout.split_id_at_path(&tab.active_split_path),
            active_id
        );
    }

    #[test]
    fn closing_the_active_pane_selects_its_next_sibling() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            vec![1.0 / 3.0; 3],
        );
        let mut tab = TabState::with_layout(layout);
        tab.set_active_split_path(vec![1]);
        let expected_id = tab.layout.split_id_at_path(&[2]);

        tab.close_pane_by_path(&[1]);

        assert_eq!(tab.active_split_path, vec![1]);
        assert_eq!(
            tab.layout.split_id_at_path(&tab.active_split_path),
            expected_id
        );
    }

    #[test]
    fn closing_the_last_active_pane_selects_its_previous_sibling() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            vec![1.0 / 3.0; 3],
        );
        let mut tab = TabState::with_layout(layout);
        tab.set_active_split_path(vec![2]);
        let expected_id = tab.layout.split_id_at_path(&[1]);

        tab.close_pane_by_path(&[2]);

        assert_eq!(tab.active_split_path, vec![1]);
        assert_eq!(
            tab.layout.split_id_at_path(&tab.active_split_path),
            expected_id
        );
    }

    #[test]
    fn closing_a_nested_active_pane_does_not_fall_back_to_another_branch() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("outside".to_string()),
                SplitNode::split(
                    SplitOrientation::Vertical,
                    vec![
                        SplitNode::leaf("active".to_string()),
                        SplitNode::leaf("next".to_string()),
                    ],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.5, 0.5],
        );
        let mut tab = TabState::with_layout(layout);
        tab.set_active_split_path(vec![1, 0]);
        let expected_id = tab.layout.split_id_at_path(&[1, 1]);

        tab.close_pane_by_path(&[1, 0]);

        assert_eq!(tab.active_split_path, vec![1]);
        assert_eq!(
            tab.layout.split_id_at_path(&tab.active_split_path),
            expected_id
        );
    }

    #[test]
    fn moving_another_pane_preserves_the_active_split() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            vec![1.0 / 3.0; 3],
        );
        let mut tab = TabState::with_layout(layout);
        tab.set_active_split_path(vec![1]);
        let active_id = tab.layout.split_id_at_path(&[1]);

        assert!(tab.move_split(&[0], &[2], DropZone::Right));

        assert_eq!(tab.active_split_path, vec![0]);
        assert_eq!(
            tab.layout.split_id_at_path(&tab.active_split_path),
            active_id
        );
    }

    fn three_side_by_side() -> TabState {
        TabState::with_layout(SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            vec![1.0 / 3.0; 3],
        ))
    }

    #[test]
    fn moving_a_split_to_the_bottom_edge_spans_the_full_width() {
        let mut tab = three_side_by_side();
        tab.set_active_split_path(vec![1]);
        let active_id = tab.layout.split_id_at_path(&[1]);
        let moved_id = tab.layout.split_id_at_path(&[0]);

        assert!(tab.move_split_to_edge(&[0], DropZone::Bottom));

        let SplitNode::Split {
            orientation,
            children,
            ratios,
            ..
        } = tab.layout.as_ref()
        else {
            panic!("root should be a split");
        };
        assert_eq!(*orientation, SplitOrientation::Horizontal);
        assert_eq!(children.len(), 2);
        assert_eq!(ratios, &vec![0.5, 0.5]);
        assert_eq!(tab.layout.channel_at_path(&[1]).as_deref(), Some("a"));
        assert_eq!(tab.layout.split_id_at_path(&[1]), moved_id);
        assert_eq!(tab.layout.channel_at_path(&[0, 0]).as_deref(), Some("b"));
        assert_eq!(tab.layout.channel_at_path(&[0, 1]).as_deref(), Some("c"));
        assert_eq!(
            tab.layout.split_id_at_path(&tab.active_split_path),
            active_id
        );
    }

    #[test]
    fn moving_a_split_to_the_top_of_a_stack_joins_it_with_an_equal_share() {
        let mut tab = TabState::with_layout(SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::leaf("x".to_string()),
                SplitNode::leaf("y".to_string()),
                SplitNode::leaf("z".to_string()),
            ],
            vec![0.5, 0.25, 0.25],
        ));

        assert!(tab.move_split_to_edge(&[2], DropZone::Top));

        let SplitNode::Split {
            orientation,
            children,
            ratios,
            ..
        } = tab.layout.as_ref()
        else {
            panic!("root should be a split");
        };
        assert_eq!(*orientation, SplitOrientation::Horizontal);
        assert_eq!(children.len(), 3);
        assert_eq!(tab.layout.channel_at_path(&[0]).as_deref(), Some("z"));
        assert!((ratios.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!((ratios[0] - 1.0 / 3.0).abs() < 1e-5);
        assert!(ratios[1] > ratios[2]);
    }

    #[test]
    fn moving_one_of_two_splits_to_an_edge_survives_the_root_collapse() {
        let mut tab = TabState::with_layout(SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
            ],
            vec![0.5, 0.5],
        ));

        assert!(tab.move_split_to_edge(&[1], DropZone::Top));

        assert_eq!(tab.layout.channel_at_path(&[0]).as_deref(), Some("b"));
        assert_eq!(tab.layout.channel_at_path(&[1]).as_deref(), Some("a"));
        assert!(!tab.move_split_to_edge(&[0], DropZone::Center));
    }
}
