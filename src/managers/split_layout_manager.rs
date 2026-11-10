//! Split Layout Manager - Extracted from app.rs monolith
//!
//! Handles:
//! - Split tree rendering (recursive binary tree)
//! - Resize drag operations (divider drag)
//! - Pane drag-drop (Ctrl+drag to reorder)
//! - Drop zone visualization
//! - Active split tracking
//! - Hit testing and bounds tracking
#![allow(clippy::wrong_self_convention)]

use crate::state::{AddSplitDirection, DragDropState, DropZone, SplitNode, SplitOrientation};
use gpui::*;
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

pub use echo_core::workspace::DividerResize;

/// A compact key for identifying split paths.
/// Uses inline storage for up to 8 levels without heap allocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PathKey {
    path: SmallVec<[usize; 8]>,
}

impl PathKey {
    /// Create a PathKey from a slice of usize indices
    pub fn from_path(path: &[usize]) -> Self {
        Self {
            path: path.iter().copied().collect(),
        }
    }

    /// Convert back to a Vec<usize> (for compatibility with existing code)
    pub fn to_vec(&self) -> Vec<usize> {
        self.path.to_vec()
    }

    /// Convert to a stable u64 hash for use in GPUI element IDs.
    #[inline]
    pub fn as_u64(&self) -> u64 {
        let mut hasher = ahash::AHasher::default();
        self.path.len().hash(&mut hasher);
        self.path.hash(&mut hasher);
        hasher.finish()
    }
}

impl std::fmt::Display for PathKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, idx) in self.path.iter().enumerate() {
            if i > 0 {
                write!(f, "-")?;
            }
            write!(f, "{idx}")?;
        }
        Ok(())
    }
}

/// Visual size of the split handle/divider (the thin line)
pub const HANDLE_VISUAL_SIZE: f32 = 1.0;

/// Extra padding on each side for easier hit detection
pub const HANDLE_HIT_PADDING: f32 = 12.0;

/// Total hit area size for the handle
pub const HANDLE_HIT_SIZE: f32 = HANDLE_VISUAL_SIZE + (HANDLE_HIT_PADDING * 2.0);

/// The keyboard and accessibility-facing state of one divider.
#[derive(Clone, Debug, PartialEq)]
pub struct DividerState {
    pub orientation: SplitOrientation,
    /// Position of the first child in the adjacent pair, from 0 through 100.
    pub value_percent: f32,
    pub min_percent: f32,
    pub max_percent: f32,
}

struct GridBandGeometry {
    orientation: SplitOrientation,
    parent_extent: f32,
    value: f32,
    minimum: f32,
    maximum: f32,
    band_minimums: [f32; 2],
    displayed_grid: SplitNode,
}

/// Drag state for split resizing
#[derive(Clone)]
pub struct SplitDragState {
    pub tab_id: String,
    /// Path to the parent split node being resized
    pub path: Vec<usize>,
    /// Index of the child whose right/bottom edge is being dragged
    pub child_idx: usize,
    /// Orientation of the split (determines drag direction)
    pub orientation: SplitOrientation,
    /// Pointer anchor, advanced only by movement the divider actually applied.
    pub start_pos: Point<Pixels>,
    /// Container size for ratio calculation
    pub container_size: f32,
}

/// State for dragging a split pane to reorder/move
#[derive(Clone)]
pub struct SplitPaneDrag {
    pub tab_id: String,
    /// Path to the split being dragged
    pub source_path: Vec<usize>,
    /// Channel name of the dragged split
    pub channel: String,
    /// Whether the drag was started by the Ctrl+drag shortcut.
    ///
    /// A move also starts by simply dragging the split header past a
    /// threshold, and such a drag must survive the modifier being released.
    pub ctrl_initiated: bool,
}

/// A header press that becomes a pane drag once the pointer moves far enough.
///
/// A press only becomes a drag after the pointer travels more than
/// [`PANE_DRAG_THRESHOLD_PX`].
#[derive(Clone)]
pub struct PendingPaneDrag {
    pub tab_id: String,
    pub source_path: Vec<usize>,
    pub channel: String,
    pub start_pos: Point<Pixels>,
}

/// Pointer travel required before a header press turns into a pane drag.
pub const PANE_DRAG_THRESHOLD_PX: f32 = 15.0;

/// Width of the band along each outer edge of the split area that drops a
/// dragged split across the whole layout instead of beside one split.
pub const ROOT_EDGE_DROP_BAND_PX: f32 = 24.0;

/// Drop target when dragging a split
#[derive(Clone)]
pub struct SplitDropTarget {
    /// Path to the target split
    pub target_path: Vec<usize>,
    /// Drop zone within the target
    pub zone: DropZone,
    /// The drop spans the whole layout along `zone` (a container edge drop);
    /// `target_path` is then the root.
    pub root_edge: bool,
}

/// Manager entity for split layout state and behavior
pub struct SplitLayoutManager {
    pub active_split_path: Vec<usize>,

    pub resize_drag: Option<SplitDragState>,

    pub pane_drag: Option<SplitPaneDrag>,
    pub pending_pane_drag: Option<PendingPaneDrag>,
    pub drop_target: Option<SplitDropTarget>,

    pub drag_drop: DragDropState,

    pub drag_mode_active: bool,
    pub add_split_mode_active: bool,
    /// Keep the add overlay hidden after it performs an action until Ctrl or
    /// Alt is physically released.
    add_split_mode_suppressed_until_release: bool,

