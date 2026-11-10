//! Loading-path benchmarks:
//! - Image decode (static + animated)
//! - GPU format conversion
//! - Disk cache save/load throughput

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use echo_gpui::cache::budget::BudgetManager;
use echo_gpui::cache::disk_layer::DiskLayer;
use echo_gpui::cache::stats::CacheStats;
use echo_gpui::image_decode::{
    convert_to_gpu_format_in_place, decode_animated_frames, decode_to_render_image,
};
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, DynamicImage, Frame, ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn gradient_rgba(width: u32, height: u32, seed: u8) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let r = ((x + y + seed as u32) % 256) as u8;
            let g = ((x * 3 + seed as u32) % 256) as u8;
            let b = ((y * 5 + seed as u32) % 256) as u8;
            img.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }
    img
}

fn encode_image_bytes(img: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut cursor, format)
        .expect("encode fixture image");
    cursor.into_inner()
}

fn encode_animated_gif(frame_count: usize, width: u32, height: u32) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut encoder = GifEncoder::new(&mut cursor);
        encoder
            .set_repeat(Repeat::Infinite)
            .expect("set gif repeat");
        for i in 0..frame_count {
            let img = gradient_rgba(width, height, (i * 13) as u8);
            let frame = Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(40, 1));
            encoder.encode_frame(frame).expect("encode gif frame");
        }
    }
    cursor.into_inner()
}

fn bench_gpu_format_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("loading_gpu_format");
    for size in [28u32, 56, 112, 224] {
        let pixel_count = (size * size * 4) as usize;
        let base: Vec<u8> = (0..pixel_count)
            .map(|i| ((i * 31 + 17) % 251) as u8)
            .collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{size}x{size}")),
            &base,
            |b, bytes| {
                b.iter_batched(
                    || bytes.clone(),
                    |mut pixels| {
                        convert_to_gpu_format_in_place(black_box(&mut pixels));
                        black_box(pixels);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_static_image_decode(c: &mut Criterion) {
    let img_112 = gradient_rgba(112, 112, 7);
    let png_112 = encode_image_bytes(&img_112, ImageFormat::Png);
    let jpeg_112 = encode_image_bytes(&img_112, ImageFormat::Jpeg);

    let mut group = c.benchmark_group("loading_decode_static");
    group.bench_function("png_112x112", |b| {
        b.iter(|| black_box(decode_to_render_image(black_box(&png_112))));
    });
    group.bench_function("jpeg_112x112", |b| {
        b.iter(|| black_box(decode_to_render_image(black_box(&jpeg_112))));
    });
    group.finish();
}

fn bench_animated_decode(c: &mut Criterion) {
    let gif_16_frames = encode_animated_gif(16, 96, 96);

    c.bench_function("loading_decode_animated_gif_16x96", |b| {
        b.iter(|| black_box(decode_animated_frames(black_box(&gif_16_frames))));
    });
}

fn bench_disk_cache_io(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let mut raw_layer = DiskLayer::new(
        std::env::temp_dir().join("echo-loading-bench-cache"),
        Arc::new(BudgetManager::new()),
        Arc::new(CacheStats::new()),
    );
    raw_layer.base_path = temp_dir.path().join("bench-cache");
    let layer = Arc::new(raw_layer);
    rt.block_on(layer.initialize()).expect("init disk layer");

    let payload: Vec<u8> = (0..(64 * 1024))
        .map(|i| ((i * 31 + 7) % 251) as u8)
        .collect();

    let save_urls: Vec<String> = (0..128)
        .map(|i| format!("https://bench.local/image-{i}.png"))
        .collect();
    let save_counter = AtomicU64::new(0);

    let warm_url = "https://bench.local/load-target.png";
    rt.block_on(layer.save_image(warm_url, &payload, 112, 112))
        .expect("seed load benchmark");

    let mut group = c.benchmark_group("loading_disk_cache");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));

    group.bench_function("save_image_64kb_compress", |b| {
        b.to_async(&rt).iter(|| async {
            let idx =
                (save_counter.fetch_add(1, Ordering::Relaxed) % save_urls.len() as u64) as usize;
            let url = &save_urls[idx];
            let result = layer.save_image(url, &payload, 112, 112).await;
            black_box(result.expect("save_image should succeed"));
        });
    });

    group.bench_function("load_image_64kb_decompress", |b| {
        b.to_async(&rt).iter(|| async {
            let bytes = layer.load_image(warm_url).await;
            black_box(bytes.expect("load_image should return bytes"));
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_gpu_format_conversion,
    bench_static_image_decode,
    bench_animated_decode,
    bench_disk_cache_io
);
criterion_main!(benches);
