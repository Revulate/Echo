//! Pure image decoding functions for GPUI RenderImage
//!
//! Separates CPU-bound decode work from I/O and caching.
//! Handles BGRA conversion for GPUI/Blade/Vulkan GPU compatibility.
//!
//! # Platform Notes
//! GPUI uses Blade graphics with Vulkan on all platforms (Windows, Linux, macOS).
//! Vulkan expects BGRA pixel format. The `image` crate decodes to RGBA, so we
//! always convert to BGRA for GPU compatibility.

use gpui::RenderImage;
use image::{AnimationDecoder, Delay, Frame, ImageFormat, ImageReader, RgbaImage};
use std::io::Cursor;
use std::sync::Arc;

/// One decoded animation frame stored in the compact refill-source format.
///
/// Animation decoders yield fully composited BGRA frames. Compressing each
/// frame before requesting the next one avoids retaining the complete raw frame
/// stack (up to 20 MiB per emote) until the bounded hot-frame window is built.
#[derive(Debug)]
pub struct CompressedDecodedFrameData {
    /// zstd-compressed BGRA pixel data.
    pub compressed: Vec<u8>,
    /// Length of the decompressed BGRA buffer.
    pub raw_len: usize,
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Frame delay in milliseconds
    pub delay_ms: u32,
}

/// Why an animation decode ended before reaching the container's final frame.
///
/// An incomplete frame prefix must never be registered as a looping animation:
/// doing so shortens the authored cycle and makes the result appear too fast or
/// reduced-frame. Callers may still inspect the prefix for diagnostics, but the
/// production loader accepts only [`DecodedAnimationData::is_complete`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationDecodeTruncation {
    FrameLimit,
    FrameTooLarge,
    DecodeWorkLimit,
    CompressedSourceLimit,
    FrameDecodeError,
}

impl AnimationDecodeTruncation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FrameLimit => "frame_limit",
            Self::FrameTooLarge => "frame_too_large",
            Self::DecodeWorkLimit => "decode_work_limit",
            Self::CompressedSourceLimit => "compressed_source_limit",
            Self::FrameDecodeError => "frame_decode_error",
        }
    }

    /// Whether a lower-resolution copy of the same animation could decode
    /// completely. Only the frame count is the same at every resolution.
    pub const fn smaller_copy_may_fit(self) -> bool {
        !matches!(self, Self::FrameLimit)
    }
}

/// Result of decoding an animation for playback.
#[derive(Debug)]
pub enum AnimatedDecodeOutcome {
    /// Every frame decoded; safe to register as a looping animation.
    Complete(DecodedAnimationData),
    /// The decode stopped before the final frame.
    Truncated(AnimationDecodeTruncation),
    /// Not an animated GIF/WebP, or the container could not be read.
    NotAnimated,
}

/// Streaming decode result for an animation.
#[derive(Debug)]
pub struct DecodedAnimationData {
    pub frames: Vec<CompressedDecodedFrameData>,
    /// Total bytes the same frames would occupy as a retained raw BGRA stack.
    pub raw_frame_bytes: usize,
    /// Resident byte estimate for the compressed refill source.
    pub compressed_source_bytes: usize,
    /// Present when only a prefix of the source animation was decoded.
    pub truncation: Option<AnimationDecodeTruncation>,
}

impl DecodedAnimationData {
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.truncation.is_none()
    }
}

/// Maximum frames decoded from one animation container.
///
/// Hot CPU/GPU residency remains independently bounded to a tiny visible
/// window. The larger source ceiling prevents common 256+ frame emotes from
/// being mistaken for complete 256-frame loops while still bounding decode
/// work for pathological inputs.
const MAX_FRAMES_PER_EMOTE: usize = 1024;

/// Maximum decoded RAM for one static image.
///
/// Animated inputs use separate transient-frame, cumulative-work, retained
/// compressed-source, and frame-count limits below.
const MAX_DECODED_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Maximum cumulative full-canvas pixel work for one animation decode.
///
/// Frames are compressed as they stream, so this is a CPU/work bound rather
/// than resident source-cache size. It permits complete wide/high-frame emotes
/// that exceeded the old 20 MiB raw-stack proxy without retaining that raw
/// stack in memory.
const MAX_ANIMATION_DECODE_WORK_BYTES: u64 = 256 * 1024 * 1024;

