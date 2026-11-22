//! Full-resolution decode for the image preview window.
//!
//! Chat thumbnails go through [`crate::decode`], which shrinks every still to
//! 768 px because no chat surface is larger. The preview window shows one image
//! at a time at up to its native size, so it decodes the original here, under
//! its own bounds:
//!
//! - Source: at most [`MAX_SOURCE_EDGE`] px per axis and
//!   [`MAX_DECODE_ALLOC_BYTES`] of decoder allocation, both checked before the
//!   decoder allocates its canvas, so a decompression bomb is refused rather
//!   than allocated. That admits every ordinary photo and screenshot (a 61 MP
//!   camera frame is 9504x6336, 180 MiB as RGB).
//! - Shown: a still is downscaled, never upscaled, to fit [`MAX_SHOWN_EDGE`] per
//!   axis and [`MAX_SHOWN_PIXELS`] in total, so the retained bitmap (and its one
//!   GPU texture) is at most 128 MiB. 8192 px is the smallest 2D texture limit
//!   GPUI's renderers rely on (wgpu's default; D3D11 allows 16384), and a
//!   single image larger than one atlas texture would fail to paint at all.
//!   Anything that size is already far more than a 3440x1440 or 4K display
//!   shows, so the cap never makes the window less sharp.
//! - Reduced copies: GPUI samples an image bilinearly from one texture with no
//!   mipmaps, so showing a 4400 px photo in a 900 px window would skip most of
//!   its pixels and alias. A still therefore also gets a chain of half-size
//!   copies down to [`MIN_LEVEL_EDGE`], and the window paints the smallest one
//!   that is still at least as large as the area it fills. The chain adds at
//!   most a third to the shown bitmap's size.
//! - Animation (GIF, animated WebP): every frame at its native size while each
//!   frame is at most [`MAX_ANIMATION_FRAME_BYTES`] and the frames together stay
//!   within [`MAX_ANIMATION_BYTES`] and [`MAX_ANIMATION_FRAMES`]; a larger
//!   animation is shown as its first frame.
//!
//! Peak memory of one decode, besides the compressed body (at most 32 MiB):
//!
//! - Still: the decoded source (at most [`MAX_DECODE_ALLOC_BYTES`], 256 MiB)
//!   plus one 8-bit RGBA copy at the shown size (at most 128 MiB), so at most
//!   384 MiB. Every reduction is an integer box filter
//!   (`imageops::thumbnail`), which allocates only its output: `resize_exact`
//!   would build a 32-bit float image of source width by new height (16 bytes
//!   a pixel) beside the source, over 1 GiB for an 8192x8192 PNG. The source is
//!   dropped as soon as its reduced or converted copy exists, and each
//!   half-size copy is made from the RGBA level above it.
//! - Animation: the frames kept so far (at most 128 MiB) plus the decoder's
//!   own canvas and frame buffers and the next frame, each at most
//!   [`MAX_ANIMATION_FRAME_BYTES`]: about 224 MiB at worst.

use gpui::RenderImage;
use image::{AnimationDecoder, Frame, ImageDecoder, ImageFormat, ImageReader};
use std::io::Cursor;
use std::sync::Arc;

/// Largest accepted source width or height, before decoding.
pub const MAX_SOURCE_EDGE: u32 = 16_384;
/// Largest allocation the decoder may make for the source canvas.
pub const MAX_DECODE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
/// Largest shown width or height.
pub const MAX_SHOWN_EDGE: u32 = 8_192;
/// Largest shown pixel count: 128 MiB as BGRA.
pub const MAX_SHOWN_PIXELS: u64 = 32 * 1024 * 1024;
/// Largest total BGRA size of an animation's frames.
pub const MAX_ANIMATION_BYTES: u64 = 128 * 1024 * 1024;
/// Largest BGRA size of one animation frame (4096x2048, for example).
pub const MAX_ANIMATION_FRAME_BYTES: u64 = 32 * 1024 * 1024;
/// Most frames kept from one animation.
pub const MAX_ANIMATION_FRAMES: usize = 1024;
/// The reduced-copy chain stops once a copy's longest edge is at most this.
pub const MIN_LEVEL_EDGE: u32 = 1024;

