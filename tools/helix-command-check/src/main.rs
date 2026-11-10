//! Contract: Helix request URLs and JSON bodies for the announcement, marker, title,
//! game and category-search commands in `src/backend/commands/helix_core.rs`,
//! including their input validation.

#[path = "../../../src/backend/commands/helix_core.rs"]
mod helix_core;

use check_support::{FIXTURE_HEADER, assert_fixture};

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

    println!("{FIXTURE_HEADER}");
    print_smoke_rows();
}

fn print_smoke_rows() {
    assert_fixture(
        "helix_announce_primary_color",
        helix_core::announcement_color("announce"),
        "primary",
    );
    assert_fixture(
        "helix_announce_blue_color",
        helix_core::announcement_color("announceblue"),
        "blue",
    );
    assert_fixture(
        "helix_announce_green_color",
        helix_core::announcement_color("announcegreen"),
        "green",
    );
    assert_fixture(
        "helix_announce_orange_color",
        helix_core::announcement_color("announceorange"),
        "orange",
    );
    assert_fixture(
        "helix_announce_purple_color",
        helix_core::announcement_color("announcepurple"),
        "purple",
    );
    assert_fixture(
        "helix_announcement_url",
        helix_core::announcement_url("broadcaster", "moderator"),
        "https://api.twitch.tv/helix/chat/announcements?broadcaster_id=broadcaster&moderator_id=moderator",
    );

    assert_fixture(
        "helix_marker_body_with_description",
        helix_core::marker_body("123", "major play")
            .unwrap()
            .to_string(),
        r#"{"description":"major play","user_id":"123"}"#,
    );
    assert_fixture(
        "helix_marker_body_without_description",
        helix_core::marker_body("123", "").unwrap().to_string(),
        r#"{"user_id":"123"}"#,
    );
    assert_fixture(
        "helix_marker_body_too_long_rejected",
        helix_core::marker_body("123", &"x".repeat(141)).unwrap_err(),
        "Usage: /marker [description up to 140 chars]",
    );

    assert_fixture(
        "helix_channel_info_url",
        helix_core::channel_info_url("123"),
        "https://api.twitch.tv/helix/channels?broadcaster_id=123",
    );
    assert_fixture(
        "helix_channel_title_body",
        helix_core::channel_title_body("  new title  ")
            .unwrap()
            .to_string(),
        r#"{"title":"new title"}"#,
    );
    assert_fixture(
        "helix_channel_title_empty_rejected",
        helix_core::channel_title_body("   ").unwrap_err(),
        "Usage: /settitle <title>",
    );
    assert_fixture(
        "helix_channel_game_body",
        helix_core::channel_game_body("509658").unwrap().to_string(),
        r#"{"game_id":"509658"}"#,
    );
    assert_fixture(
        "helix_channel_game_empty_rejected",
        helix_core::channel_game_body("").unwrap_err(),
        "Usage: /setgame <game>",
    );
    assert_fixture(
        "helix_category_search_url",
        helix_core::category_search_url("Just Chatting").unwrap(),
        "https://api.twitch.tv/helix/search/categories?query=Just%20Chatting&first=1",
    );
    assert_fixture(
        "helix_category_search_empty_rejected",
        helix_core::category_search_url("").unwrap_err(),
        "Usage: /setgame <game>",
    );
}
