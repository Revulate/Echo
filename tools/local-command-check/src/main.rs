//! Contract: channel/user argument parsing and the popout, reward-queue, low-trust,
//! viewer-card and streamlink URLs built in `src/backend/commands/local_core.rs`.

#[path = "../../../src/backend/commands/local_core.rs"]
#[allow(dead_code)] // the log-file relaunch constants are used by the command dispatcher
mod local_core;

use check_support::{FIXTURE_HEADER, assert_fixture};

fn main() {
    assert_eq!(
        local_core::current_or_arg_channel("SomeChannel", ""),
        "somechannel"
    );
    assert_eq!(
        local_core::current_or_arg_channel("current", "#OtherChannel"),
        "otherchannel"
    );

    let target =
        local_core::split_user_and_channel("CurrentChannel", "@SomeUser #OtherChannel", "usage")
            .unwrap();
    assert_eq!(target.user, "SomeUser");
    assert_eq!(target.channel, "otherchannel");
    assert!(local_core::split_user_and_channel("CurrentChannel", "", "usage").is_err());

    assert_eq!(
        local_core::twitch_popout_chat_url("#SomeChannel"),
        "https://www.twitch.tv/popout/somechannel/chat"
    );
    assert_eq!(
        local_core::twitch_reward_queue_url("#SomeChannel"),
        "https://www.twitch.tv/popout/somechannel/reward-queue"
    );
    assert_eq!(
        local_core::twitch_lowtrust_users_url("#SomeChannel"),
        "https://www.twitch.tv/popout/moderator/somechannel/low-trust-users"
    );
    assert_eq!(
        local_core::twitch_viewer_card_url("#SomeChannel", "@SomeUser"),
        "https://www.twitch.tv/popout/somechannel/viewercard/SomeUser"
    );
    assert_eq!(
        local_core::streamlink_target_url("#SomeChannel"),
        "https://twitch.tv/somechannel"
    );
    assert_eq!(
        local_core::format_debug_env_lines(
            "https://proxy.example/proxy",
            Some("socks5h://127.0.0.1:9050")
        ),
        vec![
            "recentMessagesApiUrl: https://logs.zonian.dev/api/v2/recent-messages",
            "linkResolverUrl: https://proxy.example/proxy",
            "proxyUrl: socks5h://127.0.0.1:9050",
            "twitchServerHost: irc.chat.twitch.tv",
            "twitchServerPort: 6697",
            "twitchServerSecure: 1",
        ]
    );
    assert_eq!(
        local_core::format_debug_env_lines("https://proxy.example/proxy", None)[2],
        "proxyUrl: N/A"
    );
    assert_eq!(
        local_core::logfile_path_arg(r#""C:\Users\example\Echo Logs\echo.log""#, "usage").unwrap(),
        r#"C:\Users\example\Echo Logs\echo.log"#
    );
    assert_eq!(
        local_core::logfile_path_arg("/tmp/echo debug.log", "usage").unwrap(),
        "/tmp/echo debug.log"
    );
    assert_eq!(
        local_core::logfile_path_arg("   ", "usage").unwrap_err(),
        "usage"
    );
    assert_eq!(local_core::ECHO_LOG_FILE_ENV, "ECHO_LOG_FILE");
    assert_eq!(local_core::ECHO_LOG_FILE_PATH_ENV, "ECHO_LOG_FILE_PATH");

    assert_eq!(
        local_core::split_command_args(r#"--player "C:\Program Files\mpv\mpv.exe" --retry"#)
            .unwrap(),
        vec![
            "--player".to_string(),
            r#"C:\Program Files\mpv\mpv.exe"#.to_string(),
            "--retry".to_string()
        ]
    );
    assert!(local_core::split_command_args(r#"--player "C:\mpv.exe"#).is_err());
    assert_eq!(
        local_core::format_uptime_status("#SomeChannel", true, 1_000, 8_445),
        "#somechannel has been live for 2h 4m 5s"
    );
    assert_eq!(
        local_core::format_uptime_status("#SomeChannel", false, 1_000, 8_445),
        "#somechannel is not live"
    );

    println!("{FIXTURE_HEADER}");
    print_smoke_rows();
}

fn print_smoke_rows() {
    assert_fixture(
        "local_command_channel_default",
        local_core::current_or_arg_channel("SomeChannel", ""),
        "somechannel",
    );
    assert_fixture(
        "local_command_channel_arg",
        local_core::current_or_arg_channel("current", "#OtherChannel"),
        "otherchannel",
    );

    let target =
        local_core::split_user_and_channel("CurrentChannel", "@SomeUser #OtherChannel", "usage")
            .unwrap();
    assert_fixture("local_command_target_user", &target.user, "SomeUser");
    assert_fixture(
        "local_command_target_channel",
        &target.channel,
        "otherchannel",
    );

    assert_fixture(
        "local_command_popout_url",
        local_core::twitch_popout_chat_url("#SomeChannel"),
        "https://www.twitch.tv/popout/somechannel/chat",
    );
    assert_fixture(
        "local_command_requests_url",
        local_core::twitch_reward_queue_url("#SomeChannel"),
        "https://www.twitch.tv/popout/somechannel/reward-queue",
    );
    assert_fixture(
        "local_command_lowtrust_url",
        local_core::twitch_lowtrust_users_url("#SomeChannel"),
        "https://www.twitch.tv/popout/moderator/somechannel/low-trust-users",
    );
    assert_fixture(
        "local_command_usercard_url",
        local_core::twitch_viewer_card_url("#SomeChannel", "@SomeUser"),
        "https://www.twitch.tv/popout/somechannel/viewercard/SomeUser",
    );
    assert_fixture(
        "local_command_streamlink_url",
        local_core::streamlink_target_url("#SomeChannel"),
        "https://twitch.tv/somechannel",
    );

    assert_fixture(
        "local_command_debug_env_with_proxy",
        local_core::format_debug_env_lines(
            "https://proxy.example/proxy",
            Some("socks5h://127.0.0.1:9050"),
        )
        .join("|"),
        "recentMessagesApiUrl: https://logs.zonian.dev/api/v2/recent-messages|linkResolverUrl: https://proxy.example/proxy|proxyUrl: socks5h://127.0.0.1:9050|twitchServerHost: irc.chat.twitch.tv|twitchServerPort: 6697|twitchServerSecure: 1",
    );
    assert_fixture(
        "local_command_debug_env_without_proxy",
        &local_core::format_debug_env_lines("https://proxy.example/proxy", None)[2],
        "proxyUrl: N/A",
    );

    assert_fixture(
        "local_command_logfile_path_quoted",
        local_core::logfile_path_arg(r#""C:\Users\example\Echo Logs\echo.log""#, "usage").unwrap(),
        r#"C:\Users\example\Echo Logs\echo.log"#,
    );
    assert_fixture(
        "local_command_logfile_path_spaces",
        local_core::logfile_path_arg("/tmp/echo debug.log", "usage").unwrap(),
        "/tmp/echo debug.log",
    );
    assert_fixture(
        "local_command_logfile_path_empty_rejected",
        local_core::logfile_path_arg("   ", "usage").unwrap_err(),
        "usage",
    );

    assert_fixture(
        "local_command_quoted_arg_split",
        local_core::split_command_args(r#"--player "C:\Program Files\mpv\mpv.exe" --retry"#)
            .unwrap()
            .join("|"),
        r#"--player|C:\Program Files\mpv\mpv.exe|--retry"#,
    );
    assert_fixture(
        "local_command_unterminated_quote_rejected",
        local_core::split_command_args(r#"--player "C:\mpv.exe"#).unwrap_err(),
        "unterminated quote",
    );

    assert_fixture(
        "local_command_uptime_live",
        local_core::format_uptime_status("#SomeChannel", true, 1_000, 8_445),
        "#somechannel has been live for 2h 4m 5s",
    );
    assert_fixture(
        "local_command_uptime_offline",
        local_core::format_uptime_status("#SomeChannel", false, 1_000, 8_445),
        "#somechannel is not live",
    );
}