/// Maximum transient full-canvas frame accepted from an animation decoder.
const MAX_ANIMATION_RAW_FRAME_BYTES: u64 = 8 * 1024 * 1024;

/// Maximum resident compressed refill source for one animation.
///
/// This measures the representation Echo actually retains. A source that
/// exceeds it is rejected as an animation rather than looping a partial prefix.
const MAX_ANIMATION_COMPRESSED_SOURCE_BYTES: usize = 20 * 1024 * 1024;

/// Longest-edge ceiling for decoded *static* images.
///
/// Sources larger than this are downscaled rather than dropped. Sites routinely
/// point OpenGraph `og:image` at a full-resolution upload (e.g. olrite serves a
/// 3839x2159 original), which decodes to ~33 MB and blew past
/// [`MAX_DECODED_IMAGE_BYTES`] — leaving the embed thumbnail permanently blank
/// while it re-downloaded on every repaint.
///
/// The atlas has no mipmaps, so a source far above its display size costs
/// memory and upload bandwidth and aliases when minified. The largest surfaces
/// fed by this decoder are the large embed thumbnail (240 px wide) and the
/// emote tooltip frame (236 px wide); 768 px covers either at 300% DPI.
/// Provider emote and badge copies (≤ 128 px tall, picker aspect cap 4:1, so
/// ≤ 512 px wide at 4x) stay below it, so layout that derives logical emote
/// size from decoded dimensions is unaffected. The large inspector window
/// loads through GPUI's URI image path, not this decoder. One decoded copy is
/// bounded to ≤ ~2.4 MB.
const MAX_STATIC_IMAGE_EDGE: u32 = 768;

/// Pre-downscale decoder ceilings for static sources, so a decompression bomb
/// is rejected before the decoder allocates its canvas.
const MAX_STATIC_DECODE_EDGE: u32 = 8192;
const MAX_STATIC_DECODE_ALLOC_BYTES: u64 = 128 * 1024 * 1024;

/// Minimum frame duration supported by Echo's default 50 Hz animation cadence.
///
/// Keeping decoded timing at the same 20 ms floor prevents sub-cadence sources
/// from advancing two or more authored frames between paints and appearing to
/// drop poses. Explicit higher-FPS diagnostics may wake faster, but canonical
/// metadata remains stable across chat, inline picker, and picker popout.
const MIN_FRAME_DURATION_MS: u32 = 20;

/// Default frame duration when not specified
const DEFAULT_FRAME_DURATION_MS: u32 = 100;

/// GIF legacy delay threshold - delays 0-10ms should be treated as 100ms
/// per browser convention (GIF spec stores delays in centiseconds, so 0-1cs = 0-10ms)
const GIF_LEGACY_DELAY_THRESHOLD_MS: u32 = 10;

/// zstd level for the in-RAM animation refill source. Level 3 keeps the
/// per-frame streaming compression cheap while substantially reducing the
/// retained source representation.
const ANIMATION_SOURCE_ZSTD_LEVEL: i32 = 3;

#[inline]
fn normalize_animation_delay_ms(raw_ms: u32) -> u32 {
    if raw_ms <= GIF_LEGACY_DELAY_THRESHOLD_MS {
        DEFAULT_FRAME_DURATION_MS
    } else {
        raw_ms.max(MIN_FRAME_DURATION_MS)
    }
}

#[inline]
fn compressed_frame_allocation_bytes(frame: &CompressedDecodedFrameData) -> usize {
    frame.compressed.capacity()
}

#[inline]
pub fn compressed_animation_source_allocation_bytes(
    frames: &[CompressedDecodedFrameData],
) -> usize {
    frames
        .iter()
        .map(compressed_frame_allocation_bytes)
        .sum::<usize>()
        .saturating_add(std::mem::size_of_val(frames))
}

// ============================================================================
// Public API
// ============================================================================

