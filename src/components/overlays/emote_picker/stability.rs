//! Benchmark-only stationary-viewport stability telemetry for the emote picker.
//!
//! The large-catalog sweep proves that virtual rows can be reached, but row
//! coverage alone cannot detect a decoded image that repeatedly falls back to
//! its placeholder or an advertised animation that remains frozen while the
//! viewport is not moving. This module records those visual-state boundaries
//! without depending on a particular cache implementation. It is dormant
//! unless the benchmark driver explicitly opens a stationary window with
//! [`begin_stationary_window`].

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Instant;

/// The user-visible state selected for one emote cell during a picker render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerCellVisualState {
    Rendered,
    Placeholder,
}

#[derive(Clone, Copy, Debug)]
struct CellHistory {
    last: PickerCellVisualState,
    ever_rendered: bool,
}

#[derive(Clone, Copy, Debug)]
struct AnimatedCellHistory {
    last_source_key: u64,
    last_frame_index: u16,
    frame_transitions: u64,
    last_transition_at: Instant,
    maximum_transition_stall_ms: u128,
}

#[derive(Debug)]
struct PickerStabilityWindow {
    started_at: Instant,
    expected_visible_cells: usize,
    expected_visible_animated_cells: usize,
    visible_start: usize,
    visible_end: usize,
    range_observations: u64,
    visible_range_changes: u64,
    cell_observations: u64,
    rendered_observations: u64,
    placeholder_observations: u64,
    rendered_to_placeholder_transitions: u64,
    placeholder_to_rendered_transitions: u64,
    post_rendered_placeholder_observations: u64,
    animated_frame_observations: u64,
    animated_frame_transitions: u64,
    animated_source_switches: u64,
    cells: HashMap<usize, CellHistory>,
    animated_cells: HashMap<usize, AnimatedCellHistory>,
}

impl PickerStabilityWindow {
    fn new(
        expected_visible_cells: usize,
        expected_visible_animated_cells: usize,
        visible_start: usize,
        visible_end: usize,
    ) -> Self {
        Self {
            started_at: Instant::now(),
            expected_visible_cells,
            expected_visible_animated_cells,
            visible_start,
            visible_end,
            range_observations: 0,
            visible_range_changes: 0,
            cell_observations: 0,
            rendered_observations: 0,
            placeholder_observations: 0,
            rendered_to_placeholder_transitions: 0,
            placeholder_to_rendered_transitions: 0,
            post_rendered_placeholder_observations: 0,
            animated_frame_observations: 0,
            animated_frame_transitions: 0,
            animated_source_switches: 0,
            cells: HashMap::with_capacity(expected_visible_cells),
            animated_cells: HashMap::with_capacity(expected_visible_animated_cells),
        }
    }

