//! Message-ingest parsing benchmarks: emote tags, slot parsing, structure
//! hashing and filter evaluation.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group};
use echo_gpui::backend::emotes::EmoteSender;
use echo_gpui::backend::filters::{FILTER_STORE, TestContext};
use echo_gpui::backend::settings::{self, FilterAction, FilterRule};
use echo_gpui::backend::slots::compute_structure_hash;
use echo_gpui::backend::slots::parser::{parse_emotes_tag, parse_to_slots};
use echo_gpui::storage_roots;
use rustc_hash::FxHashMap;

fn make_dense_emote_tag(count: usize) -> String {
    let mut ranges = Vec::with_capacity(count);
    for i in 0..count {
        let start = i * 6;
        let end = start + 4;
        ranges.push(format!("{start}-{end}"));
    }
    format!("25:{}", ranges.join(","))
}

fn make_dense_emote_text(count: usize) -> String {
    let mut text = String::with_capacity(count * 6);
    for i in 0..count {
        if i > 0 {
            text.push(' ');
        }
        text.push_str("Kappa");
    }
    text
}

fn bench_emote_tag_parsing(c: &mut Criterion) {
    let simple = "25:0-4,12-16/1902:6-10";
    let dense = make_dense_emote_tag(40);
    let malformed = "bad:abc-def/25:0-4,invalid/3:9-x";

    let mut group = c.benchmark_group("filter_parse_emotes_tag");
    for (name, input) in [
        ("simple", simple),
        ("dense_40", dense.as_str()),
        ("malformed_mixed", malformed),
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |b, tag| {
            b.iter(|| black_box(parse_emotes_tag(black_box(tag))));
        });
    }
    group.finish();
}

fn bench_slot_parsing(c: &mut Criterion) {
    let empty = FxHashMap::default();
    let dense_tag = make_dense_emote_tag(36);
    let dense_map = parse_emotes_tag(&dense_tag);
    let dense_text = make_dense_emote_text(36);

    let short = "hello chat this is a short line";
    let mixed = "hey @BenchUser watch https://clips.twitch.tv/abc123 😀🔥 this is great";
    let long = "this is a very long chat line with repeated tokens and urls https://example.com/a \
https://example.com/b and mention @someone and unicode 😀😀😀 repeated to emulate noisy chat \
traffic in a busy channel where parsing cost matters";

    let mut group = c.benchmark_group("filter_slot_parser");
    group.bench_function("short_text", |b| {
        b.iter(|| {
            black_box(parse_to_slots(
                black_box(short),
                black_box("whisper"),
                EmoteSender::NONE,
                &empty,
            ))
        });
    });
    group.bench_function("mixed_url_mention_emoji", |b| {
        b.iter(|| {
            black_box(parse_to_slots(
                black_box(mixed),
                black_box("whisper"),
                EmoteSender::NONE,
                &empty,
            ))
        });
    });
    group.bench_function("long_noisy_message", |b| {
        b.iter(|| {
            black_box(parse_to_slots(
                black_box(long),
                black_box("whisper"),
                EmoteSender::NONE,
                &empty,
            ))
        });
    });
    group.bench_function("dense_twitch_emote_ranges", |b| {
        b.iter(|| {
            black_box(parse_to_slots(
                black_box(&dense_text),
                black_box("whisper"),
                EmoteSender::NONE,
                black_box(&dense_map),
            ))
        });
    });
    group.finish();
}

fn bench_structure_hash(c: &mut Criterion) {
    let dense_tag = make_dense_emote_tag(28);
    let dense_map = parse_emotes_tag(&dense_tag);
    let dense_text = make_dense_emote_text(28);
    let slots = parse_to_slots(&dense_text, "whisper", EmoteSender::NONE, &dense_map);

    c.bench_function("filter_structure_hash_slots_28_emotes", |b| {
        b.iter(|| black_box(compute_structure_hash(black_box(&slots))));
    });
}

/// Id prefix of the filter rules this bench creates; it removes no others.
const BENCH_FILTER_PREFIX: &str = "bench-regex-";

/// Point Echo's storage at a profile in the temp directory before anything
/// can read settings, and stop unless every root is inside it. The filter
/// bench writes settings, so it must never run against the normal profile.
fn isolate_profile() -> std::path::PathBuf {
    let profile = std::env::temp_dir().join("echo-parse-bench-profile");
    // SAFETY: called first in `main`, before criterion or Echo start any
    // thread that reads the environment.
    unsafe { std::env::set_var("ECHO_BENCH_PROFILE_ROOT", &profile) };
    storage_roots::initialize().expect("isolated bench profile initializes");
    assert_eq!(
        storage_roots::profile_root().as_deref(),
        Some(profile.as_path()),
        "the bench must run on its temp profile"
    );
    for root in [
        storage_roots::config_root(),
        storage_roots::data_root(),
        storage_roots::cache_root(),
        storage_roots::log_root(),
    ] {
        let root = root.expect("an isolated profile has every root");
        assert!(
            root.starts_with(&profile),
            "storage root '{}' is outside the bench profile '{}'",
            root.display(),
            profile.display()
        );
    }
    profile
}

/// Make the bench's own rules `count` enabled regex Hide rules, leaving any
/// other rule in the (isolated) settings alone.
fn set_regex_filters(count: usize) {
    for rule in settings::get_filters() {
        if rule.id.starts_with(BENCH_FILTER_PREFIX) {
            settings::remove_filter(&rule.id);
        }
    }
    for index in 0..count {
        settings::add_filter(FilterRule {
            id: format!("{BENCH_FILTER_PREFIX}{index}"),
            expression: format!(r#"message.content match r"(?i)\bspam{index}[a-z]*\d+""#),
            action: FilterAction::Hide,
            enabled: true,
            replacement: None,
            pattern_is_regex: false,
        });
    }
}

/// Per-message filter evaluation (`FILTER_STORE.test_message`) with 0, 1 and
/// 5 regex rules, on the temp profile `main` set up.
fn bench_filter_evaluation(c: &mut Criterion) {
    let profile = storage_roots::profile_root().expect("main isolated the profile");
    assert!(
        storage_roots::config_root().is_some_and(|root| root.starts_with(&profile)),
        "the filter bench writes settings and must run on the bench profile"
    );
    let ctx = TestContext {
        content: "hey chat did you see that play, absolutely wild stuff 123".to_string(),
        author: "BenchUser".to_string(),
        author_login: "benchuser".to_string(),
        channel_name: "benchchannel".to_string(),
        badges: vec!["subscriber/12".to_string()],
        ..Default::default()
    };

    let mut group = c.benchmark_group("filter_test_message");
    for count in [0usize, 1, 5] {
        set_regex_filters(count);
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{count}_regex")),
            &ctx,
            |b, ctx| b.iter(|| black_box(FILTER_STORE.test_message(black_box(ctx)))),
        );
    }
    group.finish();
    set_regex_filters(0);
}

criterion_group!(
    benches,
    bench_emote_tag_parsing,
    bench_slot_parsing,
    bench_structure_hash,
    bench_filter_evaluation
);

fn main() {
    isolate_profile();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