/// Decode raw image bytes to a GPUI RenderImage
///
/// Supports PNG, JPEG, WebP, and GIF formats (single frame for GIF).
/// Returns None if decoding fails.
///
/// **Note**: Converts RGBA to BGRA for Vulkan GPU compatibility (GPUI/Blade).
pub fn decode_to_render_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    // Detect and decode through the explicitly enabled, portable formats.
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_STATIC_DECODE_EDGE);
    limits.max_image_height = Some(MAX_STATIC_DECODE_EDGE);
    limits.max_alloc = Some(MAX_STATIC_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let dynamic_image = reader.decode().ok()?;

    // Downscale oversized sources instead of dropping them. An OpenGraph image
    // pointing at a full-resolution upload (e.g. a 4K original) would otherwise
    // exceed the decode cap and render blank; every surface fed by this decoder
    // is small, so a copy near display size is plenty and keeps it bounded.
    let dynamic_image = downscale_static_to_budget(dynamic_image);

    if decoded_rgba_bytes(dynamic_image.width(), dynamic_image.height(), 1)?
        > MAX_DECODED_IMAGE_BYTES
    {
        crate::log_cache!(
            debug,
            width = dynamic_image.width(),
            height = dynamic_image.height(),
            max_bytes = MAX_DECODED_IMAGE_BYTES,
            "Skipping oversized static image decode after downscale"
        );
        return None;
    }

    // Consume the decoded image so an existing RGBA8 backing buffer can be
    // transferred instead of cloned during warm-cache hydration.
    let rgba = dynamic_image.into_rgba8();

    decode_rgba_to_render_image(rgba)
}

/// Downscale a decoded static image so its longest edge is at most
/// [`MAX_STATIC_IMAGE_EDGE`], preserving aspect ratio. Returns the image
/// unchanged when it already fits. Guarantees the result stays under
/// [`MAX_DECODED_IMAGE_BYTES`] (768² × 4 ≈ 2.4 MB).
fn downscale_static_to_budget(image: image::DynamicImage) -> image::DynamicImage {
    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    if longest <= MAX_STATIC_IMAGE_EDGE {
        return image;
    }
    let scale = MAX_STATIC_IMAGE_EDGE as f32 / longest as f32;
    let new_w = ((width as f32 * scale).round() as u32).max(1);
    let new_h = ((height as f32 * scale).round() as u32).max(1);
    image.resize(new_w, new_h, image::imageops::FilterType::Triangle)
}

/// Convert an RGBA image to GPUI RenderImage with platform-specific BGRA conversion
///
/// This is useful when you already have decoded RGBA data (e.g., from animation frames).
///
/// **Platform Note**: On Windows, converts RGBA to BGRA for Vulkan GPU compatibility.
/// On other platforms, passes through RGBA unchanged.
pub fn decode_rgba_to_render_image(rgba: RgbaImage) -> Option<Arc<RenderImage>> {
    let width = rgba.width();
    let height = rgba.height();

    // Platform-specific BGRA conversion
    let pixel_data = convert_to_gpu_format(rgba.into_raw());

    // Reconstruct image with converted data
    // Note: Type is still RgbaImage but data is BGRA on Windows
    let final_image = RgbaImage::from_raw(width, height, pixel_data)?;

    // Create Frame for GPUI (static image with no delay)
    let delay = Delay::from_numer_denom_ms(0, 1);
    let frame = Frame::from_parts(final_image, 0, 0, delay);

    Some(Arc::new(RenderImage::new(vec![frame])))
}

/// Convert raw RGBA bytes to GPU-compatible format
///
/// On Windows/Vulkan: Converts RGBA to BGRA (swaps R and B channels)
/// On other platforms: Returns unchanged
///
/// This is useful for bulk frame conversion where you have raw Vec<u8> data.
#[inline]
pub fn convert_to_gpu_format(rgba_data: Vec<u8>) -> Vec<u8> {
    convert_rgba_to_bgra_if_needed(rgba_data)
}

/// Convert raw RGBA bytes to GPU-compatible format (in-place on mutable reference)
///
/// On Windows/Vulkan: Converts RGBA to BGRA (swaps R and B channels)
/// On other platforms: No-op
///
/// This is the most efficient version when you already have mutable access.
#[inline]
pub fn convert_to_gpu_format_in_place(rgba_data: &mut [u8]) {
    convert_rgba_to_bgra_in_place(rgba_data);
}

/// Decode animated image frames from raw bytes
///
/// Returns None if the image is not animated or decoding fails.
/// Supports GIF and WebP formats.
pub fn decode_animated_frames(bytes: &[u8]) -> Option<DecodedAnimationData> {
    match decode_animated_frames_outcome(bytes) {
        AnimatedDecodeOutcome::Complete(decoded) => Some(decoded),
        AnimatedDecodeOutcome::Truncated(_) | AnimatedDecodeOutcome::NotAnimated => None,
    }
}

