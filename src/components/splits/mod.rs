//! Split pane components
//!
//! Provides self-contained split view components with proper input handling.

mod divider;
mod placeholder_pane;

pub use divider::accessible_split_divider;
pub use placeholder_pane::{render_placeholder_pane, render_watching_pane};
