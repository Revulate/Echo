//! Pure filtering rules for recent-message and local-history ingestion.

pub fn is_numeric_irc_reply(command: &str) -> bool {
    command.len() == 3 && command.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn is_history_state_only_command(command: &str) -> bool {
    matches!(
        command,
        "ROOMSTATE" | "USERSTATE" | "GLOBALUSERSTATE" | "JOIN" | "PART" | "CAP" | "PING" | "PONG"
    ) || is_numeric_irc_reply(command)
}

pub fn is_legacy_protocol_status_row(
    sender: &str,
    display_name: &str,
    has_user_id: bool,
    has_badges: bool,
    has_twitch_emotes_tag: bool,
    content: &str,
) -> bool {
    let sender_is_system =
        sender.eq_ignore_ascii_case("system") || display_name.eq_ignore_ascii_case("system");
    if !sender_is_system || has_user_id || has_badges || has_twitch_emotes_tag {
        return false;
    }

    let content = content.trim();
    [
        "Room state updated",
        "User state updated",
        "Global user state updated",
        "Capability ",
        "IRC ",
        "Received server PING",
        "Received server PONG",
        "Twitch requested a reconnect",
    ]
    .iter()
    .any(|prefix| content.starts_with(prefix))
}
