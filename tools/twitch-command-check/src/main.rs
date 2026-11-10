//! Contract: argument parsing and usage errors for the `/banid`, `/block`,
//! `/delete` and timeout-duration commands in `src/backend/commands/twitch_core.rs`.

#[path = "../../../src/backend/commands/twitch_core.rs"]
mod twitch_core;

use check_support::{FIXTURE_HEADER, assert_fixture};

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

    println!("{FIXTURE_HEADER}");
    print_smoke_rows();
}

fn print_smoke_rows() {
    assert_fixture(
        "twitch_command_banid",
        twitch_core::parse_banid_args("123456").unwrap(),
        "123456",
    );
    assert_fixture(
        "twitch_command_block_strips_at",
        twitch_core::parse_single_username("@someviewer", BLOCK_USAGE).unwrap(),
        "someviewer",
    );
    assert_fixture(
        "twitch_command_delete",
        twitch_core::parse_delete_args("abc-123-def").unwrap(),
        "abc-123-def",
    );

    assert_fixture(
        "twitch_command_banid_empty_rejected",
        twitch_core::parse_banid_args("").unwrap_err(),
        "Usage: /banid <userID>",
    );
    assert_fixture(
        "twitch_command_banid_extra_rejected",
        twitch_core::parse_banid_args("123 extra").unwrap_err(),
        "Usage: /banid <userID>",
    );
    assert_fixture(
        "twitch_command_block_empty_rejected",
        twitch_core::parse_single_username("@", BLOCK_USAGE).unwrap_err(),
        BLOCK_USAGE,
    );
    assert_fixture(
        "twitch_command_block_extra_rejected",
        twitch_core::parse_single_username("someviewer extra", BLOCK_USAGE).unwrap_err(),
        BLOCK_USAGE,
    );
    assert_fixture(
        "twitch_command_delete_empty_rejected",
        twitch_core::parse_delete_args("").unwrap_err(),
        "Usage: /delete <message_id>",
    );
}