/// Decode animated frames and report why an incomplete decode stopped, so the
/// loader can retry a lower-resolution copy when a size limit was the cause.
pub fn decode_animated_frames_outcome(bytes: &[u8]) -> AnimatedDecodeOutcome {
    let Some(decoded) = inspect_animated_frames(bytes) else {
        return AnimatedDecodeOutcome::NotAnimated;
    };
    if let Some(truncation) = decoded.truncation {
        crate::log_cache!(
            warn,
            reason = truncation.as_str(),
            decoded_frames = decoded.frames.len(),
            raw_frame_bytes = decoded.raw_frame_bytes,
            compressed_source_bytes = decoded.compressed_source_bytes,
            "Refusing to register an incomplete animation cycle"
        );
        return AnimatedDecodeOutcome::Truncated(truncation);
    }
    AnimatedDecodeOutcome::Complete(decoded)
}

/// Decode animation frames while preserving whether the container was fully
/// consumed. This is intended for the production decoder checker and other
/// diagnostics; ordinary rendering must use [`decode_animated_frames`].
pub fn inspect_animated_frames(bytes: &[u8]) -> Option<DecodedAnimationData> {
    decode_animated_frames_with_limit(bytes, MAX_FRAMES_PER_EMOTE)
}

fn decode_animated_frames_with_limit(
    bytes: &[u8],
    max_frames: usize,
) -> Option<DecodedAnimationData> {
    let cursor = Cursor::new(bytes);

    // Try to detect format
    let format = ImageReader::new(cursor)
        .with_guessed_format()
        .ok()?
        .format()?;

    let frames = match format {
        ImageFormat::Gif => decode_gif_frames(bytes, max_frames)?,
        ImageFormat::WebP => decode_webp_frames(bytes, max_frames)?,
        _ => {
            crate::log_cache!(trace, "Not an animated format: format={:?}", format);
            return None;
        }
    };

    if frames.truncation.is_none() && frames.frames.len() <= 1 {
        crate::log_cache!(
            trace,
            "Not animated (single frame): frames={}",
            frames.frames.len()
        );
        return None;
    }

    crate::log_cache!(
        debug,
        frame_count = frames.frames.len(),
        raw_frame_bytes = frames.raw_frame_bytes,
        compressed_source_bytes = frames.compressed_source_bytes,
        complete = frames.is_complete(),
        truncation = frames.truncation.map(AnimationDecodeTruncation::as_str),
        "Decoded animated image"
    );

    Some(frames)
}

