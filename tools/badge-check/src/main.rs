#[path = "../../../src/backend/badges_core.rs"]
mod badges_core;

use badges_core::{
    BTTV_PRO_BADGE_KEY, ExternalBadgeProvider, NO_AUDIO_BADGE_KEY, NO_AUDIO_BADGE_URL,
    NO_VIDEO_BADGE_KEY, NO_VIDEO_BADGE_URL, badge_key_matches, badge_key_name,
    bttv_badge_cache_key, bttv_pro_badge_cache_key, embedded_twitch_badge_fallback_url,
    external_badge_provider_for_key, ffz_badge_cache_key, is_lead_moderator_badge_name,
    is_moderator_badge_key, normalize_badge_cache_key,
};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    assert_eq!(badge_key_name("subscriber/12"), "subscriber");
    assert_eq!(badge_key_name("moderator:1"), "moderator");
    assert_eq!(normalize_badge_cache_key("no_video/1"), NO_VIDEO_BADGE_KEY);
    assert_eq!(normalize_badge_cache_key("no_audio:1"), NO_AUDIO_BADGE_KEY);
    assert_eq!(ffz_badge_cache_key("developer"), "ffz:developer");
    assert_eq!(ffz_badge_cache_key("supporter"), "ffz:supporter");
    assert_eq!(ffz_badge_cache_key("Bot"), "ffz-bot:bot");
    assert_eq!(
        bttv_badge_cache_key(1, "NightDev Developer"),
        "bttv:developer"
    );
    assert_eq!(
        bttv_badge_cache_key(2, "NightDev Support Volunteer"),
        "bttv:support-volunteer"
    );
    assert_eq!(
        bttv_badge_cache_key(3, "BetterTTV Emote Approver"),
        "bttv:emote-approver"
    );
    assert_eq!(
        bttv_badge_cache_key(99, "Custom Badge"),
        "bttv:custom-badge"
    );
    assert_eq!(bttv_pro_badge_cache_key(), BTTV_PRO_BADGE_KEY);
    assert_eq!(bttv_pro_badge_cache_key(), "bttv-pro:subscriber");

    assert!(is_lead_moderator_badge_name("lead-moderator"));
    assert!(badge_key_matches("lead_moderator:1", "lead-moderator"));
    assert!(is_moderator_badge_key("lead_moderator:1"));
    assert!(is_moderator_badge_key("moderator/1"));

    assert_eq!(
        embedded_twitch_badge_fallback_url("no_audio/1"),
        Some(NO_AUDIO_BADGE_URL)
    );
    assert_eq!(
        embedded_twitch_badge_fallback_url("no_video:1"),
        Some(NO_VIDEO_BADGE_URL)
    );
    assert!(NO_AUDIO_BADGE_URL.contains("static-cdn.jtvnw.net/badges/v1/"));
    assert!(NO_VIDEO_BADGE_URL.contains("static-cdn.jtvnw.net/badges/v1/"));

    assert_eq!(
        external_badge_provider_for_key("ffz:bot"),
        Some(ExternalBadgeProvider::Ffz)
    );
    assert_eq!(
        external_badge_provider_for_key("frankerfacez-bot:1"),
        Some(ExternalBadgeProvider::FfzBot)
    );
    assert_eq!(
        external_badge_provider_for_key("betterttv:helper"),
        Some(ExternalBadgeProvider::Bttv)
    );
    assert_eq!(
        external_badge_provider_for_key("bttv-pro:1"),
        Some(ExternalBadgeProvider::BttvPro)
    );
    assert_eq!(external_badge_provider_for_key("moderator:1"), None);
    assert_eq!(ExternalBadgeProvider::Ffz.source_label(), "FFZ Badge");
    assert_eq!(ExternalBadgeProvider::Bttv.source_label(), "BTTV Badge");
    assert_eq!(
        ExternalBadgeProvider::BttvPro.source_label(),
        "BTTV Pro Badge"
    );
    assert_eq!(
        ExternalBadgeProvider::FfzBot.source_label(),
        "FFZ Bot Badge"
    );

    let iterations = 500_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let key = normalize_badge_cache_key(black_box("lead_moderator/1"));
        checksum = checksum.wrapping_add(key.len());
        checksum = checksum.wrapping_add(is_moderator_badge_key(black_box(&key)) as usize);
        checksum = checksum.wrapping_add(ffz_badge_cache_key(black_box("supporter")).len());
        checksum = checksum
            .wrapping_add(bttv_badge_cache_key(black_box(3), black_box("Emote Approver")).len());
        checksum = checksum.wrapping_add(bttv_pro_badge_cache_key().len());
        checksum = checksum.wrapping_add(
            external_badge_provider_for_key(black_box("betterttv-pro:1"))
                .map(|provider| provider.source_label().len())
                .unwrap_or_default(),
        );
        checksum = checksum.wrapping_add(
            embedded_twitch_badge_fallback_url(black_box("no_video/1"))
                .map(str::len)
                .unwrap_or_default(),
        );
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / iterations as f64;
    let units_per_second = iterations as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows();
    println!(
        "format,badge_core_helpers,{},{:.2},{:.0},{}",
        iterations, ns_per_unit, units_per_second, checksum
    );
}

