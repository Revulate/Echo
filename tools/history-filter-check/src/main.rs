//! Contract: which IRC commands and protocol status rows recent-message history
//! drops, in `src/backend/history_filter.rs`.

#[path = "../../../src/backend/history_filter.rs"]
mod history_filter;

use history_filter::{is_history_state_only_command, is_legacy_protocol_status_row};

fn main() {
    let command_cases = [
        ("PRIVMSG", false),
        ("USERNOTICE", false),
        ("CLEARCHAT", false),
        ("ROOMSTATE", true),
        ("USERSTATE", true),
        ("GLOBALUSERSTATE", true),
        ("JOIN", true),
        ("PART", true),
        ("CAP", true),
        ("PING", true),
        ("PONG", true),
        ("001", true),
        ("372", true),
        ("42", false),
        ("AB1", false),
    ];

    println!("kind,input,filtered");
    for (command, expected) in command_cases {
        let filtered = is_history_state_only_command(command);
        assert_eq!(filtered, expected, "command filter mismatch for {command}");
        println!("command,{command},{filtered}");
    }

    let legacy_cases = [
        (
            "system_roomstate",
            "System",
            "System",
            false,
            false,
            false,
            "Room state updated: emote_only=false",
            true,
        ),
        (
            "system_capability",
            "system",
            "",
            false,
            false,
            false,
            "Capability ACK twitch.tv/tags",
            true,
        ),
        (
            "system_ping",
            "system",
            "system",
            false,
            false,
            false,
            "Received server PING",
            true,
        ),
        (
            "normal_system_message",
            "system",
            "system",
            false,
            false,
            false,
            "Connected to chat.",
            false,
        ),
        (
            "non_system_sender",
            "viewer",
            "viewer",
            false,
            false,
            false,
            "Room state updated",
            false,
        ),
        (
            "system_with_user_id",
            "system",
            "system",
            true,
            false,
            false,
            "Room state updated",
            false,
        ),
        (
            "system_with_badges",
            "system",
            "system",
            false,
            true,
            false,
            "Room state updated",
            false,
        ),
    ];

    for (name, sender, display_name, has_user_id, has_badges, has_emotes, content, expected) in
        legacy_cases
    {
        let filtered = is_legacy_protocol_status_row(
            sender,
            display_name,
            has_user_id,
            has_badges,
            has_emotes,
            content,
        );
        assert_eq!(filtered, expected, "legacy row filter mismatch for {name}");
        println!("legacy,{name},{filtered}");
    }
}