/// Decode GIF frames using the image crate's built-in disposal handling.
///
/// The image crate's `GifDecoder::into_frames()` handles GIF disposal methods
/// internally (restore-to-background, restore-to-previous, do-not-dispose)
/// and returns fully composited full-canvas frames. We use these directly
/// to avoid double-compositing artifacts (frame trails).
fn decode_gif_frames(data: &[u8], max_frames: usize) -> Option<DecodedAnimationData> {
    use image::ImageDecoder;

    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(data)).ok()?;
    let (screen_width, screen_height) = decoder.dimensions();
    let raw_frame_size = decoded_rgba_bytes(screen_width, screen_height, 1)?;
    if raw_frame_size > MAX_ANIMATION_RAW_FRAME_BYTES {
        return Some(DecodedAnimationData {
            frames: Vec::new(),
            raw_frame_bytes: 0,
            compressed_source_bytes: 0,
            truncation: Some(AnimationDecodeTruncation::FrameTooLarge),
        });
    }
    let frames_iter = decoder.into_frames();

    let mut frames = Vec::new();
    let mut raw_frame_bytes = 0usize;
    let mut compressed_source_bytes = 0usize;
    let mut truncation = None;
    // Running full-canvas accumulator for the sub-frame fallback below.
    let mut canvas: Option<RgbaImage> = None;

    for (i, frame_result) in frames_iter.enumerate() {
        if i >= max_frames {
            crate::log_cache!(debug, "Truncating animation frames: max={}", max_frames);
            truncation = Some(AnimationDecodeTruncation::FrameLimit);
            break;
        }

        let frame = match frame_result {
            Ok(frame) => frame,
            Err(err) => {
                crate::log_cache!(
                    debug,
                    frame_index = i,
                    error = %err,
                    "Stopping GIF animation decode after frame error"
                );
                truncation = Some(AnimationDecodeTruncation::FrameDecodeError);
                break;
            }
        };
        let (numer, denom) = frame.delay().numer_denom_ms();
        let raw_ms = if denom > 0 {
            (numer as f32 / denom as f32).round() as u32
        } else {
            DEFAULT_FRAME_DURATION_MS
        };
        // GIF legacy behavior: 0-10ms delays should be treated as 100ms
        // This matches browser behavior for old GIFs with 0 or 1 centisecond delays
        let duration_ms = normalize_animation_delay_ms(raw_ms);

        let left = frame.left();
        let top = frame.top();
        let buffer = frame.into_buffer();
        let (buf_w, buf_h) = buffer.dimensions();
        if decoded_rgba_bytes(screen_width, screen_height, frames.len() + 1)
            .is_none_or(|bytes| bytes > MAX_ANIMATION_DECODE_WORK_BYTES)
        {
            crate::log_cache!(
                debug,
                width = screen_width,
                height = screen_height,
                frames = frames.len() + 1,
                max_bytes = MAX_ANIMATION_DECODE_WORK_BYTES,
                "Stopping GIF animation at the decode-work limit"
            );
            truncation = Some(AnimationDecodeTruncation::DecodeWorkLimit);
            break;
        }

        // image 0.25.10's GifDecoder applies disposal/blend internally and emits
        // full-canvas frames at (0, 0), so the fast path below is what runs in
        // practice. The sub-canvas branch is a defensive fallback: it overlays
        // the frame onto the *running* canvas (not a fresh transparent one) so a
        // partial frame keeps previously-painted pixels instead of showing only
        // its own delta region.
        if left == 0 && top == 0 && buf_w == screen_width && buf_h == screen_height {
            // Keep the running canvas coherent for any following sub-frame.
            canvas = Some(buffer);
        } else {
            let base = canvas.get_or_insert_with(|| RgbaImage::new(screen_width, screen_height));
            overlay_subframe_onto_canvas(base, &buffer, left, top);
        }

        // Compress the retained canvas in place instead of copying every frame
        // into a second full-canvas scratch buffer. Channel swapping is
        // symmetric, so restore RGBA immediately afterward for any following
        // defensive sub-frame composition.
        let base = canvas.as_mut()?;
        convert_to_gpu_format_in_place(base.as_mut());
        let raw_len = base.as_raw().len();
        let compressed = zstd::bulk::compress(base.as_raw(), ANIMATION_SOURCE_ZSTD_LEVEL);
        convert_to_gpu_format_in_place(base.as_mut());
        let mut compressed = compressed.ok()?;
        // zstd allocates the destination at compress_bound(), which is slightly
        // LARGER than the uncompressed frame, then sets len to the compressed
        // size. Storing that buffer unshrunk keeps a raw-frame-sized allocation
        // alive for every "compressed" frame -- which defeats the entire point of
        // compressing the source, and made the cache's own accounting (which sums
        // len) report a small fraction of what the heap actually held.
        compressed.shrink_to_fit();
        let retained_frame_bytes = compressed.capacity();
        let projected_frame_count = frames.len().saturating_add(1);
        let projected_source_bytes = compressed_source_bytes
            .saturating_add(retained_frame_bytes)
            .saturating_add(
                projected_frame_count
                    .saturating_mul(std::mem::size_of::<CompressedDecodedFrameData>()),
            );
        if projected_source_bytes > MAX_ANIMATION_COMPRESSED_SOURCE_BYTES {
            crate::log_cache!(
                debug,
                width = screen_width,
                height = screen_height,
                frames = frames.len() + 1,
                retained_bytes = projected_source_bytes,
                max_bytes = MAX_ANIMATION_COMPRESSED_SOURCE_BYTES,
                "Stopping GIF animation at the compressed-source limit"
            );
            truncation = Some(AnimationDecodeTruncation::CompressedSourceLimit);
            break;
        }
        raw_frame_bytes = raw_frame_bytes.saturating_add(raw_len);
        compressed_source_bytes = compressed_source_bytes.saturating_add(retained_frame_bytes);
        frames.push(CompressedDecodedFrameData {
            compressed,
            raw_len,
            width: screen_width,
            height: screen_height,
            delay_ms: duration_ms,
        });
    }

    frames.shrink_to_fit();
    compressed_source_bytes = compressed_animation_source_allocation_bytes(&frames);
    Some(DecodedAnimationData {
        frames,
        raw_frame_bytes,
        compressed_source_bytes,
        truncation,
    })
}

