//! Contract: moderation-button placeholder expansion and the default button set
//! (order, message-context requirements, destructive flags) in
//! `src/backend/mod_buttons.rs`.

#[path = "../../../src/backend/mod_buttons.rs"]
mod mod_buttons;

use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};
use mod_buttons::{ModButtonConfig, default_mod_buttons};

fn main() {
    let configured = ModButtonConfig {
        label: "Audit".to_string(),
        command: "/timeout {user.name} 60 {msg.id} {channel.name} {msg.text} {user} {msg_id}"
            .to_string(),
        icon: None,
        is_destructive: false,
    };

    let resolved_context =
        configured.resolve_for_context("SomeUser", "msg-123", "bad text", "somechannel");
    assert_eq!(
        resolved_context,
        "/timeout SomeUser 60 msg-123 somechannel bad text SomeUser msg-123"
    );
    let resolved_legacy = configured.resolve_command("SomeUser", "msg-123");
    assert_eq!(
        resolved_legacy,
        "/timeout SomeUser 60 msg-123   SomeUser msg-123"
    );

    let defaults = default_mod_buttons();
    assert_eq!(
        defaults.first().map(|button| button.label.as_str()),
        Some("Delete")
    );
    let delete_button = defaults
        .iter()
        .find(|button| button.command == "/delete {msg.id}")
        .expect("delete default button");
    assert!(delete_button.requires_message_context());
    let one_second_timeout = defaults
        .iter()
        .find(|button| button.command == "/timeout {user.name} 1")
        .expect("1s timeout default button");
    assert!(!one_second_timeout.requires_message_context());
    let one_day_timeout = defaults
        .iter()
        .find(|button| button.command == "/timeout {user.name} 86400")
        .expect("1d timeout default button");
    let ban_button = defaults
        .iter()
        .find(|button| button.command == "/ban {user.name}")
        .expect("ban default button");
    assert!(ban_button.is_destructive);

    println!("{FIXTURE_HEADER}");
    print_smoke_rows(
        &resolved_context,
        &resolved_legacy,
        &defaults,
        delete_button,
        one_second_timeout,
        one_day_timeout,
        ban_button,
    );
}

fn print_smoke_rows(
    resolved_context: &str,
    resolved_legacy: &str,
    defaults: &[ModButtonConfig],
    delete_button: &ModButtonConfig,
    one_second_timeout: &ModButtonConfig,
    one_day_timeout: &ModButtonConfig,
    ban_button: &ModButtonConfig,
) {
    assert_fixture(
        "mod_button_context_resolution",
        resolved_context,
        "/timeout SomeUser 60 msg-123 somechannel bad text SomeUser msg-123",
    );
    assert_fixture(
        "mod_button_legacy_resolution",
        resolved_legacy,
        "/timeout SomeUser 60 msg-123   SomeUser msg-123",
    );
    assert_fixture("mod_button_default_count", defaults.len().to_string(), "10");
    assert_fixture(
        "mod_button_default_first",
        defaults
            .first()
            .map(|button| button.label.as_str())
            .unwrap_or("none"),
        "Delete",
    );
    assert_fixture(
        "mod_button_delete_requires_message",
        bool_text(delete_button.requires_message_context()),
        "true",
    );
    assert_fixture(
        "mod_button_timeout_without_message",
        bool_text(!one_second_timeout.requires_message_context()),
        "true",
    );
    assert_fixture(
        "mod_button_timeout_1d_command",
        &one_day_timeout.command,
        "/timeout {user.name} 86400",
    );
    assert_fixture(
        "mod_button_ban_destructive",
        bool_text(ban_button.is_destructive),
        "true",
    );
}
