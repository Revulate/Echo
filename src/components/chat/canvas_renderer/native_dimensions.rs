//! Application-owned decoded-media size adapter for chat layout.
//!
//! Layout construction must not load or decode media. This probes only images
//! already resident in the static cache or the animation frame manager, then
//! the size remembered from an earlier decode of the same URL.

use crate::cache::{loader, native_sizes};

#[inline]
fn render_image_dimensions(image: &gpui::RenderImage) -> Option<(u32, u32)> {
    let size = image.size(0);
    let width = size.width.0 as u32;
    let height = size.height.0 as u32;
    (width > 0 && height > 0).then_some((width, height))
}

/// Read a decoded image's native dimensions without scheduling cache work.
///
/// Static images take the ordinary memory-cache path. Animated media falls
/// back to a stable resident animation frame. Every composited frame has the
/// same canvas dimensions, regardless of its playback index.
#[inline]
pub(crate) fn decoded_image_size(url: &str) -> Option<(u32, u32)> {
    match loader::get_cached_render_image(url)
        .or_else(|| loader::get_stable_animated_render_image(url))
        .and_then(|image| render_image_dimensions(&image))
    {
        Some((width, height)) => {
            native_sizes::remember(url, width, height);
            Some((width, height))
        }
        None => native_sizes::remembered(url),
    }
}
