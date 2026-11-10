//! Tab Manager - Extracted from app.rs monolith
//!
//! Handles:
//! - Tab bar rendering (scroll, dropdown, multiline modes)
//! - Tab drag-and-drop reordering
//! - Tab context menu (rename, duplicate, close)
//! - Overflow management
//! - Hit area tracking

use crate::theme::colors;
use gpui::*;
use std::collections::{HashMap, HashSet};

/// Context menu item definition
pub struct TabMenuItem {
    pub label: &'static str,
    pub action: &'static str,
    pub is_divider: bool,
    pub is_toggle: bool,
    pub is_destructive: bool,
}

/// Static context menu items (toggles get their state dynamically)
pub const TAB_CONTEXT_MENU_ITEMS: &[TabMenuItem] = &[
    TabMenuItem {
        label: "Rename tab",
        action: "rename",
        is_divider: false,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "",
        action: "divider1",
        is_divider: true,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "Duplicate tab",
        action: "duplicate",
        is_divider: false,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "",
        action: "divider2",
        is_divider: true,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "Close tab",
        action: "close",
        is_divider: false,
        is_toggle: false,
        is_destructive: true,
    },
    TabMenuItem {
        label: "Close other tabs",
        action: "close_others",
        is_divider: false,
        is_toggle: false,
        is_destructive: true,
    },
    TabMenuItem {
        label: "Close tabs to the right",
        action: "close_right",
        is_divider: false,
        is_toggle: false,
        is_destructive: true,
    },
    TabMenuItem {
        label: "Reopen closed tab",
        action: "reopen_closed",
        is_divider: false,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "",
        action: "divider3",
        is_divider: true,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "Mark unread on new messages",
        action: "toggle_unread",
        is_divider: false,
        is_toggle: true,
        is_destructive: false,
    },
    TabMenuItem {
        label: "Lock tab layout",
        action: "toggle_lock",
        is_divider: false,
        is_toggle: true,
        is_destructive: false,
    },
    TabMenuItem {
        label: "",
        action: "divider4",
        is_divider: true,
        is_toggle: false,
        is_destructive: false,
    },
    TabMenuItem {
        label: "Sort tabs alphabetically",
        action: "sort_alpha",
        is_divider: false,
        is_toggle: false,
        is_destructive: false,
    },
];

/// Tab context menu state
#[derive(Clone, Default)]
pub struct TabContextMenu {
    pub is_open: bool,
    pub tab_idx: usize,
    pub tab_id: String,
    pub x: f32,
    pub y: f32,
}

/// Tab rename inline editing state
#[derive(Clone, Default)]
pub struct TabRenameState {
    pub is_active: bool,
    pub tab_id: String,
}

/// Drop zone for tab reordering
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabDropZone {
    /// Insert before the target tab (left edge)
    InsertBefore,
    /// Swap with the target tab (center)
    SwapWith,
    /// Insert after the target tab (right edge)
    InsertAfter,
}

/// Scroll direction for tab bar scroll arrows
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    Left,
    Right,
}

/// Tab drop target during drag
#[derive(Clone, Debug)]
pub struct TabDropTarget {
    /// Index of the target tab
    pub target_idx: usize,
    /// Which zone within the tab
    pub zone: TabDropZone,
}

/// Tab drag state for reordering tabs
#[derive(Clone, Default)]
pub struct TabDragState {
    /// Whether a drag is pending (mouse down but not moved enough yet)
    pub is_pending: bool,
    /// Whether a tab is currently being dragged (moved past threshold)
    pub is_dragging: bool,
    /// Index of the tab being dragged
    pub source_idx: usize,
    /// Stable identity of the tab being dragged.
    pub source_tab_id: String,
    /// The complete tab order when the pointer press began. A reorder is valid
    /// only while the rendered indices still refer to this exact collection.
    pub tab_order_snapshot: Vec<String>,
    /// Current drop target
    pub drop_target: Option<TabDropTarget>,
    /// Starting position of the drag
    pub start_pos: Point<Pixels>,
}

/// Minimum distance to move before a tab drag activates (in pixels)
pub const TAB_DRAG_THRESHOLD: f32 = 5.0;

/// A close gesture armed by a press on a tab's close button (left) or body
/// (middle). Tabs close only when that same press is released on its target
/// without becoming a drag, so grabbing a tab near its hover-revealed close
/// button reorders it instead of deleting it.
#[derive(Clone)]
pub struct TabClosePress {
    pub tab_id: String,
    pub button: MouseButton,
    pub start_pos: Point<Pixels>,
}

/// Manager entity for tab bar state and behavior
pub struct TabManager {
    pub context_menu: TabContextMenu,
    pub rename_state: TabRenameState,

