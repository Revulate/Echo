//! Workspace (tab group) state
//!
//! This module provides:
//! - `SplitNode` - Tree structure for split pane layouts
//! - `SplitOrientation` - Horizontal/Vertical split direction
//! - `AddSplitDirection` / `DropZone` - Drag-drop support
//! - Placeholder utilities for empty panes

use crate::SplitId;
use crate::message::DeletedMessageMode;
use std::collections::HashSet;
use uuid::Uuid;

// ============================================================================
// Placeholder Utilities (module-level, no struct needed)
// ============================================================================

/// Prefix for placeholder channels
pub const PLACEHOLDER_PREFIX: &str = "__empty_";

/// Generate a unique placeholder channel ID
pub fn new_placeholder() -> String {
    format!("{}{}__", PLACEHOLDER_PREFIX, Uuid::new_v4().simple())
}

/// Check if a channel name is a placeholder
pub fn is_placeholder(channel: &str) -> bool {
    channel.starts_with(PLACEHOLDER_PREFIX)
}

// ============================================================================
// 2x2 grid blocks
// ============================================================================

/// Two band ratios closer than this describe the same divider position.
///
/// Rotating a 2x2 block collapses its two band dividers into one shared
/// divider, so a rotation is only offered while those dividers agree; past this
/// tolerance the rotation would silently discard an adjustment the user made.
const GRID_RATIO_EPSILON: f32 = 0.004;

/// Ratio clamp used by [`SplitNode::adjust_ratio`], reused so a derived ratio
/// can never land somewhere a drag could not have put it.
const RATIO_MIN: f32 = 0.1;
const RATIO_MAX: f32 = 0.9;

/// A keyboard operation on one divider of a split container.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DividerResize {
    Decrease,
    Increase,
    Minimum,
    Maximum,
    Equalize,
    SetPercentage(f32),
}

/// The current and permitted positions of a divider within its adjacent pair.
///
/// Values are fractions of the pair, rather than the entire split, so a
/// keyboard step has the same meaning in a two-way or multi-way split.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DividerRatioBounds {
    pub value: f32,
    pub minimum: f32,
    pub maximum: f32,
    pair_ratio: f32,
    can_resize: bool,
    can_equalize: bool,
}

/// Ratios as they are interpreted for layout: invalid or missing entries get
/// an equal share, and the valid weights are normalized together.
pub(crate) fn effective_ratios(
    ratios: &[f32],
    child_count: usize,
) -> impl Iterator<Item = f32> + '_ {
    let fallback = if child_count == 0 {
        0.0
    } else {
        1.0 / child_count as f32
    };
    let sanitized = move |idx: usize| {
        ratios
            .get(idx)
            .copied()
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            .unwrap_or(fallback) as f64
    };
    let sum: f64 = (0..child_count).map(&sanitized).sum();
    let scale = if sum.is_finite() && sum > 0.0 {
        1.0 / sum
    } else if child_count > 0 {
        1.0 / child_count as f64
    } else {
        0.0
    };

    (0..child_count)
        .map(move |idx| (sanitized(idx) * scale) as f32)
        // An extreme, but valid, serialized ratio can underflow during the
        // f64-to-f32 conversion. Keep the stored layout usable in that case.
        .map(|ratio| ratio.max(f32::MIN_POSITIVE))
}

/// Normalize the two ratios of a two-child split, repairing missing or
/// non-finite entries the same way all layout consumers do.
fn normalized_ratio_pair(ratios: &[f32]) -> [f32; 2] {
    let mut ratios = effective_ratios(ratios, 2);
    [ratios.next().unwrap_or(0.5), ratios.next().unwrap_or(0.5)]
}

fn sanitized_minimum(minimum: f32) -> f32 {
    if minimum.is_finite() && minimum > 0.0 {
        minimum
    } else {
        0.0
    }
}

/// Normalized ratio pair of a child that is itself a two-child split.
fn child_ratio_pair(node: &SplitNode) -> Option<[f32; 2]> {
    match node {
        SplitNode::Split {
            children, ratios, ..
        } if children.len() == 2 => Some(normalized_ratio_pair(ratios)),
        _ => None,
    }
}

/// A split layout node: either a leaf (channel) or a split with children
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum SplitNode {
    Leaf {
        /// Stable unique identifier for this split pane
        /// Optional for backwards compatibility with old serialized data
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split_id: Option<SplitId>,
        /// Channel name displayed in this split
        channel: String,
        /// Whether this leaf hides its channel header.
        ///
        /// Kept on the leaf rather than a transient render cache so it follows
        /// the pane through reorder/move operations and survives restarts.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        header_collapsed: bool,
        /// How this pane presents moderator-removed messages; `None` follows
        /// the global setting. Lives on the leaf for the same reasons as
        /// `header_collapsed`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deleted_mode: Option<DeletedMessageMode>,
    },
    Split {
        /// Stable unique identifier for this container node
        /// Optional for backwards compatibility with old serialized data
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split_id: Option<SplitId>,
        orientation: SplitOrientation,
        children: Vec<SplitNode>,
        /// Proportional size ratio for each child (sum should be 1.0)
        ratios: Vec<f32>,
    },
}

impl SplitNode {
    /// Add a channel to the layout (wraps or appends to the root).
    pub fn add_channel_to_layout(&mut self, channel: String) {
        match self {
            SplitNode::Leaf {
                channel: existing, ..
            } => {
                if is_placeholder(existing) {
                    *existing = channel;
                } else {
                    let existing_node = SplitNode::leaf(existing.clone());
                    let new_node = SplitNode::leaf(channel);
                    *self = SplitNode::split(
                        SplitOrientation::Horizontal,
                        vec![existing_node, new_node],
                        vec![0.5, 0.5],
                    );
                }
            }
            SplitNode::Split {
                children, ratios, ..
            } => {
                children.push(SplitNode::leaf(channel));
                let new_count = children.len();
                *ratios = vec![1.0 / new_count as f32; new_count];
            }
        }
    }

    /// Create a single leaf from a channel name (with new SplitId)
    pub fn leaf(channel: String) -> Self {
        SplitNode::Leaf {
            split_id: Some(SplitId::new()),
            channel,
            header_collapsed: false,
            deleted_mode: None,
        }
    }

    /// Create a split container with children (with new SplitId)
    pub fn split(
        orientation: SplitOrientation,
        children: Vec<SplitNode>,
        ratios: Vec<f32>,
    ) -> Self {
        SplitNode::Split {
            split_id: Some(SplitId::new()),
            orientation,
            children,
            ratios,
        }
    }

    /// Get the SplitId of this node (works for both Leaf and Split)
    pub fn split_id(&self) -> Option<SplitId> {
        match self {
            SplitNode::Leaf { split_id, .. } => *split_id,
            SplitNode::Split { split_id, .. } => *split_id,
        }
    }

