//! Pure helpers for local command behavior.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserChannelTarget {
    pub user: String,
    pub user_id: Option<String>,
    pub channel: String,
}

const RECENT_MESSAGES_API_URL: &str = "https://logs.zonian.dev/api/v2/recent-messages";
const TWITCH_IRC_HOST: &str = "irc.chat.twitch.tv";
const TWITCH_IRC_TLS_PORT: u16 = 6697;

pub const ECHO_LOG_FILE_ENV: &str = "ECHO_LOG_FILE";
pub const ECHO_LOG_FILE_PATH_ENV: &str = "ECHO_LOG_FILE_PATH";
/// Set on a `/debug-relaunch-with-logfile` process; `rust_log` also records that the
/// relaunch forced `RUST_LOG=debug` rather than inheriting the user's value.
pub const ECHO_LOGFILE_RELAUNCH_ENV: &str = "ECHO_LOGFILE_RELAUNCH";
pub const ECHO_LOGFILE_RELAUNCH_FORCED_RUST_LOG: &str = "rust_log";

pub fn current_or_arg_channel(current_channel: &str, args: &str) -> String {
    args.split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(current_channel)
        .trim_start_matches('#')
        .to_lowercase()
}

pub fn split_user_and_channel(
    current_channel: &str,
    args: &str,
    usage: &'static str,
) -> Result<UserChannelTarget, &'static str> {
    let mut parts = args.split_whitespace();
    let Some(user) = parts.next() else {
        return Err(usage);
    };

    let channel = parts
        .next()
        .map(|channel| channel.trim_start_matches('#').to_lowercase())
        .unwrap_or_else(|| current_channel.trim_start_matches('#').to_lowercase());
    let user = user.trim_start_matches('@');
    if user.is_empty() {
        return Err(usage);
    }
    if matches!(user.strip_prefix("id:"), Some("")) {
        return Err(usage);
    }
    let user_id = user
        .strip_prefix("id:")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned);

    Ok(UserChannelTarget {
        user: user.to_string(),
        user_id,
        channel,
    })
}

pub fn twitch_popout_chat_url(channel: &str) -> String {
    format!(
        "https://www.twitch.tv/popout/{}/chat",
        channel.trim_start_matches('#').to_lowercase()
    )
}

pub fn twitch_reward_queue_url(channel: &str) -> String {
    format!(
        "https://www.twitch.tv/popout/{}/reward-queue",
        channel.trim_start_matches('#').to_lowercase()
    )
}

pub fn twitch_lowtrust_users_url(channel: &str) -> String {
    format!(
        "https://www.twitch.tv/popout/moderator/{}/low-trust-users",
        channel.trim_start_matches('#').to_lowercase()
    )
}

pub fn twitch_viewer_card_url(channel: &str, user: &str) -> String {
    format!(
        "https://www.twitch.tv/popout/{}/viewercard/{}",
        channel.trim_start_matches('#').to_lowercase(),
        user.trim_start_matches('@')
    )
}

pub fn streamlink_target_url(channel: &str) -> String {
    format!(
        "https://twitch.tv/{}",
        channel.trim_start_matches('#').to_lowercase()
    )
}

pub fn format_debug_env_lines(link_resolver_url: &str, proxy_url: Option<&str>) -> Vec<String> {
    vec![
        format!("recentMessagesApiUrl: {RECENT_MESSAGES_API_URL}"),
        format!("linkResolverUrl: {link_resolver_url}"),
        format!(
            "proxyUrl: {}",
            proxy_url.filter(|url| !url.is_empty()).unwrap_or("N/A")
        ),
        format!("twitchServerHost: {TWITCH_IRC_HOST}"),
        format!("twitchServerPort: {TWITCH_IRC_TLS_PORT}"),
        "twitchServerSecure: 1".to_string(),
    ]
}

pub fn logfile_path_arg(args: &str, usage: &'static str) -> Result<String, &'static str> {
    let path = args.trim().trim_matches('"').trim_matches('\'').trim();
    if path.is_empty() {
        Err(usage)
    } else {
        Ok(path.to_string())
    }
}

pub fn split_command_args(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if in_quotes => match chars.peek().copied() {
                Some('"') | Some('\\') => {
                    current.push(chars.next().unwrap_or(ch));
                }
                _ => current.push(ch),
            },
            '"' => in_quotes = !in_quotes,
            ch if ch.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }

    if in_quotes {
        return Err("unterminated quote".to_string());
    }

    if !current.is_empty() {
        args.push(current);
    }

    Ok(args)
}

pub fn format_uptime_status(channel: &str, is_live: bool, started_at: i64, now_ts: i64) -> String {
    let channel = channel.trim_start_matches('#').to_lowercase();
    if !is_live {
        return format!("#{channel} is not live");
    }

    let elapsed = now_ts.saturating_sub(started_at).max(0);
    let hours = elapsed / 3600;
    let minutes = (elapsed % 3600) / 60;
    let seconds = elapsed % 60;
    format!("#{channel} has been live for {hours}h {minutes}m {seconds}s")
}
