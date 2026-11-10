//! Local commands that do not require Twitch Helix calls.

use super::CommandResult;
use super::local_core::{
    ECHO_LOG_FILE_ENV, ECHO_LOG_FILE_PATH_ENV, ECHO_LOGFILE_RELAUNCH_ENV,
    ECHO_LOGFILE_RELAUNCH_FORCED_RUST_LOG, current_or_arg_channel, format_debug_env_lines,
    format_uptime_status, logfile_path_arg, split_command_args, split_user_and_channel,
    streamlink_target_url, twitch_popout_chat_url, twitch_viewer_card_url,
};
use crate::backend::chatters_core::format_observed_chatters_status;
use crate::backend::constants::API_PROXY;
use crate::backend::lock_utils::MutexExt;
use crate::backend::settings::{self, StreamlinkQuality};
use crate::backend::ui_actions::ChatterListEntry;
use chrono::Utc;
use std::process::Command;

struct ResolvedUserTarget {
    user: String,
    display_name: String,
    user_id: Option<String>,
    channel: String,
}

pub async fn cmd_clearmessages(channel: &str) -> CommandResult {
    crate::backend::irc::clear_channel_messages(channel);
    CommandResult::Success(None)
}

pub async fn cmd_fakemsg(channel: &str, args: &str) -> CommandResult {
    let raw = args.trim();
    if raw.is_empty() {
        return CommandResult::Error("Usage: /fakemsg <raw message>".to_string());
    }

    let Some(message) =
        crate::backend::history::parse_raw_irc_message_for_channel(raw, channel).await
    else {
        return CommandResult::Error("Could not parse raw IRC message for /fakemsg".to_string());
    };

    crate::backend::irc::insert_local_message(channel, message);
    CommandResult::Success(None)
}

pub async fn cmd_openurl(args: &str) -> CommandResult {
    let mut mode = crate::backend::ui_actions::LinkOpenMode::Configured;
    let mut url: Option<String> = None;

    for arg in match split_command_args(args) {
        Ok(args) => args,
        Err(error) => return CommandResult::Error(format!("Invalid /openurl arguments: {error}")),
    } {
        match arg.as_str() {
            "--incognito" => mode = crate::backend::ui_actions::LinkOpenMode::Incognito,
            "--no-incognito" => mode = crate::backend::ui_actions::LinkOpenMode::Normal,
            _ if url.is_none() => url = Some(arg),
            _ => {
                return CommandResult::Error(
                    "Usage: /openurl [--incognito|--no-incognito] <url>".to_string(),
                );
            }
        }
    }

    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return CommandResult::Error(
            "Usage: /openurl [--incognito|--no-incognito] <url>".to_string(),
        );
    };

    if let Err(reason) = crate::utils::validate_external_url(&url) {
        return CommandResult::Error(reason);
    }

    crate::backend::ui_actions::push_action(
        crate::backend::ui_actions::UiAction::ConfirmOpenLink {
            url: url.clone(),
            mode,
        },
    );

    CommandResult::Success(Some(format!("Queued external link open: {url}")))
}

pub async fn cmd_follow_removed() -> CommandResult {
    CommandResult::Success(Some(
        "Twitch has removed the ability to follow users through third-party applications."
            .to_string(),
    ))
}

pub async fn cmd_unfollow_removed() -> CommandResult {
    CommandResult::Success(Some(
        "Twitch has removed the ability to unfollow users through third-party applications."
            .to_string(),
    ))
}

/// Run the OS URL handler on the blocking pool: the `open` crate waits for its
/// helper process, which would otherwise hold an async worker.
async fn open_off_thread(url: String) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || open::that(url))
        .await
        .map_err(std::io::Error::other)?
}

pub async fn cmd_popout(channel: &str, args: &str) -> CommandResult {
    let target_channel = current_or_arg_channel(channel, args);
    if !crate::utils::channel_input::is_valid_twitch_login(&target_channel) {
        return CommandResult::Error(format!(
            "Usage: /popout [channel] - '{target_channel}' is not a valid Twitch channel name"
        ));
    }
    let url = twitch_popout_chat_url(&target_channel);
    match open_off_thread(url).await {
        Ok(()) => CommandResult::Success(Some(format!("Opened #{target_channel} popout chat"))),
        Err(error) => CommandResult::Error(format!("Failed to open popout chat: {error}")),
    }
}