    pub drag_state: TabDragState,
    pub close_press: Option<TabClosePress>,
    pub hit_areas: HashMap<usize, Bounds<Pixels>>,
    pub clipped_titles: HashSet<usize>,
    pub hovered_idx: Option<usize>,

    pub last_click_time: std::time::Instant,
    pub last_clicked_idx: Option<usize>,

    pub scroll_offset: f32,
    pub max_scroll_offset: f32,
    pub scroll_direction: Option<ScrollDirection>,
    pub scroll_viewport_bounds: Option<Bounds<Pixels>>,
    pub bar_width: f32,
    pub total_tabs_width: f32,

    pub overflow_dropdown_open: bool,
    pub overflow_button_bounds: Option<Bounds<Pixels>>,
}

/// Double-click threshold in milliseconds
const TAB_DOUBLE_CLICK_MS: u128 = 400;

impl TabManager {
    pub fn new() -> Self {
        Self {
            context_menu: TabContextMenu::default(),
            rename_state: TabRenameState::default(),
            drag_state: TabDragState::default(),
            close_press: None,
            hit_areas: HashMap::new(),
            clipped_titles: HashSet::new(),
            hovered_idx: None,
            last_click_time: std::time::Instant::now(),
            last_clicked_idx: None,
            scroll_offset: 0.0,
            max_scroll_offset: 0.0,
            scroll_direction: None,
            scroll_viewport_bounds: None,
            bar_width: 0.0,
            total_tabs_width: 0.0,
            overflow_dropdown_open: false,
            overflow_button_bounds: None,
        }
    }

    pub fn scroll_by(&mut self, delta: f32) -> bool {
        let previous = self.scroll_offset;
        self.scroll_offset = (previous + delta).clamp(0.0, self.max_scroll_offset);
        self.scroll_offset != previous
    }

    /// Check if a click on a tab is a double-click (for rename).
    /// Returns true if double-click detected, false otherwise.
    /// Updates tracking state for next click.
    pub fn check_tab_double_click(&mut self, idx: usize) -> bool {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_click_time).as_millis();

        let is_double = self.last_clicked_idx == Some(idx) && elapsed < TAB_DOUBLE_CLICK_MS;

        if is_double {
            self.last_clicked_idx = None;
        } else {
            self.last_click_time = now;
            self.last_clicked_idx = Some(idx);
        }

