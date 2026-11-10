#[path = "../../../src/backend/commands/helix_core.rs"]
mod helix_core;

use std::hint::black_box;
use std::time::Instant;

fn main() {
    assert_eq!(helix_core::announcement_color("announce"), "primary");
    assert_eq!(helix_core::announcement_color("announceblue"), "blue");
    assert_eq!(helix_core::announcement_color("announcegreen"), "green");
    assert_eq!(helix_core::announcement_color("announceorange"), "orange");
    assert_eq!(helix_core::announcement_color("announcepurple"), "purple");
    assert_eq!(
        helix_core::announcement_url("broadcaster", "moderator"),
        "https://api.twitch.tv/helix/chat/announcements?broadcaster_id=broadcaster&moderator_id=moderator"
    );

    let marker = helix_core::marker_body("123", "major play").unwrap();
    assert_eq!(marker["user_id"], "123");
    assert_eq!(marker["description"], "major play");
    let marker_without_description = helix_core::marker_body("123", "").unwrap();
    assert!(marker_without_description.get("description").is_none());
    assert!(helix_core::marker_body("123", &"x".repeat(141)).is_err());

    assert_eq!(
        helix_core::channel_info_url("123"),
        "https://api.twitch.tv/helix/channels?broadcaster_id=123"
    );
    assert_eq!(
        helix_core::channel_title_body("  new title  ").unwrap()["title"],
        "new title"
    );
    assert!(helix_core::channel_title_body("   ").is_err());
    assert_eq!(
        helix_core::channel_game_body("509658").unwrap()["game_id"],
        "509658"
    );
    assert!(helix_core::channel_game_body("").is_err());
    assert_eq!(
        helix_core::category_search_url("Just Chatting").unwrap(),
        "https://api.twitch.tv/helix/search/categories?query=Just%20Chatting&first=1"
    );

    let iterations = 400_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let color = helix_core::announcement_color(black_box("announcepurple"));
        let announce = helix_core::announcement_url(black_box("123"), black_box("456"));
        let marker = helix_core::marker_body(black_box("123"), black_box("major play")).unwrap();
        let title = helix_core::channel_title_body(black_box("new title")).unwrap();
        let game = helix_core::channel_game_body(black_box("509658")).unwrap();
        let search = helix_core::category_search_url(black_box("Just Chatting")).unwrap();
        checksum = checksum
            .wrapping_add(color.len())
            .wrapping_add(announce.len())
            .wrapping_add(marker.to_string().len())
            .wrapping_add(title.to_string().len())
            .wrapping_add(game.to_string().len())
            .wrapping_add(search.len());
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / iterations as f64;
    let units_per_second = iterations as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows();
    println!(
        "build,helix_command_requests,{},{:.2},{:.0},{}",
        iterations, ns_per_unit, units_per_second, checksum
    );
}

fn print_smoke_rows() {
    print_fixture(
        "helix_announce_primary_color",
        helix_core::announcement_color("announce"),
        "primary",
    );
    print_fixture(
        "helix_announce_blue_color",
        helix_core::announcement_color("announceblue"),
        "blue",
    );
    print_fixture(
        "helix_announce_green_color",
        helix_core::announcement_color("announcegreen"),
        "green",
    );
    print_fixture(
        "helix_announce_orange_color",
        helix_core::announcement_color("announceorange"),
        "orange",
    );
    print_fixture(
        "helix_announce_purple_color",
        helix_core::announcement_color("announcepurple"),
        "purple",
    );
    print_fixture(
        "helix_announcement_url",
        &helix_core::announcement_url("broadcaster", "moderator"),
        "https://api.twitch.tv/helix/chat/announcements?broadcaster_id=broadcaster&moderator_id=moderator",
    );

    print_fixture(
        "helix_marker_body_with_description",
        &helix_core::marker_body("123", "major play")
            .unwrap()
            .to_string(),
        r#"{"description":"major play","user_id":"123"}"#,
    );
    print_fixture(
        "helix_marker_body_without_description",
        &helix_core::marker_body("123", "").unwrap().to_string(),
        r#"{"user_id":"123"}"#,
    );
    print_fixture(
        "helix_marker_body_too_long_rejected",
        &helix_core::marker_body("123", &"x".repeat(141)).unwrap_err(),
        "Usage: /marker [description up to 140 chars]",
    );

    print_fixture(
        "helix_channel_info_url",
        &helix_core::channel_info_url("123"),
        "https://api.twitch.tv/helix/channels?broadcaster_id=123",
    );
    print_fixture(
        "helix_channel_title_body",
        &helix_core::channel_title_body("  new title  ")
            .unwrap()
            .to_string(),
        r#"{"title":"new title"}"#,
    );
    print_fixture(
        "helix_channel_title_empty_rejected",
        &helix_core::channel_title_body("   ").unwrap_err(),
        "Usage: /settitle <title>",
    );
    print_fixture(
        "helix_channel_game_body",
        &helix_core::channel_game_body("509658").unwrap().to_string(),
        r#"{"game_id":"509658"}"#,
    );
    print_fixture(
        "helix_channel_game_empty_rejected",
        &helix_core::channel_game_body("").unwrap_err(),
        "Usage: /setgame <game>",
    );
    print_fixture(
        "helix_category_search_url",
        &helix_core::category_search_url("Just Chatting").unwrap(),
        "https://api.twitch.tv/helix/search/categories?query=Just%20Chatting&first=1",
    );
    print_fixture(
        "helix_category_search_empty_rejected",
        &helix_core::category_search_url("").unwrap_err(),
        "Usage: /setgame <game>",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}