pub async fn cmd_requests(channel: &str, args: &str) -> CommandResult {
    let target_channel = current_or_arg_channel(channel, args);
    if !crate::utils::channel_input::is_valid_twitch_login(&target_channel) {
        return CommandResult::Error("Usage: /requests [channel]".to_string());
    }

    let url = super::local_core::twitch_reward_queue_url(&target_channel);
    match open_off_thread(url).await {
        Ok(()) => CommandResult::Success(Some(format!(
            "Opened #{target_channel} channel points requests"
        ))),
        Err(error) => CommandResult::Error(format!("Failed to open requests queue: {error}")),
    }
}

pub async fn cmd_lowtrust(channel: &str, args: &str) -> CommandResult {
    let target_channel = current_or_arg_channel(channel, args);
    if !crate::utils::channel_input::is_valid_twitch_login(&target_channel) {
        return CommandResult::Error("Usage: /lowtrust [channel]".to_string());
    }

    let url = super::local_core::twitch_lowtrust_users_url(&target_channel);
    match open_off_thread(url).await {
        Ok(()) => CommandResult::Success(Some(format!(
            "Opened #{target_channel} suspicious user activity feed"
        ))),
        Err(error) => {
            CommandResult::Error(format!("Failed to open suspicious users feed: {error}"))
        }
    }
}

pub async fn cmd_automod() -> CommandResult {
    crate::backend::ui_actions::push_action(
        crate::backend::ui_actions::UiAction::OpenNotifications {
            filter: crate::backend::ui_actions::NotificationFilter::Automod,
        },
    );
    CommandResult::Success(Some("Opened recent AutoMod messages".to_string()))
}

pub async fn cmd_popup(channel: &str, args: &str) -> CommandResult {
    let target_channel = current_or_arg_channel(channel, args);
    if !crate::utils::channel_input::is_valid_twitch_login(&target_channel) {
        return CommandResult::Error(format!(
            "Usage: /popup [channel] - '{target_channel}' is not a valid Twitch channel name"
        ));
    }
    crate::backend::ui_actions::push_action(
        crate::backend::ui_actions::UiAction::OpenChannelPopup {
            channel: target_channel.clone(),
        },
    );
    CommandResult::Success(Some(format!("Opened #{target_channel} popup chat")))
}

pub async fn cmd_copy(args: &str) -> CommandResult {
    let text = args.trim();
    if text.is_empty() {
        return CommandResult::Error(
            "Usage: /copy <text> - copies provided text to clipboard.".to_string(),
        );
    }

    crate::utils::copy_text(text);
    CommandResult::Success(None)
}

pub async fn cmd_debug_args() -> CommandResult {
    let args = std::env::args().collect::<Vec<_>>();
    CommandResult::Success(Some(format!("Launch args: {}", args.join(" "))))
}

pub async fn cmd_debug_env() -> CommandResult {
    let (proxy_enabled, proxy_url) = {
        let settings = settings::SETTINGS.lock_or_recover();
        (settings.proxy_enabled, settings.proxy_url.clone())
    };
    let proxy_url = if proxy_enabled {
        let proxy_url = proxy_url.trim();
        if proxy_url.is_empty() {
            None
        } else {
            Some(crate::log::redact_url(proxy_url))
        }
    } else {
        None
    };
    let link_resolver_url = API_PROXY.map(|(url, _)| format!("{url}/proxy"));
    let lines = format_debug_env_lines(
        link_resolver_url.as_deref().unwrap_or("N/A"),
        proxy_url.as_deref(),
    );
    CommandResult::Success(Some(lines.join("\n")))
}

pub async fn cmd_debug_enable_logfile(args: &str) -> CommandResult {
    let log_file_path =
        match logfile_path_arg(args, "Usage: /debug-enable-logfile <path-to-logfile>") {
            Ok(path) => path,
            Err(usage) => return CommandResult::Error(usage.to_string()),
        };

    match crate::diagnostics::profiling::enable_runtime_log_file(&log_file_path) {
        Ok(path) => {
            CommandResult::Success(Some(format!("File logging enabled: {}", path.display())))
        }
        Err(error) => CommandResult::Error(format!("Failed to enable file logging: {error}")),
    }
}

pub async fn cmd_debug_disable_logfile() -> CommandResult {
    match crate::diagnostics::profiling::disable_runtime_log_file() {
        Ok(Some(path)) => {
            CommandResult::Success(Some(format!("File logging disabled: {}", path.display())))
        }
        Ok(None) => CommandResult::Success(Some("File logging was not enabled".to_string())),
        Err(error) => CommandResult::Error(format!("Failed to disable file logging: {error}")),
    }
}

