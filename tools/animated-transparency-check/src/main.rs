//! Regression proof for animated-emote transparency and loop continuity.
//!
//! Four independent checks, no GUI:
//!
//! 1. `overlay_subframe_onto_canvas` (the actual fix): a sub-frame overlaid onto
//!    a running canvas must keep pixels outside the sub-frame, keep canvas pixels
//!    under fully-transparent source pixels, and overwrite under opaque source
//!    pixels. This is the behavior that stops a partial frame from showing only
//!    its own delta region.
//!
//! 2. End-to-end: a synthetic 2-frame transparent GIF whose second frame is a
//!    small sub-frame with disposal=Keep must decode so that frame 2 still
//!    contains frame 1's untouched pixels.
//!
//! 3. End-to-end: a synthetic 2-frame animated WebP uses
//!    dispose-to-background between frames. The second decoded full-canvas frame
//!    must clear the first frame's red pixel to transparent while retaining its
//!    own blue pixel.
//!
//! 4. The bounded cache seam policy must begin warming frame zero across the
//!    final refill window without expanding the regular one-frame lookahead.
//!
//! Run: `cargo run -p echo-animated-transparency-check`

use echo_gpui::cache::frame_layer::animation_loop_seam_prefetch_due;
use echo_gpui::image_decode::{
    CompressedDecodedFrameData, decode_animated_frames, overlay_subframe_onto_canvas,
};
use image::{ExtendedColorType, ImageEncoder, Rgba, RgbaImage};

const RED: Rgba<u8> = Rgba([255, 0, 0, 255]);
const GREEN: Rgba<u8> = Rgba([0, 255, 0, 255]);
const BLUE: Rgba<u8> = Rgba([0, 0, 255, 255]);
const CLEAR: Rgba<u8> = Rgba([0, 0, 0, 0]);

fn check_overlay_helper() {
    // 4x4 running canvas: a red pixel well outside the sub-frame, and a green
    // pixel that will sit *under* a fully-transparent source pixel.
    let mut canvas = RgbaImage::from_pixel(4, 4, CLEAR);
    canvas.put_pixel(0, 0, RED); // outside the sub-frame region entirely
    canvas.put_pixel(3, 2, GREEN); // under sub_frame(1, 0), which is transparent

    // 2x2 sub-frame placed at (left=2, top=2). Only sub(0,0) is opaque.
    //   sub(0,0) -> canvas(2,2)  opaque blue  => overwrite
    //   sub(1,0) -> canvas(3,2)  transparent  => keep green
    //   sub(0,1) -> canvas(2,3)  transparent  => keep (clear)
    //   sub(1,1) -> canvas(3,3)  transparent  => keep (clear)
    let mut sub = RgbaImage::from_pixel(2, 2, CLEAR);
    sub.put_pixel(0, 0, BLUE);

    overlay_subframe_onto_canvas(&mut canvas, &sub, 2, 2);

    assert_eq!(
        *canvas.get_pixel(0, 0),
        RED,
        "pixel outside the sub-frame must be preserved (running canvas kept)"
    );
    assert_eq!(
        *canvas.get_pixel(3, 2),
        GREEN,
        "canvas pixel under a fully-transparent source pixel must be preserved"
    );
    assert_eq!(
        *canvas.get_pixel(2, 2),
        BLUE,
        "canvas pixel under an opaque source pixel must be overwritten"
    );
    assert_eq!(
        *canvas.get_pixel(3, 3),
        CLEAR,
        "untouched transparent region must stay transparent"
    );

    println!("[ok] overlay_subframe_onto_canvas preserves the running canvas");
}

