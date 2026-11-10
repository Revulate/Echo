#[path = "../../../src/backend/embed_thumbnails.rs"]
mod embed_thumbnails;

use embed_thumbnails::{
    EmbedThumbnailMode, is_valid_embed_thumbnail_url, preview_height, should_render_thumbnail,
    thumbnail_dimensions,
};
use std::hint::black_box;
use std::time::Instant;

const ITERS: usize = 5_000_000;

fn check_aspect_ratio_sizing() {
    use embed_thumbnails::fit_thumbnail_to_height;
    let (width, height) = fit_thumbnail_to_height(517.0, 219.0, 700.0, 90.0);
    assert!((width - 212.46576).abs() < 0.001);
    assert_eq!(height, 90.0);
    let (width, height) = fit_thumbnail_to_height(517.0, 219.0, 500.0, 56.0);
    assert!((width - 132.20091).abs() < 0.001);
    assert_eq!(height, 56.0);
    let (width, height) = fit_thumbnail_to_height(2000.0, 100.0, 600.0, 90.0);
    assert_eq!(width, 600.0);
    assert!((height - 30.0).abs() < 0.001);
    assert_eq!(
        fit_thumbnail_to_height(100.0, 200.0, 600.0, 90.0),
        (45.0, 90.0)
    );
    assert_eq!(fit_thumbnail_to_height(100.0, 100.0, 0.0, 90.0), (0.0, 0.0));
    assert_eq!(
        fit_thumbnail_to_height(f32::NAN, 100.0, 600.0, 90.0),
        (0.0, 0.0)
    );
}

#[derive(Clone, Copy)]
struct ProviderThumbnailCase {
    provider: &'static str,
    url: &'static str,
    renderable: bool,
}

const PROVIDER_THUMBNAIL_CASES: &[ProviderThumbnailCase] = &[
    ProviderThumbnailCase {
        provider: "betterttv",
        url: "https://cdn.betterttv.net/emote/54fa8f1401e468494b85b537/3x.webp",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "discord",
        url: "https://cdn.discordapp.com/attachments/123/456/preview.png",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "frankerfacez",
        url: "https://cdn.frankerfacez.com/emote/12345/4",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "imgur",
        url: "https://i.imgur.com/example.jpg",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "livestreamfails",
        url: "https://cdn.livestreamfails.com/poster/example.jpg",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "reddit_external_preview",
        url: "https://external-preview.redd.it/example.jpg",
        renderable: false,
    },
    ProviderThumbnailCase {
        provider: "reddit_video",
        url: "https://v.redd.it/example/DASH_720.mp4",
        renderable: false,
    },
    ProviderThumbnailCase {
        provider: "supinic",
        url: "https://supinic.com/api/bot/image/example.png",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "twitch_clip",
        url: "https://clips-media-assets2.twitch.tv/FunnyClip-preview-480x272.jpg",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "twitter",
        url: "https://pbs.twimg.com/media/example.jpg",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "wikipedia",
        url: "https://upload.wikimedia.org/wikipedia/commons/thumb/a/a9/Example.jpg/320px-Example.jpg",
        renderable: true,
    },
    ProviderThumbnailCase {
        provider: "youtube",
        url: "https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg",
        renderable: true,
    },
];