pub async fn cmd_debug_relaunch_with_logfile(args: &str) -> CommandResult {
    let log_file_path = match logfile_path_arg(
        args,
        "Usage: /debug-relaunch-with-logfile <path-to-logfile>",
    ) {
        Ok(path) => path,
        Err(usage) => return CommandResult::Error(usage.to_string()),
    };

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return CommandResult::Error(format!("Failed to resolve Echo executable: {error}"));
        }
    };

    let mut command = Command::new(exe);
    command.args(std::env::args_os().skip(1));
    command.env(ECHO_LOG_FILE_ENV, "1");
    command.env(ECHO_LOG_FILE_PATH_ENV, &log_file_path);
    let forced_rust_log = std::env::var_os("RUST_LOG").is_none();
    if forced_rust_log {
        command.env("RUST_LOG", "debug");
    }
    let inherited_forced_rust_log = echo_core::env::os_value(ECHO_LOGFILE_RELAUNCH_ENV)
        .is_some_and(|marker| marker == ECHO_LOGFILE_RELAUNCH_FORCED_RUST_LOG);
    command.env(
        ECHO_LOGFILE_RELAUNCH_ENV,
        if forced_rust_log || inherited_forced_rust_log {
            ECHO_LOGFILE_RELAUNCH_FORCED_RUST_LOG
        } else {
            "1"
        },
    );

    crate::config::flush_tab_saves(std::time::Duration::from_secs(2));
    match command.spawn() {
        Ok(_) => {
            std::process::exit(0);
        }
        Err(error) => CommandResult::Error(format!("Failed to start process: {error}")),
    }
}

/// Drop the logging environment a `/debug-relaunch-with-logfile` session set for
/// itself, so a process that relaunches Echo later (the updater) starts it normally.
pub fn strip_logfile_relaunch_env(command: &mut Command) {
    let Some(marker) = echo_core::env::os_value(ECHO_LOGFILE_RELAUNCH_ENV) else {
        return;
    };
    command
        .env_remove(ECHO_LOGFILE_RELAUNCH_ENV)
        .env_remove(ECHO_LOG_FILE_ENV)
        .env_remove(ECHO_LOG_FILE_PATH_ENV);
    if marker == ECHO_LOGFILE_RELAUNCH_FORCED_RUST_LOG {
        command.env_remove("RUST_LOG");
    }
}

pub async fn cmd_unstable_set_user_color(args: &str) -> CommandResult {
    let mut parts = args.split_whitespace();
    let Some(target) = parts.next() else {
        return CommandResult::Error(
            "Usage: /unstable-set-user-color <TwitchUserID|username> [#RGB|#RRGGBB]".to_string(),
        );
    };
    let color = parts.next();
    if parts.next().is_some() {
        return CommandResult::Error(
            "Usage: /unstable-set-user-color <TwitchUserID|username> [#RGB|#RRGGBB]".to_string(),
        );
    }

    let (user_id, username, target_label) = if target.chars().all(|ch| ch.is_ascii_digit()) {
        let username =
            crate::backend::twitch_api::get_cached_channel_login_by_id(target).unwrap_or_default();
        let target_label = if username.is_empty() {
            format!("Twitch user ID {target}")
        } else {
            username.clone()
        };
        (target.to_string(), username, target_label)
    } else {
        let username = target.trim_start_matches('@').to_lowercase();
        if username.is_empty() {
            return CommandResult::Error(
                "Usage: /unstable-set-user-color <TwitchUserID|username> [#RGB|#RRGGBB]"
                    .to_string(),
            );
        }
        let user_id = match crate::backend::twitch_api::get_user_id_cached(&username).await {
            Ok(user_id) => user_id,
            Err(error) => {
                return CommandResult::Error(format!(
                    "Could not resolve Twitch user {username}: {error}"
                ));
            }
        };
        (user_id, username.clone(), username)
    };

    match color {
        Some(color) => {
            let Some(color) = crate::backend::irc::normalize_user_color_hex(color) else {
                return CommandResult::Error("Color must be #RGB or #RRGGBB".to_string());
            };
            if let Err(error) =
                crate::backend::irc::set_user_color_override(&user_id, &username, &color)
            {
                return CommandResult::Error(error);
            }
            crate::backend::settings::broadcast_username_color_overrides_changed();
            match crate::backend::irc::save_user_color_overrides() {
                Ok(()) => CommandResult::Success(Some(format!(
                    "Set local color for {target_label} to {color}"
                ))),
                Err(error) => CommandResult::Error(format!(
                    "Applied local color for this session, but could not save it: {error}"
                )),
            }
        }
        None => {
            crate::backend::irc::remove_user_color_override(&user_id);
            crate::backend::settings::broadcast_username_color_overrides_changed();
            match crate::backend::irc::save_user_color_overrides() {
                Ok(()) => {
                    CommandResult::Success(Some(format!("Cleared local color for {target_label}")))
                }
                Err(error) => CommandResult::Error(format!(
                    "Cleared local color for this session, but could not save it: {error}"
                )),
            }
        }
    }
}

