//! Canvas-based message renderer
//!
//! Uses direct canvas painting with pre-shaped text for:
//! - Multi-message text selection
//! - Tooltips on all interactive elements
//! - RTL/BiDi text support for Hebrew, Arabic, and mixed-direction text

pub mod async_layout;
pub mod bidi;
pub mod eviction_cache;
pub mod font_metrics;
pub mod global_layout_cache;
pub mod height_tree;
pub mod hit_testing;
pub mod layout;
pub mod layout_mode;
pub mod layout_scale;
pub mod line_buffer;
pub mod native_dimensions;
pub mod painter;
pub mod painter_async;
pub mod relayout_trace;
pub mod selection;
pub mod seventv_paint;
pub mod text_run_intern;
pub mod text_shaping;
pub mod tooltip;

pub(crate) use async_layout::layout_worker_diagnostics;
pub use font_metrics::initialize_font_metrics;
pub use painter::CanvasMessageList;

/// Install Echo's settings accessors into the GPUI-dependent render model.
///
/// `echo-chat-render` keeps neutral defaults so its geometry unit tests run
/// standalone; the application binds the real user settings once at startup,
/// before any layout is built.
pub fn install_render_ports() {
    use crate::backend::settings;

    echo_chat_render::ports::install_font_scale(settings::get_font_scale);
    echo_chat_render::ports::install_chat_density_multiplier(|| {
        settings::get_chat_density().multiplier()
    });
    echo_chat_render::ports::install_badge_scale(settings::get_badge_scale);
    echo_chat_render::ports::install_emote_scale(settings::get_emote_scale);
    echo_chat_render::ports::install_clipboard_writer(crate::utils::copy_text);
    echo_chat_render::ports::install_text_primary(crate::theme::colors::text_primary);
    echo_chat_render::ports::install_decoded_image_size(native_dimensions::decoded_image_size);
    echo_chat_render::ports::install_badge_set_pending(
        crate::backend::badges::is_badge_set_pending,
    );
    echo_core::install_log_category_enabled(crate::log::enabled);
}
