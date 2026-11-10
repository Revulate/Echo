use echo_emote_core::quality::{
    EmoteImageScale, NOMINAL_ONE_X_EMOTE_PX, resolve_auto_tier, select_emote_display_url,
};
use std::hint::black_box;
use std::time::Instant;

const ITERS: usize = 5_000_000;

fn main() {
    let default_url = "https://cdn.example/emote/default.webp";
    let one_x = "https://cdn.example/emote/1x.webp";
    let two_x = "https://cdn.example/emote/2x.webp";
    let four_x = "https://cdn.example/emote/4x.webp";

    assert_eq!(
        select_emote_display_url(
            default_url,
            Some(one_x),
            Some(two_x),
            Some(four_x),
            EmoteImageScale::Auto,
        ),
        two_x
    );
    assert_eq!(
        select_emote_display_url(
            default_url,
            Some(one_x),
            Some(two_x),
            Some(four_x),
            EmoteImageScale::OneX,
        ),
        one_x
    );
    assert_eq!(
        select_emote_display_url(
            default_url,
            Some(one_x),
            Some(two_x),
            Some(four_x),
            EmoteImageScale::TwoX,
        ),
        two_x
    );
    assert_eq!(
        select_emote_display_url(
            default_url,
            Some(one_x),
            Some(two_x),
            Some(four_x),
            EmoteImageScale::FourX,
        ),
        four_x
    );
    assert_eq!(
        select_emote_display_url(default_url, None, Some(two_x), None, EmoteImageScale::FourX),
        two_x
    );
    assert_eq!(
        select_emote_display_url(default_url, Some(one_x), None, None, EmoteImageScale::FourX),
        one_x
    );
    assert_eq!(
        select_emote_display_url(default_url, None, None, None, EmoteImageScale::FourX),
        default_url
    );
    assert_eq!(
        select_emote_display_url(default_url, None, None, Some(four_x), EmoteImageScale::OneX),
        four_x
    );
    assert_eq!(
        select_emote_display_url(
            default_url,
            Some(one_x),
            None,
            Some(four_x),
            EmoteImageScale::TwoX
        ),
        four_x
    );
    assert_eq!(
        EmoteImageScale::from_settings_value("quality"),
        EmoteImageScale::FourX
    );
    assert_eq!(
        EmoteImageScale::from_settings_value("performance"),
        EmoteImageScale::OneX
    );
    assert_eq!(
        EmoteImageScale::from_settings_value("balanced"),
        EmoteImageScale::TwoX
    );
    assert_eq!(
        EmoteImageScale::from_settings_value("unknown"),
        EmoteImageScale::Auto
    );
    assert_eq!(EmoteImageScale::FourX.as_u8(), 4);
    assert_eq!(EmoteImageScale::from_u8(4).settings_value(), "4x");
    assert_eq!(EmoteImageScale::from_u8(9), EmoteImageScale::Auto);
    assert_eq!(resolve_auto_tier(1.0, 1.0), EmoteImageScale::TwoX);
    assert_eq!(resolve_auto_tier(1.5, 1.0), EmoteImageScale::TwoX);
    assert_eq!(resolve_auto_tier(2.0, 1.0), EmoteImageScale::FourX);
    assert_eq!(resolve_auto_tier(1.0, 0.75), EmoteImageScale::OneX);
    assert_eq!(resolve_auto_tier(1.0, 1.5), EmoteImageScale::TwoX);
    assert_eq!(
        resolve_auto_tier(1.0, 46.0 / NOMINAL_ONE_X_EMOTE_PX),
        EmoteImageScale::TwoX
    );
    assert_eq!(
        resolve_auto_tier(1.25, 46.0 / NOMINAL_ONE_X_EMOTE_PX),
        EmoteImageScale::FourX
    );

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows(default_url, one_x, two_x, four_x);
    bench_scale(
        "select_auto",
        EmoteImageScale::Auto,
        default_url,
        Some(one_x),
        Some(two_x),
        Some(four_x),
    );
    bench_scale(
        "select_4x",
        EmoteImageScale::FourX,
        default_url,
        Some(one_x),
        Some(two_x),
        Some(four_x),
    );
    bench_scale(
        "select_4x_fallback",
        EmoteImageScale::FourX,
        default_url,
        Some(one_x),
        Some(two_x),
        None,
    );
    bench_mixed(default_url, one_x, two_x, four_x);
}

