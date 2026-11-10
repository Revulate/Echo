//! Contract: `image_decode::convert_to_gpu_format_in_place` swaps RGBA to BGRA
//! exactly like a scalar per-pixel swap, for empty, partial-pixel, SIMD-boundary
//! and emote-sized buffers. Throughput lives in `benches/loading_bench.rs`.

use check_support::FIXTURE_HEADER;
use echo_gpui::image_decode::convert_to_gpu_format_in_place;

fn main() {
    println!("{FIXTURE_HEADER}");
    for len in [0usize, 1, 2, 3, 4, 5, 31, 32, 33, 63, 64, 65, 28 * 28 * 4] {
        let mut actual = fixture_bytes(len);
        let mut expected = actual.clone();
        scalar_rgba_to_bgra(&mut expected);
        convert_to_gpu_format_in_place(&mut actual);
        assert_eq!(actual, expected, "RGBA->BGRA mismatch at len={len}");
        println!("fixture,rgba_to_bgra_len_{len},pass");
    }
}

fn fixture_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((i.wrapping_mul(31).wrapping_add(17)) % 251) as u8)
        .collect()
}

fn scalar_rgba_to_bgra(bytes: &mut [u8]) {
    for chunk in bytes.as_chunks_mut::<4>().0 {
        chunk.swap(0, 2);
    }
}
