//! Production-shaped GPUI scene benchmarks for Echo's retained chat canvas.

use std::sync::Arc;

use echo_gpui::backend::emotes::EmoteSender;
use echo_gpui::backend::models::{ChatMessage, MessageFlags};
use echo_gpui::backend::slots::compute_structure_hash;
use echo_gpui::backend::slots::parser::{parse_emotes_tag, parse_to_slots};
use echo_gpui::components::chat::canvas_renderer::{CanvasMessageList, install_render_ports};
use echo_gpui::state::SplitId;
use gpui::{BenchAppContext, BenchWindowContext, Empty, Entity};
use smallvec::{SmallVec, smallvec};

const CHANNEL: &str = "gpui-scene-bench";
const MESSAGE_COUNT: usize = 25_000;

#[derive(Clone, Copy)]
enum SceneFixture {
    Text,
    Badges,
    DenseEmotes,
}

fn make_message(id: usize, fixture: SceneFixture) -> ChatMessage {
    let (content, emotes_tag, badges): (&str, Option<&str>, SmallVec<[Arc<str>; 4]>) = match fixture
    {
        SceneFixture::Text => (
            "hello @bench check https://example.com this is a realistic wrapped chat line",
            None,
            SmallVec::new(),
        ),
        SceneFixture::Badges => (
            "badge-heavy production row with a username and enough text to shape",
            None,
            smallvec![Arc::from("moderator:1"), Arc::from("vip:1")],
        ),
        SceneFixture::DenseEmotes => (
            "Kappa Kappa Kappa Kappa Kappa Kappa",
            Some("25:0-4,6-10,12-16,18-22,24-28,30-34"),
            smallvec![Arc::from("moderator:1")],
        ),
    };
    let twitch_emotes = emotes_tag.map(parse_emotes_tag).unwrap_or_default();
    let slots = parse_to_slots(content, CHANNEL, EmoteSender::NONE, &twitch_emotes);

    ChatMessage {
        id: Arc::from(format!("scene-{id}")),
        channel: Arc::from(CHANNEL),
        sender: Arc::from(format!("sender-{}", id % 200)),
        display_name: Arc::from(format!("Sender{}", id % 200)),
        content: Arc::from(content),
        color: Some(Arc::from("#00AAFF")),
        badges,
        timestamp: 1_709_500_000 + id as i64,
        structure_hash: compute_structure_hash(&slots),
        slots: Arc::from(slots),
        reply: None,
        flags: MessageFlags::default(),
        twitch_emotes_tag: emotes_tag.map(Arc::from),
        user_id: Some(Arc::from(format!("user-{id}"))),
        reply_emote_offset: 0,
        ..ChatMessage::default()
    }
}

fn make_messages(fixture: SceneFixture) -> Arc<Vec<Arc<ChatMessage>>> {
    Arc::new(
        (0..MESSAGE_COUNT)
            .map(|id| Arc::new(make_message(id, fixture)))
            .collect(),
    )
}

fn setup_scene<'a, 'measurement>(
    cx: &mut BenchAppContext<'a, 'measurement>,
    fixture: SceneFixture,
) -> (
    BenchWindowContext<'a, 'measurement>,
    Entity<CanvasMessageList>,
) {
    install_render_ports();
    echo_gpui::backend::badges::TWITCH_BADGE_CACHE.publish_ffz_channel_badges(
        CHANNEL,
        Some("https://static-cdn.jtvnw.net/badges/v1/moderator/3".to_string()),
        Some("https://static-cdn.jtvnw.net/badges/v1/vip/3".to_string()),
        Default::default(),
    );

    let messages = make_messages(fixture);
    let mut window = cx.add_empty_window();
    let canvas = window.update(|window, cx| {
        window.replace_root(cx, |_window, cx| {
            let mut canvas = CanvasMessageList::new(CHANNEL.to_string(), SplitId::new(), 980.0, cx);
            canvas.set_viewport_height(720.0);
            canvas.set_messages(messages);
            canvas.scroll_to_fraction(0.55, false);
            canvas
        })
    });
    window.run_until_idle();
    (window, canvas)
}

fn finish_scene(
    cx: &mut BenchAppContext<'_, '_>,
    mut window: BenchWindowContext<'_, '_>,
    canvas: Entity<CanvasMessageList>,
) {
    for _ in 0..100 {
        let drained = cx.update(|cx| {
            canvas.update(cx, |canvas, cx| {
                canvas.set_tab_visible(false);
                canvas.set_active(false);
                canvas.set_messages(Arc::new(Vec::new()));
                let drained = canvas.prepare_benchmark_teardown();
                cx.notify();
                drained
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        cx.run_until_idle();
        if drained {
            break;
        }
    }
    window.update(|window, cx| {
        window.replace_root(cx, |_window, _cx| Empty);
    });
    drop(canvas);
    std::thread::sleep(std::time::Duration::from_millis(50));
    cx.run_until_idle();
}

#[gpui::bench]
fn chat_scene_steady_text(cx: &mut BenchAppContext) {
    let (window, canvas) = setup_scene(cx, SceneFixture::Text);
    cx.bench_renderer(canvas.clone(), |_canvas, _window, cx| cx.notify());
    finish_scene(cx, window, canvas);
}

#[gpui::bench]
fn chat_scene_badges(cx: &mut BenchAppContext) {
    let (window, canvas) = setup_scene(cx, SceneFixture::Badges);
    cx.bench_renderer(canvas.clone(), |_canvas, _window, cx| cx.notify());
    finish_scene(cx, window, canvas);
}

#[gpui::bench]
fn chat_scene_dense_emotes(cx: &mut BenchAppContext) {
    let (window, canvas) = setup_scene(cx, SceneFixture::DenseEmotes);
    cx.bench_renderer(canvas.clone(), |_canvas, _window, cx| cx.notify());
    finish_scene(cx, window, canvas);
}

#[gpui::bench]
fn chat_scene_scroll_translation(cx: &mut BenchAppContext) {
    let (window, canvas) = setup_scene(cx, SceneFixture::DenseEmotes);
    let mut direction = 1.0;
    cx.bench_renderer(canvas.clone(), move |canvas, _window, cx| {
        canvas.scroll_by_pixels(120.0 * direction, cx);
        direction = -direction;
    });
    finish_scene(cx, window, canvas);
}

gpui::bench_group!(
    benches,
    chat_scene_steady_text,
    chat_scene_badges,
    chat_scene_dense_emotes,
    chat_scene_scroll_translation
);
gpui::bench_main!(benches);