/// A decoded image for the preview window.
pub struct FullResolutionImage {
    /// The shown bitmap first, then successively half-size copies of it.
    pub levels: Vec<Arc<RenderImage>>,
    /// The source's own size.
    pub source_size: (u32, u32),
    /// The decoded bitmap's size: the source size unless the caps reduced it.
    pub shown_size: (u32, u32),
    pub frame_count: usize,
    /// The source was an animation over the caps, shown as its first frame.
    pub animation_reduced_to_still: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullResolutionError {
    /// The bytes are not a raster format Echo decodes.
    NotAnImage,
    /// The source exceeds [`MAX_SOURCE_EDGE`] or [`MAX_DECODE_ALLOC_BYTES`].
    TooLarge { width: u32, height: u32 },
    /// The decoder failed partway.
    Corrupt,
}

/// The size a `width` x `height` still is shown at: unchanged when it fits the
/// caps, otherwise scaled down, keeping its aspect ratio, to fit them.
pub fn shown_size(width: u32, height: u32) -> (u32, u32) {
    let pixels = u64::from(width) * u64::from(height);
    let edge_scale = f64::from(MAX_SHOWN_EDGE) / f64::from(width.max(height).max(1));
    let pixel_scale = (MAX_SHOWN_PIXELS as f64 / pixels.max(1) as f64).sqrt();
    let scale = edge_scale.min(pixel_scale);
    if scale >= 1.0 {
        return (width, height);
    }
    let scaled = |value: u32| ((f64::from(value) * scale).floor() as u32).max(1);
    (scaled(width), scaled(height))
}

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

fn source_fits(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_SOURCE_EDGE
        && height <= MAX_SOURCE_EDGE
        && u64::from(width) * u64::from(height) * 4 <= MAX_DECODE_ALLOC_BYTES
}

/// The level to paint for an area `target` device pixels in size: the smallest
/// level that still covers it, or the full-size level when none is smaller.
pub fn level_for(level_sizes: &[(u32, u32)], target: (f32, f32)) -> usize {
    level_sizes
        .iter()
        .rposition(|(width, height)| *width as f32 >= target.0 && *height as f32 >= target.1)
        .unwrap_or(0)
}

/// Decode `bytes` for the preview window under the bounds in the module docs.
pub fn decode_full_resolution(bytes: &[u8]) -> Result<FullResolutionImage, FullResolutionError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| FullResolutionError::NotAnImage)?;
    let format = reader.format().ok_or(FullResolutionError::NotAnImage)?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| FullResolutionError::NotAnImage)?;
    if !source_fits(width, height) {
        return Err(FullResolutionError::TooLarge { width, height });
    }

    let mut animation_reduced_to_still = false;
    if matches!(format, ImageFormat::Gif | ImageFormat::WebP) {
        match decode_animation(bytes, format, width, height) {
            Animation::Frames(frames) => {
                let frame_count = frames.len();
                return Ok(FullResolutionImage {
                    levels: vec![Arc::new(RenderImage::new(frames))],
                    source_size: (width, height),
                    shown_size: (width, height),
                    frame_count,
                    animation_reduced_to_still: false,
                });
            }
            Animation::OverCaps => animation_reduced_to_still = true,
            Animation::Still => {}
        }
    }

    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(decode_limits());
    let decoded = reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => FullResolutionError::TooLarge { width, height },
        image::ImageError::Unsupported(_) => FullResolutionError::NotAnImage,
        _ => FullResolutionError::Corrupt,
    })?;
    let shown = shown_size(decoded.width(), decoded.height());
    // Reduce in the source's own pixel type, so a large RGB photo is never
    // widened to RGBA at full size, then let the source go before anything
    // else is allocated (see the peak in the module docs).
    let reduced = if shown == (decoded.width(), decoded.height()) {
        decoded
    } else {
        let reduced = decoded.thumbnail_exact(shown.0, shown.1);
        drop(decoded);
        reduced
    };
    let mut levels = Vec::new();
    let mut level = reduced.into_rgba8();
    loop {
        let next = (level.width().max(level.height()) > MIN_LEVEL_EDGE).then(|| {
            image::imageops::thumbnail(
                &level,
                (level.width() / 2).max(1),
                (level.height() / 2).max(1),
            )
        });
        levels.push(Arc::new(RenderImage::new(vec![bgra_frame(level)])));
        match next {
            Some(next) => level = next,
            None => break,
        }
    }
    Ok(FullResolutionImage {
        levels,
        source_size: (width, height),
        shown_size: shown,
        frame_count: 1,
        animation_reduced_to_still,
    })
}

