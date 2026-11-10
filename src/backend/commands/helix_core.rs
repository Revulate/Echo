//! Pure helpers for Twitch Helix command request construction.

use serde_json::{Value, json};

const MARKER_DESCRIPTION_MAX_CHARS: usize = 140;

pub fn announcement_color(command: &str) -> &'static str {
    match command {
        "announceblue" => "blue",
        "announcegreen" => "green",
        "announceorange" => "orange",
        "announcepurple" => "purple",
        _ => "primary",
    }
}

pub fn announcement_url(broadcaster_id: &str, moderator_id: &str) -> String {
    format!(
        "https://api.twitch.tv/helix/chat/announcements?broadcaster_id={broadcaster_id}&moderator_id={moderator_id}"
    )
}

pub fn marker_body(user_id: &str, description: &str) -> Result<Value, String> {
    let description = description.trim();
    if description.chars().count() > MARKER_DESCRIPTION_MAX_CHARS {
        return Err("Usage: /marker [description up to 140 chars]".to_string());
    }

    let mut body = json!({ "user_id": user_id });
    if !description.is_empty() {
        body["description"] = Value::String(description.to_string());
    }
    Ok(body)
}

pub fn channel_info_url(broadcaster_id: &str) -> String {
    format!("https://api.twitch.tv/helix/channels?broadcaster_id={broadcaster_id}")
}

pub fn channel_title_body(title: &str) -> Result<Value, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Usage: /settitle <title>".to_string());
    }
    Ok(json!({ "title": title }))
}

pub fn channel_game_body(game_id: &str) -> Result<Value, String> {
    let game_id = game_id.trim();
    if game_id.is_empty() {
        return Err("Usage: /setgame <game>".to_string());
    }
    Ok(json!({ "game_id": game_id }))
}

pub fn category_search_url(query: &str) -> Result<String, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Usage: /setgame <game>".to_string());
    }

    Ok(format!(
        "https://api.twitch.tv/helix/search/categories?query={}&first=1",
        urlencoding::encode(query)
    ))
}
