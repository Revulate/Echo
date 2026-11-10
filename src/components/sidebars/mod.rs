//! Sidebar components for the single global, combined Activity rail.
//!
//! This module provides inline render functions for the thin wrapper pattern used in app.rs.

mod activity_feed;
mod activity_sidebar;
mod mentions_sidebar;
mod whispers_sidebar;

pub use activity_feed::render_combined_activity_content;
pub use activity_sidebar::{ActivityCounts, render_activity_sidebar};
pub(crate) use activity_sidebar::{format_activity_copy_text, render_activity_copy_action};
pub use mentions_sidebar::render_mentions_activity_content;
pub use whispers_sidebar::render_whispers_activity_content;