enum Animation {
    Frames(Vec<Frame>),
    /// A single-frame GIF or WebP: decode it as a still.
    Still,
    /// More frames or bytes than the caps allow.
    OverCaps,
}

fn decode_animation(bytes: &[u8], format: ImageFormat, width: u32, height: u32) -> Animation {
    let frames = match format {
        ImageFormat::Gif => image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
            .and_then(|mut decoder| {
                decoder.set_limits(decode_limits())?;
                Ok(decoder.into_frames())
            })
            .map(|frames| collect_frames(frames, width, height)),
        ImageFormat::WebP => {
            image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).and_then(|mut decoder| {
                if !decoder.has_animation() {
                    return Ok(Animation::Still);
                }
                decoder.set_limits(decode_limits())?;
                Ok(collect_frames(decoder.into_frames(), width, height))
            })
        }
        _ => return Animation::Still,
    };
    frames.unwrap_or(Animation::Still)
}

fn collect_frames(frames: image::Frames<'_>, width: u32, height: u32) -> Animation {
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    if width.max(height) > MAX_SHOWN_EDGE || frame_bytes > MAX_ANIMATION_FRAME_BYTES {
        return Animation::OverCaps;
    }
    let mut collected = Vec::new();
    for frame in frames {
        // Checked before this frame joins the kept ones, so the kept frames
        // never exceed the caps; the frame just decoded is the one extra.
        if collected.len() >= MAX_ANIMATION_FRAMES
            || frame_bytes.saturating_mul(collected.len() as u64 + 1) > MAX_ANIMATION_BYTES
        {
            return Animation::OverCaps;
        }
        let Ok(frame) = frame else {
            // A broken frame mid-stream: show the first frame as a still
            // rather than loop a partial animation.
            return if collected.is_empty() {
                Animation::Still
            } else {
                Animation::OverCaps
            };
        };
        if frame.buffer().dimensions() != (width, height) {
            return Animation::Still;
        }
        let delay = frame.delay();
        let mut buffer = frame.into_buffer();
        crate::decode::convert_to_gpu_format_in_place(&mut buffer);
        collected.push(Frame::from_parts(buffer, 0, 0, delay));
    }
    if collected.len() > 1 {
        Animation::Frames(collected)
    } else {
        Animation::Still
    }
}