/// Decode WebP frames using the image crate's built-in compositing.
///
/// Same approach as GIF: use full-canvas frames directly when available,
/// fall back to per-frame compositing for sub-frames.
fn decode_webp_frames(data: &[u8], max_frames: usize) -> Option<DecodedAnimationData> {
    use image::ImageDecoder;

    let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(data)).ok()?;
    let (screen_width, screen_height) = decoder.dimensions();
    let raw_frame_size = decoded_rgba_bytes(screen_width, screen_height, 1)?;
    if raw_frame_size > MAX_ANIMATION_RAW_FRAME_BYTES {
        return Some(DecodedAnimationData {
            frames: Vec::new(),
            raw_frame_bytes: 0,
            compressed_source_bytes: 0,
            truncation: Some(AnimationDecodeTruncation::FrameTooLarge),
        });
    }
    // image-webp deliberately leaves its disposal clear color unset by
    // default. In that state a dispose-to-background frame performs no clear,
    // so transparent 7TV animations accumulate pixels from earlier frames.
    // Chat media must composite over the UI rather than the container's opaque
    // background-color hint, so explicitly clear disposed regions to
    // transparent before the decoder emits its full-canvas frames.
    decoder
        .set_background_color(image::Rgba([0, 0, 0, 0]))
        .ok()?;
    let frames_iter = decoder.into_frames();

    let mut frames = Vec::new();
    let mut raw_frame_bytes = 0usize;
    let mut compressed_source_bytes = 0usize;
    let mut truncation = None;
    // Running full-canvas accumulator for the sub-frame fallback below.
    let mut canvas: Option<RgbaImage> = None;

    for (i, frame_result) in frames_iter.enumerate() {
        if i >= max_frames {
            crate::log_cache!(debug, "Truncating animation frames: max={}", max_frames);
            truncation = Some(AnimationDecodeTruncation::FrameLimit);
            break;
        }

        let frame = match frame_result {
            Ok(frame) => frame,
            Err(err) => {
                crate::log_cache!(
                    debug,
                    frame_index = i,
                    error = %err,
                    "Stopping WebP animation decode after frame error"
                );
                truncation = Some(AnimationDecodeTruncation::FrameDecodeError);
                break;
            }
        };
        let (numer, denom) = frame.delay().numer_denom_ms();
        let raw_ms = if denom > 0 {
            (numer as f32 / denom as f32).round() as u32
        } else {
            DEFAULT_FRAME_DURATION_MS
        };
        let duration_ms = normalize_animation_delay_ms(raw_ms);

        let left = frame.left();
        let top = frame.top();
        let buffer = frame.into_buffer();
        let (buf_w, buf_h) = buffer.dimensions();
        if decoded_rgba_bytes(screen_width, screen_height, frames.len() + 1)
            .is_none_or(|bytes| bytes > MAX_ANIMATION_DECODE_WORK_BYTES)
        {
            crate::log_cache!(
                debug,
                width = screen_width,
                height = screen_height,
                frames = frames.len() + 1,
                max_bytes = MAX_ANIMATION_DECODE_WORK_BYTES,
                "Stopping WebP animation at the decode-work limit"
            );
            truncation = Some(AnimationDecodeTruncation::DecodeWorkLimit);
            break;
        }

        // image 0.25.10's WebPDecoder (via image-webp) composites blend and the
        // explicitly configured transparent disposal clear onto an internal
        // canvas, then emits full-canvas frames at (0, 0), so the fast path
        // below is what runs in practice. The sub-canvas branch is a defensive
        // fallback: it overlays the frame onto the *running* canvas (not a
        // fresh transparent one) so a partial frame keeps previously-painted
        // pixels instead of showing only its own delta region.
        if left == 0 && top == 0 && buf_w == screen_width && buf_h == screen_height {
            // Keep the running canvas coherent for any following sub-frame.
            canvas = Some(buffer);
        } else {
            let base = canvas.get_or_insert_with(|| RgbaImage::new(screen_width, screen_height));
            overlay_subframe_onto_canvas(base, &buffer, left, top);
        }

        let base = canvas.as_mut()?;
        convert_to_gpu_format_in_place(base.as_mut());
        let raw_len = base.as_raw().len();
        let compressed = zstd::bulk::compress(base.as_raw(), ANIMATION_SOURCE_ZSTD_LEVEL);
        convert_to_gpu_format_in_place(base.as_mut());
        let mut compressed = compressed.ok()?;
        // zstd allocates the destination at compress_bound(), which is slightly
        // LARGER than the uncompressed frame, then sets len to the compressed
        // size. Storing that buffer unshrunk keeps a raw-frame-sized allocation
        // alive for every "compressed" frame -- which defeats the entire point of
        // compressing the source, and made the cache's own accounting (which sums
        // len) report a small fraction of what the heap actually held.
        compressed.shrink_to_fit();
        let retained_frame_bytes = compressed.capacity();
        let projected_frame_count = frames.len().saturating_add(1);
        let projected_source_bytes = compressed_source_bytes
            .saturating_add(retained_frame_bytes)
            .saturating_add(
                projected_frame_count
                    .saturating_mul(std::mem::size_of::<CompressedDecodedFrameData>()),
            );
        if projected_source_bytes > MAX_ANIMATION_COMPRESSED_SOURCE_BYTES {
            crate::log_cache!(
                debug,
                width = screen_width,
                height = screen_height,
                frames = frames.len() + 1,
                retained_bytes = projected_source_bytes,
                max_bytes = MAX_ANIMATION_COMPRESSED_SOURCE_BYTES,
                "Stopping WebP animation at the compressed-source limit"
            );
            truncation = Some(AnimationDecodeTruncation::CompressedSourceLimit);
            break;
        }
        raw_frame_bytes = raw_frame_bytes.saturating_add(raw_len);
        compressed_source_bytes = compressed_source_bytes.saturating_add(retained_frame_bytes);
        frames.push(CompressedDecodedFrameData {
            compressed,
            raw_len,
            width: screen_width,
            height: screen_height,
            delay_ms: duration_ms,
        });
    }

    frames.shrink_to_fit();
    compressed_source_bytes = compressed_animation_source_allocation_bytes(&frames);
    Some(DecodedAnimationData {
        frames,
        raw_frame_bytes,
        compressed_source_bytes,
        truncation,
    })
}