    fn snapshot(&self) -> PickerStabilitySnapshot {
        let rendered_unique_cells = self
            .cells
            .values()
            .filter(|history| history.ever_rendered)
            .count();
        let final_placeholder_cells = self
            .cells
            .values()
            .filter(|history| history.last == PickerCellVisualState::Placeholder)
            .count();
        let advanced_animated_cells = self
            .animated_cells
            .values()
            .filter(|history| history.frame_transitions > 0)
            .count();
        let minimum_animated_cell_transitions = if self.expected_visible_animated_cells > 0
            && self.animated_cells.len() == self.expected_visible_animated_cells
        {
            self.animated_cells
                .values()
                .map(|history| history.frame_transitions)
                .min()
                .unwrap_or(0)
        } else {
            0
        };
        let now = Instant::now();
        let maximum_animated_cell_stall_ms = if self.expected_visible_animated_cells > 0
            && self.animated_cells.len() == self.expected_visible_animated_cells
        {
            self.animated_cells
                .values()
                .map(|history| {
                    history
                        .maximum_transition_stall_ms
                        .max(now.duration_since(history.last_transition_at).as_millis())
                })
                .max()
                .unwrap_or(0)
        } else {
            self.started_at.elapsed().as_millis()
        };
        let mut advanced_animated_cell_keys = self
            .animated_cells
            .iter()
            .filter_map(|(cell_key, history)| (history.frame_transitions > 0).then_some(*cell_key))
            .collect::<Vec<_>>();
        advanced_animated_cell_keys.sort_unstable();

        PickerStabilitySnapshot {
            elapsed_ms: self.started_at.elapsed().as_millis(),
            expected_visible_cells: self.expected_visible_cells,
            expected_visible_animated_cells: self.expected_visible_animated_cells,
            visible_start: self.visible_start,
            visible_end: self.visible_end,
            range_observations: self.range_observations,
            visible_range_changes: self.visible_range_changes,
            cell_observations: self.cell_observations,
            rendered_observations: self.rendered_observations,
            placeholder_observations: self.placeholder_observations,
            unique_cells: self.cells.len(),
            rendered_unique_cells,
            final_placeholder_cells,
            rendered_to_placeholder_transitions: self.rendered_to_placeholder_transitions,
            placeholder_to_rendered_transitions: self.placeholder_to_rendered_transitions,
            post_rendered_placeholder_observations: self.post_rendered_placeholder_observations,
            metadata_ready_animated_cells: self.animated_cells.len(),
            advanced_animated_cells,
            animated_frame_observations: self.animated_frame_observations,
            animated_frame_transitions: self.animated_frame_transitions,
            animated_source_switches: self.animated_source_switches,
            minimum_animated_cell_transitions,
            maximum_animated_cell_stall_ms,
            advanced_animated_cell_keys,
        }
    }
}

/// Aggregate for one settled, stationary picker viewport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerStabilitySnapshot {
    pub elapsed_ms: u128,
    pub expected_visible_cells: usize,
    pub expected_visible_animated_cells: usize,
    pub visible_start: usize,
    pub visible_end: usize,
    pub range_observations: u64,
    pub visible_range_changes: u64,
    pub cell_observations: u64,
    pub rendered_observations: u64,
    pub placeholder_observations: u64,
    pub unique_cells: usize,
    pub rendered_unique_cells: usize,
    pub final_placeholder_cells: usize,
    pub rendered_to_placeholder_transitions: u64,
    pub placeholder_to_rendered_transitions: u64,
    pub post_rendered_placeholder_observations: u64,
    pub metadata_ready_animated_cells: usize,
    pub advanced_animated_cells: usize,
    pub animated_frame_observations: u64,
    pub animated_frame_transitions: u64,
    pub animated_source_switches: u64,
    pub minimum_animated_cell_transitions: u64,
    pub maximum_animated_cell_stall_ms: u128,
    advanced_animated_cell_keys: Vec<usize>,
}

impl PickerStabilitySnapshot {
    /// Whether this cell changed between actual resident frame indices while
    /// the stationary viewport was being observed.
    pub fn animated_cell_advanced(&self, cell_key: usize) -> bool {
        self.advanced_animated_cell_keys
            .binary_search(&cell_key)
            .is_ok()
    }

    /// A stable window must repeatedly observe the same viewport, see every
    /// expected cell render at least once, finish with no placeholders, never
    /// regress a rendered cell, and visibly advance every advertised animation.
    pub fn is_stable(&self, minimum_range_observations: u64) -> bool {
        self.expected_visible_cells > 0
            && self.range_observations >= minimum_range_observations
            && self.visible_range_changes == 0
            && self.unique_cells == self.expected_visible_cells
            && self.rendered_unique_cells == self.expected_visible_cells
            && self.final_placeholder_cells == 0
            && self.rendered_to_placeholder_transitions == 0
            && self.post_rendered_placeholder_observations == 0
            && self.expected_visible_animated_cells > 0
            && self.metadata_ready_animated_cells == self.expected_visible_animated_cells
            && self.advanced_animated_cells == self.expected_visible_animated_cells
            && self.animated_frame_transitions >= self.expected_visible_animated_cells as u64
    }

