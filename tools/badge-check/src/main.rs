//! Contract: badge-key normalization, provider cache keys, moderator aliases and
//! embedded fallback URLs in `src/backend/badges_core.rs`.

#[path = "../../../src/backend/badges_core.rs"]
#[allow(unused_imports)] // the real module re-exports more badge-key helpers than this check uses
mod badges_core;

use badges_core::{
    BTTV_PRO_BADGE_KEY, ExternalBadgeProvider, NO_AUDIO_BADGE_KEY, NO_AUDIO_BADGE_URL,
    NO_VIDEO_BADGE_KEY, NO_VIDEO_BADGE_URL, badge_key_matches, badge_key_name,
    bttv_badge_cache_key, bttv_pro_badge_cache_key, embedded_twitch_badge_fallback_url,
    external_badge_provider_for_key, ffz_badge_cache_key, is_lead_moderator_badge_name,
    is_moderator_badge_key, normalize_badge_cache_key,
};
use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};

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

    println!("{FIXTURE_HEADER}");
    print_smoke_rows();
}

fn print_smoke_rows() {
    assert_fixture(
        "badge_name_slash",
        badge_key_name("subscriber/12"),
        "subscriber",
    );
    assert_fixture(
        "badge_name_colon",
        badge_key_name("moderator:1"),
        "moderator",
    );
    assert_fixture(
        "badge_normalize_no_video",
        normalize_badge_cache_key("no_video/1"),
        NO_VIDEO_BADGE_KEY,
    );
    assert_fixture(
        "badge_normalize_no_audio",
        normalize_badge_cache_key("no_audio:1"),
        NO_AUDIO_BADGE_KEY,
    );
    assert_fixture(
        "badge_ffz_bot_key",
        ffz_badge_cache_key("Bot"),
        "ffz-bot:bot",
    );
    assert_fixture(
        "badge_bttv_developer_key",
        bttv_badge_cache_key(1, "NightDev Developer"),
        "bttv:developer",
    );
    assert_fixture(
        "badge_bttv_custom_key",
        bttv_badge_cache_key(99, "Custom Badge"),
        "bttv:custom-badge",
    );
    assert_fixture(
        "badge_bttv_pro_key",
        bttv_pro_badge_cache_key(),
        BTTV_PRO_BADGE_KEY,
    );
    assert_fixture(
        "badge_lead_moderator_alias",
        bool_text(badge_key_matches("lead_moderator:1", "lead-moderator")),
        "true",
    );
    assert_fixture(
        "badge_lead_moderator_is_mod",
        bool_text(is_moderator_badge_key("lead_moderator:1")),
        "true",
    );
    assert_fixture(
        "badge_no_audio_fallback",
        bool_text(embedded_twitch_badge_fallback_url("no_audio/1") == Some(NO_AUDIO_BADGE_URL)),
        "true",
    );
    assert_fixture(
        "badge_no_video_fallback",
        bool_text(embedded_twitch_badge_fallback_url("no_video:1") == Some(NO_VIDEO_BADGE_URL)),
        "true",
    );
    assert_fixture(
        "badge_external_ffz_label",
        external_badge_provider_for_key("ffz:bot")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "FFZ Badge",
    );
    assert_fixture(
        "badge_external_ffz_bot_label",
        external_badge_provider_for_key("frankerfacez-bot:1")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "FFZ Bot Badge",
    );
    assert_fixture(
        "badge_external_bttv_label",
        external_badge_provider_for_key("betterttv:helper")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "BTTV Badge",
    );
    assert_fixture(
        "badge_external_bttv_pro_label",
        external_badge_provider_for_key("bttv-pro:1")
            .map(ExternalBadgeProvider::source_label)
            .unwrap_or("none"),
        "BTTV Pro Badge",
    );
    assert_fixture(
        "badge_builtin_provider_none",
        bool_text(external_badge_provider_for_key("moderator:1").is_none()),
        "true",
    );
}