/// Overlay a sub-canvas animation frame onto a running full-canvas buffer.
///
/// GIF and WebP transparency share the same "keep previous" semantics: a fully
/// transparent source pixel (alpha 0) leaves the destination canvas pixel
/// untouched, while any other pixel overwrites the destination. This mirrors the
/// compositing the container's own decoder performs.
///
/// It is only exercised by the defensive sub-frame fallback in
/// [`decode_gif_frames`]/[`decode_webp_frames`]: image 0.25.10's GIF and WebP
/// decoders composite disposal/blend internally and always emit full-canvas
/// frames at (0, 0), so those paths take the fast branch instead. Compositing
/// onto the *running* canvas (rather than a fresh transparent one) is what keeps
/// a partial frame from showing only its own delta region.
///
/// `left`/`top` are the sub-frame's offset within the full canvas; source pixels
/// that fall outside the canvas bounds are skipped.
pub fn overlay_subframe_onto_canvas(
    canvas: &mut RgbaImage,
    sub_frame: &RgbaImage,
    left: u32,
    top: u32,
) {
    let (canvas_w, canvas_h) = canvas.dimensions();
    for (x, y, pixel) in sub_frame.enumerate_pixels() {
        // Fully transparent source pixels keep the existing canvas pixel.
        if pixel.0[3] == 0 {
            continue;
        }
        let canvas_x = left + x;
        let canvas_y = top + y;
        if canvas_x < canvas_w && canvas_y < canvas_h {
            canvas.put_pixel(canvas_x, canvas_y, *pixel);
        }
    }
}

// ============================================================================
// Platform-Specific Conversion
// ============================================================================

#[inline]
fn decoded_rgba_bytes(width: u32, height: u32, frames: usize) -> Option<u64> {
    (width as u64)
        .checked_mul(height as u64)?
        .checked_mul(frames as u64)?
        .checked_mul(4)
}

/// Convert RGBA to BGRA for GPUI/Blade/Vulkan GPU compatibility
///
/// GPUI uses Blade graphics with Vulkan on all platforms, which expects BGRA format.
/// The `image` crate decodes to RGBA, so we always convert.
#[inline]
fn convert_rgba_to_bgra_if_needed(mut rgba_data: Vec<u8>) -> Vec<u8> {
    convert_rgba_to_bgra_in_place(&mut rgba_data);
    crate::log_cache!(
        trace,
        "Converted RGBA to BGRA for Vulkan: bytes={}",
        rgba_data.len()
    );
    rgba_data
}

