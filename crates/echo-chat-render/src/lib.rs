//! GPUI-dependent chat render model and interaction policies.
//!
//! The canvas renderer's data model, hit testing, and 7TV name-paint recipe are
//! independent of Echo's settings, theme, and cache runtimes: they only need
//! GPUI's geometry and text primitives. Keeping them here lets the render
//! policies compile and be tested without the application shell. The
//! application binds its settings and services through [`ports`].

pub mod font_metrics;
pub mod hit_mapping;
pub mod hit_testing;
pub mod layout;
pub mod line_buffer;
pub mod native_emote_dimensions;
pub mod paint_order;
pub mod ports;
pub mod selection;
pub mod seventv_paint;
pub mod text_run_intern;
pub mod width_bucket;
