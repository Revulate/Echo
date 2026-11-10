#[allow(dead_code)]
#[path = "../../../src/backend/commands/local_core.rs"]
mod local_core;

use std::hint::black_box;
use std::time::Instant;

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

    let iterations = 500_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let channel = local_core::current_or_arg_channel(black_box("SomeChannel"), black_box(""));
        let target = local_core::split_user_and_channel(
            black_box(&channel),
            black_box("@SomeUser #OtherChannel"),
            "usage",
        )
        .unwrap();
        let popout = local_core::twitch_popout_chat_url(black_box(&target.channel));
        let user =
            local_core::twitch_viewer_card_url(black_box(&target.channel), black_box(&target.user));
        let streamlink = local_core::streamlink_target_url(black_box(&target.channel));
        checksum = checksum
            .wrapping_add(popout.len())
            .wrapping_add(user.len())
            .wrapping_add(streamlink.len());
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / iterations as f64;
    let units_per_second = iterations as f64 / elapsed.as_secs_f64();
    let url_checksum = local_core::twitch_popout_chat_url("#SomeChannel").len()
        + local_core::twitch_reward_queue_url("#SomeChannel").len()
        + local_core::twitch_lowtrust_users_url("#SomeChannel").len()
        + local_core::twitch_viewer_card_url("#SomeChannel", "@SomeUser").len()
        + local_core::streamlink_target_url("#SomeChannel").len();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows();
    println!(
        "format,local_command_targets,{},{:.2},{:.0},{}",
        iterations, ns_per_unit, units_per_second, checksum
    );
    println!("format,local_command_popout_urls,5,0.00,0,{}", url_checksum);
}

fn print_smoke_rows() {
    print_fixture(
        "local_command_channel_default",
        &local_core::current_or_arg_channel("SomeChannel", ""),
        "somechannel",
    );
    print_fixture(
        "local_command_channel_arg",
        &local_core::current_or_arg_channel("current", "#OtherChannel"),
        "otherchannel",
    );

    let target =
        local_core::split_user_and_channel("CurrentChannel", "@SomeUser #OtherChannel", "usage")
            .unwrap();
    print_fixture("local_command_target_user", &target.user, "SomeUser");
    print_fixture(
        "local_command_target_channel",
        &target.channel,
        "otherchannel",
    );

    print_fixture(
        "local_command_popout_url",
        &local_core::twitch_popout_chat_url("#SomeChannel"),
        "https://www.twitch.tv/popout/somechannel/chat",
    );
    print_fixture(
        "local_command_requests_url",
        &local_core::twitch_reward_queue_url("#SomeChannel"),
        "https://www.twitch.tv/popout/somechannel/reward-queue",
    );
    print_fixture(
        "local_command_lowtrust_url",
        &local_core::twitch_lowtrust_users_url("#SomeChannel"),
        "https://www.twitch.tv/popout/moderator/somechannel/low-trust-users",
    );
    print_fixture(
        "local_command_usercard_url",
        &local_core::twitch_viewer_card_url("#SomeChannel", "@SomeUser"),
        "https://www.twitch.tv/popout/somechannel/viewercard/SomeUser",
    );
    print_fixture(
        "local_command_streamlink_url",
        &local_core::streamlink_target_url("#SomeChannel"),
        "https://twitch.tv/somechannel",
    );

    print_fixture(
        "local_command_debug_env_with_proxy",
        &local_core::format_debug_env_lines(
            "https://proxy.example/proxy",
            Some("socks5h://127.0.0.1:9050"),
        )
        .join("|"),
        "recentMessagesApiUrl: https://logs.zonian.dev/api/v2/recent-messages|linkResolverUrl: https://proxy.example/proxy|proxyUrl: socks5h://127.0.0.1:9050|twitchServerHost: irc.chat.twitch.tv|twitchServerPort: 6697|twitchServerSecure: 1",
    );
    print_fixture(
        "local_command_debug_env_without_proxy",
        &local_core::format_debug_env_lines("https://proxy.example/proxy", None)[2],
        "proxyUrl: N/A",
    );

    print_fixture(
        "local_command_logfile_path_quoted",
        &local_core::logfile_path_arg(r#""C:\Users\example\Echo Logs\echo.log""#, "usage").unwrap(),
        r#"C:\Users\example\Echo Logs\echo.log"#,
    );
    print_fixture(
        "local_command_logfile_path_spaces",
        &local_core::logfile_path_arg("/tmp/echo debug.log", "usage").unwrap(),
        "/tmp/echo debug.log",
    );
    print_fixture(
        "local_command_logfile_path_empty_rejected",
        local_core::logfile_path_arg("   ", "usage").unwrap_err(),
        "usage",
    );

    print_fixture(
        "local_command_quoted_arg_split",
        &local_core::split_command_args(r#"--player "C:\Program Files\mpv\mpv.exe" --retry"#)
            .unwrap()
            .join("|"),
        r#"--player|C:\Program Files\mpv\mpv.exe|--retry"#,
    );
    print_fixture(
        "local_command_unterminated_quote_rejected",
        &local_core::split_command_args(r#"--player "C:\mpv.exe"#).unwrap_err(),
        "unterminated quote",
    );

    print_fixture(
        "local_command_uptime_live",
        &local_core::format_uptime_status("#SomeChannel", true, 1_000, 8_445),
        "#somechannel has been live for 2h 4m 5s",
    );
    print_fixture(
        "local_command_uptime_offline",
        &local_core::format_uptime_status("#SomeChannel", false, 1_000, 8_445),
        "#somechannel is not live",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}
