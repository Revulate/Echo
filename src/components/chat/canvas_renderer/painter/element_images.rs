//! Image and layered-emote fallback element rendering.

use super::*;

use crate::components::chat::canvas_renderer::layout;

#[inline]
fn get_visible_animated_render_image(url: &str) -> Option<Arc<RenderImage>> {
    if let Some((render_image, metadata)) =
        image_cache::get_current_animated_render_image_with_metadata(url)
    {
        emote_renderer::register_visible_metadata(metadata);
        return Some(render_image);
    }

    if matches!(
        image_cache::get_animated_cache_state(url),
        image_cache::AnimatedCacheState::NotLoaded
    ) {
        image_cache::trigger_animated_decode_with_source(
            url,
            image_cache::PreloadSource::VisibleChat,
        );
    }

    None
}

/// Whether a display rule shows this emote as its static first frame.
#[inline]
fn shows_still_frame(meta: Option<&Arc<layout::EmoteTooltipMeta>>) -> bool {
    meta.is_some_and(|meta| meta.still_image)
}

/// [`shows_still_frame`] for a segment's image.
#[inline]
pub(super) fn image_shows_still_frame(image_type: &layout::ImageType) -> bool {
    matches!(image_type, layout::ImageType::Emote { tooltip_meta, .. } if shows_still_frame(tooltip_meta.as_ref()))
}

/// Render an image segment (emote or badge)
/// PERF: #[inline] for hot-path function called per-image-segment per-frame
///
/// A `still` image draws only Echo's static first frame. Until that is
/// resident it shows the placeholder, never GPUI's `img(url)`, which would play
/// a GIF/WebP; the split's image-load waker repaints once the static layout
/// preload lands.
#[inline]
pub(super) fn render_image_segment(
    url: &SharedString,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    is_animated: bool,
    still: bool,
    overlays: &[OverlayEmote],
) -> AnyElement {
    if !overlays.is_empty() {
        return render_layered_emote(url, x, y, width, height, is_animated, still, overlays);
    }

    if !is_animated {
        if let Some(render_image) = image_cache::get_cached_render_image(url.as_ref()) {
            return img(ImageSource::Render(render_image))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(width))
                .h(px(height))
                .object_fit(ObjectFit::Contain)
                .into_any_element();
        }

        if still || image_cache::is_downloading(url.as_ref()) {
            return render_emote_placeholder(x, y, width, height);
        }
    }

    if is_animated && let Some(render_image) = get_visible_animated_render_image(url.as_ref()) {
        return img(ImageSource::Render(render_image))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .h(px(height))
            .object_fit(ObjectFit::Contain)
            .into_any_element();
    }

    let state = image_cache::get_state(url.as_ref());

    let final_url: SharedString = match state {
        image_cache::ImageLoadState::Failed => {
            if image_cache::retry_failed_media(
                url.as_ref(),
                is_animated,
                image_cache::PreloadSource::VisibleChat,
            ) {
                crate::backend::metrics::record_emote_event("canvas.render.retry_scheduled");
                url.clone()
            } else {
                return div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(3.0))
                    .bg(colors::bg_elevated())
                    .into_any_element();
            }
        }
        _ => url.clone(),
    };

    if is_animated {
        return render_emote_placeholder(x, y, width, height);
    }

    let base_img = img(final_url.clone())
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .h(px(height))
        .object_fit(ObjectFit::Contain)
        .with_fallback({
            let (w, h) = (width, height);
            move || {
                div()
                    .w(px(w))
                    .h(px(h))
                    .rounded(px(3.0))
                    .bg(hsla(0.0, 0.0, 0.3, 0.15))
                    .into_any_element()
            }
        });

    base_img.into_any_element()
}

/// Render a shimmer placeholder for loading emotes
/// PERF: Provides immediate visual feedback while emotes load
/// Shows a subtle rounded rectangle that indicates content is loading
#[inline]
fn render_emote_placeholder(x: f32, y: f32, width: f32, height: f32) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .h(px(height))
        .rounded(px(3.0))
        .bg(hsla(0.0, 0.0, 0.3, 0.15))
        .into_any_element()
}

/// Render a layered emote (base emote with zero-width overlays centered on top)
#[inline]
fn render_layered_emote(
    base_url: &SharedString,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    base_is_animated: bool,
    base_still: bool,
    overlays: &[OverlayEmote],
) -> AnyElement {
    let mut layers: SmallVec<[AnyElement; 4]> = SmallVec::with_capacity(1 + overlays.len());

    layers.push(render_single_emote_for_layer(
        base_url,
        width,
        height,
        base_is_animated,
        base_still,
    ));

    for overlay in overlays {
        layers.push(render_single_emote_for_layer(
            &overlay.url,
            width,
            height,
            overlay.is_animated,
            shows_still_frame(overlay.tooltip_meta.as_ref()),
        ));
    }

    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .h(px(height))
        .children(layers)
        .into_any_element()
}

