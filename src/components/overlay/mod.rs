//! Streamer Overlay Window
//!
//! Borderless, transparent overlay window for streamers to monitor chat while gaming.
//!
//! Features:
//! - Transparent background with text/emotes visible
//! - Click-through mode (passthrough) on supported platforms
//! - Minimal "ghost" mode with a lighter backdrop for readability
//! - Persisted window position and size
//! - Smart idle fade with configurable timing and opacity

mod overlay_window;

pub use overlay_window::*;
