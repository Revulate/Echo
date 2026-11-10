//! Real layout/render-path benchmarks.
//!
//! Covers critical hot paths that affect rendering smoothness:
//! - Message layout build and text shaping
//! - Height tree viewport/index queries
//! - Scroll marker computation for chat minimap

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use echo_gpui::backend::emotes::EmoteSender;
use echo_gpui::backend::models::{ChatMessage, HighlightType, MessageFlags, ReplyInfo};
use echo_gpui::backend::slots::compute_structure_hash;
use echo_gpui::backend::slots::parser::{parse_emotes_tag, parse_to_slots};
use echo_gpui::components::chat::canvas_renderer::height_tree::HeightTree;
use echo_gpui::components::chat::canvas_renderer::layout::{ImageType, LayoutSegment};
use echo_gpui::components::chat::canvas_renderer::text_shaping::build_message_layout;
use echo_gpui::components::chat::scroll_map::compute_scroll_markers;
use smallvec::SmallVec;
use std::sync::Arc;

fn make_chat_message(
    id: usize,
    content: &str,
    channel: &str,
    twitch_emotes_tag: Option<&str>,
    highlight_type: Option<HighlightType>,
    is_first_message: bool,
    with_reply: bool,
) -> ChatMessage {
    let twitch_emotes = twitch_emotes_tag.map(parse_emotes_tag).unwrap_or_default();
    let slots = parse_to_slots(content, channel, EmoteSender::NONE, &twitch_emotes);
    let structure_hash = compute_structure_hash(&slots);

    ChatMessage {
        id: Arc::<str>::from(format!("bench-msg-{id}")),
        channel: Arc::<str>::from(channel.to_string()),
        sender: Arc::<str>::from("bench_sender"),
        display_name: Arc::<str>::from("BenchSender"),
        content: Arc::<str>::from(content.to_string()),
        color: Some(Arc::<str>::from("#00AAFF")),
        badges: SmallVec::new(),
        timestamp: 1_709_500_000 + id as i64,
        slots: Arc::from(slots),
        structure_hash,
        reply: with_reply.then_some(ReplyInfo {
            parent_msg_id: Arc::<str>::from("parent-msg-1"),
            parent_user_login: Arc::<str>::from("another_user"),
            parent_display_name: Arc::<str>::from("AnotherUser"),
            parent_user_id: None,
            parent_msg_body: Some(Arc::<str>::from("that earlier message was wild")),
        }),
        flags: MessageFlags {
            is_first_message,
            highlight_type,
            ..MessageFlags::default()
        },
        twitch_emotes_tag: twitch_emotes_tag.map(|s| Arc::<str>::from(s.to_string())),
        user_id: Some(Arc::<str>::from("12345")),
        reply_emote_offset: 0,
        ..ChatMessage::default()
    }
}

fn make_dense_twitch_emote_fixture(count: usize) -> (String, String) {
    let mut content = String::new();
    let mut ranges = Vec::with_capacity(count);
    let mut char_pos = 0usize;

    for i in 0..count {
        if i > 0 {
            content.push(' ');
            char_pos += 1;
        }
        content.push_str("Kappa");
        let start = char_pos;
        let end = char_pos + 4;
        ranges.push(format!("{start}-{end}"));
        char_pos += 5;
    }

    (content, format!("25:{}", ranges.join(",")))
}

const BENCH_BADGES: [&str; 4] = [
    "moderator:1",
    "vip:1",
    "lead_moderator:1",
    "lead-moderator:1",
];

fn make_badged_chat_message(id: usize, badge_count: usize) -> ChatMessage {
    let mut message = make_chat_message(
        id,
        "hello chat, this production-shaped badge fixture still exercises wrapping and username geometry",
        "badge-bench",
        None,
        None,
        false,
        false,
    );
    message.badges = BENCH_BADGES[..badge_count]
        .iter()
        .map(|badge| Arc::<str>::from(*badge))
        .collect();
    message
}

fn badge_segment_count(
    layout: &echo_gpui::components::chat::canvas_renderer::layout::CanvasMessageLayout,
) -> usize {
    layout
        .segments
        .iter()
        .filter(|segment| {
            matches!(
                segment,
                LayoutSegment::Image {
                    image_type: ImageType::Badge { .. },
                    ..
                }
            )
        })
        .count()
}

