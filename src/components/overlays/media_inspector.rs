//! Reusable native inspector for image media attached to rich embed cards.
//!
//! The window shows the card's image at full resolution. It asks
//! `cache::inspector_image` for the best rendition the link offers (the
//! original behind a known thumbnail URL family, then the card's own image
//! URL), which downloads through the link-preview fetch policy and decodes
//! outside the chat image caches. The window holds that one image and releases
//! it when it closes or is pointed at another; it has no player and persists
//! no window preference.

use crate::cache::inspector_image::{InspectorImage, InspectorLoadError};
use crate::components::ui::{Retarget, SingletonWindow};
use crate::theme::colors;
use crate::theme::tokens::Spacing;
use echo_media_cache::full_resolution::{self, FullResolutionError};
use gpui::*;
use std::sync::Arc;

const MEDIA_INSPECTOR_SIZE: (f32, f32) = (960.0, 720.0);
const MEDIA_INSPECTOR_MIN_SIZE: (f32, f32) = (320.0, 240.0);
/// Height kept below the image for the dimensions caption and its gap.
const CAPTION_ALLOWANCE: f32 = Spacing::SM + 16.0;

/// Pending payload: the image source to show next.
static MEDIA_INSPECTOR: SingletonWindow<String> = SingletonWindow::new();

/// Open the reusable inspector, or retarget and focus the existing window.
///
/// The caller only supplies a `MediaKind::Image` source that was already
/// classified while shaping the rich card. This does not open the card's
/// external browser target and deliberately has no playback path.
pub fn open_media_inspector(source_url: String, cx: &mut App) {
    if source_url.trim().is_empty() || crate::backend::streamer_mode::should_hide_link_thumbnails()
    {
        return;
    }

    let source_url = match MEDIA_INSPECTOR.retarget(source_url, cx) {
        Retarget::Retargeted => return,
        Retarget::NotOpen(source_url) => source_url,
        Retarget::Stale(source_url) => {
            MEDIA_INSPECTOR.mark_closed();
            source_url
        }
    };

    let window_size = size(px(MEDIA_INSPECTOR_SIZE.0), px(MEDIA_INSPECTOR_SIZE.1));
    let (bounds, display_id) = crate::utils::popup_window_placement(None, window_size, cx);
    let open_result = crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Image preview".into()),
                appears_transparent: false,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            kind: WindowKind::Floating,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(
                px(MEDIA_INSPECTOR_MIN_SIZE.0),
                px(MEDIA_INSPECTOR_MIN_SIZE.1),
            )),
            ..Default::default()
        },
        move |window, cx| {
            MEDIA_INSPECTOR.set_handle(window.window_handle());
            MEDIA_INSPECTOR.mark_open();
            cx.new(|cx| {
                let mut inspector = MediaInspector::new(cx);
                inspector.show_source(source_url.clone(), window, cx);
                window.focus(&inspector.focus_handle, cx);
                inspector
            })
        },
    );

    if open_result.is_err() {
        MEDIA_INSPECTOR.mark_closed();
    }
}

/// The candidate URLs for `source_url`, best first, ending with the source.
fn source_candidates(source_url: &str) -> Vec<String> {
    let mut candidates = crate::backend::link_types::full_size_image_candidates(source_url);
    candidates.push(source_url.to_string());
    candidates
}

enum ImageState {
    /// Hidden by streamer mode: nothing was requested.
    Idle,
    Loading,
    Ready {
        /// The decoded bitmap, then its half-size copies.
        levels: Vec<Arc<RenderImage>>,
        caption: SharedString,
    },
    Failed(SharedString),
}

pub struct MediaInspector {
    source_url: String,
    state: ImageState,
    /// Bumped per source so a superseded load's result is discarded.
    generation: u64,
    _load: Option<Task<()>>,
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
    focus_handle: FocusHandle,
}

