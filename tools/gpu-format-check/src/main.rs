use echo_gpui::image_decode::convert_to_gpu_format_in_place;
use std::hint::black_box;
use std::time::Instant;

const SIZES: [usize; 4] = [28, 56, 112, 224];
const TARGET_BYTES_PER_SIZE: usize = 256 * 1024 * 1024;

fn main() {
    verify_edge_cases();

    println!("kind,name,bytes_per_iter,iters,total_bytes,ns_per_byte,gb_per_second,checksum");
    for size in SIZES {
        bench_size(size);
    }
}

fn verify_edge_cases() {
    for len in [0usize, 1, 2, 3, 4, 5, 31, 32, 33, 63, 64, 65, 28 * 28 * 4] {
        let mut actual = fixture_bytes(len);
        let mut expected = actual.clone();
        scalar_rgba_to_bgra(&mut expected);
        convert_to_gpu_format_in_place(&mut actual);
        assert_eq!(actual, expected, "RGBA->BGRA mismatch at len={len}");
    }
}

fn bench_size(size: usize) {
    let bytes_per_iter = size * size * 4;
    let iters = (TARGET_BYTES_PER_SIZE / bytes_per_iter).max(256);
    let mut pixels = fixture_bytes(bytes_per_iter);
    let mut checksum = 0u64;

    let started = Instant::now();
    for i in 0..iters {
        convert_to_gpu_format_in_place(black_box(&mut pixels));
        checksum = checksum.wrapping_add(sample_checksum(&pixels, i));
    }
    let elapsed = started.elapsed();
    let total_bytes = bytes_per_iter * iters;
    let ns_per_byte = elapsed.as_nanos() as f64 / total_bytes as f64;
    let gb_per_second = total_bytes as f64 / elapsed.as_secs_f64() / 1_000_000_000.0;

    println!(
        "bench,{0}x{0},{1},{2},{3},{4:.6},{5:.3},{6}",
        size, bytes_per_iter, iters, total_bytes, ns_per_byte, gb_per_second, checksum
    );
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

fn sample_checksum(bytes: &[u8], iteration: usize) -> u64 {
    if bytes.is_empty() {
        return iteration as u64;
    }

    let a = bytes[iteration % bytes.len()] as u64;
    let b = bytes[(iteration.wrapping_mul(17).wrapping_add(3)) % bytes.len()] as u64;
    let c = bytes[(iteration.wrapping_mul(97).wrapping_add(11)) % bytes.len()] as u64;
    (a << 32) ^ (b << 16) ^ c ^ iteration as u64
}
