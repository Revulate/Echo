//! Paging and formatting for the Helix chatters list.

use serde::Deserialize;
use std::cmp::Ordering;

pub const CHATTERS_PAGE_SIZE: usize = 1000;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HelixChatter {
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct ChattersPagination {
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ChattersPage {
    #[serde(default)]
    pub data: Vec<HelixChatter>,
    #[serde(default)]
    pub pagination: ChattersPagination,
    pub total: Option<usize>,
}

pub fn build_chatters_url(broadcaster_id: &str, moderator_id: &str, after: Option<&str>) -> String {
    let mut url = format!(
        "https://api.twitch.tv/helix/chat/chatters?broadcaster_id={}&moderator_id={}&first={}",
        urlencoding::encode(broadcaster_id),
        urlencoding::encode(moderator_id),
        CHATTERS_PAGE_SIZE
    );

    if let Some(after) = after.filter(|cursor| !cursor.is_empty()) {
        url.push_str("&after=");
        url.push_str(&urlencoding::encode(after));
    }

    url
}

pub fn parse_chatters_page(json: &str) -> Result<ChattersPage, String> {
    serde_json::from_str(json)
        .map_err(|error| format!("Failed to parse chatters response: {error}"))
}

pub fn normalize_chatters(mut chatters: Vec<HelixChatter>) -> Vec<HelixChatter> {
    chatters.retain(|chatter| !chatter.user_login.trim().is_empty());
    chatters.sort_unstable_by(compare_chatters);
    chatters.dedup_by(|a, b| a.user_login.eq_ignore_ascii_case(&b.user_login));
    chatters
}

pub fn format_chatters_status(channel: &str, loaded_count: usize, total: Option<usize>) -> String {
    let channel = channel.trim_start_matches('#');
    match total {
        Some(total) if total != loaded_count => {
            format!("Fetched {loaded_count} of {total} Twitch chatters in #{channel}")
        }
        Some(total) => format!("Fetched {total} Twitch chatters in #{channel}"),
        None => format!("Fetched {loaded_count} Twitch chatters in #{channel}"),
    }
}

pub fn format_observed_chatters_status(channel: &str, count: usize, reason: &str) -> String {
    let channel = channel.trim_start_matches('#');
    format!(
        "Twitch chatter list unavailable ({reason}); observed {count} recent chatters in #{channel}"
    )
}

fn compare_chatters(a: &HelixChatter, b: &HelixChatter) -> Ordering {
    cmp_ascii_case_insensitive(&a.user_login, &b.user_login)
        .then_with(|| a.user_login.cmp(&b.user_login))
        .then_with(|| a.user_id.cmp(&b.user_id))
}

fn cmp_ascii_case_insensitive(a: &str, b: &str) -> Ordering {
    let mut a_bytes = a.bytes();
    let mut b_bytes = b.bytes();

    loop {
        match (a_bytes.next(), b_bytes.next()) {
            (Some(a), Some(b)) => {
                let cmp = a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}
