//! Direct image paint preparation and paint execution.

use super::direct_paint::PreparedImagePaintCommand;
use super::*;

/// Optional audit list for direct-paint image URLs.
/// Set `ECHO_AUDIT_EMOTE_IDS` to comma-separated substrings (e.g. 7TV emote IDs).
static AUDIT_EMOTE_ID_FILTERS: LazyLock<Vec<String>> = LazyLock::new(|| {
    echo_core::env::value("ECHO_AUDIT_EMOTE_IDS")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
});

#[inline]
fn should_audit_emote_url(url: &str) -> bool {
    if AUDIT_EMOTE_ID_FILTERS.is_empty() {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    AUDIT_EMOTE_ID_FILTERS
        .iter()
        .any(|needle| lower.contains(needle))
}

#[inline]
pub(super) fn image_load_state_tag(state: &image_cache::ImageLoadState) -> u8 {
    match state {
        image_cache::ImageLoadState::Loading => 0,
        image_cache::ImageLoadState::Loaded => 1,
        image_cache::ImageLoadState::Failed => 2,
    }
}

#[inline]
pub(super) fn animated_cache_state_tag(state: &image_cache::AnimatedCacheState) -> u8 {
    match state {
        image_cache::AnimatedCacheState::NotLoaded => 0,
        image_cache::AnimatedCacheState::Loading => 1,
        image_cache::AnimatedCacheState::Ready => 2,
        image_cache::AnimatedCacheState::Failed => 3,
    }
}

pub(super) fn prepare_image_paint_commands(
    layout: &CanvasMessageLayout,
) -> SmallVec<[PreparedImagePaintCommand; 8]> {
    let mut prepared = SmallVec::with_capacity(layout.image_paint_commands.len());
    for (command_index, cmd) in layout.image_paint_commands.iter().enumerate() {
        prepared.push(PreparedImagePaintCommand {
            command_index,
            static_render_image: if cmd.is_animated {
                None
            } else {
                image_cache::get_cached_render_image(cmd.url.as_ref())
            },
        });
    }

    prepared
}

/// Where the viewport canvas records the animated images it paints as slots.
pub(super) struct AnimationSlotSink<'a> {
    pub(super) slots: &'a mut PaintedAnimationSlots,
    /// Identifies the painting split, so keys are unique within the window.
    pub(super) owner: u64,
    /// Message index of the row being painted. Two rows can share one layout
    /// `Arc`, so the layout alone does not identify a painted image.
    pub(super) row: usize,
}

impl AnimationSlotSink<'_> {
    /// Window-unique key for one painted image command of one row layout.
    fn key(&self, layout: &CanvasMessageLayout, command_index: usize) -> u64 {
        self.key_for(layout, 0, command_index)
    }

    /// Window-unique key for the name paint of one shaped line of one row layout.
    pub(super) fn name_paint_key(&self, layout: &CanvasMessageLayout, line_index: usize) -> u64 {
        self.key_for(layout, 1, line_index)
    }

    fn key_for(&self, layout: &CanvasMessageLayout, kind: u8, index: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        self.owner.hash(&mut hasher);
        self.row.hash(&mut hasher);
        (layout as *const CanvasMessageLayout as usize).hash(&mut hasher);
        kind.hash(&mut hasher);
        index.hash(&mut hasher);
        hasher.finish()
    }

    /// Paint a name-paint overlay, as a slot when the paint is animated.
    pub(super) fn paint_name_overlay(
        &mut self,
        overlay: &super::super::seventv_paint::PaintedLineOverlay,
        spec: &Arc<super::super::seventv_paint::SevenTvLinePaint>,
        shaped_line: &Rc<ShapedLine>,
        line_height: f32,
        layout: &CanvasMessageLayout,
        line_index: usize,
        position: Point<Pixels>,
        window: &mut Window,
    ) {
        if !spec.is_animated() {
            overlay.paint(position, window);
            return;
        }
        let key = self.name_paint_key(layout, line_index);
        if !overlay.paint_animated(position, key, window) {
            return;
        }
        self.slots.slots.push((
            key,
            AnimationSlotSource::NamePaint {
                spec: Arc::clone(spec),
                shaped_line: Rc::clone(shaped_line),
                font_size: f32::from(shaped_line.font_size),
                line_height,
                geometry: overlay.geometry(),
            },
        ));
    }
}

/// Paint all of a layout's images directly using window.paint_image(),
/// recording animated ones as slots.
#[inline]
pub(super) fn paint_images_direct(
    layout: &CanvasMessageLayout,
    origin: Point<Pixels>,
    window: &mut Window,
    sink: &mut AnimationSlotSink<'_>,
) {
    for (command_index, cmd) in layout.image_paint_commands.iter().enumerate() {
        let key = sink.key(layout, command_index);
        paint_single_image_direct(cmd, origin, window, Some((sink, key)));
    }
}