        is_double
    }

    pub fn is_context_menu_open(&self) -> bool {
        self.context_menu.is_open
    }

    pub fn is_overflow_dropdown_open(&self) -> bool {
        self.overflow_dropdown_open
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_state.is_dragging
    }

    pub fn end_drag(&mut self) -> Option<(usize, TabDropTarget)> {
        let result = if self.drag_state.is_dragging {
            self.drag_state
                .drop_target
                .take()
                .map(|target| (self.drag_state.source_idx, target))
        } else {
            None
        };

        self.drag_state = TabDragState::default();
        result
    }

    pub fn cancel_drag(&mut self) {
        self.drag_state = TabDragState::default();
    }

    pub fn arm_close_press(&mut self, tab_id: String, button: MouseButton, pos: Point<Pixels>) {
        self.close_press = Some(TabClosePress {
            tab_id,
            button,
            start_pos: pos,
        });
    }

    /// Whether this left press is the one that just armed `tab_id`'s close
    /// button. The close button's listener runs before the tab body's in the
    /// bubble phase, so the body uses this to avoid switching tabs for it.
    pub fn is_close_button_press(&self, tab_id: &str, pos: Point<Pixels>) -> bool {
        self.close_press.as_ref().is_some_and(|press| {
            press.button == MouseButton::Left && press.tab_id == tab_id && press.start_pos == pos
        })
    }

    /// Disarm `tab_id`'s armed close after its button was released outside
    /// that tab's target. Every tab registers this capture-phase listener, so
    /// it must ignore presses armed by other tabs: a release on the armed
    /// target is still "outside" every other tab's target.
    pub fn disarm_close_press(&mut self, tab_id: &str, button: MouseButton) {
        if self
            .close_press
            .as_ref()
            .is_some_and(|press| press.button == button && press.tab_id == tab_id)
        {
            self.close_press = None;
        }
    }

    /// Consume the armed close gesture for a release on `tab_id`'s target.
    /// Returns true only for a completed click: same tab and button, released
    /// within the drag threshold of the press, and never promoted to a drag.
    /// A left press must also still own the pending drag it started, which
    /// every left release elsewhere cancels, so a stale arm cannot close.
    pub fn take_completed_close(
        &mut self,
        tab_id: &str,
        button: MouseButton,
        pos: Point<Pixels>,
    ) -> bool {
        let Some(press) = self.close_press.take() else {
            return false;
        };
        if press.button != button || press.tab_id != tab_id || self.drag_state.is_dragging {
            return false;
        }
        if button == MouseButton::Left
            && !(self.drag_state.is_pending
                && self.drag_state.source_tab_id == tab_id
                && self.drag_state.start_pos == press.start_pos)
        {
            return false;
        }
        let dx: f32 = (pos.x - press.start_pos.x).into();
        let dy: f32 = (pos.y - press.start_pos.y).into();
        (dx * dx + dy * dy).sqrt() < TAB_DRAG_THRESHOLD
    }

    pub fn begin_drag(
        &mut self,
        source_idx: usize,
        source_tab_id: String,
        tab_order_snapshot: Vec<String>,
        start_pos: Point<Pixels>,
    ) {
        self.drag_state = TabDragState {
            is_pending: true,
            is_dragging: false,
            source_idx,
            source_tab_id,
            tab_order_snapshot,
            drop_target: None,
            start_pos,
        };
    }

    pub fn drag_snapshot_matches<'a>(&self, mut tab_order: impl Iterator<Item = &'a str>) -> bool {
        if !self.drag_state.is_pending && !self.drag_state.is_dragging {
            return true;
        }

        let mut snapshot = self.drag_state.tab_order_snapshot.iter();
        let exact_order = tab_order
            .all(|tab_id| snapshot.next().is_some_and(|expected| expected == tab_id))
            && snapshot.next().is_none();
        exact_order
            && self.drag_state.source_tab_id
                == self
                    .drag_state
                    .tab_order_snapshot
                    .get(self.drag_state.source_idx)
                    .map(String::as_str)
                    .unwrap_or_default()
    }

    /// Advance an armed drag only while the original left-button press is
    /// still held. Mouse-up can be consumed by an overlay or occur outside the
    /// window; without this check, a later hover could promote a stale press
    /// into a phantom reorder.
    pub fn update_drag_position(
        &mut self,
        position: Point<Pixels>,
        left_button_pressed: bool,
    ) -> bool {
        if !self.drag_state.is_pending && !self.drag_state.is_dragging {
            return false;
        }
        if !left_button_pressed {
            self.cancel_drag();
            self.last_clicked_idx = None;
            return true;
        }
        if !self.drag_state.is_pending {
            return false;
        }

        let start = self.drag_state.start_pos;
        let dx: f32 = (position.x - start.x).into();
        let dy: f32 = (position.y - start.y).into();
        if (dx * dx + dy * dy).sqrt() < TAB_DRAG_THRESHOLD {
            return false;
        }

        self.drag_state.is_pending = false;
        self.drag_state.is_dragging = true;
        self.last_clicked_idx = None;
        true
    }

    /// Compute tab reorder moves based on drop zone.
    /// Returns a list of (from, to) moves to execute (may be 1 or 2 for swap).
    /// Returns None if no move is needed.
    pub fn compute_reorder(
        source_idx: usize,
        target: &TabDropTarget,
    ) -> Option<Vec<(usize, usize)>> {
        let target_idx = target.target_idx;

        match target.zone {
            TabDropZone::SwapWith => {
                if source_idx == target_idx {
                    return None;
                }
                if source_idx < target_idx {
                    Some(vec![(source_idx, target_idx), (target_idx - 1, source_idx)])
                } else {
                    Some(vec![(source_idx, target_idx), (target_idx + 1, source_idx)])
                }
            }
            TabDropZone::InsertBefore => {
                let final_idx = if source_idx < target_idx {
                    target_idx - 1
                } else {
                    target_idx
                };
                if final_idx == source_idx {
                    return None;
                }
                Some(vec![(source_idx, final_idx)])
            }
            TabDropZone::InsertAfter => {
                let final_idx = if source_idx <= target_idx {
                    target_idx
                } else {
                    target_idx + 1
                };
                if final_idx == source_idx {
                    return None;
                }
                Some(vec![(source_idx, final_idx)])
            }
        }
    }

    pub fn close_context_menu(&mut self) {
        self.context_menu.is_open = false;
    }

    pub fn start_rename(&mut self, tab_id: String) {
        self.rename_state = TabRenameState {
            is_active: true,
            tab_id,
        };
    }

    pub fn cancel_rename(&mut self) {
        self.rename_state = TabRenameState::default();
    }

    /// Calculate which drop zone the cursor is in for tab reordering
    pub fn calculate_tab_drop_zone(cursor: Point<Pixels>, bounds: Bounds<Pixels>) -> TabDropZone {
        let w: f32 = bounds.size.width.into();
        let x: f32 = cursor.x.into();
        let origin_x: f32 = bounds.origin.x.into();

        if w <= 0.0 {
            return TabDropZone::SwapWith;
        }

        let rel_x = (x - origin_x) / w;

        if rel_x < 0.25 {
            TabDropZone::InsertBefore
        } else if rel_x > 0.75 {
            TabDropZone::InsertAfter
        } else {
            TabDropZone::SwapWith
        }
    }
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn apply_moves(mut tabs: Vec<&'static str>, moves: &[(usize, usize)]) -> Vec<&'static str> {
        for &(from, to) in moves {
            let tab = tabs.remove(from);
            tabs.insert(to, tab);
        }
        tabs
    }

    #[test]
    fn drag_reorder_preserves_every_tab_for_each_drop_zone() {
        let original = vec!["one", "two", "three", "four"];

        for (source_idx, target) in [
            (
                0,
                TabDropTarget {
                    target_idx: 2,
                    zone: TabDropZone::SwapWith,
                },
            ),
            (
                2,
                TabDropTarget {
                    target_idx: 0,
                    zone: TabDropZone::SwapWith,
                },
            ),
            (
                0,
                TabDropTarget {
                    target_idx: 3,
                    zone: TabDropZone::InsertBefore,
                },
            ),
            (
                3,
                TabDropTarget {
                    target_idx: 0,
                    zone: TabDropZone::InsertAfter,
                },
            ),
        ] {
            let moves = TabManager::compute_reorder(source_idx, &target)
                .expect("the selected source and target require a move");
            let mut reordered = apply_moves(original.clone(), &moves);
            let mut expected = original.clone();
            reordered.sort_unstable();
            expected.sort_unstable();
            assert_eq!(
                reordered, expected,
                "source={source_idx}, target={target:?}"
            );
        }
    }

    fn armed_left_press(manager: &mut TabManager, tab_id: &str, pos: Point<Pixels>) {
        manager.arm_close_press(tab_id.to_string(), MouseButton::Left, pos);
        assert!(manager.is_close_button_press(tab_id, pos));
        manager.begin_drag(
            0,
            tab_id.to_string(),
            vec![tab_id.to_string(), "other".to_string()],
            pos,
        );
    }

    #[test]
    fn close_button_closes_only_on_a_completed_click() {
        let start = point(px(100.0), px(10.0));
        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        assert!(manager.take_completed_close("one", MouseButton::Left, point(px(102.0), px(11.0))));
        assert!(manager.close_press.is_none());

        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        assert!(manager.update_drag_position(point(px(120.0), px(10.0)), true));
        assert!(!manager.take_completed_close("one", MouseButton::Left, start));

        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        assert!(!manager.take_completed_close("other", MouseButton::Left, start));
        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        assert!(!manager.take_completed_close("one", MouseButton::Middle, start));

        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        manager.cancel_drag();
        assert!(!manager.take_completed_close("one", MouseButton::Left, start));

        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        manager.disarm_close_press("one", MouseButton::Left);
        assert!(!manager.take_completed_close("one", MouseButton::Left, start));

        let mut manager = TabManager::new();
        armed_left_press(&mut manager, "one", start);
        manager.disarm_close_press("other", MouseButton::Left);
        assert!(manager.take_completed_close("one", MouseButton::Left, start));
    }

    #[test]
    fn middle_click_closes_only_without_travel() {
        let start = point(px(40.0), px(10.0));
        let mut manager = TabManager::new();
        manager.arm_close_press("one".to_string(), MouseButton::Middle, start);
        assert!(!manager.is_close_button_press("one", start));
        manager.disarm_close_press("two", MouseButton::Middle);
        assert!(manager.take_completed_close("one", MouseButton::Middle, start));

        manager.arm_close_press("one".to_string(), MouseButton::Middle, start);
        assert!(!manager.take_completed_close(
            "one",
            MouseButton::Middle,
            point(px(60.0), px(10.0))
        ));
    }

    #[test]
    fn released_pending_drag_cannot_promote_on_a_later_hover() {
        let mut manager = TabManager::new();
        manager.last_clicked_idx = Some(3);
        manager.drag_state = TabDragState {
            is_pending: true,
            is_dragging: false,
            source_idx: 1,
            source_tab_id: "two".to_string(),
            tab_order_snapshot: vec!["one".to_string(), "two".to_string()],
            drop_target: None,
            start_pos: point(px(0.0), px(0.0)),
        };

        assert!(manager.update_drag_position(point(px(20.0), px(0.0)), false));
        assert!(!manager.drag_state.is_pending);
        assert!(!manager.drag_state.is_dragging);
        assert_eq!(manager.last_clicked_idx, None);
    }
}

impl TabManager {
    /// Get toggle state for a menu item action
    pub fn get_toggle_state(action: &str, is_locked: bool, mark_unread: bool) -> bool {
        match action {
            "toggle_lock" => is_locked,
            "toggle_unread" => mark_unread,
            _ => false,
        }
    }
    /// Get tab background color based on state
    pub fn tab_bg(is_active: bool) -> Hsla {
        if is_active {
            colors::bg_plate()
        } else {
            colors::bg_surface()
        }
    }
}
