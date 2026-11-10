//! Theme system for Echo GPUI
//!
//! Provides color palettes, typography, and styling utilities.
//!
//! # Modules
//! - `colors`: Dynamic theme-aware color palette
//! - `fonts`: Platform font families
//! - `tokens`: Design tokens for spacing, radii, shadows, and sizes

pub mod colors;
pub mod fonts;
pub mod tokens;

use gpui::*;

/// Initialize the theme system
pub fn init(_cx: &mut App) {
    crate::log_ui!(info, "Theme system initialized (Dark theme)");
}