impl MediaInspector {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            source_url: String::new(),
            state: ImageState::Idle,
            generation: 0,
            _load: None,
            always_on_top: Default::default(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Point the window at `source_url`: release the shown image and start
    /// loading the new one.
    fn show_source(&mut self, source_url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.release_image(Some(window), cx);
        self.source_url = source_url;
        self.generation = self.generation.wrapping_add(1);
        self._load = None;
        if crate::backend::streamer_mode::should_hide_link_thumbnails() {
            self.state = ImageState::Idle;
            return;
        }
        self.state = ImageState::Loading;
        let generation = self.generation;
        let candidates = source_candidates(&self.source_url);
        let candidate_count = candidates.len();
        let receiver = crate::cache::inspector_image::load(candidates);
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = receiver.await.unwrap_or(Err(InspectorLoadError::Network));
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    if let Ok(loaded) = result {
                        loaded
                            .decoded
                            .levels
                            .into_iter()
                            .for_each(crate::cache::queue_render_image_drop);
                    }
                    return;
                }
                this.state = match result {
                    Ok(loaded) => ImageState::Ready {
                        caption: caption(&loaded, candidate_count).into(),
                        levels: loaded.decoded.levels,
                    },
                    Err(error) => ImageState::Failed(failure_text(error).into()),
                };
                cx.notify();
            });
        }));
    }

    fn release_image(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        if let ImageState::Ready { levels, .. } =
            std::mem::replace(&mut self.state, ImageState::Idle)
        {
            for level in levels {
                cx.drop_image(level, window.as_deref_mut());
            }
        }
    }

    fn apply_pending_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next_source = MEDIA_INSPECTOR.take_pending();
        if let Some(source_url) = next_source.filter(|source_url| !source_url.trim().is_empty()) {
            self.show_source(source_url, window, cx);
        }
    }
}

/// Device pixels an `image` (device pixels) covers when fitted, without
/// upscaling past its own pixels, into an `area` of logical pixels.
fn painted_device_size(image: (f32, f32), area: (f32, f32), scale: f32) -> (f32, f32) {
    let area = (area.0.max(1.0) * scale, area.1.max(1.0) * scale);
    let fit = (area.0 / image.0.max(1.0))
        .min(area.1 / image.1.max(1.0))
        .min(1.0);
    (image.0 * fit, image.1 * fit)
}

/// The dimensions line under the image, saying when it is not the original.
fn caption(loaded: &InspectorImage, candidate_count: usize) -> String {
    let decoded = &loaded.decoded;
    let (width, height) = decoded.source_size;
    let mut text = format!("{width} × {height}");
    if decoded.shown_size != decoded.source_size {
        let (shown_width, shown_height) = decoded.shown_size;
        text.push_str(&format!(
            ", shown at {shown_width} × {shown_height} (preview size limit)"
        ));
    }
    if decoded.animation_reduced_to_still {
        text.push_str(", first frame of an animation too large to play");
    }
    if candidate_count > 1 && loaded.candidate_index + 1 == candidate_count {
        text.push_str(", the card's own image (no larger version could be loaded)");
    }
    text
}

fn failure_text(error: InspectorLoadError) -> String {
    match error {
        InspectorLoadError::Decode(FullResolutionError::TooLarge { width, height }) => {
            format!("This image is too large to preview ({width} × {height})")
        }
        InspectorLoadError::TooLarge => "This image is too large to preview".to_string(),
        InspectorLoadError::Decode(FullResolutionError::NotAnImage) => {
            "This link is not an image Echo can show".to_string()
        }
        InspectorLoadError::Decode(FullResolutionError::Corrupt) => {
            "This image could not be decoded".to_string()
        }
        InspectorLoadError::Blocked => "This image address is not allowed".to_string(),
        InspectorLoadError::Network => "This image could not be loaded".to_string(),
    }
}

impl Drop for MediaInspector {
    fn drop(&mut self) {
        if let ImageState::Ready { levels, .. } =
            std::mem::replace(&mut self.state, ImageState::Idle)
        {
            levels
                .into_iter()
                .for_each(crate::cache::queue_render_image_drop);
        }
        MEDIA_INSPECTOR.mark_closed();
    }
}