    pub unified_input_mode: bool,

    /// Splits in moderation mode; keyed by id so tab switches and moves keep it.
    split_mod_mode: HashSet<crate::state::SplitId>,

    pub cursor_position: Point<Pixels>,
    /// Window-space bounds per split, tagged with the render pass that wrote
    /// them. Bounds are keyed by tree path, so a closed or moved split would
    /// otherwise leave an entry that still answers hit tests from a position it
    /// no longer occupies.
    pub split_hit_areas: HashMap<PathKey, (Bounds<Pixels>, u64)>,
    painted_leaves: HashMap<crate::state::SplitId, (PathKey, Bounds<Pixels>, u64)>,
    divider_focus_handles: HashMap<(crate::state::SplitId, crate::state::SplitId), FocusHandle>,
    /// Current split-tree render pass.
    pub hit_area_generation: u64,

    /// Split currently under the pointer.
    ///
    /// The add/move overlay shows only on the split the mouse is over, never
    /// on every split at once.
    pub hovered_split: Option<PathKey>,

    /// Add-split button currently hovered inside that overlay, which drives the
    /// half-pane drop preview.
    pub hovered_add_direction: Option<AddSplitDirection>,

    pub split_bounds: HashMap<PathKey, (f32, f32)>,
    split_minimums: HashMap<PathKey, (f32, f32)>,

    pub last_window_width: f32,
    pub last_split_count: usize,
}

impl SplitLayoutManager {
    pub fn new() -> Self {
        Self {
            active_split_path: vec![],
            resize_drag: None,
            pane_drag: None,
            pending_pane_drag: None,
            drop_target: None,
            drag_drop: DragDropState::default(),
            drag_mode_active: false,
            add_split_mode_active: false,
            add_split_mode_suppressed_until_release: false,
            unified_input_mode: crate::backend::settings::get_unified_input_mode(),
            split_mod_mode: HashSet::new(),
            cursor_position: Point::default(),
            split_hit_areas: HashMap::new(),
            painted_leaves: HashMap::new(),
            divider_focus_handles: HashMap::new(),
            hit_area_generation: 0,
            hovered_split: None,
            hovered_add_direction: None,
            split_bounds: HashMap::new(),
            split_minimums: HashMap::new(),
            last_window_width: 1280.0,
            last_split_count: 1,
        }
    }

    pub fn get_active_split_path(&self) -> &[usize] {
        &self.active_split_path
    }

    pub fn set_active_split(&mut self, path: Vec<usize>) {
        self.active_split_path = path;
    }