fn main() {
    check_aspect_ratio_sizing();
    assert_eq!(
        EmbedThumbnailMode::from_settings_value("compact"),
        EmbedThumbnailMode::Small
    );
    assert_eq!(
        EmbedThumbnailMode::from_settings_value("160x90"),
        EmbedThumbnailMode::Medium
    );
    assert_eq!(
        EmbedThumbnailMode::from_settings_value("big").settings_value(),
        "large"
    );
    assert_eq!(
        EmbedThumbnailMode::from_settings_value("disabled"),
        EmbedThumbnailMode::None
    );

    assert_eq!(thumbnail_dimensions(EmbedThumbnailMode::None), None);
    assert_eq!(
        thumbnail_dimensions(EmbedThumbnailMode::Small),
        Some((96.0, 54.0))
    );
    assert_eq!(
        thumbnail_dimensions(EmbedThumbnailMode::Medium),
        Some((160.0, 90.0))
    );
    assert_eq!(
        thumbnail_dimensions(EmbedThumbnailMode::Large),
        Some((240.0, 135.0))
    );

    let valid = "https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg";
    let reddit_video = "https://v.redd.it/example/DASH_720.mp4";
    let reddit_preview = "https://external-preview.redd.it/example.jpg";
    let reddit_video_uppercase = "https://V.REDD.IT/example/DASH_720.mp4";
    let reddit_preview_uppercase = "https://EXTERNAL-PREVIEW.REDD.IT/example.jpg";

    assert!(is_valid_embed_thumbnail_url(valid));
    assert!(!is_valid_embed_thumbnail_url(reddit_video));
    assert!(!is_valid_embed_thumbnail_url(reddit_preview));
    assert!(!is_valid_embed_thumbnail_url(reddit_video_uppercase));
    assert!(!is_valid_embed_thumbnail_url(reddit_preview_uppercase));
    assert!(!should_render_thumbnail(
        EmbedThumbnailMode::None,
        Some(valid)
    ));
    assert!(should_render_thumbnail(
        EmbedThumbnailMode::Medium,
        Some(valid)
    ));
    assert!(!should_render_thumbnail(
        EmbedThumbnailMode::Medium,
        Some(reddit_preview)
    ));
    assert!(!should_render_thumbnail(
        EmbedThumbnailMode::Medium,
        Some(reddit_preview_uppercase)
    ));
    assert!(!should_render_thumbnail(EmbedThumbnailMode::Medium, None));

    assert_eq!(
        preview_height(EmbedThumbnailMode::None, false, 12.0, 100.0, 76.0),
        76.0
    );
    assert_eq!(
        preview_height(EmbedThumbnailMode::Small, true, 12.0, 100.0, 76.0),
        100.0
    );
    assert_eq!(
        preview_height(EmbedThumbnailMode::Large, true, 12.0, 100.0, 76.0),
        159.0
    );

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_provider_thumbnail_fixture_rows();
    bench_render_decision(valid, reddit_preview);
    bench_dimensions();
    bench_preview_height();
}

fn print_provider_thumbnail_fixture_rows() {
    for case in PROVIDER_THUMBNAIL_CASES {
        assert_eq!(
            is_valid_embed_thumbnail_url(case.url),
            case.renderable,
            "thumbnail URL filter mismatch for {}",
            case.provider
        );
        assert_eq!(
            should_render_thumbnail(EmbedThumbnailMode::Medium, Some(case.url)),
            case.renderable,
            "thumbnail render decision mismatch for {}",
            case.provider
        );
        println!(
            "fixture,provider_thumbnail_{},1,0.00,0,{}",
            case.provider, case.renderable as usize
        );
    }
}

fn bench_render_decision(valid: &str, invalid: &str) {
    let urls = [Some(valid), Some(invalid), None, Some(valid)];
    let modes = [
        EmbedThumbnailMode::None,
        EmbedThumbnailMode::Small,
        EmbedThumbnailMode::Medium,
        EmbedThumbnailMode::Large,
    ];

    let start = Instant::now();
    let mut checksum = 0usize;
    for i in 0..ITERS {
        let render =
            should_render_thumbnail(black_box(modes[i & 3]), black_box(urls[(i >> 2) & 3]));
        checksum = checksum.wrapping_add(render as usize);
    }
    print_measurement(
        "resolve",
        "thumbnail_render_decision",
        ITERS,
        start,
        checksum,
    );
}

fn bench_dimensions() {
    let modes = [
        EmbedThumbnailMode::None,
        EmbedThumbnailMode::Small,
        EmbedThumbnailMode::Medium,
        EmbedThumbnailMode::Large,
    ];

    let start = Instant::now();
    let mut checksum = 0usize;
    for i in 0..ITERS {
        let dims = thumbnail_dimensions(black_box(modes[i & 3]));
        if let Some((width, height)) = dims {
            checksum = checksum.wrapping_add(width as usize + height as usize);
        }
    }
    print_measurement("resolve", "thumbnail_dimensions", ITERS, start, checksum);
}

fn bench_preview_height() {
    let modes = [
        EmbedThumbnailMode::None,
        EmbedThumbnailMode::Small,
        EmbedThumbnailMode::Medium,
        EmbedThumbnailMode::Large,
    ];

    let start = Instant::now();
    let mut checksum = 0usize;
    for i in 0..ITERS {
        let height = preview_height(
            black_box(modes[i & 3]),
            black_box((i & 1) == 0),
            black_box(12.0),
            black_box(100.0),
            black_box(76.0),
        );
        checksum = checksum.wrapping_add(height as usize);
    }
    print_measurement("resolve", "preview_height", ITERS, start, checksum);
}

fn print_measurement(kind: &str, name: &str, units: usize, start: Instant, checksum: usize) {
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / units as f64;
    let units_per_second = units as f64 / elapsed.as_secs_f64();
    println!(
        "{},{},{},{:.2},{:.0},{}",
        kind, name, units, ns_per_unit, units_per_second, checksum
    );
}