impl Render for MediaInspector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.always_on_top.sync(window);
        self.apply_pending_source(window, cx);

        let hidden_by_streamer_mode = crate::backend::streamer_mode::should_hide_link_thumbnails();
        let status_text = |text: SharedString| {
            div()
                .text_color(colors::text_secondary())
                .text_sm()
                .child(text)
                .into_any_element()
        };
        if hidden_by_streamer_mode {
            // Streamer mode turned on while an image was shown: let it go.
            if matches!(self.state, ImageState::Ready { .. }) {
                self.release_image(Some(window), cx);
            }
        } else if matches!(self.state, ImageState::Idle) && !self.source_url.is_empty() {
            // Streamer mode hid the image when it was requested or shown.
            let source_url = self.source_url.clone();
            self.show_source(source_url, window, cx);
        }
        let body = if hidden_by_streamer_mode {
            status_text("Image preview hidden by streamer mode".into())
        } else {
            match &self.state {
                ImageState::Idle | ImageState::Loading => {
                    status_text("Loading full-size image…".into())
                }
                ImageState::Failed(text) => status_text(text.clone()),
                ImageState::Ready { levels, caption } => {
                    // Never scale past the bitmap's own pixels: a small image
                    // stays sharp at its native size instead of being blown
                    // up to fill the window.
                    let size = levels[0].size(0);
                    let scale = window.scale_factor().max(f32::EPSILON);
                    let viewport = window.viewport_size();
                    let target = painted_device_size(
                        (size.width.0 as f32, size.height.0 as f32),
                        (
                            f32::from(viewport.width) - 2.0 * Spacing::MD,
                            f32::from(viewport.height) - 2.0 * Spacing::MD - CAPTION_ALLOWANCE,
                        ),
                        scale,
                    );
                    let level_sizes: Vec<(u32, u32)> = levels
                        .iter()
                        .map(|level| {
                            let size = level.size(0);
                            (size.width.0.max(0) as u32, size.height.0.max(0) as u32)
                        })
                        .collect();
                    let image = levels[full_resolution::level_for(&level_sizes, target)].clone();
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(Spacing::SM))
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.0))
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    img(image)
                                        .id("media-inspector-image")
                                        .size_full()
                                        .max_w(px(size.width.0 as f32 / scale))
                                        .max_h(px(size.height.0 as f32 / scale))
                                        .object_fit(ObjectFit::Contain),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(caption.clone()),
                        )
                        .into_any_element()
                }
            }
        };

        div()
            .id("media-inspector")
            .track_focus(&self.focus_handle)
            .on_key_down(|event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    window.remove_window();
                }
            })
            .size_full()
            .p(px(Spacing::MD))
            .bg(colors::bg_void())
            .flex()
            .items_center()
            .justify_center()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn candidates_try_the_full_size_rendition_before_the_card_image() {
        let thumb = "https://thumb.wikimedia.org/wikipedia/commons/thumb/1/16/A.jpg/960px-A.jpg";
        assert_eq!(
            source_candidates(thumb),
            vec![
                "https://upload.wikimedia.org/wikipedia/commons/1/16/A.jpg".to_string(),
                thumb.to_string(),
            ]
        );
        let direct = "https://upload.wikimedia.org/wikipedia/commons/b/b6/Cat.jpg";
        assert_eq!(source_candidates(direct), vec![direct.to_string()]);
    }

    #[test]
    fn painted_size_fits_the_area_and_never_exceeds_the_bitmap() {
        // 4400x4600 in the default window's image area at 100%.
        let (width, height) = painted_device_size((4400.0, 4600.0), (928.0, 664.0), 1.0);
        assert!((height - 664.0).abs() < 0.5 && (width - 635.1).abs() < 0.5);
        // At 150% the same logical area covers 1.5x the device pixels.
        let (_, height) = painted_device_size((4400.0, 4600.0), (928.0, 664.0), 1.5);
        assert!((height - 996.0).abs() < 0.5);
        // A small image keeps its own size in a large window.
        assert_eq!(
            painted_device_size((128.0, 128.0), (3400.0, 1300.0), 1.0),
            (128.0, 128.0)
        );
    }

    #[test]
    fn failure_text_names_the_size_of_a_refused_image() {
        assert_eq!(
            failure_text(InspectorLoadError::Decode(FullResolutionError::TooLarge {
                width: 20_000,
                height: 1
            })),
            "This image is too large to preview (20000 × 1)"
        );
    }
}
