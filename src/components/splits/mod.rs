//! Split pane components
//!
//! Provides self-contained split view components with proper input handling.

mod divider;
mod placeholder_pane;
mod split_tree_renderer;

pub use divider::accessible_split_divider;
pub use placeholder_pane::{render_placeholder_pane, render_watching_pane};
pub use split_tree_renderer::SplitTreeRenderer;
