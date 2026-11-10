use echo_gpui::image_decode::{decode_to_render_image, inspect_animated_frames};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const MAX_FILES_TO_SCAN: usize = 512;
const MAX_COMPRESSED_FILE_BYTES: u64 = 1024 * 1024;

fn main() {
    let Some(cache_dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: echo-animation-decode-check <cached-image-file-or-directory>");
        std::process::exit(2);
    };

    let input_is_file = cache_dir.is_file();
    let mut candidates = if input_is_file {
        match cache_dir.metadata() {
            Ok(metadata) => vec![(cache_dir.clone(), metadata.len())],
            Err(error) => {
                eprintln!("failed to inspect {}: {error}", cache_dir.display());
                std::process::exit(2);
            }
        }
    } else {
        match fs::read_dir(&cache_dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let path = entry.path();
                    let metadata = entry.metadata().ok()?;
                    if !metadata.is_file()
                        || metadata.len() == 0
                        || metadata.len() > MAX_COMPRESSED_FILE_BYTES
                    {
                        return None;
                    }
                    Some((path, metadata.len()))
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                eprintln!("failed to read {}: {error}", cache_dir.display());
                std::process::exit(2);
            }
        }
    };
    candidates.sort_unstable_by_key(|(_, len)| std::cmp::Reverse(*len));

    let mut scanned = 0usize;
    let mut animated_containers = 0usize;
    let mut static_fallbacks = 0usize;
    let mut incomplete_animations = 0usize;
    let mut static_fallback_file = None;
    let mut best = None;
    for (path, _) in candidates.into_iter().take(MAX_FILES_TO_SCAN) {
        scanned += 1;
        let Ok(stored) = fs::read(&path) else {
            continue;
        };
        let bytes = if path.extension().is_some_and(|extension| extension == "zst") {
            match zstd::stream::decode_all(Cursor::new(stored)) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            }
        } else {
            stored
        };
        if !is_gif_or_webp(&bytes) {
            continue;
        }
        animated_containers += 1;
        let Some(decoded) = inspect_animated_frames(&bytes) else {
            if decode_to_render_image(&bytes).is_some() {
                static_fallbacks += 1;
                static_fallback_file = Some(file_name(&path));
            }
            continue;
        };
        if !decoded.is_complete() {
            incomplete_animations += 1;
        }
        let max_raw_frame_bytes = decoded
            .frames
            .iter()
            .map(|frame| frame.raw_len)
            .max()
            .unwrap_or(0);
        let total_duration_ms = decoded
            .frames
            .iter()
            .map(|frame| u64::from(frame.delay_ms))
            .sum();
        let min_frame_duration_ms = decoded
            .frames
            .iter()
            .map(|frame| frame.delay_ms)
            .min()
            .unwrap_or(0);
        let max_frame_duration_ms = decoded
            .frames
            .iter()
            .map(|frame| frame.delay_ms)
            .max()
            .unwrap_or(0);
        let frame_duration_runs = frame_duration_runs(&decoded);
        let name_paint_initial_window_frames =
            echo_gpui::cache::loader::name_paint_animation_decode_window(
                decoded.frames.len(),
                decoded.raw_frame_bytes,
            );
        let legacy_peak_payload_bytes = decoded
            .raw_frame_bytes
            .saturating_add(decoded.compressed_source_bytes);
        let streaming_peak_payload_bound_bytes = decoded
            .compressed_source_bytes
            .saturating_add(max_raw_frame_bytes.saturating_mul(2));
        let result = DecodeResult {
            file_name: file_name(&path),
            frame_count: decoded.frames.len(),
            raw_frame_bytes: decoded.raw_frame_bytes,
            compressed_source_bytes: decoded.compressed_source_bytes,
            max_raw_frame_bytes,
            total_duration_ms,
            min_frame_duration_ms,
            max_frame_duration_ms,
            frame_duration_runs,
            name_paint_initial_window_frames,
            legacy_peak_payload_bytes,
            streaming_peak_payload_bound_bytes,
            complete: decoded.is_complete(),
            truncation: decoded.truncation.map(|reason| reason.as_str()),
        };
        if best
            .as_ref()
            .is_none_or(|current: &DecodeResult| result.raw_frame_bytes > current.raw_frame_bytes)
        {
            best = Some(result);
        }
    }

    let Some(result) = best else {
        if input_is_file && static_fallbacks == 1 {
            println!("metric,value");
            println!("files_scanned,{scanned}");
            println!("animated_containers,{animated_containers}");
            println!("static_fallbacks,{static_fallbacks}");
            println!(
                "selected_cache_file,{}",
                static_fallback_file.as_deref().unwrap_or("unknown")
            );
            println!("decision,pass_static_fallback");
            return;
        }
        eprintln!(
            "no animated GIF/WebP found after scanning {scanned} files ({animated_containers} containers, {static_fallbacks} static fallbacks)"
        );
        std::process::exit(1);
    };

    println!("metric,value");
    println!("files_scanned,{scanned}");
    println!("animated_containers,{animated_containers}");
    println!("incomplete_animations,{incomplete_animations}");
    println!("selected_cache_file,{}", result.file_name);
    println!("frame_count,{}", result.frame_count);
    println!("complete,{}", result.complete);
    println!("truncation,{}", result.truncation.unwrap_or("none"));
    println!("total_duration_ms,{}", result.total_duration_ms);
    println!("min_frame_duration_ms,{}", result.min_frame_duration_ms);
    println!("max_frame_duration_ms,{}", result.max_frame_duration_ms);
    println!("frame_duration_runs,{}", result.frame_duration_runs);
    println!(
        "name_paint_initial_window_frames,{}",
        result.name_paint_initial_window_frames
    );
    println!("raw_frame_bytes,{}", result.raw_frame_bytes);
    println!("compressed_source_bytes,{}", result.compressed_source_bytes);
    println!("max_raw_frame_bytes,{}", result.max_raw_frame_bytes);
    println!(
        "legacy_peak_payload_bytes,{}",
        result.legacy_peak_payload_bytes
    );
    println!(
        "streaming_peak_payload_bound_bytes,{}",
        result.streaming_peak_payload_bound_bytes
    );
    println!(
        "payload_peak_reduction_percent,{:.3}",
        percent_reduction(
            result.legacy_peak_payload_bytes,
            result.streaming_peak_payload_bound_bytes
        )
    );
    println!("decision,pass");
}