    /// Require repeated progress from every animated cell and reject a source
    /// that advanced once before becoming a long-lived static fallback.
    pub fn is_continuously_animating(
        &self,
        minimum_range_observations: u64,
        minimum_transitions_per_cell: u64,
        maximum_stall_ms: u128,
    ) -> bool {
        self.is_stable(minimum_range_observations)
            && self.animated_source_switches == 0
            && self.minimum_animated_cell_transitions >= minimum_transitions_per_cell
            && self.maximum_animated_cell_stall_ms <= maximum_stall_ms
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static WINDOW: LazyLock<Mutex<Option<PickerStabilityWindow>>> = LazyLock::new(|| Mutex::new(None));

fn lock_window() -> MutexGuard<'static, Option<PickerStabilityWindow>> {
    WINDOW
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Begin measuring a viewport after the benchmark driver has stopped scrolling
/// and allowed normal image hydration to settle.
pub fn begin_stationary_window(
    expected_visible_cells: usize,
    expected_visible_animated_cells: usize,
    visible_start: usize,
    visible_end: usize,
) {
    *lock_window() = Some(PickerStabilityWindow::new(
        expected_visible_cells,
        expected_visible_animated_cells,
        visible_start,
        visible_end,
    ));
    ACTIVE.store(true, Ordering::Release);
}

/// Record the exact animation frame selected for a visible animated cell.
///
/// Callers must report the frame index that supplied the displayed image, not
/// merely the wall-clock target index. A cache fallback can legitimately hold a
/// different resident frame while the target is being refilled; counting the
/// target would incorrectly certify a visually frozen picker.
#[allow(dead_code, reason = "exercised by tools/emote-picker-check")]
pub fn record_animated_frame(cell_key: usize, displayed_frame_index: u16) {
    record_animated_frame_key(cell_key, 0, displayed_frame_index);
}

/// Record the exact displayed frame together with the URL that supplied it.
/// Source changes never count as animation progress and are rejected during the
/// post-readiness continuity phase.
pub fn record_animated_frame_source(cell_key: usize, active_url: &str, displayed_frame_index: u16) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    active_url.hash(&mut hasher);
    record_animated_frame_key(cell_key, hasher.finish(), displayed_frame_index);
}

fn record_animated_frame_key(cell_key: usize, source_key: u64, displayed_frame_index: u16) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }

    let mut guard = lock_window();
    let Some(window) = guard.as_mut() else {
        return;
    };

    window.animated_frame_observations = window.animated_frame_observations.saturating_add(1);
    let observed_at = Instant::now();
    match window.animated_cells.get_mut(&cell_key) {
        Some(history) => {
            if history.last_source_key != source_key {
                history.last_source_key = source_key;
                history.last_frame_index = displayed_frame_index;
                history.frame_transitions = 0;
                history.last_transition_at = observed_at;
                history.maximum_transition_stall_ms = 0;
                window.animated_source_switches = window.animated_source_switches.saturating_add(1);
            } else if history.last_frame_index != displayed_frame_index {
                history.maximum_transition_stall_ms = history.maximum_transition_stall_ms.max(
                    observed_at
                        .duration_since(history.last_transition_at)
                        .as_millis(),
                );
                history.frame_transitions = history.frame_transitions.saturating_add(1);
                history.last_transition_at = observed_at;
                window.animated_frame_transitions =
                    window.animated_frame_transitions.saturating_add(1);
                history.last_frame_index = displayed_frame_index;
            }
        }
        None => {
            window.animated_cells.insert(
                cell_key,
                AnimatedCellHistory {
                    last_source_key: source_key,
                    last_frame_index: displayed_frame_index,
                    frame_transitions: 0,
                    last_transition_at: observed_at,
                    maximum_transition_stall_ms: 0,
                },
            );
        }
    }
}

/// Start the strict continuity phase after every advertised animation has first
/// become renderable and advanced. Visual churn counters remain cumulative, but
/// frame transitions, source switches, and stall timing are baselined here so
/// readiness activity cannot satisfy the sustained-playback gate.
pub fn begin_continuity_phase() -> bool {
    if !ACTIVE.load(Ordering::Acquire) {
        return false;
    }
    let mut guard = lock_window();
    let Some(window) = guard.as_mut() else {
        return false;
    };
    let started_at = Instant::now();
    for history in window.animated_cells.values_mut() {
        history.frame_transitions = 0;
        history.last_transition_at = started_at;
        history.maximum_transition_stall_ms = 0;
    }
    window.animated_frame_transitions = 0;
    window.animated_source_switches = 0;
    true
}

