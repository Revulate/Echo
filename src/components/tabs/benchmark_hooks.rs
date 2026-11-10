//! The tab bar's adapter onto the disposable benchmark UI fixture.
//!
//! Every fixture override the tab bar honours goes through this module, so
//! the renderer calls one named hook per decision point and the fixture's
//! surface on the tab bar is visible in one place. Outside a fixture run each
//! hook is a no-op that returns the normal value.

use super::TabBarRenderer;
use crate::backend::{TabOverflowMode, TabTheme};
use crate::bench::ui_fixture as fixture;

/// Tab appearance forced by the fixture, if any.
pub(super) fn tab_theme_override() -> Option<TabTheme> {
    fixture::tab_theme_override()
}

/// Overflow mode forced by the fixture, if any.
pub(super) fn overflow_mode_override() -> Option<TabOverflowMode> {
    fixture::tab_overflow_override()
}

/// Whether tab changes made under the fixture must stay in memory.
pub(super) fn suppress_tab_persistence() -> bool {
    fixture::should_suppress_persistence()
}

/// The bar width tab layout solves against; the fixture caps it so layouts
/// are reproducible across window sizes.
pub(super) fn layout_width(bar_width: f32) -> f32 {
    fixture::cap_tab_layout_width(bar_width)
}

/// Whether the tab bar must render uncached every frame, so fixture probes
/// observe a fresh layout.
pub(super) fn bypass_view_cache() -> bool {
    fixture::is_enabled()
}

/// Fixed width of the multiline bar under the fixture, matching
/// [`layout_width`]'s cap.
pub(super) fn multiline_bar_width_px() -> Option<f32> {
    fixture::is_enabled().then_some(fixture::FIXTURE_TAB_LAYOUT_WIDTH_PX)
}

impl TabBarRenderer {
    /// Clear the transient tab-bar state created by the disposable UI
    /// fixture. This deliberately avoids changing saved tabs or the normal
    /// interaction paths; the benchmark driver calls it only while tearing
    /// down its in-memory fixture.
    #[doc(hidden)]
    pub fn benchmark_close_transients(&mut self) -> bool {
        self.close_overflow();
        self.close_context_menu();
        self.tab_manager.hovered_idx = None;
        self.tab_manager.scroll_direction = None;
        !self.is_overflow_open()
            && !self.is_context_menu_open()
            && self.hovered_tab_idx().is_none()
            && self.tab_manager.scroll_direction.is_none()
    }
}