/// Build a 2-frame animated GIF: frame 1 paints one opaque pixel at (0,0) on an
/// otherwise-transparent 4x4 canvas; frame 2 is a 2x2 sub-frame at (2,2) that
/// paints one opaque pixel and (via disposal=Keep) leaves frame 1's pixel alone.
fn build_two_frame_transparent_gif() -> Vec<u8> {
    let (w, h) = (4u16, 4u16);

    // Frame 1: full canvas, red at (0,0), transparent elsewhere.
    let mut rgba1 = vec![0u8; (w as usize) * (h as usize) * 4];
    rgba1[0..4].copy_from_slice(&[255, 0, 0, 255]);

    // Frame 2: 2x2 sub-frame, blue at its (0,0), transparent elsewhere.
    let mut rgba2 = vec![0u8; 2 * 2 * 4];
    rgba2[0..4].copy_from_slice(&[0, 0, 255, 255]);

    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, w, h, &[]).expect("gif encoder");
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .expect("set repeat");

        let mut frame1 = gif::Frame::from_rgba_speed(w, h, &mut rgba1, 10);
        frame1.dispose = gif::DisposalMethod::Keep;
        frame1.delay = 10;
        encoder.write_frame(&frame1).expect("write frame 1");

        let mut frame2 = gif::Frame::from_rgba_speed(2, 2, &mut rgba2, 10);
        frame2.left = 2;
        frame2.top = 2;
        frame2.dispose = gif::DisposalMethod::Keep;
        frame2.delay = 10;
        encoder.write_frame(&frame2).expect("write frame 2");
    }
    out
}

/// Pixel (x, y) as a 4-byte slice into a decoded (BGRA) frame buffer.
fn px(data: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [data[i], data[i + 1], data[i + 2], data[i + 3]]
}

fn decoded_frame_bytes(frame: &CompressedDecodedFrameData) -> Vec<u8> {
    zstd::bulk::decompress(&frame.compressed, frame.raw_len)
        .expect("decompress decoded animation frame")
}

fn check_gif_roundtrip() {
    let gif = build_two_frame_transparent_gif();
    let decoded = decode_animated_frames(&gif).expect("decode_animated_frames returned None");
    let frames = decoded.frames;

    assert_eq!(
        frames.len(),
        2,
        "expected 2 decoded frames, got {}",
        frames.len()
    );
    let w = frames[0].width;
    assert_eq!(w, 4, "unexpected canvas width");
    let frame0 = decoded_frame_bytes(&frames[0]);
    let frame1 = decoded_frame_bytes(&frames[1]);

    // Compare decoded-to-decoded so palette quantization can't make this flaky.
    let f0_00 = px(&frame0, w, 0, 0);
    let f1_00 = px(&frame1, w, 0, 0);
    let f0_22 = px(&frame0, w, 2, 2);
    let f1_22 = px(&frame1, w, 2, 2);

    // Frame 1's pixel must be opaque and non-empty, or the test proves nothing.
    assert_eq!(f0_00[3], 255, "frame 1 pixel (0,0) should be opaque");
    // Core assertion: frame 2 still contains frame 1's untouched pixel.
    assert_eq!(
        f1_00, f0_00,
        "frame 2 must preserve frame 1's pixel at (0,0): frame1={f0_00:?} frame2={f1_00:?}"
    );
    // Sanity: frame 2's sub-frame delta actually landed (the region changed).
    assert_ne!(
        f1_22, f0_22,
        "frame 2's sub-frame delta at (2,2) should differ from frame 1"
    );

    println!(
        "[ok] 2-frame transparent GIF decode preserves earlier pixels (frame2(0,0)={f1_00:?})"
    );
}

fn push_u24(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&[
        (value & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        ((value >> 16) & 0xff) as u8,
    ]);
}

fn push_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        out.push(0);
    }
}

fn encoded_webp_image_chunk(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut encoded = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .expect("encode lossless WebP frame");
    assert!(
        encoded.len() >= 12 && &encoded[..4] == b"RIFF" && &encoded[8..12] == b"WEBP",
        "WebP encoder returned an invalid container"
    );

    let mut offset = 12usize;
    while offset + 8 <= encoded.len() {
        let size = u32::from_le_bytes(
            encoded[offset + 4..offset + 8]
                .try_into()
                .expect("WebP chunk size"),
        ) as usize;
        let padded_size = size + (size & 1);
        let end = offset + 8 + padded_size;
        assert!(end <= encoded.len(), "WebP frame chunk exceeds container");
        if &encoded[offset..offset + 4] == b"VP8L" {
            return encoded[offset..end].to_vec();
        }
        offset = end;
    }
    panic!("lossless WebP encoder did not emit a VP8L image chunk");
}