pub async fn cmd_streamlink(channel: &str, args: &str) -> CommandResult {
    let target_channel = current_or_arg_channel(channel, args);
    let url = streamlink_target_url(&target_channel);
    let (executable, quality, extra_args) = match streamlink_launch_parts() {
        Ok(parts) => parts,
        Err(error) => return CommandResult::Error(error),
    };

    let mut command = Command::new(&executable);
    command.arg(&url).arg(&quality).args(extra_args);

    match command.spawn() {
        Ok(_) => CommandResult::Success(Some(format!(
            "Opening #{target_channel} with Streamlink ({quality})"
        ))),
        Err(error) => CommandResult::Error(format!("Failed to launch Streamlink: {error}")),
    }
}

pub fn streamlink_launch_parts() -> Result<(String, String, Vec<String>), String> {
    let executable = settings::get_streamlink_path()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| "streamlink".to_string());
    let quality = streamlink_quality_arg(settings::get_streamlink_quality());
    let extra_args = parse_streamlink_extra_args(&settings::get_streamlink_extra_args())?;

    Ok((executable, quality, extra_args))
}

/// External player a channel can be opened in from a hotkey or split menu.
pub enum StreamPlayer {
    Streamlink,
    Custom(String),
}

/// Launch `channel` in `player` on a background thread. Every failure is
/// logged and shown as a warning toast. Returns `None` when the launch could
/// not be prepared, otherwise the Streamlink quality (empty for a custom player).
pub fn launch_stream_player(channel: &str, player: StreamPlayer) -> Option<String> {
    let url = format!("https://twitch.tv/{channel}");
    let (label, executable, args, quality) = match player {
        StreamPlayer::Streamlink => match streamlink_launch_parts() {
            Ok((executable, quality, extra_args)) => {
                let mut args = vec![url, quality.clone()];
                args.extend(extra_args);
                ("Streamlink", executable, args, quality)
            }
            Err(error) => {
                report_player_failure("Failed to prepare Streamlink launch", channel, error);
                return None;
            }
        },
        StreamPlayer::Custom(path) => ("custom player", path, vec![url], String::new()),
    };
    let channel = channel.to_string();
    std::thread::spawn(move || {
        if let Err(error) = Command::new(&executable).args(&args).spawn() {
            report_player_failure(
                &format!("Failed to launch {label}"),
                &channel,
                error.to_string(),
            );
        }
    });
    Some(quality)
}

fn report_player_failure(message: &str, channel: &str, error: String) {
    crate::log_ui!(warn, "{message} for #{channel}: {error}");
    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
        message: message.to_string(),
        detail: Some(error),
    });
}

pub fn validate_streamlink_extra_args(args: &str) -> Result<(), String> {
    parse_streamlink_extra_args(args).map(|_| ())
}

fn parse_streamlink_extra_args(args: &str) -> Result<Vec<String>, String> {
    split_command_args(args).map_err(|error| format!("Invalid streamlink extra args: {error}"))
}