/// In-place RGBA to BGRA conversion for Vulkan
#[inline]
fn convert_rgba_to_bgra_in_place(rgba_data: &mut [u8]) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if rgba_data.len() >= 32 && std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: Runtime feature detection above guarantees AVX2 support.
            unsafe {
                convert_rgba_to_bgra_avx2(rgba_data);
            }
            return;
        }
    }

    convert_rgba_to_bgra_scalar(rgba_data);
}

#[inline]
fn convert_rgba_to_bgra_scalar(rgba_data: &mut [u8]) {
    for chunk in rgba_data.as_chunks_mut::<4>().0 {
        chunk.swap(0, 2); // R ↔ B
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn convert_rgba_to_bgra_avx2(rgba_data: &mut [u8]) {
    use std::arch::x86_64::{
        __m256i, _mm256_loadu_si256, _mm256_setr_epi8, _mm256_shuffle_epi8, _mm256_storeu_si256,
    };

    let vector_len = rgba_data.len() & !31;
    // SAFETY: AVX2 is enabled for this function, and each unaligned load/store stays
    // within the mutable slice because `vector_len` is rounded down to 32 bytes.
    unsafe {
        let shuffle = _mm256_setr_epi8(
            2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8, 11, 14, 13, 12, 15, 2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8,
            11, 14, 13, 12, 15,
        );

        let mut offset = 0;
        while offset < vector_len {
            let ptr = rgba_data.as_mut_ptr().add(offset);
            let pixels = _mm256_loadu_si256(ptr.cast::<__m256i>());
            let converted = _mm256_shuffle_epi8(pixels, shuffle);
            _mm256_storeu_si256(ptr.cast::<__m256i>(), converted);
            offset += 32;
        }
    }

    convert_rgba_to_bgra_scalar(&mut rgba_data[vector_len..]);
}

#[cfg(target_arch = "x86")]
#[target_feature(enable = "avx2")]
unsafe fn convert_rgba_to_bgra_avx2(rgba_data: &mut [u8]) {
    use std::arch::x86::{
        __m256i, _mm256_loadu_si256, _mm256_setr_epi8, _mm256_shuffle_epi8, _mm256_storeu_si256,
    };

    let vector_len = rgba_data.len() & !31;
    // SAFETY: AVX2 is enabled for this function, and each unaligned load/store stays
    // within the mutable slice because `vector_len` is rounded down to 32 bytes.
    unsafe {
        let shuffle = _mm256_setr_epi8(
            2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8, 11, 14, 13, 12, 15, 2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8,
            11, 14, 13, 12, 15,
        );

        let mut offset = 0;
        while offset < vector_len {
            let ptr = rgba_data.as_mut_ptr().add(offset);
            let pixels = _mm256_loadu_si256(ptr.cast::<__m256i>());
            let converted = _mm256_shuffle_epi8(pixels, shuffle);
            _mm256_storeu_si256(ptr.cast::<__m256i>(), converted);
            offset += 32;
        }
    }

    convert_rgba_to_bgra_scalar(&mut rgba_data[vector_len..]);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgra_conversion_consistency() {
        // Test that convert_to_gpu_format and convert_to_gpu_format_in_place produce same result
        let rgba1 = vec![255, 0, 0, 255, 0, 255, 0, 255]; // Red pixel, Green pixel
        let mut rgba2 = rgba1.clone();

        let result1 = convert_to_gpu_format(rgba1);
        convert_to_gpu_format_in_place(&mut rgba2);

        assert_eq!(result1, rgba2);
    }

    #[test]
    fn test_bgra_conversion() {
        // Should always swap R and B for Vulkan/BGRA format
        let rgba = vec![255, 128, 64, 255]; // R=255, G=128, B=64, A=255
        let bgra = convert_to_gpu_format(rgba);
        assert_eq!(bgra, vec![64, 128, 255, 255]); // B=64, G=128, R=255, A=255
    }

    #[test]
    fn test_decode_simple_png() {
        // 1x1 red PNG (minimal valid PNG)
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // 8-bit RGB
            0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, // IDAT chunk
            0x54, 0x08, 0xD7, 0x63, 0xF8, 0xFF, 0xFF, 0x3F, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0xDC,
            0xCC, 0x59, 0xE7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, // IEND chunk
            0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        // This may or may not decode depending on the exact PNG validity
        // The main point is it shouldn't panic
        let _ = decode_to_render_image(&png_bytes);
    }
}