/// Render a single emote for use within a layered container (fills container)
#[inline]
fn render_single_emote_for_layer(
    url: &SharedString,
    _width: f32,
    _height: f32,
    is_animated: bool,
    still: bool,
) -> AnyElement {
    if !is_animated && let Some(render_image) = image_cache::get_cached_render_image(url.as_ref()) {
        return img(ImageSource::Render(render_image))
            .absolute()
            .inset_0()
            .size_full()
            .object_fit(ObjectFit::Contain)
            .into_any_element();
    }
    if still {
        return div()
            .absolute()
            .inset_0()
            .size_full()
            .rounded(px(3.0))
            .bg(hsla(0.0, 0.0, 0.3, 0.15))
            .into_any_element();
    }

    let state = image_cache::get_state(url.as_ref());

    let final_url: SharedString = match state {
        image_cache::ImageLoadState::Failed => {
            if image_cache::retry_failed_media(
                url.as_ref(),
                is_animated,
                image_cache::PreloadSource::VisibleChat,
            ) {
                crate::backend::metrics::record_emote_event("canvas.render.retry_scheduled");
                url.clone()
            } else {
                return div()
                    .absolute()
                    .inset_0()
                    .rounded(px(3.0))
                    .bg(colors::bg_elevated())
                    .into_any_element();
            }
        }
        _ => url.clone(),
    };

    if is_animated {
        if let Some(render_image) = get_visible_animated_render_image(final_url.as_ref()) {
            return img(ImageSource::Render(render_image))
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element();
        }

        return div()
            .absolute()
            .inset_0()
            .size_full()
            .rounded(px(3.0))
            .bg(hsla(0.0, 0.0, 0.3, 0.15))
            .into_any_element();
    }

    let base_img = img(final_url.clone())
        .absolute()
        .inset_0()
        .size_full()
        .object_fit(ObjectFit::Contain);

    base_img.into_any_element()
}

/// Render a layered image with centered overlays
///
/// Each layer is positioned at its absolute coordinates (already computed with centering).
/// The bounding box is used as the container dimensions for layout purposes.
#[inline]
pub(super) fn render_layered_image(
    layers: &[layout::PositionedLayer],
    bbox_x: f32,
    bbox_y: f32,
    bbox_width: f32,
    bbox_height: f32,
) -> AnyElement {
    let rendered_layers: Vec<AnyElement> = layers
        .iter()
        .map(|layer| {
            let rel_x = layer.x - bbox_x;
            let rel_y = layer.y - bbox_y;

            render_positioned_layer(layer, rel_x, rel_y)
        })
        .collect();

    div()
        .absolute()
        .left(px(bbox_x))
        .top(px(bbox_y))
        .w(px(bbox_width))
        .h(px(bbox_height))
        .children(rendered_layers)
        .into_any_element()
}

/// Render a single positioned layer within a layered image container
#[inline]
fn render_positioned_layer(layer: &layout::PositionedLayer, x: f32, y: f32) -> AnyElement {
    if !layer.is_animated
        && let Some(render_image) = image_cache::get_cached_render_image(layer.url.as_ref())
    {
        return img(ImageSource::Render(render_image))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(layer.width))
            .h(px(layer.height))
            .object_fit(ObjectFit::Contain)
            .into_any_element();
    }
    if shows_still_frame(layer.tooltip_meta.as_ref()) {
        return render_emote_placeholder(x, y, layer.width, layer.height);
    }

    let state = image_cache::get_state(layer.url.as_ref());

    let final_url: SharedString = match state {
        image_cache::ImageLoadState::Failed => {
            if image_cache::retry_failed_media(
                layer.url.as_ref(),
                layer.is_animated,
                image_cache::PreloadSource::VisibleChat,
            ) {
                crate::backend::metrics::record_emote_event("canvas.render.retry_scheduled");
                layer.url.clone()
            } else {
                return div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(layer.width))
                    .h(px(layer.height))
                    .rounded(px(3.0))
                    .bg(colors::bg_elevated())
                    .into_any_element();
            }
        }
        _ => layer.url.clone(),
    };

    if layer.is_animated {
        if let Some(render_image) = get_visible_animated_render_image(final_url.as_ref()) {
            return img(ImageSource::Render(render_image))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(layer.width))
                .h(px(layer.height))
                .object_fit(ObjectFit::Contain)
                .into_any_element();
        }

        return render_emote_placeholder(x, y, layer.width, layer.height);
    }

    img(final_url)
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(layer.width))
        .h(px(layer.height))
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}