async fn resolve_user_target(
    channel: &str,
    args: &str,
    usage: &'static str,
) -> Result<ResolvedUserTarget, CommandResult> {
    let target = split_user_and_channel(channel, args, usage)
        .map_err(|usage| CommandResult::Error(usage.to_string()))?;

    if let Some(user_id) = target.user_id.as_deref() {
        if !user_id.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(CommandResult::Error(usage.to_string()));
        }

        let cached_profile = crate::backend::twitch_api::get_cached_user_profile(user_id);
        let login = match cached_profile
            .as_ref()
            .map(|profile| profile.login.clone())
            .filter(|login| !login.trim().is_empty())
        {
            Some(login) => login,
            None => match crate::backend::twitch_api::get_channel_login_by_id_cached(user_id).await
            {
                Some(login) => login,
                None => {
                    return Err(CommandResult::Error(format!(
                        "Could not resolve Twitch user ID {user_id}"
                    )));
                }
            },
        };
        let profile =
            cached_profile.or_else(|| crate::backend::twitch_api::get_cached_user_profile(&login));
        let display_name = profile
            .as_ref()
            .map(|profile| profile.display_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| login.clone());

        return Ok(ResolvedUserTarget {
            user: login,
            display_name,
            user_id: Some(user_id.to_string()),
            channel: target.channel,
        });
    }

    let profile = crate::backend::twitch_api::get_cached_user_profile(&target.user);
    let user = profile
        .as_ref()
        .map(|profile| profile.login.clone())
        .filter(|login| !login.trim().is_empty())
        .unwrap_or_else(|| target.user.clone());
    let display_name = profile
        .as_ref()
        .map(|profile| profile.display_name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| target.user.clone());
    let user_id = profile
        .as_ref()
        .map(|profile| profile.id.clone())
        .filter(|id| !id.trim().is_empty());

    Ok(ResolvedUserTarget {
        user,
        display_name,
        user_id,
        channel: target.channel,
    })
}

pub async fn cmd_user(channel: &str, args: &str) -> CommandResult {
    let target =
        match resolve_user_target(channel, args, "Usage: /user <user|id:userID> [channel]").await {
            Ok(target) => target,
            Err(error) => return error,
        };
    let user_login = target.user.trim_start_matches('@').to_ascii_lowercase();
    let channel_login = echo_core::channel_identity::normalize_channel_key(&target.channel);
    if !crate::utils::channel_input::is_valid_twitch_login(&user_login)
        || !crate::utils::channel_input::is_valid_twitch_login(&channel_login)
    {
        return CommandResult::Error("Usage: /user <user|id:userID> [channel]".to_string());
    }
    let url = twitch_viewer_card_url(&channel_login, &user_login);
    match open_off_thread(url).await {
        Ok(()) => {
            CommandResult::Success(Some(format!("Opened Twitch usercard for {}", target.user)))
        }
        Err(error) => CommandResult::Error(format!("Failed to open Twitch usercard: {error}")),
    }
}

pub async fn cmd_usercard(channel: &str, args: &str) -> CommandResult {
    let target =
        match resolve_user_target(channel, args, "Usage: /usercard <user|id:userID> [channel]")
            .await
        {
            Ok(target) => target,
            Err(error) => return error,
        };

    crate::backend::ui_actions::push_action(crate::backend::ui_actions::UiAction::ShowUserCard {
        username: target.user.clone(),
        display_name: target.display_name,
        user_id: target.user_id,
        channel: target.channel,
        color: None,
        badges: Vec::new(),
        x: f32::NAN,
        y: f32::NAN,
    });

    CommandResult::Success(None)
}

pub fn cmd_observed_chatters(channel: &str, reason: Option<&str>) -> CommandResult {
    let chatters = crate::backend::get_recent_chatters(channel, usize::MAX);
    let entries: Vec<ChatterListEntry> = chatters
        .iter()
        .map(|chatter| ChatterListEntry {
            username: chatter.login.clone(),
            display_name: chatter.display_name.clone(),
            user_id: None,
        })
        .collect();
    let count = entries.len();

    crate::backend::ui_actions::push_action(
        crate::backend::ui_actions::UiAction::ShowChatterList {
            channel: channel.trim_start_matches('#').to_lowercase(),
            chatters: entries,
            total_count: count,
            source: crate::backend::ui_actions::ChatterListSource::Observed,
            login_sorted: false,
            is_loading: false,
        },
    );

    let message = match reason {
        Some(reason) => format_observed_chatters_status(channel, count, reason),
        None => format!("Observed {count} recent chatters in #{channel}"),
    };
    CommandResult::Success(Some(message))
}

pub async fn cmd_uptime(channel: &str, args: &str) -> CommandResult {
    let target_channel = current_or_arg_channel(channel, args);
    let now_ts = Utc::now().timestamp();
    let Some(info) = crate::backend::get_stream_info_async(&target_channel).await else {
        return CommandResult::Success(Some(format_uptime_status(
            &target_channel,
            false,
            0,
            now_ts,
        )));
    };

    CommandResult::Success(Some(format_uptime_status(
        &target_channel,
        info.is_live,
        info.started_at,
        now_ts,
    )))
}