fn build_two_frame_disposal_webp() -> Vec<u8> {
    const WIDTH: u32 = 2;
    const HEIGHT: u32 = 1;
    const FRAME_DURATION_MS: u32 = 40;

    let first_rgba = [
        255, 0, 0, 255, // opaque red at x=0
        0, 0, 0, 0, // transparent at x=1
    ];
    let second_rgba = [
        0, 0, 0, 0, // transparent at x=0: must expose the disposal clear
        0, 0, 255, 255, // opaque blue at x=1
    ];
    let first_image = encoded_webp_image_chunk(&first_rgba, WIDTH, HEIGHT);
    let second_image = encoded_webp_image_chunk(&second_rgba, WIDTH, HEIGHT);

    let mut webp = Vec::from(&b"RIFF\0\0\0\0WEBP"[..]);
    let mut vp8x = vec![0x12, 0, 0, 0]; // alpha + animation
    push_u24(&mut vp8x, WIDTH - 1);
    push_u24(&mut vp8x, HEIGHT - 1);
    push_chunk(&mut webp, b"VP8X", &vp8x);
    push_chunk(&mut webp, b"ANIM", &[0, 0, 0, 0, 0, 0]);

    let mut first_frame = Vec::new();
    push_u24(&mut first_frame, 0); // x / 2
    push_u24(&mut first_frame, 0); // y / 2
    push_u24(&mut first_frame, WIDTH - 1);
    push_u24(&mut first_frame, HEIGHT - 1);
    push_u24(&mut first_frame, FRAME_DURATION_MS);
    first_frame.push(0b11); // dispose to background + do not blend
    first_frame.extend_from_slice(&first_image);
    push_chunk(&mut webp, b"ANMF", &first_frame);

    let mut second_frame = Vec::new();
    push_u24(&mut second_frame, 0);
    push_u24(&mut second_frame, 0);
    push_u24(&mut second_frame, WIDTH - 1);
    push_u24(&mut second_frame, HEIGHT - 1);
    push_u24(&mut second_frame, FRAME_DURATION_MS);
    second_frame.push(0); // keep after display + alpha blend
    second_frame.extend_from_slice(&second_image);
    push_chunk(&mut webp, b"ANMF", &second_frame);

    let riff_size = (webp.len() - 8) as u32;
    webp[4..8].copy_from_slice(&riff_size.to_le_bytes());
    webp
}

fn check_webp_disposal_roundtrip() {
    let webp = build_two_frame_disposal_webp();
    let decoded = decode_animated_frames(&webp).expect("animated WebP decode returned None");
    assert_eq!(decoded.frames.len(), 2, "expected two animated WebP frames");

    let width = decoded.frames[0].width;
    let frame0 = decoded_frame_bytes(&decoded.frames[0]);
    let frame1 = decoded_frame_bytes(&decoded.frames[1]);
    let first_red = px(&frame0, width, 0, 0);
    let cleared = px(&frame1, width, 0, 0);
    let second_blue = px(&frame1, width, 1, 0);

    assert_eq!(first_red[3], 255, "first WebP pixel must be opaque");
    assert_eq!(
        cleared,
        [0, 0, 0, 0],
        "dispose-to-background must clear the previous WebP frame to transparent"
    );
    assert!(
        second_blue[0] >= 254
            && second_blue[1] == 0
            && second_blue[2] == 0
            && second_blue[3] == 255,
        "second WebP frame's blue pixel must remain visible in BGRA: {second_blue:?}"
    );

    println!("[ok] animated WebP dispose-to-background clears prior transparent-frame pixels");
}

fn check_loop_seam_runway() {
    assert!(!animation_loop_seam_prefetch_due(5, 10));
    for frame in 6..10 {
        assert!(
            animation_loop_seam_prefetch_due(frame, 10),
            "frame {frame} should be inside the final four-frame seam runway"
        );
    }
    assert!(
        !animation_loop_seam_prefetch_due(0, 4),
        "animations already covered by one refill window need no seam request"
    );
    println!("[ok] frame-zero prefetch starts across the final four-frame loop runway");
}

fn main() {
    check_overlay_helper();
    check_gif_roundtrip();
    check_webp_disposal_roundtrip();
    check_loop_seam_runway();
    println!("all animated-transparency checks passed");
}
