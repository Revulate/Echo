//! Split tree rendering helpers
//!
//! Provides static helper methods for split tree rendering.
//! The main recursive rendering stays in app.rs to avoid borrow checker complexity.

use crate::managers::{SplitLayoutManager, SplitPaneDrag};
use crate::state::DropZone;
use gpui::*;

/// Split tree renderer (static helpers only)
///
/// Provides helper methods for rendering split-related UI elements.
/// The main render_split_tree method remains in EchoApp to avoid borrow checker issues.
pub struct SplitTreeRenderer;

impl SplitTreeRenderer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SplitTreeRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitTreeRenderer {
    /// Render the drag ghost when dragging a split pane
    pub fn render_drag_ghost(
        &self,
        pane_drag: Option<&SplitPaneDrag>,
        cursor_position: Point<Pixels>,
    ) -> impl IntoElement + use<> {
        let Some(drag) = pane_drag else {
            return div().into_any_element();
        };

        SplitLayoutManager::render_drag_ghost(cursor_position, &drag.channel).into_any_element()
    }

    /// Render drop zone overlay when dragging over a split
    pub fn render_drop_zone_overlay(
        &self,
        zone: Option<DropZone>,
        width: f32,
        height: f32,
    ) -> impl IntoElement + use<> {
        SplitLayoutManager::render_drop_zone_overlay(zone, width, height)
    }
}
