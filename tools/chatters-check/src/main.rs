//! Contract: Helix chatters page parsing, request URL encoding, chatter ordering and
//! status text in `src/backend/chatters_core.rs`.

#[path = "../../../src/backend/chatters_core.rs"]
mod chatters_core;

use chatters_core::{
    build_chatters_url, format_chatters_status, format_observed_chatters_status,
    normalize_chatters, parse_chatters_page,
};
use check_support::{FIXTURE_HEADER, assert_fixture};

fn main() {
    let first_page = sample_chatters_page("cursor two");
    let parsed = parse_chatters_page(&first_page).expect("chatters page should parse");
    assert_eq!(parsed.total, Some(4));
    assert_eq!(parsed.pagination.cursor.as_deref(), Some("cursor two"));
    assert_eq!(parsed.data.len(), 3);
    assert_eq!(parsed.data[0].user_login, "zeta_viewer");
    let parsed_total = parsed.total.unwrap_or_default();
    let parsed_cursor = parsed.pagination.cursor.clone().unwrap_or_default();
    let parsed_first_login = parsed.data[0].user_login.clone();

    let url = build_chatters_url("123 456", "987", Some("cursor two"));
    assert_eq!(
        url,
        "https://api.twitch.tv/helix/chat/chatters?broadcaster_id=123%20456&moderator_id=987&first=1000&after=cursor%20two"
    );
    let url_without_cursor = build_chatters_url("123456", "987654", None);
    assert_eq!(
        url_without_cursor,
        "https://api.twitch.tv/helix/chat/chatters?broadcaster_id=123456&moderator_id=987654&first=1000"
    );

    let normalized = normalize_chatters(parsed.data);
    assert_eq!(
        normalized
            .iter()
            .map(|chatter| chatter.user_login.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "BetaViewer", "zeta_viewer"]
    );

    let full_status = format_chatters_status("alice", 4, Some(4));
    assert_eq!(full_status, "Fetched 4 Twitch chatters in #alice");
    let partial_status = format_chatters_status("#alice", 3, Some(4));
    assert_eq!(partial_status, "Fetched 3 of 4 Twitch chatters in #alice");
    let observed_status = format_observed_chatters_status("alice", 2, "not authenticated");
    assert_eq!(
        observed_status,
        "Twitch chatter list unavailable (not authenticated); observed 2 recent chatters in #alice"
    );

    println!("{FIXTURE_HEADER}");
    print_smoke_rows(ChattersSmokeRows {
        parsed_total,
        parsed_cursor: &parsed_cursor,
        parsed_first_login: &parsed_first_login,
        url: &url,
        url_without_cursor: &url_without_cursor,
        normalized: &normalized,
        full_status: &full_status,
        partial_status: &partial_status,
        observed_status: &observed_status,
    });
}

struct ChattersSmokeRows<'a> {
    parsed_total: usize,
    parsed_cursor: &'a str,
    parsed_first_login: &'a str,
    url: &'a str,
    url_without_cursor: &'a str,
    normalized: &'a [chatters_core::HelixChatter],
    full_status: &'a str,
    partial_status: &'a str,
    observed_status: &'a str,
}

fn print_smoke_rows(rows: ChattersSmokeRows<'_>) {
    let ChattersSmokeRows {
        parsed_total,
        parsed_cursor,
        parsed_first_login,
        url,
        url_without_cursor,
        normalized,
        full_status,
        partial_status,
        observed_status,
    } = rows;
    assert_fixture("chatters_total", parsed_total.to_string(), "4");
    assert_fixture("chatters_cursor", parsed_cursor, "cursor two");
    assert_fixture("chatters_first_login", parsed_first_login, "zeta_viewer");
    assert_fixture(
        "chatters_encoded_url",
        url,
        "https://api.twitch.tv/helix/chat/chatters?broadcaster_id=123%20456&moderator_id=987&first=1000&after=cursor%20two",
    );
    assert_fixture(
        "chatters_url_without_cursor",
        url_without_cursor,
        "https://api.twitch.tv/helix/chat/chatters?broadcaster_id=123456&moderator_id=987654&first=1000",
    );
    let normalized_order = normalized
        .iter()
        .map(|chatter| chatter.user_login.as_str())
        .collect::<Vec<_>>()
        .join("|");
    assert_fixture(
        "chatters_normalized_order",
        &normalized_order,
        "alpha|BetaViewer|zeta_viewer",
    );
    assert_fixture(
        "chatters_normalized_count",
        normalized.len().to_string(),
        "3",
    );
    assert_fixture(
        "chatters_full_status",
        full_status,
        "Fetched 4 Twitch chatters in #alice",
    );
    assert_fixture(
        "chatters_partial_status",
        partial_status,
        "Fetched 3 of 4 Twitch chatters in #alice",
    );
    assert_fixture(
        "chatters_observed_fallback_status",
        observed_status,
        "Twitch chatter list unavailable (not authenticated); observed 2 recent chatters in #alice",
    );
}

fn sample_chatters_page(cursor: &str) -> String {
    format!(
        r#"{{
  "data": [
    {{
      "user_id": "3",
      "user_login": "zeta_viewer",
      "user_name": "Zeta_Viewer"
    }},
    {{
      "user_id": "1",
      "user_login": "alpha",
      "user_name": "Alpha"
    }},
    {{
      "user_id": "2",
      "user_login": "BetaViewer",
      "user_name": "BetaViewer"
    }}
  ],
  "pagination": {{
    "cursor": "{cursor}"
  }},
  "total": 4
}}"#
    )
}