/// Paint a single image directly to GPU.
///
/// With `slot`, an animated image showing a decoded frame is painted as a
/// patchable slot and recorded in the sink.
#[inline]
fn paint_single_image_direct(
    cmd: &ImagePaintCommand,
    origin: Point<Pixels>,
    window: &mut Window,
    slot: Option<(&mut AnimationSlotSink<'_>, u64)>,
) {
    let bounds = Bounds::new(
        origin + point(px(cmd.x), px(cmd.y)),
        size(px(cmd.width), px(cmd.height)),
    );
    if let Some(background) = cmd.background {
        window.paint_quad(fill(bounds, background));
    }

    let render_image = if cmd.is_animated {
        crate::log_emotes!(trace, "[PAINT] Animated path for url={}", cmd.url);
        match image_cache::get_current_animated_render_image(cmd.url.as_ref()) {
            Some(img) => {
                if let Some((sink, key)) = slot {
                    // Fully clipped images paint nothing and have no slot.
                    if let Ok(true) = window.paint_animated_image(
                        key,
                        bounds,
                        bounds,
                        Corners::default(),
                        img,
                        0,
                        false,
                    ) {
                        sink.slots
                            .slots
                            .push((key, AnimationSlotSource::Image(cmd.url.clone())));
                    }
                    if cmd.is_emote {
                        crate::diagnostics::startup_metrics::record(
                            crate::diagnostics::startup_metrics::StartupMarker::FirstVisibleEmotePaint,
                        );
                    }
                    return;
                }
                Some(img)
            }
            _ => {
                let cache_state = image_cache::get_animated_cache_state(cmd.url.as_ref());
                // A failed animation stays on its static image, which never
                // changes frame; the image-load waker repaints a later recovery.
                if let Some((sink, _)) = slot
                    && !matches!(cache_state, image_cache::AnimatedCacheState::Failed)
                {
                    sink.slots.unslotted |=
                        1 << (UNSLOTTED_IMAGE_STATE_SHIFT + animated_cache_state_tag(&cache_state));
                }
                match cache_state {
                    image_cache::AnimatedCacheState::NotLoaded => {
                        crate::backend::metrics::record_emote_event("canvas.paint.animated_miss");
                    }
                    image_cache::AnimatedCacheState::Loading => {
                        crate::backend::metrics::record_emote_event(
                            "canvas.paint.animated_loading",
                        );
                    }
                    _ => {}
                }
                image_cache::get_cached_render_image(cmd.url.as_ref())
            }
        }
    } else {
        let img = image_cache::get_cached_render_image(cmd.url.as_ref());
        if img.is_none() {
            crate::backend::metrics::record_emote_event("canvas.paint.static_miss_placeholder");
            if should_audit_emote_url(cmd.url.as_ref()) {
                crate::log_emotes!(
                    debug,
                    "[PAINT-AUDIT] Static miss in direct paint: url={}, state={:?}, downloading={}",
                    cmd.url,
                    image_cache::get_state(cmd.url.as_ref()),
                    image_cache::is_downloading(cmd.url.as_ref())
                );
            }
        }
        img
    };

    if let Some(img) = render_image {
        let _ = window.paint_image(bounds, bounds, Corners::default(), img, 0, false);
        if cmd.is_emote {
            crate::diagnostics::startup_metrics::record(
                crate::diagnostics::startup_metrics::StartupMarker::FirstVisibleEmotePaint,
            );
        }
    } else {
        let (state, is_downloading) = image_cache::get_state_and_downloading(cmd.url.as_ref());
        if should_audit_emote_url(cmd.url.as_ref()) {
            crate::log_emotes!(
                debug,
                "[PAINT-AUDIT] Placeholder path: url={}, state={:?}, downloading={}",
                cmd.url,
                state,
                is_downloading
            );
        }

        match state {
            image_cache::ImageLoadState::Failed => {
                if image_cache::retry_failed_media(
                    cmd.url.as_ref(),
                    cmd.is_animated,
                    image_cache::PreloadSource::VisibleChat,
                ) {
                    crate::backend::metrics::record_emote_event("canvas.paint.retry_scheduled");
                }
                window.paint_quad(fill(bounds, colors::bg_elevated()).corner_radii(px(3.0)));
            }
            _ if is_downloading => {
                window.paint_quad(fill(bounds, hsla(0.0, 0.0, 0.3, 0.15)).corner_radii(px(3.0)));
            }
            _ => {
                window.paint_quad(fill(bounds, hsla(0.0, 0.0, 0.2, 0.1)).corner_radii(px(3.0)));
            }
        }
    }
}

#[inline]
pub(super) fn paint_prepared_images_direct(
    layout: &CanvasMessageLayout,
    commands: &[PreparedImagePaintCommand],
    origin: Point<Pixels>,
    window: &mut Window,
    sink: &mut AnimationSlotSink<'_>,
) {
    let source_commands = &layout.image_paint_commands;
    for prepared in commands {
        let Some(command) = source_commands.get(prepared.command_index) else {
            continue;
        };
        if !command.is_animated
            && let Some(img) = prepared.static_render_image.as_ref()
        {
            let bounds = Bounds::new(
                origin + point(px(command.x), px(command.y)),
                size(px(command.width), px(command.height)),
            );
            if let Some(background) = command.background {
                window.paint_quad(fill(bounds, background));
            }
            let _ = window.paint_image(
                bounds,
                bounds,
                Corners::default(),
                Arc::clone(img),
                0,
                false,
            );
            if command.is_emote {
                crate::diagnostics::startup_metrics::record(
                    crate::diagnostics::startup_metrics::StartupMarker::FirstVisibleEmotePaint,
                );
            }
            continue;
        }

        let key = sink.key(layout, prepared.command_index);
        paint_single_image_direct(command, origin, window, Some((sink, key)));
    }
}