/// Whether the active stationary window already satisfies the complete visual
/// and animation gate. The live driver uses this to stop as soon as every
/// advertised animated cell has visibly advanced, subject to the same minimum
/// stationary-range evidence as the final snapshot.
pub fn stationary_window_is_stable(minimum_range_observations: u64) -> bool {
    if !ACTIVE.load(Ordering::Acquire) {
        return false;
    }

    lock_window()
        .as_ref()
        .is_some_and(|window| window.snapshot().is_stable(minimum_range_observations))
}

/// Whether every expected animation has continued to make recent, repeated
/// displayed-frame progress during the active stationary window.
pub fn stationary_window_has_continuity(
    minimum_range_observations: u64,
    minimum_transitions_per_cell: u64,
    maximum_stall_ms: u128,
) -> bool {
    if !ACTIVE.load(Ordering::Acquire) {
        return false;
    }

    lock_window().as_ref().is_some_and(|window| {
        window.snapshot().is_continuously_animating(
            minimum_range_observations,
            minimum_transitions_per_cell,
            maximum_stall_ms,
        )
    })
}

/// Record a cell-bearing virtual row range reported by the real `UniformList`
/// callback.
pub fn record_visible_range(visible_start: usize, visible_end: usize, observed_emote_cells: usize) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }

    if observed_emote_cells == 0 || visible_end <= visible_start {
        return;
    }

    let mut guard = lock_window();
    let Some(window) = guard.as_mut() else {
        return;
    };
    window.range_observations = window.range_observations.saturating_add(1);
    if (visible_start, visible_end) != (window.visible_start, window.visible_end) {
        window.visible_range_changes = window.visible_range_changes.saturating_add(1);
    }
}

/// Record the final visual branch selected for one visible emote cell.
///
/// `cell_key` should be stable for the catalog generation under test; the
/// popout's packed `emote_idx` is sufficient because the sweep restarts on a
/// catalog/render-version change before opening this window.
pub fn record_cell(cell_key: usize, state: PickerCellVisualState) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }

    let mut guard = lock_window();
    let Some(window) = guard.as_mut() else {
        return;
    };

    window.cell_observations = window.cell_observations.saturating_add(1);
    match state {
        PickerCellVisualState::Rendered => {
            window.rendered_observations = window.rendered_observations.saturating_add(1);
        }
        PickerCellVisualState::Placeholder => {
            window.placeholder_observations = window.placeholder_observations.saturating_add(1);
        }
    }

    match window.cells.get_mut(&cell_key) {
        Some(history) => {
            if history.last != state {
                match (history.last, state) {
                    (PickerCellVisualState::Rendered, PickerCellVisualState::Placeholder) => {
                        window.rendered_to_placeholder_transitions =
                            window.rendered_to_placeholder_transitions.saturating_add(1);
                    }
                    (PickerCellVisualState::Placeholder, PickerCellVisualState::Rendered) => {
                        window.placeholder_to_rendered_transitions =
                            window.placeholder_to_rendered_transitions.saturating_add(1);
                    }
                    _ => {}
                }
            }
            if state == PickerCellVisualState::Placeholder && history.ever_rendered {
                window.post_rendered_placeholder_observations = window
                    .post_rendered_placeholder_observations
                    .saturating_add(1);
            }
            history.last = state;
            history.ever_rendered |= state == PickerCellVisualState::Rendered;
        }
        None => {
            window.cells.insert(
                cell_key,
                CellHistory {
                    last: state,
                    ever_rendered: state == PickerCellVisualState::Rendered,
                },
            );
        }
    }
}

/// Stop recording and return the final aggregate for the harness log marker.
pub fn finish_stationary_window() -> Option<PickerStabilitySnapshot> {
    ACTIVE.store(false, Ordering::Release);
    lock_window().take().map(|window| window.snapshot())
}

/// Discard a partial window when a catalog-generation retry restarts the sweep.
pub fn cancel_stationary_window() {
    ACTIVE.store(false, Ordering::Release);
    *lock_window() = None;
}
