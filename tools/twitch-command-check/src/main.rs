#[path = "../../../src/backend/commands/twitch_core.rs"]
mod twitch_core;

use std::hint::black_box;
use std::time::Instant;

const BLOCK_USAGE: &str = "Usage: /block <user>";

fn main() {
    let valid_cases = [
        (
            "banid",
            twitch_core::parse_banid_args("123456").unwrap(),
            "123456",
        ),
        (
            "block",
            twitch_core::parse_single_username("@someviewer", BLOCK_USAGE).unwrap(),
            "someviewer",
        ),
        (
            "delete",
            twitch_core::parse_delete_args("abc-123-def").unwrap(),
            "abc-123-def",
        ),
    ];

    for (name, actual, expected) in valid_cases {
        assert_eq!(actual, expected, "{name} arguments parsed incorrectly");
    }

    assert!(twitch_core::parse_banid_args("").is_err());
    assert!(twitch_core::parse_banid_args("123 extra").is_err());
    assert!(twitch_core::parse_single_username("@", BLOCK_USAGE).is_err());
    assert!(twitch_core::parse_single_username("someviewer extra", BLOCK_USAGE).is_err());
    assert!(twitch_core::parse_single_username("", BLOCK_USAGE).is_err());
    assert!(twitch_core::parse_delete_args("").is_err());
    assert!(twitch_core::parse_delete_args("id extra").is_err());
    assert_eq!(
        twitch_core::parse_timeout_duration_token("1s").unwrap(),
        Some(1)
    );
    assert_eq!(
        twitch_core::parse_timeout_duration_token("2m").unwrap(),
        Some(120)
    );
    assert_eq!(
        twitch_core::parse_timeout_duration_token("1h").unwrap(),
        Some(3600)
    );
    assert_eq!(
        twitch_core::parse_timeout_duration_token("1d").unwrap(),
        Some(86400)
    );
    assert_eq!(
        twitch_core::parse_timeout_duration_token("spam").unwrap(),
        None
    );
    assert!(twitch_core::parse_timeout_duration_token("1x").is_err());
    for invalid in [
        "-1",
        "+1",
        ".5s",
        "1.5s",
        "0",
        "15d",
        "18446744073709551615d",
    ] {
        assert!(
            twitch_core::parse_timeout_duration_token(invalid).is_err(),
            "{invalid}"
        );
    }

    let iterations = 1_000_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for index in 0..iterations {
        let ban = twitch_core::parse_banid_args(black_box("123456")).unwrap();
        let block =
            twitch_core::parse_single_username(black_box("@someviewer"), BLOCK_USAGE).unwrap();
        let delete = twitch_core::parse_delete_args(black_box("abc-123-def")).unwrap();
        checksum = checksum
            .wrapping_add(ban.len())
            .wrapping_add(block.len())
            .wrapping_add(delete.len())
            .wrapping_add(index & 1);
    }
    let elapsed = start.elapsed();
    let units = iterations * 3;
    let ns_per_unit = elapsed.as_nanos() as f64 / units as f64;
    let units_per_second = units as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows();
    println!(
        "format,twitch_moderation_commands,{},{:.2},{:.0},{}",
        units, ns_per_unit, units_per_second, checksum
    );
}

fn print_smoke_rows() {
    print_ok(
        "twitch_command_banid",
        twitch_core::parse_banid_args("123456").unwrap(),
        "123456",
    );
    print_ok(
        "twitch_command_block_strips_at",
        twitch_core::parse_single_username("@someviewer", BLOCK_USAGE).unwrap(),
        "someviewer",
    );
    print_ok(
        "twitch_command_delete",
        twitch_core::parse_delete_args("abc-123-def").unwrap(),
        "abc-123-def",
    );

    print_err(
        "twitch_command_banid_empty_rejected",
        twitch_core::parse_banid_args("").unwrap_err(),
        "Usage: /banid <userID>",
    );
    print_err(
        "twitch_command_banid_extra_rejected",
        twitch_core::parse_banid_args("123 extra").unwrap_err(),
        "Usage: /banid <userID>",
    );
    print_err(
        "twitch_command_block_empty_rejected",
        twitch_core::parse_single_username("@", BLOCK_USAGE).unwrap_err(),
        BLOCK_USAGE,
    );
    print_err(
        "twitch_command_block_extra_rejected",
        twitch_core::parse_single_username("someviewer extra", BLOCK_USAGE).unwrap_err(),
        BLOCK_USAGE,
    );
    print_err(
        "twitch_command_delete_empty_rejected",
        twitch_core::parse_delete_args("").unwrap_err(),
        "Usage: /delete <message_id>",
    );
}

fn print_ok(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn print_err(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "error fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}