fn print_smoke_rows() {
    print_fixture(
        "badge_name_slash",
        badge_key_name("subscriber/12"),
        "subscriber",
    );
    print_fixture(
        "badge_name_colon",
        badge_key_name("moderator:1"),
        "moderator",
    );
    print_fixture(
        "badge_normalize_no_video",
        &normalize_badge_cache_key("no_video/1"),
        NO_VIDEO_BADGE_KEY,
    );
    print_fixture(
        "badge_normalize_no_audio",
        &normalize_badge_cache_key("no_audio:1"),
        NO_AUDIO_BADGE_KEY,
    );
    print_fixture(
        "badge_ffz_bot_key",
        &ffz_badge_cache_key("Bot"),
        "ffz-bot:bot",
    );
    print_fixture(
        "badge_bttv_developer_key",
        &bttv_badge_cache_key(1, "NightDev Developer"),
        "bttv:developer",
    );
    print_fixture(
        "badge_bttv_custom_key",
        &bttv_badge_cache_key(99, "Custom Badge"),
        "bttv:custom-badge",
    );
    print_fixture(
        "badge_bttv_pro_key",
        bttv_pro_badge_cache_key(),
        BTTV_PRO_BADGE_KEY,
    );
    print_fixture(
        "badge_lead_moderator_alias",
        bool_text(badge_key_matches("lead_moderator:1", "lead-moderator")),
        "true",
    );
    print_fixture(
        "badge_lead_moderator_is_mod",
        bool_text(is_moderator_badge_key("lead_moderator:1")),
        "true",
    );
    print_fixture(
        "badge_no_audio_fallback",
        bool_text(embedded_twitch_badge_fallback_url("no_audio/1") == Some(NO_AUDIO_BADGE_URL)),
        "true",
    );
    print_fixture(
        "badge_no_video_fallback",
        bool_text(embedded_twitch_badge_fallback_url("no_video:1") == Some(NO_VIDEO_BADGE_URL)),
        "true",
    );
    print_fixture(
        "badge_external_ffz_label",
        external_badge_provider_for_key("ffz:bot")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "FFZ Badge",
    );
    print_fixture(
        "badge_external_ffz_bot_label",
        external_badge_provider_for_key("frankerfacez-bot:1")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "FFZ Bot Badge",
    );
    print_fixture(
        "badge_external_bttv_label",
        external_badge_provider_for_key("betterttv:helper")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "BTTV Badge",
    );
    print_fixture(
        "badge_external_bttv_pro_label",
        external_badge_provider_for_key("bttv-pro:1")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "BTTV Pro Badge",
    );
    print_fixture(
        "badge_builtin_provider_none",
        bool_text(external_badge_provider_for_key("moderator:1").is_none()),
        "true",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