fn bgra_frame(mut buffer: image::RgbaImage) -> Frame {
    crate::decode::convert_to_gpu_format_in_place(&mut buffer);
    Frame::new(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, DynamicImage, RgbaImage};

    fn encode(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn shown_size_never_upscales_and_fits_both_caps() {
        assert_eq!(shown_size(300, 300), (300, 300));
        assert_eq!(shown_size(4400, 4600), (4400, 4600));
        assert_eq!(shown_size(7680, 4320), (7680, 4320));
        // Over the edge cap: longest edge becomes 8192.
        assert_eq!(shown_size(16_000, 2_000), (8192, 1024));
        // Over the pixel cap: 9504x6336 is 60 MP, scaled to at most 32 Mi pixels.
        let (width, height) = shown_size(9504, 6336);
        assert!(u64::from(width) * u64::from(height) <= MAX_SHOWN_PIXELS);
        assert!(width.max(height) <= MAX_SHOWN_EDGE);
        assert!((f64::from(width) / f64::from(height) - 1.5).abs() < 0.01);
    }

    #[test]
    fn a_large_still_decodes_at_its_native_size() {
        let image = DynamicImage::ImageRgb8(image::RgbImage::new(3000, 2000));
        let decoded = decode_full_resolution(&encode(&image, ImageFormat::Png)).unwrap();
        assert_eq!(decoded.source_size, (3000, 2000));
        assert_eq!(decoded.shown_size, (3000, 2000));
        let sizes: Vec<_> = decoded
            .levels
            .iter()
            .map(|level| (level.size(0).width.0, level.size(0).height.0))
            .collect();
        assert_eq!(sizes, vec![(3000, 2000), (1500, 1000), (750, 500)]);
        assert_eq!(decoded.frame_count, 1);
    }

    #[test]
    fn the_painted_level_is_the_smallest_that_covers_the_area() {
        let sizes = [(4400, 4600), (2200, 2300), (1100, 1150), (550, 575)];
        // The default 960x720 window fills about 660x690 with this image.
        assert_eq!(level_for(&sizes, (660.0, 690.0)), 2);
        // Maximized on a 3440x1440 display: about 1280x1340.
        assert_eq!(level_for(&sizes, (1280.0, 1340.0)), 1);
        assert_eq!(level_for(&sizes, (300.0, 300.0)), 3);
        // Larger than the image: the full-size level, never anything smaller.
        assert_eq!(level_for(&sizes, (5000.0, 5000.0)), 0);
    }

    #[test]
    fn decompression_bombs_are_refused_before_decoding() {
        // A 1x20000 PNG is tiny on disk; its header alone exceeds the edge cap.
        let image = DynamicImage::ImageLuma8(image::GrayImage::new(1, 20_000));
        assert_eq!(
            decode_full_resolution(&encode(&image, ImageFormat::Png)).err(),
            Some(FullResolutionError::TooLarge {
                width: 1,
                height: 20_000
            })
        );
        // 16384x16384 fits the edge cap but needs 1 GiB as RGBA.
        assert!(!source_fits(16_384, 16_384));
        assert!(source_fits(9504, 6336));
    }

    #[test]
    fn non_images_are_rejected() {
        assert_eq!(
            decode_full_resolution(b"<!doctype html><html></html>").err(),
            Some(FullResolutionError::NotAnImage)
        );
    }

    #[test]
    fn animated_gifs_keep_every_frame_at_native_size() {
        let frames = (0..3).map(|index| {
            let mut buffer = RgbaImage::new(40, 30);
            buffer.put_pixel(index, 0, image::Rgba([255, 0, 0, 255]));
            Frame::from_parts(buffer, 0, 0, Delay::from_numer_denom_ms(70, 1))
        });
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.encode_frames(frames).unwrap();
        }
        let decoded = decode_full_resolution(&bytes).unwrap();
        assert_eq!(decoded.frame_count, 3);
        assert_eq!(decoded.levels.len(), 1);
        assert_eq!(decoded.levels[0].frame_count(), 3);
        assert_eq!(decoded.shown_size, (40, 30));
        assert!(!decoded.animation_reduced_to_still);
    }

    fn gif(width: u32, height: u32, frames: usize) -> Vec<u8> {
        let frames = (0..frames).map(|index| {
            let mut buffer = RgbaImage::new(width, height);
            buffer.put_pixel(index as u32 % width, 0, image::Rgba([255, 0, 0, 255]));
            Frame::from_parts(buffer, 0, 0, Delay::from_numer_denom_ms(20, 1))
        });
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut bytes, 30);
            encoder.encode_frames(frames).unwrap();
        }
        bytes
    }

    #[test]
    fn an_animation_over_its_caps_shows_its_first_frame() {
        let decoded = decode_full_resolution(&gif(4, 3, MAX_ANIMATION_FRAMES + 1)).unwrap();
        assert!(decoded.animation_reduced_to_still);
        assert_eq!(decoded.frame_count, 1);
        assert_eq!(decoded.levels[0].frame_count(), 1);
        assert_eq!(decoded.shown_size, (4, 3));
    }

    /// Bytes allocated on the measuring thread, counted only inside
    /// [`peak_allocation`]. The decoder runs on the calling thread, so tests
    /// running in parallel do not disturb the count.
    mod counting {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static COUNTING: Cell<bool> = const { Cell::new(false) };
            static CURRENT: Cell<isize> = const { Cell::new(0) };
            static PEAK: Cell<isize> = const { Cell::new(0) };
        }

        fn record(delta: isize) {
            let _ = COUNTING.try_with(|counting| {
                if counting.get() {
                    let current = CURRENT.get() + delta;
                    CURRENT.set(current);
                    PEAK.set(PEAK.get().max(current));
                }
            });
        }

        pub struct ThreadCountingAllocator;

        // SAFETY: every call forwards to `System` unchanged; the bookkeeping
        // only touches allocation-free thread-local cells.
        unsafe impl GlobalAlloc for ThreadCountingAllocator {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                let pointer = unsafe { System.alloc(layout) };
                if !pointer.is_null() {
                    record(layout.size() as isize);
                }
                pointer
            }

            unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
                let pointer = unsafe { System.alloc_zeroed(layout) };
                if !pointer.is_null() {
                    record(layout.size() as isize);
                }
                pointer
            }

            unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
                unsafe { System.dealloc(pointer, layout) };
                record(-(layout.size() as isize));
            }

            unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
                let moved = unsafe { System.realloc(pointer, layout, size) };
                if !moved.is_null() {
                    record(size as isize - layout.size() as isize);
                }
                moved
            }
        }

        /// Run `work` and return its result with the most bytes it held at once.
        pub fn peak_allocation<T>(work: impl FnOnce() -> T) -> (T, usize) {
            CURRENT.set(0);
            PEAK.set(0);
            COUNTING.set(true);
            let result = work();
            COUNTING.set(false);
            (result, PEAK.get().max(0) as usize)
        }
    }

    #[global_allocator]
    static ALLOCATOR: counting::ThreadCountingAllocator = counting::ThreadCountingAllocator;

    const MIB: usize = 1024 * 1024;

    #[test]
    fn decoding_stays_within_the_stated_peak() {
        // A one-colour 8192x8192 PNG is about 1 MiB on disk and anyone can
        // link it: 256 MiB decoded, shown at 5792x5792 (128 MiB). The bound
        // is the decoded source plus one shown-size RGBA copy (384 MiB), with
        // a little room for the decoder's own buffers.
        let png = encode(
            &DynamicImage::ImageRgba8(RgbaImage::new(8192, 8192)),
            ImageFormat::Png,
        );
        let (decoded, peak) = counting::peak_allocation(|| decode_full_resolution(&png).unwrap());
        assert_eq!(decoded.shown_size, (5792, 5792));
        assert!(peak <= 400 * MIB, "8192x8192 peak {} MiB", peak / MIB);
        drop(decoded);

        // An RGB photo at the shown-pixel cap keeps its size; its RGBA copy and
        // reduced copies must not stack on the source.
        let jpeg = encode(
            &DynamicImage::ImageRgb8(image::RgbImage::new(4400, 4600)),
            ImageFormat::Jpeg,
        );
        let (decoded, peak) = counting::peak_allocation(|| decode_full_resolution(&jpeg).unwrap());
        assert_eq!(decoded.shown_size, (4400, 4600));
        assert!(peak <= 200 * MIB, "4400x4600 peak {} MiB", peak / MIB);
    }

    #[test]
    fn a_large_animation_stays_within_the_stated_peak() {
        // 16 frames of 1024x1024 (4 MiB each) fill half the animation budget.
        let bytes = gif(1024, 1024, 16);
        let (decoded, peak) = counting::peak_allocation(|| decode_full_resolution(&bytes).unwrap());
        assert_eq!(decoded.frame_count, 16);
        // The kept frames (64 MiB) plus a few frame-sized decoder buffers.
        assert!(
            peak <= 64 * MIB + 5 * 4 * MIB,
            "animation peak {} MiB",
            peak / MIB
        );
    }
}