fn print_smoke_rows(default_url: &str, one_x: &str, two_x: &str, four_x: &str) {
    for (name, scale, expected) in [
        ("emote_quality_select_auto", EmoteImageScale::Auto, two_x),
        ("emote_quality_select_1x", EmoteImageScale::OneX, one_x),
        ("emote_quality_select_2x", EmoteImageScale::TwoX, two_x),
        ("emote_quality_select_4x", EmoteImageScale::FourX, four_x),
    ] {
        print_fixture(
            name,
            select_emote_display_url(default_url, Some(one_x), Some(two_x), Some(four_x), scale),
            expected,
        );
    }

    print_fixture(
        "emote_quality_4x_fallback_2x",
        select_emote_display_url(default_url, None, Some(two_x), None, EmoteImageScale::FourX),
        two_x,
    );
    print_fixture(
        "emote_quality_4x_fallback_1x",
        select_emote_display_url(default_url, Some(one_x), None, None, EmoteImageScale::FourX),
        one_x,
    );
    print_fixture(
        "emote_quality_4x_fallback_default",
        select_emote_display_url(default_url, None, None, None, EmoteImageScale::FourX),
        default_url,
    );
    print_fixture(
        "emote_quality_1x_fallback_4x",
        select_emote_display_url(default_url, None, None, Some(four_x), EmoteImageScale::OneX),
        four_x,
    );
    print_fixture(
        "emote_quality_2x_fallback_4x",
        select_emote_display_url(
            default_url,
            Some(one_x),
            None,
            Some(four_x),
            EmoteImageScale::TwoX,
        ),
        four_x,
    );

    for (name, value, expected) in [
        ("emote_quality_setting_quality", "quality", "4x"),
        ("emote_quality_setting_performance", "performance", "1x"),
        ("emote_quality_setting_balanced", "balanced", "2x"),
        ("emote_quality_setting_unknown", "unknown", "auto"),
    ] {
        print_fixture(
            name,
            EmoteImageScale::from_settings_value(value).settings_value(),
            expected,
        );
    }

    print_fixture(
        "emote_quality_from_u8_4",
        EmoteImageScale::from_u8(4).settings_value(),
        "4x",
    );
    print_fixture(
        "emote_quality_from_u8_unknown",
        EmoteImageScale::from_u8(9).settings_value(),
        "auto",
    );

    for (name, device_scale, rendered_scale, expected) in [
        (
            "emote_quality_auto_chat_1080p",
            1.0,
            1.0,
            EmoteImageScale::TwoX,
        ),
        (
            "emote_quality_auto_chat_150pct",
            1.5,
            1.0,
            EmoteImageScale::TwoX,
        ),
        (
            "emote_quality_auto_chat_200pct",
            2.0,
            1.0,
            EmoteImageScale::FourX,
        ),
        (
            "emote_quality_auto_small_chat",
            1.0,
            0.75,
            EmoteImageScale::OneX,
        ),
        (
            "emote_quality_auto_picker_1080p",
            1.0,
            46.0 / NOMINAL_ONE_X_EMOTE_PX,
            EmoteImageScale::TwoX,
        ),
        (
            "emote_quality_auto_picker_125pct",
            1.25,
            46.0 / NOMINAL_ONE_X_EMOTE_PX,
            EmoteImageScale::FourX,
        ),
    ] {
        print_fixture(
            name,
            resolve_auto_tier(device_scale, rendered_scale).settings_value(),
            expected.settings_value(),
        );
    }
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn bench_scale(
    name: &str,
    scale: EmoteImageScale,
    default_url: &str,
    one_x: Option<&str>,
    two_x: Option<&str>,
    four_x: Option<&str>,
) {
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..ITERS {
        let url = select_emote_display_url(
            black_box(default_url),
            black_box(one_x),
            black_box(two_x),
            black_box(four_x),
            black_box(scale),
        );
        checksum = checksum.wrapping_add(url.len());
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / ITERS as f64;
    let units_per_second = ITERS as f64 / elapsed.as_secs_f64();
    println!(
        "resolve,{},{},{:.2},{:.0},{}",
        name, ITERS, ns_per_unit, units_per_second, checksum
    );
}

fn bench_mixed(default_url: &str, one_x: &str, two_x: &str, four_x: &str) {
    let scales = [
        EmoteImageScale::Auto,
        EmoteImageScale::OneX,
        EmoteImageScale::TwoX,
        EmoteImageScale::FourX,
    ];
    let start = Instant::now();
    let mut checksum = 0usize;
    for i in 0..ITERS {
        let url = select_emote_display_url(
            black_box(default_url),
            black_box(Some(one_x)),
            black_box(Some(two_x)),
            black_box(Some(four_x)),
            black_box(scales[i & 3]),
        );
        checksum = checksum.wrapping_add(url.as_bytes()[0] as usize + url.len());
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / ITERS as f64;
    let units_per_second = ITERS as f64 / elapsed.as_secs_f64();
    println!(
        "resolve,select_mixed,{},{:.2},{:.0},{}",
        ITERS, ns_per_unit, units_per_second, checksum
    );
}