    /// Get the SplitId at a given path
    pub fn get_split_id_at_path(&self, path: &[usize]) -> Option<SplitId> {
        if path.is_empty() {
            return self.split_id();
        }

        match self {
            SplitNode::Leaf { .. } => None,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                children.get(idx)?.get_split_id_at_path(&path[1..])
            }
        }
    }

    /// Borrow the node at a given path.
    pub fn get_node_at_path(&self, path: &[usize]) -> Option<&SplitNode> {
        if path.is_empty() {
            return Some(self);
        }

        match self {
            SplitNode::Leaf { .. } => None,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                children.get(idx)?.get_node_at_path(&path[1..])
            }
        }
    }

    /// Return the left-most leaf SplitId in this subtree.
    pub fn leftmost_leaf_split_id(&self) -> Option<SplitId> {
        match self {
            SplitNode::Leaf { split_id, .. } => *split_id,
            SplitNode::Split { children, .. } => children.first()?.leftmost_leaf_split_id(),
        }
    }

    /// Return the right-most leaf SplitId in this subtree.
    pub fn rightmost_leaf_split_id(&self) -> Option<SplitId> {
        match self {
            SplitNode::Leaf { split_id, .. } => *split_id,
            SplitNode::Split { children, .. } => children.last()?.rightmost_leaf_split_id(),
        }
    }

    /// Ensure all nodes have SplitIds (for migration from old data)
    /// Returns true if any IDs were added
    pub fn ensure_split_ids(&mut self) -> bool {
        match self {
            SplitNode::Leaf { split_id, .. } => {
                if split_id.is_none() {
                    *split_id = Some(SplitId::new());
                    true
                } else {
                    false
                }
            }
            SplitNode::Split {
                split_id, children, ..
            } => {
                let mut any_added = false;
                // Ensure container has an ID
                if split_id.is_none() {
                    *split_id = Some(SplitId::new());
                    any_added = true;
                }
                // Recurse to children
                for child in children.iter_mut() {
                    if child.ensure_split_ids() {
                        any_added = true;
                    }
                }
                any_added
            }
        }
    }

    /// Return an identical layout with new stable IDs for every node.
    pub fn with_fresh_split_ids(&self) -> Self {
        let mut copy = self.clone();
        copy.assign_fresh_split_ids();
        copy
    }

    /// Ensure every node has an ID that is not already present in `seen`.
    ///
    /// Callers should retain `seen` while walking persisted tab layouts so old
    /// duplicated tabs are repaired without changing their structure.
    pub fn ensure_unique_split_ids(&mut self, seen: &mut HashSet<SplitId>) -> bool {
        let split_id = match self {
            SplitNode::Leaf { split_id, .. } | SplitNode::Split { split_id, .. } => split_id,
        };

        let mut changed = false;
        if split_id.is_none() || split_id.is_some_and(|id| seen.contains(&id)) {
            let mut fresh_id = SplitId::new();
            while seen.contains(&fresh_id) {
                fresh_id = SplitId::new();
            }
            *split_id = Some(fresh_id);
            changed = true;
        }
        seen.insert(split_id.expect("split ID assigned above"));

        if let SplitNode::Split { children, .. } = self {
            for child in children {
                changed |= child.ensure_unique_split_ids(seen);
            }
        }

        changed
    }

    fn assign_fresh_split_ids(&mut self) {
        match self {
            SplitNode::Leaf { split_id, .. } => *split_id = Some(SplitId::new()),
            SplitNode::Split {
                split_id, children, ..
            } => {
                *split_id = Some(SplitId::new());
                for child in children {
                    child.assign_fresh_split_ids();
                }
            }
        }
    }

    /// Collect all SplitIds in the tree (both leaves and containers)
    pub fn collect_split_ids(&self) -> Vec<SplitId> {
        match self {
            SplitNode::Leaf { split_id, .. } => split_id.iter().copied().collect(),
            SplitNode::Split {
                split_id, children, ..
            } => {
                let mut result: Vec<SplitId> = split_id.iter().copied().collect();
                for child in children {
                    result.extend(child.collect_split_ids());
                }
                result
            }
        }
    }

    /// Collect channels in depth-first order (may include duplicates/placeholders)
    pub fn channels(&self) -> Vec<String> {
        match self {
            SplitNode::Leaf { channel, .. } => vec![channel.clone()],
            SplitNode::Split { children, .. } => {
                let mut result = Vec::new();
                for child in children {
                    result.extend(child.channels());
                }
                result
            }
        }
    }

    /// Collect all "channel:path" keys in the layout tree (for cache validation)
    /// Path format: "channel:0-1-2" where numbers are indices in the split tree
    pub fn collect_channel_path_keys(&self, current_path: &[usize]) -> Vec<String> {
        match self {
            SplitNode::Leaf { channel, .. } => {
                let path_str = current_path
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join("-");
                vec![format!("{}:{}", channel, path_str)]
            }
            SplitNode::Split { children, .. } => {
                let mut result = Vec::new();
                for (idx, child) in children.iter().enumerate() {
                    let mut child_path = current_path.to_vec();
                    child_path.push(idx);
                    result.extend(child.collect_channel_path_keys(&child_path));
                }
                result
            }
        }
    }

    /// Count the number of leaf nodes (visible chat panes)
    pub fn count_leaves(&self) -> usize {
        match self {
            SplitNode::Leaf { .. } => 1,
            SplitNode::Split { children, .. } => children.iter().map(|c| c.count_leaves()).sum(),
        }
    }

    /// Replace the channel at `path` (for placeholders) with a concrete channel.
    pub fn replace_channel_at_path(&mut self, path: &[usize], channel: String) -> bool {
        if path.is_empty() {
            if let SplitNode::Leaf {
                channel: leaf_channel,
                ..
            } = self
            {
                *leaf_channel = channel;
                return true;
            }
            return false;
        }

        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                if idx >= children.len() {
                    return false;
                }
                children[idx].replace_channel_at_path(&path[1..], channel)
            }
        }
    }

    /// Return whether the leaf at `path` has its channel header collapsed.
    pub fn header_collapsed_at_path(&self, path: &[usize]) -> Option<bool> {
        if path.is_empty() {
            return match self {
                SplitNode::Leaf {
                    header_collapsed, ..
                } => Some(*header_collapsed),
                SplitNode::Split { .. } => None,
            };
        }

        match self {
            SplitNode::Leaf { .. } => None,
            SplitNode::Split { children, .. } => {
                children.get(path[0])?.header_collapsed_at_path(&path[1..])
            }
        }
    }

    /// Set the persisted channel-header visibility for exactly one leaf.
    pub fn set_header_collapsed_at_path(&mut self, path: &[usize], collapsed: bool) -> bool {
        if path.is_empty() {
            if let SplitNode::Leaf {
                header_collapsed, ..
            } = self
                && *header_collapsed != collapsed
            {
                *header_collapsed = collapsed;
                return true;
            }
            return false;
        }

        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => children
                .get_mut(path[0])
                .is_some_and(|child| child.set_header_collapsed_at_path(&path[1..], collapsed)),
        }
    }

    /// The deleted-message mode this pane overrides the global setting with.
    pub fn deleted_mode_at_path(&self, path: &[usize]) -> Option<DeletedMessageMode> {
        if path.is_empty() {
            return match self {
                SplitNode::Leaf { deleted_mode, .. } => *deleted_mode,
                SplitNode::Split { .. } => None,
            };
        }

        match self {
            SplitNode::Leaf { .. } => None,
            SplitNode::Split { children, .. } => {
                children.get(path[0])?.deleted_mode_at_path(&path[1..])
            }
        }
    }

    /// Persist the deleted-message override for exactly one leaf.
    pub fn set_deleted_mode_at_path(
        &mut self,
        path: &[usize],
        mode: Option<DeletedMessageMode>,
    ) -> bool {
        if path.is_empty() {
            if let SplitNode::Leaf { deleted_mode, .. } = self
                && *deleted_mode != mode
            {
                *deleted_mode = mode;
                return true;
            }
            return false;
        }

        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => children
                .get_mut(path[0])
                .is_some_and(|child| child.set_deleted_mode_at_path(&path[1..], mode)),
        }
    }

    /// Remove a node at the provided path. Collapses single-child splits.
    pub fn remove_at_path(self, path: &[usize]) -> Option<SplitNode> {
        if path.is_empty() {
            return None;
        }

        match self {
            SplitNode::Leaf { .. } => Some(self),
            SplitNode::Split {
                split_id,
                orientation,
                mut children,
                mut ratios,
            } => {
                let idx = path[0];
                if idx >= children.len() {
                    return Some(SplitNode::Split {
                        split_id,
                        orientation,
                        children,
                        ratios,
                    });
                }

                if path.len() == 1 {
                    let mut effective: Vec<_> = effective_ratios(&ratios, children.len()).collect();
                    children.remove(idx);
                    effective.remove(idx);
                    ratios = effective_ratios(&effective, children.len()).collect();
                } else {
                    let child = children.remove(idx);
                    if let Some(updated) = child.remove_at_path(&path[1..]) {
                        children.insert(idx, updated);
                    }
                }

                if children.is_empty() {
                    None
                } else if children.len() == 1 {
                    Some(children.remove(0))
                } else {
                    Some(SplitNode::Split {
                        split_id,
                        orientation,
                        children,
                        ratios,
                    })
                }
            }
        }
    }

    /// Recursively remove a leaf matching `target`.
    /// Returns `Some(node)` if a subtree remains, or `None` if this branch was removed.
    pub fn remove_leaf(self, target: &str) -> Option<SplitNode> {
        match self {
            SplitNode::Leaf {
                split_id,
                channel,
                header_collapsed,
                deleted_mode,
            } => {
                if channel == target {
                    None
                } else {
                    Some(SplitNode::Leaf {
                        split_id,
                        channel,
                        header_collapsed,
                        deleted_mode,
                    })
                }
            }
            SplitNode::Split {
                split_id,
                orientation,
                children,
                ratios,
            } => {
                let mut new_children: Vec<SplitNode> = Vec::new();
                let mut new_ratios: Vec<f32> = Vec::new();
                let ratios: Vec<_> = effective_ratios(&ratios, children.len()).collect();

                for (idx, child) in children.into_iter().enumerate() {
                    if let Some(node) = child.remove_leaf(target) {
                        new_children.push(node);
                        new_ratios.push(ratios[idx]);
                    }
                }

                new_ratios = effective_ratios(&new_ratios, new_children.len()).collect();

                if new_children.is_empty() {
                    None
                } else if new_children.len() == 1 {
                    Some(new_children.remove(0))
                } else {
                    Some(SplitNode::Split {
                        split_id,
                        orientation,
                        children: new_children,
                        ratios: new_ratios,
                    })
                }
            }
        }
    }

    /// Adjust the ratio between child at idx and idx+1
    ///
    /// Returns true only when the stored ratios actually moved. A divider held
    /// against its clamp keeps producing pointer moves whose clamped result is
    /// the current ratio; reporting those as adjustments repaints the window
    /// root at pointer rate for an unchanged layout.
    pub fn adjust_ratio(&mut self, path: &[usize], child_idx: usize, delta: f32) -> bool {
        if path.is_empty() {
            // We're at the target split node
            if let SplitNode::Split {
                ratios, children, ..
            } = self
            {
                // Make sure we have valid indices
                if child_idx >= children.len().saturating_sub(1) {
                    return false;
                }

                if !delta.is_finite() {
                    return false;
                }

                let mut effective: Vec<_> = effective_ratios(ratios, children.len()).collect();
                let current_left = effective[child_idx];
                let pair_budget = current_left + effective[child_idx + 1];
                let pair_min = pair_budget * RATIO_MIN;
                let pair_max = pair_budget * RATIO_MAX;
                let new_ratio_left = (current_left + delta).clamp(pair_min, pair_max);
                let actual_delta = new_ratio_left - current_left;
                let ratios_changed = ratios.len() != effective.len()
                    || ratios
                        .iter()
                        .zip(&effective)
                        .any(|(stored, effective)| stored != effective);
                if actual_delta == 0.0 && !ratios_changed {
                    // Already at the clamp (or a sub-epsilon delta): nothing to
                    // apply, and the ratios were normalized by the change that
                    // put them here.
                    return false;
                }

                effective[child_idx] = new_ratio_left;
                effective[child_idx + 1] = pair_budget - new_ratio_left;
                *ratios = effective;

                return true;
            }
            return false;
        }

        // Navigate deeper
        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                if idx >= children.len() {
                    return false;
                }
                children[idx].adjust_ratio(&path[1..], child_idx, delta)
            }
        }
    }

    /// Ratios for this split after applying per-child minimum pixel extents.
    ///
    /// When the available extent can satisfy every minimum, unconstrained
    /// children preserve their stored proportional weights while constrained
    /// children stay at their floor. If it cannot, the floors themselves are
    /// scaled proportionally so the layout still has a stable, finite shape.
    pub fn ratios_with_minimums(&self, extent: f32, minimums: &[f32]) -> Vec<f32> {
        let SplitNode::Split {
            ratios, children, ..
        } = self
        else {
            return Vec::new();
        };

        let child_count = children.len();
        let weights: Vec<_> = effective_ratios(ratios, child_count).collect();
        if child_count == 0 || minimums.len() != child_count {
            return weights;
        }

        let minimum_sum: f64 = minimums
            .iter()
            .map(|minimum| sanitized_minimum(*minimum) as f64)
            .sum();
        if !minimum_sum.is_finite() || minimum_sum <= 0.0 {
            return weights;
        }

        let extent = extent.max(0.0);
        if !extent.is_finite() {
            return weights;
        }
        if extent == 0.0 || minimum_sum > extent as f64 {
            return minimums
                .iter()
                .map(|minimum| (sanitized_minimum(*minimum) as f64 / minimum_sum) as f32)
                .collect();
        }

        if weights.iter().zip(minimums).all(|(&weight, &minimum)| {
            weight as f64 * extent as f64 >= sanitized_minimum(minimum) as f64
        }) {
            return weights;
        }

        let floors: Vec<_> = minimums
            .iter()
            .map(|minimum| sanitized_minimum(*minimum) / extent)
            .collect();
        let mut scale = 1.0;
        for _ in 0..child_count {
            let mut floor_total = 0.0;
            let mut remaining_weight = 0.0;
            for (&weight, &floor) in weights.iter().zip(&floors) {
                if floor > scale * weight {
                    floor_total += floor;
                } else {
                    remaining_weight += weight;
                }
            }

            let next_scale = (1.0 - floor_total) / remaining_weight;
            if !next_scale.is_finite() {
                scale = 0.0;
                break;
            }
            if (next_scale - scale).abs() <= f32::EPSILON {
                scale = next_scale.max(0.0);
                break;
            }
            scale = next_scale.max(0.0);
        }

        weights
            .into_iter()
            .zip(floors)
            .map(|(weight, floor)| (scale * weight).max(floor).max(0.0))
            .collect()
    }

    /// Return the visible position and legal range for a divider, expressed as
    /// fractions of the two children it separates.
    pub fn divider_ratio_bounds_with_minimums(
        &self,
        path: &[usize],
        child_idx: usize,
        extent: f32,
        minimums: &[f32],
    ) -> Option<DividerRatioBounds> {
        if !extent.is_finite()
            || extent <= 0.0
            || minimums
                .iter()
                .any(|minimum| !minimum.is_finite() || *minimum < 0.0)
        {
            return None;
        }

        let SplitNode::Split { children, .. } = self.get_node_at_path(path)? else {
            return None;
        };
        if minimums.len() != children.len() || child_idx >= children.len().saturating_sub(1) {
            return None;
        }

        let effective = self
            .get_node_at_path(path)?
            .ratios_with_minimums(extent, minimums);
        let left = *effective.get(child_idx)?;
        let right = *effective.get(child_idx + 1)?;
        let pair_ratio = left + right;
        if !left.is_finite() || !right.is_finite() || !pair_ratio.is_finite() || pair_ratio <= 0.0 {
            return None;
        }

        let value = (left / pair_ratio).clamp(0.0, 1.0);
        let minimum_total: f32 = minimums.iter().sum();
        if !minimum_total.is_finite() {
            return None;
        }
        if minimum_total >= extent {
            return Some(DividerRatioBounds {
                value,
                minimum: value,
                maximum: value,
                pair_ratio,
                can_resize: false,
                can_equalize: false,
            });
        }

        let minimum = (minimums[child_idx] / extent / pair_ratio).clamp(0.0, 1.0);
        let maximum =
            ((pair_ratio - minimums[child_idx + 1] / extent) / pair_ratio).clamp(0.0, 1.0);
        if maximum < minimum {
            return None;
        }

        Some(DividerRatioBounds {
            value: value.clamp(minimum, maximum),
            minimum,
            maximum,
            pair_ratio,
            can_resize: maximum > minimum,
            can_equalize: true,
        })
    }

    /// Apply one keyboard operation to a divider while retaining the same
    /// minimum-size constraints used by pointer dragging.
    pub fn resize_divider_with_minimums(
        &mut self,
        path: &[usize],
        child_idx: usize,
        action: DividerResize,
        extent: f32,
        minimums: &[f32],
    ) -> bool {
        let Some(bounds) =
            self.divider_ratio_bounds_with_minimums(path, child_idx, extent, minimums)
        else {
            return false;
        };
        if action == DividerResize::Equalize {
            if !bounds.can_equalize {
                return false;
            }
            let Some(SplitNode::Split {
                children, ratios, ..
            }) = self.get_node_at_path(path)
            else {
                return false;
            };
            let share = 1.0 / children.len().max(1) as f32;
            if ratios.len() == children.len() && ratios.iter().all(|ratio| *ratio == share) {
                return false;
            }
            return self.reset_ratios(path);
        }
        if !bounds.can_resize {
            return false;
        }
        let target = match action {
            DividerResize::Decrease => bounds.value - 0.03,
            DividerResize::Increase => bounds.value + 0.03,
            DividerResize::Minimum => bounds.minimum,
            DividerResize::Maximum => bounds.maximum,
            DividerResize::SetPercentage(percentage) if percentage.is_finite() => {
                percentage / 100.0
            }
            DividerResize::SetPercentage(_) | DividerResize::Equalize => return false,
        }
        .clamp(bounds.minimum, bounds.maximum);

        self.adjust_ratio_with_minimums(
            path,
            child_idx,
            (target - bounds.value) * bounds.pair_ratio,
            extent,
            minimums,
        )
    }

    /// Adjust one divider while preserving per-child minimum pixel extents.
    ///
    /// `extent` and `minimums` describe the split node identified by `path`.
    /// The stored ratios are replaced with the current painted geometry only
    /// after a divider actually moves, avoiding persistence churn while every
    /// child is pinned at its minimum.
    pub fn adjust_ratio_with_minimums(
        &mut self,
        path: &[usize],
        child_idx: usize,
        delta: f32,
        extent: f32,
        minimums: &[f32],
    ) -> bool {
        self.adjust_ratio_with_minimums_applied(path, child_idx, delta, extent, minimums) != 0.0
    }

    /// Returns the applied ratio delta so pointer tracking can retain unapplied movement at a clamp.
    pub fn adjust_ratio_with_minimums_applied(
        &mut self,
        path: &[usize],
        child_idx: usize,
        delta: f32,
        extent: f32,
        minimums: &[f32],
    ) -> f32 {
        if !delta.is_finite()
            || !extent.is_finite()
            || extent <= 0.0
            || minimums
                .iter()
                .any(|minimum| !minimum.is_finite() || *minimum < 0.0)
        {
            return 0.0;
        }

        if !path.is_empty() {
            let SplitNode::Split { children, .. } = self else {
                return 0.0;
            };
            let Some(child) = children.get_mut(path[0]) else {
                return 0.0;
            };
            return child.adjust_ratio_with_minimums_applied(
                &path[1..],
                child_idx,
                delta,
                extent,
                minimums,
            );
        }

        let child_count = match self {
            SplitNode::Split { children, .. } => children.len(),
            SplitNode::Leaf { .. } => return 0.0,
        };
        if minimums.len() != child_count
            || child_idx >= child_count.saturating_sub(1)
            || minimums.iter().sum::<f32>() >= extent
        {
            return 0.0;
        }

        let mut effective = self.ratios_with_minimums(extent, minimums);
        let current_left = effective[child_idx];
        let pair_budget = current_left + effective[child_idx + 1];
        let lower = minimums[child_idx] / extent;
        let upper = pair_budget - minimums[child_idx + 1] / extent;
        if !lower.is_finite() || !upper.is_finite() || upper < lower {
            return 0.0;
        }
        let new_left = (current_left + delta).clamp(lower, upper);
        if new_left == current_left {
            return 0.0;
        }

        effective[child_idx] = new_left;
        effective[child_idx + 1] = pair_budget - new_left;
        let SplitNode::Split { ratios, .. } = self else {
            unreachable!("split node was checked above");
        };
        *ratios = effective;
        new_left - current_left
    }

    // ========================================================================
    // 2x2 grid blocks
    // ========================================================================

    /// Whether this node is an exact 2x2 rectangular block of panes: two child
    /// splits perpendicular to this one, each holding exactly two leaves.
    ///
    /// A 2x2 block is the one shape a binary split tree cannot describe
    /// completely. One axis gets a single divider spanning the whole block, the
    /// other gets one divider per band, so dragging the shared divider always
    /// moves the far band too. Rotating the block moves that limitation to the
    /// other axis; see [`Self::transpose_2x2`].
    pub fn is_exact_2x2_block(&self) -> bool {
        let SplitNode::Split {
            orientation,
            children,
            ..
        } = self
        else {
            return false;
        };
        if children.len() != 2 {
            return false;
        }

        children.iter().all(|child| match child {
            SplitNode::Split {
                orientation: child_orientation,
                children: grandchildren,
                ..
            } => {
                child_orientation != orientation
                    && grandchildren.len() == 2
                    && grandchildren
                        .iter()
                        .all(|pane| matches!(pane, SplitNode::Leaf { .. }))
            }
            SplitNode::Leaf { .. } => false,
        })
    }

    /// A 2x2 block whose shared divider runs top-to-bottom, i.e. two stacked
    /// panes per column. This is the shape that makes one row's resize move the
    /// other row, and the shape creation-time normalization rotates away.
    pub fn is_column_major_2x2(&self) -> bool {
        matches!(
            self,
            SplitNode::Split {
                orientation: SplitOrientation::Vertical,
                ..
            }
        ) && self.is_exact_2x2_block()
    }

    /// Where the shared divider of the rotated block would sit, as a fraction
    /// of the block along the current shared divider's length.
    ///
    /// `None` unless this is an exact 2x2 block whose two band dividers still
    /// agree, so callers can use `Some` as "rotating here is free". The value is
    /// area-weighted, so the rotated shared divider keeps the same share of the
    /// block that the two band dividers described.
    pub fn grid_band_split_ratio(&self) -> Option<f32> {
        if !self.is_exact_2x2_block() {
            return None;
        }
        let SplitNode::Split {
            children, ratios, ..
        } = self
        else {
            return None;
        };

        let first = child_ratio_pair(children.first()?)?;
        let second = child_ratio_pair(children.get(1)?)?;
        if (first[0] - second[0]).abs() > GRID_RATIO_EPSILON {
            return None;
        }

        let shared = normalized_ratio_pair(ratios);
        Some((shared[0] * first[0] + shared[1] * second[0]).clamp(RATIO_MIN, RATIO_MAX))
    }

    /// Whether rotating this 2x2 block would leave every pane exactly where it
    /// is painted right now.
    pub fn can_transpose_2x2(&self) -> bool {
        self.grid_band_split_ratio().is_some()
    }

    /// Rotate an exact 2x2 block between column-major and row-major.
    ///
    /// The pane at `[i][j]` moves to `[j][i]` and keeps its `SplitId`, the
    /// shared divider's ratio is copied into both new bands, and the new shared
    /// ratio comes from [`Self::grid_band_split_ratio`]. Nothing outside the
    /// block moves, and because the rotation only runs while the two band
    /// dividers agree, no pane changes size.
    ///
    /// This is the only sanctioned fix for the shared-divider problem:
    /// `adjust_ratio` must never reach into descendants, because a divider that
    /// legitimately spans a whole column (a full-height column beside a 2x2
    /// block, say) has to keep resizing that whole column.
    pub fn transpose_2x2(&mut self) -> bool {
        let Some(cross_first) = self.grid_band_split_ratio() else {
            return false;
        };
        let SplitNode::Split {
            orientation,
            children,
            ratios,
            ..
        } = self
        else {
            return false;
        };

        let shared = normalized_ratio_pair(ratios);
        let mut band_ids: Vec<Option<SplitId>> = Vec::with_capacity(2);
        let mut panes: Vec<Vec<SplitNode>> = Vec::with_capacity(2);
        let mut rotated_orientation = orientation.clone();

        for child in children.iter() {
            // Shape guaranteed by `grid_band_split_ratio`.
            let SplitNode::Split {
                split_id,
                orientation: child_orientation,
                children: grandchildren,
                ..
            } = child
            else {
                return false;
            };
            band_ids.push(*split_id);
            panes.push(grandchildren.clone());
            rotated_orientation = child_orientation.clone();
        }

        let bands = (0..2)
            .map(|band| SplitNode::Split {
                split_id: band_ids[band],
                orientation: orientation.clone(),
                children: vec![panes[0][band].clone(), panes[1][band].clone()],
                ratios: vec![shared[0], shared[1]],
            })
            .collect::<Vec<_>>();

        *orientation = rotated_orientation;
        *children = bands;
        *ratios = vec![cross_first, 1.0 - cross_first];
        true
    }

    /// [`Self::transpose_2x2`] for the node at `path`.
    pub fn transpose_2x2_at_path(&mut self, path: &[usize]) -> bool {
        if path.is_empty() {
            return self.transpose_2x2();
        }

        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                if idx >= children.len() {
                    return false;
                }
                children[idx].transpose_2x2_at_path(&path[1..])
            }
        }
    }

    /// Replace the node at `path` wholesale, keeping the tree shape.
    pub fn set_node_at_path(&mut self, path: &[usize], node: SplitNode) -> bool {
        if path.is_empty() {
            *self = node;
            return true;
        }

        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                if idx >= children.len() {
                    return false;
                }
                children[idx].set_node_at_path(&path[1..], node)
            }
        }
    }

    /// Give every child of the split at `path` an equal share. A double click
    /// on a resize handle triggers this.
    pub fn reset_ratios(&mut self, path: &[usize]) -> bool {
        if path.is_empty() {
            let SplitNode::Split {
                children, ratios, ..
            } = self
            else {
                return false;
            };
            let share = 1.0 / children.len().max(1) as f32;
            *ratios = vec![share; children.len()];
            return true;
        }

        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                if idx >= children.len() {
                    return false;
                }
                children[idx].reset_ratios(&path[1..])
            }
        }
    }

    /// Get the channel name at the given path (only for leaf nodes)
    pub fn get_channel_at_path(&self, path: &[usize]) -> Option<String> {
        self.get_channel_ref_at_path(path).map(ToOwned::to_owned)
    }

    /// Borrow the channel name at the given path (only for leaf nodes).
    pub fn get_channel_ref_at_path(&self, path: &[usize]) -> Option<&str> {
        if path.is_empty() {
            if let SplitNode::Leaf { channel, .. } = self {
                return Some(channel.as_str());
            }
            return None;
        }

        match self {
            SplitNode::Leaf { .. } => None,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                children.get(idx)?.get_channel_ref_at_path(&path[1..])
            }
        }
    }

    /// Return the first leaf path in depth-first order.
    pub fn first_leaf_path(&self) -> Option<Vec<usize>> {
        fn walk(node: &SplitNode, path: &mut Vec<usize>) -> bool {
            match node {
                SplitNode::Leaf { .. } => true,
                SplitNode::Split { children, .. } => {
                    for (idx, child) in children.iter().enumerate() {
                        path.push(idx);
                        if walk(child, path) {
                            return true;
                        }
                        path.pop();
                    }
                    false
                }
            }
        }

        let mut path = Vec::new();
        if walk(self, &mut path) {
            Some(path)
        } else {
            None
        }
    }

    /// Insert a new child at the given path with a direction
    /// This wraps the target in a new split container if needed
    pub fn insert_relative(&mut self, path: &[usize], direction: AddSplitDirection) -> bool {
        self.insert_node_relative(path, direction, SplitNode::leaf(new_placeholder()))
    }

    /// Insert `new_node` along an outer edge of this whole layout so it spans
    /// the full width (Top/Bottom) or height (Left/Right), as when a split is
    /// dropped on the layout's outer edge. A root already stacked in that
    /// direction gains a sibling with an equal share; any other root is wrapped
    /// half and half.
    pub fn insert_node_at_edge(&mut self, direction: AddSplitDirection, new_node: SplitNode) {
        let orientation = match direction {
            AddSplitDirection::Left | AddSplitDirection::Right => SplitOrientation::Vertical,
            AddSplitDirection::Top | AddSplitDirection::Bottom => SplitOrientation::Horizontal,
        };
        let insert_first = matches!(direction, AddSplitDirection::Left | AddSplitDirection::Top);

        if let SplitNode::Split {
            orientation: existing_orientation,
            children,
            ratios,
            ..
        } = self
            && *existing_orientation == orientation
        {
            let share = 1.0 / (children.len() + 1) as f32;
            // Scale what layout actually shows, so persisted zero or missing
            // weights cannot skew the new pane's share.
            *ratios = effective_ratios(ratios, children.len())
                .map(|ratio| ratio * (1.0 - share))
                .collect();
            if insert_first {
                children.insert(0, new_node);
                ratios.insert(0, share);
            } else {
                children.push(new_node);
                ratios.push(share);
            }
            return;
        }

        self.insert_node_relative(&[], direction, new_node);
    }

    /// Insert an existing node at the given path with a direction.
    /// This preserves the inserted node's `SplitId`, unlike `insert_relative`.
    pub fn insert_node_relative(
        &mut self,
        path: &[usize],
        direction: AddSplitDirection,
        new_node: SplitNode,
    ) -> bool {
        let orientation = match direction {
            AddSplitDirection::Left | AddSplitDirection::Right => SplitOrientation::Vertical,
            AddSplitDirection::Top | AddSplitDirection::Bottom => SplitOrientation::Horizontal,
        };
        let insert_first = matches!(direction, AddSplitDirection::Left | AddSplitDirection::Top);

        if path.is_empty() {
            // Wrap this node in a new split
            let existing = std::mem::replace(self, SplitNode::leaf(String::new()));

            let (first, second) = if insert_first {
                (new_node, existing)
            } else {
                (existing, new_node)
            };

            *self = SplitNode::split(orientation, vec![first, second], vec![0.5, 0.5]);
            return true;
        }

        // Navigate to parent
        match self {
            SplitNode::Leaf { .. } => false,
            SplitNode::Split { children, .. } => {
                let idx = path[0];
                if idx >= children.len() {
                    return false;
                }
                children[idx].insert_node_relative(&path[1..], direction, new_node)
            }
        }
    }
}