fn frame_duration_runs(decoded: &echo_gpui::image_decode::DecodedAnimationData) -> String {
    let mut runs = Vec::new();
    for frame in &decoded.frames {
        match runs.last_mut() {
            Some((delay_ms, count)) if *delay_ms == frame.delay_ms => *count += 1,
            _ => runs.push((frame.delay_ms, 1usize)),
        }
    }
    runs.into_iter()
        .map(|(delay_ms, count)| format!("{delay_ms}x{count}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn is_gif_or_webp(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF8")
        || (bytes.len() >= 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn percent_reduction(baseline: usize, candidate: usize) -> f64 {
    if baseline == 0 {
        return 0.0;
    }
    100.0 * baseline.saturating_sub(candidate) as f64 / baseline as f64
}

struct DecodeResult {
    file_name: String,
    frame_count: usize,
    raw_frame_bytes: usize,
    compressed_source_bytes: usize,
    max_raw_frame_bytes: usize,
    total_duration_ms: u64,
    min_frame_duration_ms: u32,
    max_frame_duration_ms: u32,
    frame_duration_runs: String,
    name_paint_initial_window_frames: u16,
    legacy_peak_payload_bytes: usize,
    streaming_peak_payload_bound_bytes: usize,
    complete: bool,
    truncation: Option<&'static str>,
}