fn bench_build_message_layout(c: &mut Criterion) {
    echo_gpui::components::chat::canvas_renderer::install_render_ports();
    let base = make_chat_message(
        1,
        "hello @bench check https://example.com 😀 this is a realistic twitch line with wrapping",
        "whisper",
        None,
        None,
        false,
        false,
    );
    let reply = make_chat_message(
        2,
        "this is a reply with more content to force wrapping and extra shaping work across lines",
        "whisper",
        None,
        Some(HighlightType::Mention),
        false,
        true,
    );
    let (dense_content, dense_tag) = make_dense_twitch_emote_fixture(18);
    let emote_dense = make_chat_message(
        3,
        &dense_content,
        "whisper",
        Some(&dense_tag),
        Some(HighlightType::Subscription),
        true,
        false,
    );

    let mut group = c.benchmark_group("layout_build");
    for width in [360.0f32, 720.0, 1080.0] {
        group.bench_with_input(
            BenchmarkId::new("base_message", format!("{width:.0}px")),
            &width,
            |b, &w| {
                b.iter(|| {
                    black_box(build_message_layout(
                        black_box(&base),
                        black_box("whisper"),
                        black_box(w),
                        black_box(true),
                        None,
                    ))
                });
            },
        );
    }

    group.bench_function("reply_indicator_path_720px", |b| {
        b.iter(|| {
            black_box(build_message_layout(
                black_box(&reply),
                black_box("whisper"),
                black_box(720.0),
                black_box(true),
                None,
            ))
        });
    });

    group.bench_function("twitch_emote_dense_720px", |b| {
        b.iter(|| {
            black_box(build_message_layout(
                black_box(&emote_dense),
                black_box("whisper"),
                black_box(720.0),
                black_box(true),
                None,
            ))
        });
    });
    group.finish();
}

fn bench_badged_message_layout(c: &mut Criterion) {
    echo_gpui::components::chat::canvas_renderer::install_render_ports();

    // Seed the production badge resolver with deterministic room-authority URLs.
    // Layout records geometry and URLs only; it does not fetch these images.
    echo_gpui::backend::badges::TWITCH_BADGE_CACHE.publish_ffz_channel_badges(
        "badge-bench",
        Some("https://static-cdn.jtvnw.net/badges/v1/moderator/3".to_string()),
        Some("https://static-cdn.jtvnw.net/badges/v1/vip/3".to_string()),
        Default::default(),
    );

    let mut group = c.benchmark_group("layout_badges");
    for badge_count in 1..=BENCH_BADGES.len() {
        let warm_message = make_badged_chat_message(100 + badge_count, badge_count);
        let warm_layout = build_message_layout(&warm_message, "badge-bench", 720.0, true, None);
        assert_eq!(badge_segment_count(&warm_layout), badge_count);

        group.bench_with_input(
            BenchmarkId::new("fresh_message_and_layout", badge_count),
            &badge_count,
            |b, &count| {
                b.iter(|| {
                    let message = make_badged_chat_message(black_box(count), count);
                    black_box(build_message_layout(
                        black_box(&message),
                        black_box("badge-bench"),
                        black_box(720.0),
                        black_box(true),
                        None,
                    ))
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("repeated_layout_warm_caches", badge_count),
            &warm_message,
            |b, message| {
                b.iter(|| {
                    black_box(build_message_layout(
                        black_box(message),
                        black_box("badge-bench"),
                        black_box(720.0),
                        black_box(true),
                        None,
                    ))
                });
            },
        );
    }
    group.finish();
}

fn bench_height_tree(c: &mut Criterion) {
    let mut tree = HeightTree::with_capacity(16_384);
    for i in 0..10_000 {
        let h = 18.0 + ((i % 7) as f32 * 2.75);
        tree.push(h);
    }

    let mut group = c.benchmark_group("height_tree");
    group.bench_function("visible_range_10k", |b| {
        b.iter(|| {
            black_box(tree.visible_range(black_box(82_000.0), black_box(900.0), black_box(250.0)))
        });
    });

    group.bench_function("index_at_y_10k", |b| {
        b.iter(|| black_box(tree.index_at_y(black_box(93_500.0))));
    });

    group.bench_function("update_200_indices", |b| {
        b.iter_batched(
            || tree.clone(),
            |mut t| {
                for i in 0..200usize {
                    let idx = (i * 47) % 10_000;
                    let new_h = 20.0 + (i % 5) as f32 * 3.0;
                    t.update(idx, new_h);
                }
                black_box(t.total_height());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_scroll_marker_compute(c: &mut Criterion) {
    let mut messages = Vec::with_capacity(5_000);
    for i in 0..5_000usize {
        let highlight = if i % 97 == 0 {
            Some(HighlightType::Mention)
        } else if i % 311 == 0 {
            Some(HighlightType::Subscription)
        } else {
            None
        };
        let is_first = i % 53 == 0;
        let content = if i % 29 == 0 {
            "hey BenchUser check this clip https://twitch.tv/clip/abc123"
        } else {
            "normal chat line without mention"
        };
        messages.push(std::sync::Arc::new(make_chat_message(
            i, content, "whisper", None, highlight, is_first, false,
        )));
    }

    c.bench_function("scroll_map_markers_5k_messages", |b| {
        b.iter(|| {
            black_box(compute_scroll_markers(
                black_box(&messages),
                Some("benchuser"),
                None,
            ))
        });
    });
}

criterion_group!(
    benches,
    bench_build_message_layout,
    bench_badged_message_layout,
    bench_height_tree,
    bench_scroll_marker_compute
);
criterion_main!(benches);