    /// Keep keyboard focus attached to the two panes a divider separates,
    /// rather than its transient tree path.
    pub fn divider_focus(
        &mut self,
        first: crate::state::SplitId,
        second: crate::state::SplitId,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        self.divider_focus_handles
            .entry((first, second))
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Rewrite every cached path after the 2x2 block at `block_path` rotated.
    ///
    /// Rotation maps the pane at `[i][j]` to `[j][i]` and touches nothing
    /// outside the block, so this is a permutation of the block's own paths:
    /// each key stays valid, it just describes the other pane now. Measured
    /// sizes and bounds are carried across with their pane because a rotation
    /// only runs when it leaves every pane exactly where it was painted.
    pub fn remap_after_grid_transpose(&mut self, block_path: &[usize]) {
        Self::remap_grid_path(block_path, &mut self.active_split_path);
        Self::remap_grid_keys(&mut self.split_bounds, block_path);
        Self::remap_grid_keys(&mut self.split_minimums, block_path);
        Self::remap_grid_keys(&mut self.split_hit_areas, block_path);
        for (path, _, _) in self.painted_leaves.values_mut() {
            *path = Self::remap_grid_key(block_path, path);
        }
        if let Some(hovered) = self.hovered_split.take() {
            self.hovered_split = Some(Self::remap_grid_key(block_path, &hovered));
        }
        if let Some(drag) = self.resize_drag.as_mut() {
            Self::remap_grid_path(block_path, &mut drag.path);
        }
    }

    /// Swap the two path components that address a pane inside the rotated
    /// block. Paths outside the block are left alone.
    fn remap_grid_path(block_path: &[usize], path: &mut [usize]) -> bool {
        let band = block_path.len();
        if path.len() < band + 2 || !path.starts_with(block_path) {
            return false;
        }
        path.swap(band, band + 1);
        true
    }

    fn remap_grid_key(block_path: &[usize], key: &PathKey) -> PathKey {
        let mut path = key.to_vec();
        Self::remap_grid_path(block_path, &mut path);
        PathKey::from_path(&path)
    }

    fn remap_grid_keys<V>(map: &mut HashMap<PathKey, V>, block_path: &[usize]) {
        let remapped: Vec<(PathKey, V)> = map
            .drain()
            .map(|(key, value)| (Self::remap_grid_key(block_path, &key), value))
            .collect();
        map.extend(remapped);
    }

    pub fn resize_drag(&self) -> Option<&SplitDragState> {
        self.resize_drag.as_ref()
    }

    pub fn pane_drag(&self) -> Option<&SplitPaneDrag> {
        self.pane_drag.as_ref()
    }

    pub fn drop_target(&self) -> Option<&SplitDropTarget> {
        self.drop_target.as_ref()
    }

    pub fn is_unified_input_mode(&self) -> bool {
        self.unified_input_mode
    }

    /// Update unified input mode from runtime settings.
    /// Returns true when the value actually changed.
    pub fn set_unified_input_mode(&mut self, enabled: bool) -> bool {
        if self.unified_input_mode != enabled {
            self.unified_input_mode = enabled;
            true
        } else {
            false
        }
    }

    /// Bounds for a split, provided the current or previous render pass wrote
    /// them. Anything older belongs to a layout that no longer exists.
    pub fn hit_area_for(&self, path_key: &PathKey) -> Option<Bounds<Pixels>> {
        self.split_hit_areas
            .get(path_key)
            .filter(|(_, generation)| self.is_live_generation(*generation))
            .map(|(bounds, _)| *bounds)
    }

    /// The bounds-registration pass runs while the map is being refilled, so the
    /// previous pass stays valid until the current one replaces it.
    fn is_live_generation(&self, generation: u64) -> bool {
        generation + 1 >= self.hit_area_generation
    }

    pub fn cursor_position(&self) -> Point<Pixels> {
        self.cursor_position
    }

    pub fn last_window_width(&self) -> f32 {
        self.last_window_width
    }

    /// Update modifier state and return true if it changed
    pub fn update_modifiers(&mut self, ctrl: bool, alt: bool) -> bool {
        let add_split_chord = ctrl && alt;
        if !add_split_chord {
            self.add_split_mode_suppressed_until_release = false;
        }
        let new_drag_mode = ctrl && !alt;
        let new_add_split_mode = add_split_chord && !self.add_split_mode_suppressed_until_release;

        let changed = self.drag_mode_active != new_drag_mode
            || self.add_split_mode_active != new_add_split_mode;

        self.drag_mode_active = new_drag_mode;
        self.add_split_mode_active = new_add_split_mode;

        if !new_add_split_mode {
            self.hovered_add_direction = None;
        }

        changed
    }

    /// Dismiss the Ctrl+Alt overlay after an action and prevent mouse movement
    /// from re-arming it before the initiating chord is released.
    pub fn dismiss_add_split_overlay_until_release(&mut self) -> bool {
        let changed = self.add_split_mode_active || self.hovered_add_direction.is_some();
        self.add_split_mode_suppressed_until_release = true;
        self.add_split_mode_active = false;
        self.hovered_add_direction = None;
        changed
    }

    pub fn is_drag_mode_active(&self) -> bool {
        self.drag_mode_active
    }

    pub fn is_add_split_mode_active(&self) -> bool {
        self.add_split_mode_active
    }

    pub fn start_resize_drag(&mut self, drag: SplitDragState) {
        self.resize_drag = Some(drag);
    }

    pub fn is_resizing(&self) -> bool {
        self.resize_drag.is_some()
    }

    pub fn start_pane_drag(
        &mut self,
        tab_id: String,
        source_path: Vec<usize>,
        channel: String,
        ctrl: bool,
    ) {
        self.pending_pane_drag = None;
        self.pane_drag = Some(SplitPaneDrag {
            tab_id,
            source_path: source_path.clone(),
            channel,
            ctrl_initiated: ctrl,
        });
        self.drag_drop.is_dragging = true;
        self.drag_drop.source_path = Some(source_path);
        self.drag_drop.hover_target = None;
    }

    /// Arm a header press so pointer travel can promote it into a pane drag.
    pub fn arm_pane_drag(
        &mut self,
        tab_id: String,
        source_path: Vec<usize>,
        channel: String,
        start_pos: Point<Pixels>,
    ) {
        self.pending_pane_drag = Some(PendingPaneDrag {
            tab_id,
            source_path,
            channel,
            start_pos,
        });
    }

    pub fn clear_pending_pane_drag(&mut self) {
        self.pending_pane_drag = None;
    }

    /// Promote an armed header press once the pointer passes the 15px travel
    /// threshold. Returns true when a drag actually started.
    pub fn promote_pending_pane_drag(&mut self, pos: Point<Pixels>) -> bool {
        let Some(pending) = self.pending_pane_drag.as_ref() else {
            return false;
        };

        let dx: f32 = (pos.x - pending.start_pos.x).into();
        let dy: f32 = (pos.y - pending.start_pos.y).into();
        if dx.hypot(dy) <= PANE_DRAG_THRESHOLD_PX {
            return false;
        }

        let pending = self
            .pending_pane_drag
            .take()
            .expect("pending drag checked above");
        self.start_pane_drag(pending.tab_id, pending.source_path, pending.channel, false);
        true
    }

    /// Whether a running pane drag should end when Ctrl is released.
    ///
    /// Only Ctrl+drag is modifier-scoped; a header drag keeps running until
    /// the button is released.
    pub fn pane_drag_needs_ctrl(&self) -> bool {
        self.pane_drag
            .as_ref()
            .is_some_and(|drag| drag.ctrl_initiated)
    }

    pub fn update_pane_drop_target(&mut self, pos: Point<Pixels>) {
        self.cursor_position = pos;

        if let Some(zone) = self.root_edge_zone_at(pos) {
            self.drop_target = Some(SplitDropTarget {
                target_path: Vec::new(),
                zone,
                root_edge: true,
            });
            self.drag_drop.hover_target = Some((Vec::new(), zone));
            return;
        }

        for (path_key, (bounds, generation)) in &self.split_hit_areas {
            if self.is_live_generation(*generation) && bounds.contains(&pos) {
                let target_path = path_key.to_vec();

                if let Some(ref drag) = self.pane_drag
                    && drag.source_path == target_path
                {
                    continue;
                }

                let zone = Self::calculate_drop_zone(pos, bounds);

                self.drop_target = Some(SplitDropTarget {
                    target_path,
                    zone,
                    root_edge: false,
                });
                self.drag_drop.hover_target = self
                    .drop_target
                    .as_ref()
                    .map(|t| (t.target_path.clone(), t.zone));
                return;
            }
        }

        self.drop_target = None;
    }

    /// Bounds of the whole split area: the union of the live pane hit areas.
    pub fn split_area_bounds(&self) -> Option<Bounds<Pixels>> {
        let mut live = self
            .split_hit_areas
            .values()
            .filter(|(_, generation)| self.is_live_generation(*generation))
            .map(|(bounds, _)| *bounds);
        let first = live.next()?;
        Some(live.fold(first, |union, bounds| union.union(&bounds)))
    }

    /// The outer edge whose drop band contains `pos`, while a pane drag is
    /// live and the layout has another pane to span across.
    fn root_edge_zone_at(&self, pos: Point<Pixels>) -> Option<DropZone> {
        let drag = self.pane_drag.as_ref()?;
        if drag.source_path.is_empty() {
            return None;
        }
        let area = self.split_area_bounds()?;
        if !area.contains(&pos) {
            return None;
        }
        let width: f32 = area.size.width.into();
        let height: f32 = area.size.height.into();
        let band_x = ROOT_EDGE_DROP_BAND_PX.min(width * 0.2);
        let band_y = ROOT_EDGE_DROP_BAND_PX.min(height * 0.2);
        let left: f32 = (pos.x - area.origin.x).into();
        let top: f32 = (pos.y - area.origin.y).into();
        let distances = [
            (top, band_y, DropZone::Top),
            (height - top, band_y, DropZone::Bottom),
            (left, band_x, DropZone::Left),
            (width - left, band_x, DropZone::Right),
        ];
        distances
            .into_iter()
            .filter(|(distance, band, _)| distance < band)
            .min_by(|a, b| (a.0 / a.1).total_cmp(&(b.0 / b.1)))
            .map(|(_, _, zone)| zone)
    }

    /// Where a root-edge drop would land, in window coordinates: a quarter of
    /// the split area along the hovered edge, spanning it fully.
    pub fn root_edge_preview(&self) -> Option<Bounds<Pixels>> {
        let target = self.drop_target.as_ref().filter(|t| t.root_edge)?;
        let area = self.split_area_bounds()?;
        let width = area.size.width;
        let height = area.size.height;
        let (origin, size) = match target.zone {
            DropZone::Top => (area.origin, size(width, height * 0.25)),
            DropZone::Bottom => (
                point(area.origin.x, area.origin.y + height * 0.75),
                size(width, height * 0.25),
            ),
            DropZone::Left => (area.origin, size(width * 0.25, height)),
            DropZone::Right => (
                point(area.origin.x + width * 0.75, area.origin.y),
                size(width * 0.25, height),
            ),
            DropZone::Center => return None,
        };
        Some(Bounds { origin, size })
    }

    pub fn end_pane_drag(&mut self) -> Option<(SplitPaneDrag, SplitDropTarget)> {
        let drag = self.pane_drag.take()?;
        self.drag_drop = DragDropState::default();
        let target = self.drop_target.take()?;
        Some((drag, target))
    }

    pub fn cancel_pane_drag(&mut self) {
        self.pane_drag = None;
        self.pending_pane_drag = None;
        self.drop_target = None;
        self.drag_drop = DragDropState::default();
    }

    pub fn is_dragging_pane(&self) -> bool {
        self.pane_drag.is_some()
    }

    pub fn register_split_bounds(&mut self, path_key: PathKey, bounds: Bounds<Pixels>) {
        self.split_hit_areas
            .insert(path_key, (bounds, self.hit_area_generation));
    }

    pub fn register_leaf_bounds(
        &mut self,
        split_id: Option<crate::state::SplitId>,
        path: PathKey,
        bounds: Bounds<Pixels>,
    ) {
        if let Some(split_id) = split_id {
            self.painted_leaves
                .insert(split_id, (path.clone(), bounds, self.hit_area_generation));
        }
        self.register_split_bounds(path, bounds);
    }

    pub fn painted_leaf_bounds(&self, root: &SplitNode) -> Option<Vec<(Vec<usize>, [f32; 4])>> {
        fn collect(
            manager: &SplitLayoutManager,
            node: &SplitNode,
            path: &mut Vec<usize>,
            result: &mut Vec<(Vec<usize>, [f32; 4])>,
        ) -> Option<()> {
            match node {
                SplitNode::Leaf { split_id, .. } => {
                    let (painted_path, bounds, generation) =
                        manager.painted_leaves.get(&(*split_id)?)?;
                    if painted_path.path.as_slice() != path.as_slice()
                        || *generation != manager.hit_area_generation
                    {
                        return None;
                    }
                    result.push((
                        path.clone(),
                        [
                            bounds.origin.x.into(),
                            bounds.origin.y.into(),
                            bounds.right().into(),
                            bounds.bottom().into(),
                        ],
                    ));
                }
                SplitNode::Split { children, .. } => {
                    for (idx, child) in children.iter().enumerate() {
                        path.push(idx);
                        collect(manager, child, path, result)?;
                        path.pop();
                    }
                }
            }
            Some(())
        }
        let mut result = Vec::new();
        collect(self, root, &mut Vec::new(), &mut result)?;
        Some(result)
    }

    /// Open a new bounds-registration pass and expire anything that missed the
    /// previous one, so a closed or moved split stops answering hit tests.
    ///
    /// The map is deliberately not cleared: hover has to stay stable while the
    /// pass is refilling it, or the transient miss would drop overlay state.
    pub fn begin_hit_area_pass(&mut self) {
        self.hit_area_generation = self.hit_area_generation.wrapping_add(1);
        let generation = self.hit_area_generation;
        self.split_hit_areas
            .retain(|_, (_, written)| *written + 1 >= generation);
        self.painted_leaves
            .retain(|_, (_, _, written)| written.wrapping_add(1) >= generation);
        let painted_leaves = &self.painted_leaves;
        self.divider_focus_handles.retain(|(first, second), _| {
            painted_leaves.contains_key(first) && painted_leaves.contains_key(second)
        });
    }

    /// Resolve which split the pointer is over from the registered hit areas.
    /// Returns true when the hovered split changed, so callers can redraw.
    pub fn recompute_hovered_split(&mut self) -> bool {
        let pos = self.cursor_position;
        let hovered = self
            .split_hit_areas
            .iter()
            .find(|(_, (bounds, generation))| {
                self.is_live_generation(*generation) && bounds.contains(&pos)
            })
            .map(|(path_key, _)| path_key.clone());

        if self.hovered_split == hovered {
            return false;
        }
        self.hovered_split = hovered;
        self.hovered_add_direction = None;
        true
    }

    pub fn hovered_split(&self) -> Option<&PathKey> {
        self.hovered_split.as_ref()
    }

    pub fn hovered_add_direction(&self) -> Option<AddSplitDirection> {
        self.hovered_add_direction
    }

    /// Record which add-split button is hovered. Returns true when it changed.
    pub fn set_hovered_add_direction(&mut self, direction: Option<AddSplitDirection>) -> bool {
        if self.hovered_add_direction == direction {
            return false;
        }
        self.hovered_add_direction = direction;
        true
    }

    /// Drop a stale hit area (a closed or moved split must stop being hovered).
    pub fn forget_split_bounds(&mut self, path_key: &PathKey) {
        self.split_hit_areas.remove(path_key);
        self.painted_leaves
            .retain(|_, (path, _, _)| path != path_key);
        if self.hovered_split.as_ref() == Some(path_key) {
            self.hovered_split = None;
        }
    }

    pub fn register_split_size(&mut self, path_key: PathKey, width: f32, height: f32) {
        const EPS: f32 = 0.5;
        if let Some((prev_w, prev_h)) = self.split_bounds.get(&path_key).copied()
            && (prev_w - width).abs() < EPS
            && (prev_h - height).abs() < EPS
        {
            return;
        }
        self.split_bounds.insert(path_key, (width, height));
    }

    pub fn update_split_minimums(
        &mut self,
        root: &SplitNode,
        mut leaf_size: impl FnMut(Option<crate::state::SplitId>, &str, bool) -> (f32, f32),
    ) {
        fn collect(
            node: &SplitNode,
            path: &mut Vec<usize>,
            minimums: &mut HashMap<PathKey, (f32, f32)>,
            leaf_size: &mut impl FnMut(Option<crate::state::SplitId>, &str, bool) -> (f32, f32),
        ) -> (f32, f32) {
            let size = match node {
                SplitNode::Leaf {
                    split_id,
                    channel,
                    header_collapsed,
                    ..
                } => leaf_size(*split_id, channel, *header_collapsed),
                SplitNode::Split {
                    orientation,
                    children,
                    ..
                } => {
                    let mut size = (0.0_f32, 0.0_f32);
                    for (idx, child) in children.iter().enumerate() {
                        path.push(idx);
                        let child_size = collect(child, path, minimums, leaf_size);
                        path.pop();
                        match orientation {
                            SplitOrientation::Vertical => {
                                size.0 += child_size.0;
                                size.1 = size.1.max(child_size.1);
                            }
                            SplitOrientation::Horizontal => {
                                size.0 = size.0.max(child_size.0);
                                size.1 += child_size.1;
                            }
                        }
                    }
                    let dividers = children.len().saturating_sub(1) as f32 * HANDLE_VISUAL_SIZE;
                    match orientation {
                        SplitOrientation::Vertical => size.0 += dividers,
                        SplitOrientation::Horizontal => size.1 += dividers,
                    }
                    size
                }
            };
            minimums.insert(PathKey::from_path(path), size);
            size
        }

        self.split_minimums.clear();
        collect(
            root,
            &mut Vec::new(),
            &mut self.split_minimums,
            &mut leaf_size,
        );
    }

    pub fn child_minimum_extents(
        &self,
        path: &[usize],
        child_count: usize,
        orientation: &SplitOrientation,
    ) -> Vec<f32> {
        let mut child_path = PathKey::from_path(path);
        child_path.path.push(0);
        (0..child_count)
            .map(|idx| {
                *child_path.path.last_mut().unwrap() = idx;
                let size = self
                    .split_minimums
                    .get(&child_path)
                    .copied()
                    .unwrap_or_default();
                match orientation {
                    SplitOrientation::Vertical => size.0,
                    SplitOrientation::Horizontal => size.1,
                }
            })
            .collect()
    }

    fn divider_inputs(
        &self,
        root: &SplitNode,
        path: &[usize],
        child_idx: usize,
    ) -> Option<(SplitOrientation, f32, Vec<f32>)> {
        let SplitNode::Split {
            orientation,
            children,
            ..
        } = root.get_node_at_path(path)?
        else {
            return None;
        };
        if child_idx >= children.len().saturating_sub(1) {
            return None;
        }

        let (width, height) = self.split_bounds.get(&Self::path_key(path)).copied()?;
        let full_extent = match orientation {
            SplitOrientation::Vertical => width,
            SplitOrientation::Horizontal => height,
        };
        let extent = full_extent - children.len().saturating_sub(1) as f32 * HANDLE_VISUAL_SIZE;
        if !extent.is_finite() || extent <= 0.0 {
            return None;
        }

        let minimums = self.child_minimum_extents(path, children.len(), orientation);
        Some((orientation.clone(), extent, minimums))
    }

    /// Return the current accessible range for one divider, including the
    /// pixel floors already used by pointer resizing.
    pub fn divider_state(
        &self,
        root: &SplitNode,
        path: &[usize],
        child_idx: usize,
    ) -> Option<DividerState> {
        let (orientation, extent, minimums) = self.divider_inputs(root, path, child_idx)?;
        let bounds = root.divider_ratio_bounds_with_minimums(path, child_idx, extent, &minimums)?;
        Some(DividerState {
            orientation,
            value_percent: bounds.value * 100.0,
            min_percent: bounds.minimum * 100.0,
            max_percent: bounds.maximum * 100.0,
        })
    }

    /// Apply a keyboard divider operation using the currently painted extent.
    /// Returns false for stale paths, unavailable geometry, or a no-op clamp.
    pub fn resize_divider(
        &self,
        root: &mut SplitNode,
        path: &[usize],
        child_idx: usize,
        action: DividerResize,
    ) -> bool {
        let (_, extent, minimums) = match self.divider_inputs(root, path, child_idx) {
            Some(inputs) => inputs,
            None => return false,
        };
        root.resize_divider_with_minimums(path, child_idx, action, extent, &minimums)
    }

    fn grid_band_geometry(
        &self,
        root: &SplitNode,
        path: &[usize],
        band_idx: usize,
    ) -> Option<GridBandGeometry> {
        let (orientation, parent_extent, parent_minimums) = self.divider_inputs(root, path, 0)?;
        let node = root.get_node_at_path(path)?;
        if band_idx >= 2 || node.grid_band_split_ratio().is_none() {
            return None;
        }
        let SplitNode::Split { children, .. } = node else {
            return None;
        };
        if children.len() != 2 {
            return None;
        }

        let (width, height) = self.split_bounds.get(&Self::path_key(path)).copied()?;
        let displayed_parent = node.ratios_with_minimums(parent_extent, &parent_minimums);
        if displayed_parent.len() != 2 {
            return None;
        }

        let mut displayed_children = Vec::with_capacity(2);
        for (side, child) in children.iter().enumerate() {
            let SplitNode::Split {
                orientation: child_orientation,
                children: grandchildren,
                ..
            } = child
            else {
                return None;
            };
            if grandchildren.len() != 2 {
                return None;
            }

            let child_extent = match child_orientation {
                SplitOrientation::Vertical => width,
                SplitOrientation::Horizontal => height,
            } - HANDLE_VISUAL_SIZE;
            if !child_extent.is_finite() || child_extent <= 0.0 {
                return None;
            }
            let mut child_path = path.to_vec();
            child_path.push(side);
            let minimums = self.child_minimum_extents(&child_path, 2, child_orientation);
            let displayed = child.ratios_with_minimums(child_extent, &minimums);
            if displayed.len() != 2 {
                return None;
            }
            displayed_children.push(displayed);
        }
        if (displayed_children[0][0] - displayed_children[1][0]).abs() >= 0.004 {
            return None;
        }

        let mut first_leaf_path = path.to_vec();
        first_leaf_path.extend([0, band_idx]);
        let mut second_leaf_path = path.to_vec();
        second_leaf_path.extend([1, band_idx]);
        let band_minimums = [
            self.minimum_extent(&first_leaf_path, &orientation),
            self.minimum_extent(&second_leaf_path, &orientation),
        ];
        if band_minimums
            .iter()
            .any(|minimum| !minimum.is_finite() || *minimum < 0.0)
        {
            return None;
        }

        let value = displayed_parent[0].clamp(0.0, 1.0);
        let parent_minimum_total: f32 = parent_minimums.iter().sum();
        let band_minimum_total = band_minimums.iter().sum::<f32>();
        if !parent_minimum_total.is_finite() || !band_minimum_total.is_finite() {
            return None;
        }
        let (minimum, maximum) =
            if parent_minimum_total >= parent_extent || band_minimum_total >= parent_extent {
                (value, value)
            } else {
                (
                    (band_minimums[0] / parent_extent).clamp(0.0, 1.0),
                    (1.0 - band_minimums[1] / parent_extent).clamp(0.0, 1.0),
                )
            };
        if maximum < minimum {
            return None;
        }

        let mut displayed_grid = node.clone();
        let SplitNode::Split {
            ratios, children, ..
        } = &mut displayed_grid
        else {
            return None;
        };
        *ratios = displayed_parent;
        for (child, displayed_ratios) in children.iter_mut().zip(displayed_children) {
            let SplitNode::Split { ratios, .. } = child else {
                return None;
            };
            *ratios = displayed_ratios;
        }

        Some(GridBandGeometry {
            orientation,
            parent_extent,
            value: value.clamp(minimum, maximum),
            minimum,
            maximum,
            band_minimums,
            displayed_grid,
        })
    }

    fn minimum_extent(&self, path: &[usize], orientation: &SplitOrientation) -> f32 {
        let (width, height) = self
            .split_minimums
            .get(&Self::path_key(path))
            .copied()
            .unwrap_or_default();
        match orientation {
            SplitOrientation::Vertical => width,
            SplitOrientation::Horizontal => height,
        }
    }

    /// State for one row or column band of a 2x2 grid's shared divider.
    pub fn divider_band_state(
        &self,
        root: &SplitNode,
        path: &[usize],
        band_idx: usize,
    ) -> Option<DividerState> {
        let geometry = self.grid_band_geometry(root, path, band_idx)?;
        Some(DividerState {
            orientation: geometry.orientation,
            value_percent: geometry.value * 100.0,
            min_percent: geometry.minimum * 100.0,
            max_percent: geometry.maximum * 100.0,
        })
    }

    /// Painted cross-axis extents for the two independently resizable bands
    /// of an aligned 2x2 grid divider.
    pub fn divider_band_extents(
        &self,
        root: &SplitNode,
        path: &[usize],
    ) -> Option<(SplitOrientation, [f32; 2])> {
        let geometry = self.grid_band_geometry(root, path, 0)?;
        let ratio = geometry.displayed_grid.grid_band_split_ratio()?;
        let (width, height) = self.split_bounds.get(&Self::path_key(path)).copied()?;
        let extent = match &geometry.orientation {
            SplitOrientation::Vertical => height,
            SplitOrientation::Horizontal => width,
        };
        if !extent.is_finite() || extent <= 0.0 {
            return None;
        }
        let first = (extent * ratio).clamp(0.0, extent);
        Some((geometry.orientation, [first, extent - first]))
    }

    /// Resize one band of a 2x2 grid by rotating it into an ordinary split.
    ///
    /// The probe prevents a no-op action from changing the grid's orientation.
    pub fn resize_grid_band(
        &mut self,
        tab: &mut crate::state::TabState,
        path: &[usize],
        band_idx: usize,
        action: DividerResize,
    ) -> bool {
        if tab.is_locked {
            return false;
        }
        let Some(geometry) = self.grid_band_geometry(&tab.layout, path, band_idx) else {
            return false;
        };
        if geometry.maximum <= geometry.minimum {
            return false;
        }
        let mut divider_path = path.to_vec();
        divider_path.push(band_idx);

        let mut probe = (*tab.layout).clone();
        if !probe.set_node_at_path(path, geometry.displayed_grid.clone())
            || !probe.transpose_2x2_at_path(path)
            || !probe.resize_divider_with_minimums(
                &divider_path,
                0,
                action,
                geometry.parent_extent,
                &geometry.band_minimums,
            )
        {
            return false;
        }

        if !std::sync::Arc::make_mut(&mut tab.layout)
            .set_node_at_path(path, geometry.displayed_grid)
        {
            return false;
        }
        if !tab.transpose_grid_block(path) {
            return false;
        }
        self.remap_after_grid_transpose(path);
        std::sync::Arc::make_mut(&mut tab.layout).resize_divider_with_minimums(
            &divider_path,
            0,
            action,
            geometry.parent_extent,
            &geometry.band_minimums,
        )
    }

    /// Width most recently laid out for the split at `path`, falling back to an
    /// even share of the window when the pane has not registered a size yet.
    ///
    /// Callers that already know the width they are laying a pane out at should
    /// use that directly; this exists for the composer paths that render outside
    /// a split leaf and therefore have no width in hand.
    pub fn pane_width_for_path(&self, path: &[usize]) -> f32 {
        self.split_bounds
            .get(&Self::path_key(path))
            .map(|(width, _)| *width)
            .unwrap_or_else(|| self.last_window_width / self.last_split_count.max(1) as f32)
    }

    pub fn update_cursor_position(&mut self, pos: Point<Pixels>) {
        self.cursor_position = pos;
    }

    /// Flip moderation mode for `split_id` and return the new state.
    pub fn toggle_mod_mode(&mut self, split_id: crate::state::SplitId) -> bool {
        if self.split_mod_mode.remove(&split_id) {
            false
        } else {
            self.split_mod_mode.insert(split_id);
            true
        }
    }

    pub fn is_mod_mode(&self, split_id: crate::state::SplitId) -> bool {
        self.split_mod_mode.contains(&split_id)
    }

    pub fn retain_mod_mode(&mut self, valid_ids: &HashSet<crate::state::SplitId>) {
        self.split_mod_mode
            .retain(|split_id| valid_ids.contains(split_id));
    }

    /// Record the current window width and split count.
    ///
    /// Resize and split-count changes deliberately do NOT invalidate layout
    /// caches: `LayoutCacheKey` includes the quantized container width, so
    /// entries built for other widths are simply never hit again and age out
    /// via the global cache's eviction. Clearing here would cold-start every
    /// split's layouts on each 10px resize tick.
    pub fn track_window_layout(&mut self, window_width: f32, split_count: usize) {
        self.last_window_width = window_width;
        self.last_split_count = split_count;
    }

    /// Create a PathKey from a path slice (zero-allocation)
    pub fn path_key(path: &[usize]) -> PathKey {
        PathKey::from_path(path)
    }

    pub fn calculate_drop_zone(pos: Point<Pixels>, bounds: &Bounds<Pixels>) -> DropZone {
        let relative_x: f32 = (pos.x - bounds.origin.x).into();
        let relative_y: f32 = (pos.y - bounds.origin.y).into();
        let width: f32 = bounds.size.width.into();
        let height: f32 = bounds.size.height.into();

        if width == 0.0 || height == 0.0 {
            return DropZone::Center;
        }

        let left_pct = relative_x / width;
        let right_pct = 1.0 - left_pct;
        let top_pct = relative_y / height;
        let bottom_pct = 1.0 - top_pct;

        let edge_threshold = 0.25;

        if left_pct < edge_threshold && left_pct <= top_pct && left_pct <= bottom_pct {
            DropZone::Left
        } else if right_pct < edge_threshold && right_pct <= top_pct && right_pct <= bottom_pct {
            DropZone::Right
        } else if top_pct < edge_threshold {
            DropZone::Top
        } else if bottom_pct < edge_threshold {
            DropZone::Bottom
        } else {
            DropZone::Center
        }
    }

    /// Render drop zone overlay when dragging over a split
    ///
    /// Creates a visual overlay showing where content will be placed.
    /// Includes directional arrows and clear zone indicators.
    pub fn render_drop_zone_overlay(
        zone: Option<DropZone>,
        width: f32,
        height: f32,
    ) -> impl IntoElement {
        use crate::theme::colors;

        let zone = zone.unwrap_or(DropZone::Center);
        let accent = colors::accent();

        let (left, top, w, h) = match zone {
            DropZone::Top => (0.0, 0.0, width, height * 0.25),
            DropZone::Bottom => (0.0, height * 0.75, width, height * 0.25),
            DropZone::Left => (0.0, 0.0, width * 0.25, height),
            DropZone::Right => (width * 0.75, 0.0, width * 0.25, height),
            DropZone::Center => (0.0, 0.0, width, height),
        };

        let bg_opacity = if zone == DropZone::Center { 0.12 } else { 0.20 };
        let border_width = if zone == DropZone::Center { 1.0 } else { 2.0 };

        let (arrow, label) = match zone {
            DropZone::Top => ("↑", "Split Above"),
            DropZone::Bottom => ("↓", "Split Below"),
            DropZone::Left => ("←", "Split Left"),
            DropZone::Right => ("→", "Split Right"),
            DropZone::Center => ("⇄", "Swap"),
        };

        div()
            .id("drop-zone-overlay")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(w))
            .h(px(h))
            .bg(accent.opacity(bg_opacity))
            .border(px(border_width))
            .border_color(accent.opacity(0.8))
            .rounded_md()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(accent)
                    .child(arrow),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_on_accent_contrast())
                    .bg(accent.opacity(0.85))
                    .px_2()
                    .py(px(2.0))
                    .rounded_sm()
                    .shadow_sm()
                    .child(label),
            )
    }

    /// Scrim painted behind the Ctrl+Alt split overlay.
    ///
    /// Fills the split with `rgba(0,0,0,150)` on dark themes and
    /// `rgba(255,255,255,200)` on light ones.
    pub fn split_overlay_scrim() -> Hsla {
        use crate::theme::colors;

        if colors::is_light_surface() {
            gpui::white().opacity(200.0 / 255.0)
        } else {
            gpui::black().opacity(150.0 / 255.0)
        }
    }

    /// Half of the split that a directional add-split button would occupy.
    ///
    /// This is the preview rect painted while that button is hovered.
    pub fn add_split_preview_rect(
        direction: AddSplitDirection,
        width: f32,
        height: f32,
    ) -> (f32, f32, f32, f32) {
        match direction {
            AddSplitDirection::Left => (0.0, 0.0, width / 2.0, height),
            AddSplitDirection::Right => (width / 2.0, 0.0, width / 2.0, height),
            AddSplitDirection::Top => (0.0, 0.0, width, height / 2.0),
            AddSplitDirection::Bottom => (0.0, height / 2.0, width, height / 2.0),
        }
    }

    /// Render drag ghost at cursor position
    pub fn render_drag_ghost(cursor: Point<Pixels>, channel: &str) -> impl IntoElement + use<> {
        use crate::theme::colors;

        div()
            .id("drag-ghost")
            .absolute()
            .left(cursor.x - px(60.0))
            .top(cursor.y - px(15.0))
            .w(px(120.0))
            .h(px(30.0))
            .bg(colors::bg_elevated().opacity(0.9))
            .border_2()
            .border_color(colors::accent())
            .rounded_md()
            .shadow_lg()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child(channel.to_string()),
            )
    }
}

impl Default for SplitLayoutManager {
    fn default() -> Self {
        Self::new()
    }
}