/// Orientation of a split
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitOrientation {
    Vertical,
    Horizontal,
}

/// Direction for adding a new split (used by drag/drop system)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AddSplitDirection {
    Top,
    Bottom,
    Left,
    Right,
}

/// Drop zone for drag and drop operations
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    Top,
    Bottom,
    Left,
    Right,
    Center,
}

/// Drag and drop state for split pane operations.
#[derive(Debug, Clone, Default)]
pub struct DragDropState {
    pub is_dragging: bool,
    pub source_path: Option<Vec<usize>>,
    pub hover_target: Option<(Vec<usize>, DropZone)>,
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // SplitNode Tests
    // ========================================================================

    #[test]
    fn test_leaf_creation() {
        let leaf = SplitNode::leaf("xqc".to_string());
        assert!(
            matches!(leaf, SplitNode::Leaf { channel, split_id, .. } if channel == "xqc" && split_id.is_some())
        );
    }

    #[test]
    fn edge_insert_normalizes_degenerate_ratios_first() {
        let mut layout = SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
            ],
            vec![0.8, 0.0],
        );
        layout.insert_node_at_edge(AddSplitDirection::Top, SplitNode::leaf("c".to_string()));
        let SplitNode::Split { ratios, .. } = &layout else {
            panic!("root should be a split");
        };
        assert_eq!(ratios.len(), 3);
        assert!((ratios[0] - 1.0 / 3.0).abs() < 1e-5);
        assert!(ratios.iter().all(|ratio| *ratio > 0.0));
        assert!((ratios.iter().sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn fresh_split_ids_are_disjoint_from_the_source_layout() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
            ],
            vec![0.5, 0.5],
        );
        let copy = layout.with_fresh_split_ids();
        let source_ids: HashSet<_> = layout.collect_split_ids().into_iter().collect();

        assert!(
            copy.collect_split_ids()
                .into_iter()
                .all(|id| !source_ids.contains(&id))
        );
    }

    #[test]
    fn duplicate_split_ids_are_reassigned_against_prior_layouts() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
            ],
            vec![0.5, 0.5],
        );
        let mut duplicate = layout.clone();
        let mut seen: HashSet<_> = layout.collect_split_ids().into_iter().collect();

        assert!(duplicate.ensure_unique_split_ids(&mut seen));
        assert!(
            duplicate
                .collect_split_ids()
                .into_iter()
                .all(|id| !layout.collect_split_ids().contains(&id))
        );
    }

    #[test]
    fn test_add_channel_replaces_placeholder_leaf() {
        let mut layout = SplitNode::leaf(new_placeholder());

        layout.add_channel_to_layout("alpha".to_string());

        assert_eq!(layout.channels(), vec!["alpha"]);
    }

    #[test]
    fn test_add_channel_wraps_non_placeholder_leaf() {
        let mut layout = SplitNode::leaf("alpha".to_string());

        layout.add_channel_to_layout("bravo".to_string());

        let SplitNode::Split {
            orientation,
            ratios,
            ..
        } = &layout
        else {
            panic!("channel addition should wrap a populated leaf");
        };
        assert_eq!(*orientation, SplitOrientation::Horizontal);
        assert_eq!(ratios, &[0.5, 0.5]);
        assert_eq!(layout.channels(), vec!["alpha", "bravo"]);
    }

    #[test]
    fn test_add_channel_appends_and_rebalances_existing_split() {
        let mut layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("alpha".to_string()),
                SplitNode::leaf("bravo".to_string()),
            ],
            vec![0.25, 0.75],
        );

        layout.add_channel_to_layout("charlie".to_string());

        let SplitNode::Split {
            orientation,
            ratios,
            ..
        } = &layout
        else {
            panic!("channel addition should preserve an existing split");
        };
        assert_eq!(*orientation, SplitOrientation::Vertical);
        assert_eq!(ratios, &vec![1.0 / 3.0; 3]);
        assert_eq!(layout.channels(), vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn test_channels_single_leaf() {
        let leaf = SplitNode::leaf("forsen".to_string());
        let channels = leaf.channels();
        assert_eq!(channels, vec!["forsen"]);
    }

    #[test]
    fn test_channels_nested_split() {
        let split = SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::leaf("xqc".to_string()),
                SplitNode::split(
                    SplitOrientation::Vertical,
                    vec![
                        SplitNode::leaf("forsen".to_string()),
                        SplitNode::leaf("nmplol".to_string()),
                    ],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.5, 0.5],
        );
        let channels = split.channels();
        assert_eq!(channels, vec!["xqc", "forsen", "nmplol"]);
    }

    #[test]
    fn test_count_leaves() {
        let split = SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::split(
                    SplitOrientation::Vertical,
                    vec![
                        SplitNode::leaf("c".to_string()),
                        SplitNode::leaf("d".to_string()),
                    ],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.33, 0.33, 0.34],
        );
        assert_eq!(split.count_leaves(), 4);
    }

    #[test]
    fn test_get_channel_at_path() {
        let split = SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::leaf("xqc".to_string()),
                SplitNode::leaf("forsen".to_string()),
            ],
            vec![0.5, 0.5],
        );

        assert_eq!(split.get_channel_at_path(&[0]), Some("xqc".to_string()));
        assert_eq!(split.get_channel_at_path(&[1]), Some("forsen".to_string()));
        assert_eq!(split.get_channel_at_path(&[2]), None);
        assert_eq!(split.get_channel_at_path(&[]), None); // Split node, not leaf
    }

    #[test]
    fn test_remove_leaf() {
        let split = SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::leaf("xqc".to_string()),
                SplitNode::leaf("forsen".to_string()),
                SplitNode::leaf("nmplol".to_string()),
            ],
            vec![0.33, 0.33, 0.34],
        );

        let result = split.remove_leaf("forsen");
        assert!(result.is_some());

        let remaining = result.unwrap();
        let channels = remaining.channels();
        assert_eq!(channels.len(), 2);
        assert!(channels.contains(&"xqc".to_string()));
        assert!(channels.contains(&"nmplol".to_string()));
        assert!(!channels.contains(&"forsen".to_string()));
    }

    #[test]
    fn test_remove_leaf_collapses_single_child() {
        let split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("xqc".to_string()),
                SplitNode::leaf("forsen".to_string()),
            ],
            ratios: vec![0.5, 0.5],
        };

        let result = split.remove_leaf("forsen");
        assert!(result.is_some());

        // Should collapse to single leaf
        let remaining = result.unwrap();
        assert!(matches!(remaining, SplitNode::Leaf { channel, .. } if channel == "xqc"));
    }

    #[test]
    fn test_ratio_normalization_after_remove() {
        let split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            ratios: vec![0.33, 0.33, 0.34],
        };

        let result = split.remove_leaf("b").unwrap();
        if let SplitNode::Split { ratios, .. } = result {
            let sum: f32 = ratios.iter().sum();
            assert!(
                (sum - 1.0).abs() < 0.01,
                "Ratios should sum to ~1.0, got {}",
                sum
            );
        }
    }

    #[test]
    fn test_adjust_ratio() {
        let mut split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
            ],
            ratios: vec![0.5, 0.5],
        };

        let success = split.adjust_ratio(&[], 0, 0.2);
        assert!(success);

        if let SplitNode::Split { ratios, .. } = &split {
            assert!((ratios[0] - 0.7).abs() < 0.01);
            assert!((ratios[1] - 0.3).abs() < 0.01);
            let sum: f32 = ratios.iter().sum();
            assert!((sum - 1.0).abs() < 0.01, "Ratios should sum to ~1.0");
        }
    }

    #[test]
    fn test_adjust_ratio_clamping() {
        let mut split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
            ],
            ratios: vec![0.5, 0.5],
        };

        // Try to adjust beyond bounds
        split.adjust_ratio(&[], 0, 1.0);

        if let SplitNode::Split { ratios, .. } = &split {
            // Should be clamped to 0.9 max
            assert!(ratios[0] <= 0.9);
            assert!(ratios[1] >= 0.1);
        }
    }

    #[test]
    fn test_adjust_ratio_keeps_multi_child_pair_valid() {
        let mut split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            ratios: vec![0.33, 0.33, 0.34],
        };

        assert!(split.adjust_ratio(&[], 0, 0.8));

        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert!(ratios.iter().all(|ratio| ratio.is_finite() && *ratio > 0.0));
        assert!((ratios[0] + ratios[1] - 0.66).abs() < 0.01);
        assert!(ratios[1] >= 0.066 - f32::EPSILON);

        let clamped = ratios.clone();
        assert!(!split.adjust_ratio(&[], 0, 0.8));
        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert_eq!(ratios, &clamped);
    }

    #[test]
    fn test_effective_ratios_repair_invalid_entries() {
        let ratios: Vec<_> = effective_ratios(&[f32::NAN, -1.0, f32::INFINITY], 3).collect();
        assert!(ratios.iter().all(|ratio| ratio.is_finite() && *ratio > 0.0));
        assert!((ratios.iter().sum::<f32>() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_ratios_with_minimums_preserves_unconstrained_weight() {
        let split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            ratios: vec![0.6, 0.3, 0.1],
        };

        let ratios = split.ratios_with_minimums(100.0, &[10.0, 40.0, 1.0]);
        assert!((ratios[1] - 0.4).abs() < f32::EPSILON);
        assert!((ratios[0] / ratios[2] - 6.0).abs() < f32::EPSILON);
        assert!((ratios.iter().sum::<f32>() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_ratios_with_minimums_scales_undersized_extent_proportionally() {
        let split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            ratios: vec![0.2, 0.6, 0.2],
        };

        let ratios = split.ratios_with_minimums(50.0, &[40.0, 20.0, 40.0]);
        assert_eq!(ratios, vec![0.4, 0.2, 0.4]);
    }

    #[test]
    fn test_adjust_ratio_with_minimums_clamps_the_selected_pair() {
        let mut split = SplitNode::Split {
            split_id: None,
            orientation: SplitOrientation::Horizontal,
            children: vec![
                SplitNode::leaf("a".to_string()),
                SplitNode::leaf("b".to_string()),
                SplitNode::leaf("c".to_string()),
            ],
            ratios: vec![0.5, 0.3, 0.2],
        };

        assert!(split.adjust_ratio_with_minimums(&[], 0, 0.8, 100.0, &[30.0, 20.0, 10.0]));
        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert!((ratios[0] - 0.6).abs() < f32::EPSILON);
        assert!((ratios[1] - 0.2).abs() < f32::EPSILON);
        assert_eq!(ratios[2], 0.2);
        assert!(!split.adjust_ratio_with_minimums(&[], 0, 0.1, 100.0, &[30.0, 20.0, 10.0]));
    }

    #[test]
    fn test_keyboard_divider_minimum_and_maximum_respect_pixel_floors() {
        let mut split = SplitNode::split(
            SplitOrientation::Vertical,
            vec![SplitNode::leaf("a".into()), SplitNode::leaf("b".into())],
            vec![0.5, 0.5],
        );

        assert!(split.resize_divider_with_minimums(
            &[],
            0,
            DividerResize::Minimum,
            100.0,
            &[30.0, 20.0],
        ));
        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert!((ratios[0] - 0.3).abs() < f32::EPSILON);
        assert!((ratios[1] - 0.7).abs() < f32::EPSILON);

        assert!(split.resize_divider_with_minimums(
            &[],
            0,
            DividerResize::Maximum,
            100.0,
            &[30.0, 20.0],
        ));
        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert!((ratios[0] - 0.8).abs() < f32::EPSILON);
        assert!((ratios[1] - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_keyboard_divider_noops_when_extent_cannot_satisfy_minimums() {
        let mut split = SplitNode::split(
            SplitOrientation::Vertical,
            vec![SplitNode::leaf("a".into()), SplitNode::leaf("b".into())],
            vec![0.5, 0.5],
        );

        let bounds = split
            .divider_ratio_bounds_with_minimums(&[], 0, 40.0, &[30.0, 20.0])
            .expect("undersized dividers still expose their fixed position");
        assert!((bounds.value - 0.6).abs() < f32::EPSILON);
        assert_eq!(bounds.minimum, bounds.value);
        assert_eq!(bounds.maximum, bounds.value);

        assert!(!split.resize_divider_with_minimums(
            &[],
            0,
            DividerResize::Decrease,
            40.0,
            &[30.0, 20.0],
        ));
        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert_eq!(ratios, &vec![0.5, 0.5]);
    }

    #[test]
    fn test_keyboard_divider_equalizes_its_parent_split() {
        let mut split = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("a".into()),
                SplitNode::leaf("b".into()),
                SplitNode::leaf("c".into()),
            ],
            vec![0.1, 0.1, 0.8],
        );
        assert!(
            !split
                .divider_ratio_bounds_with_minimums(&[], 0, 100.0, &[10.0, 10.0, 10.0])
                .expect("valid split")
                .can_resize
        );

        assert!(split.resize_divider_with_minimums(
            &[],
            0,
            DividerResize::Equalize,
            100.0,
            &[10.0, 10.0, 10.0],
        ));
        let SplitNode::Split { ratios, .. } = &split else {
            panic!("expected split");
        };
        assert!(
            ratios
                .iter()
                .all(|ratio| (*ratio - 1.0 / 3.0).abs() < f32::EPSILON)
        );
        assert!(!split.resize_divider_with_minimums(
            &[],
            0,
            DividerResize::Equalize,
            100.0,
            &[10.0, 10.0, 10.0],
        ));
    }

    #[test]
    fn test_minimum_drag_retains_unapplied_pointer_movement() {
        let mut split = SplitNode::split(
            SplitOrientation::Vertical,
            vec![SplitNode::leaf("a".into()), SplitNode::leaf("b".into())],
            vec![0.5, 0.5],
        );
        let mut anchor = 200.0;
        for (pointer, expected_divider) in [(100.0, 120.0), (101.0, 120.0), (121.0, 121.0)] {
            let applied = split.adjust_ratio_with_minimums_applied(
                &[],
                0,
                (pointer - anchor) / 400.0,
                400.0,
                &[120.0, 120.0],
            );
            anchor += applied * 400.0;
            assert!((anchor - expected_divider).abs() < 0.0001);
            let shown = split.ratios_with_minimums(400.0, &[120.0, 120.0]);
            assert!((shown[0] * 400.0 - expected_divider).abs() < 0.0001);
        }
    }

    // ========================================================================
    // Placeholder Utility Tests
    // ========================================================================

    #[test]
    fn test_is_placeholder() {
        assert!(is_placeholder("__empty_0__"));
        assert!(is_placeholder("__empty_123__"));
        assert!(!is_placeholder("xqc"));
        assert!(!is_placeholder(""));
    }

    #[test]
    fn test_new_placeholder_unique() {
        let p1 = new_placeholder();
        let p2 = new_placeholder();
        assert!(is_placeholder(&p1));
        assert!(is_placeholder(&p2));
        assert_ne!(p1, p2);
    }
}
