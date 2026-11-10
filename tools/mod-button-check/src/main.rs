#[path = "../../../src/backend/mod_buttons.rs"]
mod mod_buttons;

use mod_buttons::{ModButtonConfig, default_mod_buttons};
use std::hint::black_box;
use std::time::Instant;

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

    let iterations = 1_000_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for index in 0..iterations {
        let button = black_box(&defaults[index % defaults.len()]);
        let command = button.resolve_for_context(
            black_box("SomeUser"),
            black_box("msg-123"),
            black_box("bad text"),
            black_box("somechannel"),
        );
        checksum = checksum.wrapping_add(command.len());
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / iterations as f64;
    let units_per_second = iterations as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows(
        &resolved_context,
        &resolved_legacy,
        &defaults,
        delete_button,
        one_second_timeout,
        one_day_timeout,
        ban_button,
    );
    println!(
        "resolve,mod_button_placeholders,{},{:.2},{:.0},{}",
        iterations, ns_per_unit, units_per_second, checksum
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
    print_fixture(
        "mod_button_context_resolution",
        resolved_context,
        "/timeout SomeUser 60 msg-123 somechannel bad text SomeUser msg-123",
    );
    print_fixture(
        "mod_button_legacy_resolution",
        resolved_legacy,
        "/timeout SomeUser 60 msg-123   SomeUser msg-123",
    );
    print_fixture(
        "mod_button_default_count",
        &defaults.len().to_string(),
        "10",
    );
    print_fixture(
        "mod_button_default_first",
        defaults
            .first()
            .map(|button| button.label.as_str())
            .unwrap_or("none"),
        "Delete",
    );
    print_fixture(
        "mod_button_delete_requires_message",
        bool_text(delete_button.requires_message_context()),
        "true",
    );
    print_fixture(
        "mod_button_timeout_without_message",
        bool_text(!one_second_timeout.requires_message_context()),
        "true",
    );
    print_fixture(
        "mod_button_timeout_1d_command",
        &one_day_timeout.command,
        "/timeout {user.name} 86400",
    );
    print_fixture(
        "mod_button_ban_destructive",
        bool_text(ban_button.is_destructive),
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