pub async fn cmd_whisper(_channel: &str, args: &str) -> CommandResult {
    let mut parts = args.splitn(2, char::is_whitespace);
    let Some(user) = parts.next().filter(|user| !user.is_empty()) else {
        return CommandResult::Error("Usage: /w <user> <message>".to_string());
    };

    let message = parts.next().unwrap_or("").trim();
    if message.is_empty() {
        return CommandResult::Error("Usage: /w <user> <message>".to_string());
    }

    let target_user = user.trim_start_matches('@');
    if target_user.trim().is_empty() {
        return CommandResult::Error("Usage: /w <user> <message>".to_string());
    }

    super::helix::cmd_whisper(target_user, message).await
}

pub async fn cmd_reply_whisper(channel: &str, args: &str) -> CommandResult {
    let message = args.trim();
    if message.is_empty() {
        return CommandResult::Error("Usage: /r <message>".to_string());
    }

    let Some(user) = crate::backend::whispers::last_received_sender() else {
        return CommandResult::Error("No received whisper to reply to".to_string());
    };

    cmd_whisper(channel, &format!("{user} {message}")).await
}

pub async fn cmd_reply(channel: &str, args: &str) -> CommandResult {
    let mut parts = args.splitn(2, char::is_whitespace);
    let username = parts
        .next()
        .unwrap_or("")
        .trim_start_matches(['@', '#'])
        .trim_end_matches(',')
        .to_lowercase();
    let message = parts.next().unwrap_or("").trim();
    if username.is_empty() || message.is_empty() {
        return CommandResult::Error("Usage: /reply <username> <message>".to_string());
    }

    let channel_key = channel.trim_start_matches('#').to_lowercase();
    let parent = crate::backend::irc::CHAT_MESSAGES
        .get(&channel_key)
        .and_then(|buffer| {
            let mut latest = None;
            for candidate in buffer.iter() {
                if candidate.allows_remote_message_actions()
                    && candidate.sender.eq_ignore_ascii_case(&username)
                {
                    latest = Some((candidate.id.to_string(), candidate.sender.to_string()));
                }
            }
            latest
        });

    let Some((reply_to_msg_id, reply_to_user)) = parent else {
        return CommandResult::Error("A message from that user wasn't found.".to_string());
    };

    let Some(auth) = crate::backend::auth::capture_authenticated_user_resource() else {
        return CommandResult::Error(
            "No authenticated Twitch account is ready to send this reply.".to_string(),
        );
    };
    match crate::backend::irc::send_reply_with_auth(
        &channel_key,
        message,
        &reply_to_msg_id,
        &reply_to_user,
        auth,
    )
    .await
    {
        Ok(()) => CommandResult::Success(None),
        Err(error) => CommandResult::from_failure(super::CommandFailure {
            message: error.message,
            delivery_unknown: error.delivery_unknown,
        }),
    }
}

pub async fn cmd_themefile(args: &str) -> CommandResult {
    let path = args.trim().trim_matches('"');
    if path.is_empty() {
        return CommandResult::Error("Usage: /themefile <path-to-theme-json>".to_string());
    }

    match settings::load_custom_theme_file(path.to_string()) {
        Ok(name) => CommandResult::Success(Some(format!("Loaded theme file: {name}"))),
        Err(error) => CommandResult::Error(format!("Failed to load theme file: {error}")),
    }
}

pub async fn cmd_themefile_export(args: &str) -> CommandResult {
    let path = args.trim().trim_matches('"');
    if path.is_empty() {
        return CommandResult::Error("Usage: /themefile-export <path-to-theme-json>".to_string());
    }

    match settings::export_current_theme_file(path.to_string()) {
        Ok(path) => CommandResult::Success(Some(format!("Exported theme file: {path}"))),
        Err(error) => CommandResult::Error(format!("Failed to export theme file: {error}")),
    }
}

fn streamlink_quality_arg(quality: StreamlinkQuality) -> String {
    match quality {
        StreamlinkQuality::Best => "best".to_string(),
        StreamlinkQuality::P720_60 => "720p60".to_string(),
        StreamlinkQuality::P480 => "480p".to_string(),
        StreamlinkQuality::AudioOnly => "audio_only".to_string(),
        StreamlinkQuality::Custom => {
            let custom = settings::get_streamlink_custom_quality();
            let custom = custom.trim();
            if custom.is_empty() {
                "best".to_string()
            } else {
                custom.to_string()
            }
        }
    }
}
