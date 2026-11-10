//! Chat command dispatch engine
//! Parses /commands from chat input and routes to appropriate handlers

pub mod custom;
pub mod helix;
mod helix_core;
mod local;
mod local_core;
pub mod twitch;
mod twitch_core;

/// Result of executing a command
pub enum CommandResult {
    /// Command executed successfully (optional status message to show user)
    Success(Option<String>),
    /// Preflight failed or the remote service definitely rejected the command.
    Error(String),
    /// The request may have applied; callers must not suggest automatic retry.
    DeliveryUnknown(String),
    /// Input is not a command, send as regular chat message
    NotACommand,
}
pub(crate) struct CommandFailure {
    pub message: String,
    pub delivery_unknown: bool,
}

impl CommandFailure {
    pub(crate) fn definite(message: String) -> Self {
        Self {
            message,
            delivery_unknown: false,
        }
    }

    pub(crate) fn unknown(message: String) -> Self {
        Self {
            message,
            delivery_unknown: true,
        }
    }

    pub(crate) fn http_error(status: u16, message: String) -> Self {
        if status >= 500 || status == 408 {
            Self::unknown(message)
        } else {
            Self::definite(message)
        }
    }
}

impl CommandResult {
    pub(crate) fn from_failure(error: CommandFailure) -> Self {
        if error.delivery_unknown {
            Self::DeliveryUnknown(error.message)
        } else {
            Self::Error(error.message)
        }
    }

    pub(crate) fn http_error(status: u16, message: String) -> Self {
        Self::from_failure(CommandFailure::http_error(status, message))
    }
}

pub use custom::expand_custom_command;
pub use local::{
    StreamPlayer, launch_stream_player, strip_logfile_relaunch_env, validate_streamlink_extra_args,
};

pub use echo_autocomplete_core::{
    CommandCompletionKind as CommandArgumentCompletion,
    CommandDescriptor as CommandAutocompleteSpec, CommandOptionSpec, CommandValueSpec,
};

macro_rules! command_spec {
    ($name:literal, $description:literal) => {
        CommandAutocompleteSpec {
            name: $name,
            description: $description,
            usage: None,
            options: &[],
            argument_completion: CommandArgumentCompletion::None,
        }
    };
    ($name:literal, $description:literal, usage: $usage:literal) => {
        CommandAutocompleteSpec {
            name: $name,
            description: $description,
            usage: Some($usage),
            options: &[],
            argument_completion: CommandArgumentCompletion::None,
        }
    };
    ($name:literal, $description:literal, usage: $usage:literal, argument_completion: $argument_completion:expr_2021) => {
        CommandAutocompleteSpec {
            name: $name,
            description: $description,
            usage: Some($usage),
            options: &[],
            argument_completion: $argument_completion,
        }
    };
    ($name:literal, $description:literal, usage: $usage:literal, options: $options:expr_2021) => {
        CommandAutocompleteSpec {
            name: $name,
            description: $description,
            usage: Some($usage),
            options: $options,
            argument_completion: CommandArgumentCompletion::None,
        }
    };
    ($name:literal, $description:literal, usage: $usage:literal, options: $options:expr_2021, argument_completion: $argument_completion:expr_2021) => {
        CommandAutocompleteSpec {
            name: $name,
            description: $description,
            usage: Some($usage),
            options: $options,
            argument_completion: $argument_completion,
        }
    };
}

const POLL_COMMAND_OPTIONS: &[CommandOptionSpec] = &[
    ("--title, -t", "Poll title"),
    (
        "--choice, -c",
        "Add one poll choice; repeat for each choice",
    ),
    ("--duration, -d", "Poll duration in seconds or minutes"),
    ("--points, -p", "Optional channel-points voting cost"),
];

const PREDICTION_COMMAND_OPTIONS: &[CommandOptionSpec] = &[
    ("--title, -t", "Prediction title"),
    (
        "--choice, -c",
        "Add one prediction outcome; repeat for each outcome",
    ),
    (
        "--duration, -d",
        "Prediction duration in seconds or minutes",
    ),
    (
        "--points, -p",
        "Accepted for compatibility; Twitch predictions always use channel points",
    ),
];

const COMPLETE_PREDICTION_COMMAND_OPTIONS: &[CommandOptionSpec] = &[
    ("--choice, -c", "Resolve by outcome title"),
    ("--index, -i", "Resolve by one-based outcome index"),
];

const COLOR_COMMAND_VALUES: &[CommandValueSpec] = &[
    ("blue", "Blue"),
    ("blueviolet", "Blue Violet"),
    ("cadetblue", "Cadet Blue"),
    ("chocolate", "Chocolate"),
    ("coral", "Coral"),
    ("dodgerblue", "Dodger Blue"),
    ("firebrick", "Firebrick"),
    ("goldenrod", "Golden Rod"),
    ("green", "Green"),
    ("hotpink", "Hot Pink"),
    ("orangered", "Orange Red"),
    ("red", "Red"),
    ("seagreen", "Sea Green"),
    ("springgreen", "Spring Green"),
    ("yellowgreen", "Yellow Green"),
];

const COMMERCIAL_COMMAND_VALUES: &[CommandValueSpec] = &[
    ("30", "30 seconds"),
    ("60", "60 seconds"),
    ("90", "90 seconds"),
    ("120", "120 seconds"),
    ("150", "150 seconds"),
    ("180", "180 seconds"),
];

const SLOW_COMMAND_VALUES: &[CommandValueSpec] = &[
    ("1", "1 second"),
    ("5", "5 seconds"),
    ("10", "10 seconds"),
    ("30", "30 seconds"),
    ("60", "60 seconds"),
    ("120", "120 seconds"),
];

const OPENURL_COMMAND_OPTIONS: &[CommandOptionSpec] = &[
    ("--incognito", "Open in a private browser window"),
    (
        "--no-incognito",
        "Open in a normal browser window, ignoring the global setting",
    ),
];

pub const COMMAND_AUTOCOMPLETE_SPECS: &[CommandAutocompleteSpec] = &[
    command_spec!("announce", "Send a Twitch announcement", usage: "/announce <message>"),
    command_spec!("announceblue", "Send a blue announcement", usage: "/announceblue <message>"),
    command_spec!("announcegreen", "Send a green announcement", usage: "/announcegreen <message>"),
    command_spec!("announceorange", "Send an orange announcement", usage: "/announceorange <message>"),
    command_spec!("announcepurple", "Send a purple announcement", usage: "/announcepurple <message>"),
    command_spec!("automod", "Open recent AutoMod held-message notifications"),
    command_spec!("ban", "Ban a user from chat", usage: "/ban <user|id:userID> [reason]", argument_completion: CommandArgumentCompletion::User),
    command_spec!("banid", "Ban a user by Twitch user ID instead of username", usage: "/banid <userID>"),
    command_spec!("block", "Block a user on Twitch", usage: "/block <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("cancelpoll", "Cancel the active poll"),
    command_spec!("cancelprediction", "Cancel the open prediction"),
    command_spec!("chatters", "Show the chatters connected to the channel"),
    command_spec!("clear", "Clear chat"),
    command_spec!(
        "clearmessages",
        "Clear all messages from the current split locally"
    ),
    command_spec!(
        "clip",
        "Create a clip from the last 30 seconds of the current channel"
    ),
    command_spec!("color", "Change name color", usage: "/color <color>", argument_completion: CommandArgumentCompletion::Values(COLOR_COMMAND_VALUES)),
    command_spec!("commercial", "Start a commercial break", usage: "/commercial <30|60|90|120|150|180>", argument_completion: CommandArgumentCompletion::Values(COMMERCIAL_COMMAND_VALUES)),
    command_spec!(
        "completeprediction",
        "Resolve the open prediction",
        usage: "/completeprediction --choice|-c <choice> | --index|-i <index>",
        options: COMPLETE_PREDICTION_COMMAND_OPTIONS,
        argument_completion: CommandArgumentCompletion::PredictionOutcome
    ),
    command_spec!("copy", "Copy text to the clipboard", usage: "/copy <text>"),
    command_spec!("debug-args", "Display the arguments Echo was launched with"),
    command_spec!("debug-disable-logfile", "Disable runtime file logging"),
    command_spec!("debug-enable-logfile", "Enable runtime file logging", usage: "/debug-enable-logfile <path-to-logfile>"),
    command_spec!("debug-env", "Show Echo runtime environment"),
    command_spec!("debug-relaunch-with-logfile", "Restart Echo with file logging enabled", usage: "/debug-relaunch-with-logfile <path-to-logfile>"),
    command_spec!("delete", "Delete a chat message by message ID", usage: "/delete <msg-id>", argument_completion: CommandArgumentCompletion::MessageId),
    command_spec!("emoteonly", "Enable emote-only mode"),
    command_spec!("emoteonlyoff", "Disable emote-only"),
    command_spec!("endpoll", "End the active poll"),
    command_spec!("fakemsg", "Display a raw IRC message locally as if Twitch sent it", usage: "/fakemsg <raw message>"),
    command_spec!("follow", "Show Twitch follow command status"),
    command_spec!("followers", "Enable followers-only mode", usage: "/followers [duration]"),
    command_spec!("followersoff", "Disable followers-only"),
    command_spec!("ignore", "Alias for /block", usage: "/ignore <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("lockprediction", "Lock the open prediction"),
    command_spec!("lowtrust", "Open suspicious-user activity", usage: "/lowtrust [channel]", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("marker", "Create a stream marker in the current live stream", usage: "/marker [description]"),
    command_spec!("me", "Send an action message", usage: "/me <message>"),
    command_spec!("mod", "Give moderator to user", usage: "/mod <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("mods", "Open the moderator list"),
    command_spec!("monitor", "Monitor a suspicious user", usage: "/monitor <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!(
        "openurl",
        "Open a URL in the default web browser",
        usage: "/openurl [--incognito|--no-incognito] <url>",
        options: OPENURL_COMMAND_OPTIONS
    ),
    command_spec!(
        "poll",
        "Create a poll",
        usage: "/poll --title|-t <title> --duration|-d <duration>[s|m] --choice|-c <choice> --choice|-c <choice> [--points|-p <points>]",
        options: POLL_COMMAND_OPTIONS
    ),
    command_spec!("popout", "Open the current or provided channel in Twitch popout chat", usage: "/popout [channel]", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("popup", "Open the current or provided channel in a new Echo window", usage: "/popup [channel]", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("profile", "Alias for /user", usage: "/profile <user|id:userID> [channel]", argument_completion: CommandArgumentCompletion::UserThenChannel),
    command_spec!(
        "prediction",
        "Create a prediction",
        usage: "/prediction --title|-t <title> --duration|-d <duration>[s|m] --choice|-c <choice> --choice|-c <choice> [--points|-p <points>]",
        options: PREDICTION_COMMAND_OPTIONS
    ),
    command_spec!("r", "Reply to the last whisper", usage: "/r <message>"),
    command_spec!("r9k", "Enable unique chat"),
    command_spec!("r9kbeta", "Enable unique chat"),
    command_spec!("r9kbetaoff", "Disable unique chat"),
    command_spec!("r9koff", "Disable unique chat"),
    command_spec!("raid", "Raid another channel", usage: "/raid <channel>", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("raw", "Send a raw IRC command", usage: "/raw <raw IRC command>"),
    command_spec!("reply", "Reply to a recent chat message", usage: "/reply <user> <message>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("requests", "Open channel-points requests", usage: "/requests [channel]", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("restrict", "Restrict a suspicious user", usage: "/restrict <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("setgame", "Set the current channel's game to the best matching category", usage: "/setgame <game>"),
    command_spec!("settitle", "Set the current channel's title", usage: "/settitle <title>"),
    command_spec!("shield", "Enable Shield Mode"),
    command_spec!("shieldoff", "Disable Shield Mode"),
    command_spec!("shoutout", "Send a shoutout", usage: "/shoutout <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("slow", "Enable slow mode", usage: "/slow <seconds>", argument_completion: CommandArgumentCompletion::Values(SLOW_COMMAND_VALUES)),
    command_spec!("slowoff", "Disable slow mode"),
    command_spec!("so", "Alias for /shoutout", usage: "/so <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("streamlink", "Open the current or specified stream in Streamlink", usage: "/streamlink [channel]", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("subscribers", "Enable subscribers-only mode"),
    command_spec!("subscribersoff", "Disable subscribers-only"),
    command_spec!("themefile", "Load a theme file", usage: "/themefile <path>"),
    command_spec!("themefile-export", "Export the current theme", usage: "/themefile-export <path>"),
    command_spec!("test-chatters", "Alias for /chatters"),
    command_spec!("timeout", "Timeout a user", usage: "/timeout <user|id:userID> [seconds] [reason]", argument_completion: CommandArgumentCompletion::User),
    command_spec!("to", "Alias for /timeout", usage: "/to <user|id:userID> [seconds] [reason]", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unban", "Unban a user", usage: "/unban <user|id:userID>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unblock", "Unblock a user on Twitch", usage: "/unblock <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unfollow", "Show Twitch unfollow command status"),
    command_spec!("unignore", "Alias for /unblock", usage: "/unignore <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("uniquechat", "Enable unique chat"),
    command_spec!("uniquechatoff", "Disable unique chat"),
    command_spec!("unmod", "Remove moderator from user", usage: "/unmod <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unmonitor", "Stop monitoring a suspicious user", usage: "/unmonitor <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unraid", "Cancel an ongoing raid"),
    command_spec!("unrestrict", "Unrestrict a suspicious user", usage: "/unrestrict <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unstable-set-user-color", "Set a local username color", usage: "/unstable-set-user-color <TwitchUserID|username> [#RGB|#RRGGBB]", argument_completion: CommandArgumentCompletion::User),
    command_spec!("untimeout", "Remove timeout from user", usage: "/untimeout <user|id:userID>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("unvip", "Remove VIP from user", usage: "/unvip <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("uptime", "Display the uptime of the current or specified livestream", usage: "/uptime [channel]", argument_completion: CommandArgumentCompletion::Channel),
    command_spec!("user", "Open the specified user's Twitch user card in the browser", usage: "/user <user|id:userID> [channel]", argument_completion: CommandArgumentCompletion::UserThenChannel),
    command_spec!("usercard", "Open the specified user's Echo user card for a channel", usage: "/usercard <user|id:userID> [channel]", argument_completion: CommandArgumentCompletion::UserThenChannel),
    command_spec!("vip", "Give VIP to user", usage: "/vip <user>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("vips", "Open the VIP list"),
    command_spec!("w", "Whisper the provided text to a user on Twitch", usage: "/w <user> <message>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("whisper", "Alias for /w", usage: "/whisper <user> <message>", argument_completion: CommandArgumentCompletion::User),
    command_spec!("warn", "Warn a user", usage: "/warn <user> <reason>", argument_completion: CommandArgumentCompletion::User),
];

pub fn is_builtin_command(command: &str) -> bool {
    COMMAND_AUTOCOMPLETE_SPECS
        .iter()
        .any(|spec| spec.name.eq_ignore_ascii_case(command))
}

/// Parse and execute a command from chat input
/// Returns CommandResult indicating what happened
pub async fn execute_command(channel: &str, input: &str) -> CommandResult {
    let input = input.trim();

    if !input.starts_with('/') {
        return CommandResult::NotACommand;
    }

    if let Err(error) = crate::bench::reject_live_outbound_user_action("slash command") {
        return CommandResult::Error(error);
    }

    if !crate::backend::settings::get_slash_commands_enabled() {
        return CommandResult::NotACommand;
    }

    let (command, args) = command_parts(&input[1..]);
    let command = command.to_lowercase();

    match command.as_str() {
        "timeout" | "to" => twitch::cmd_timeout(channel, args).await,
        "ban" => twitch::cmd_ban(channel, args).await,
        "unban" => twitch::cmd_unban(channel, args).await,
        "untimeout" => twitch::cmd_untimeout(channel, args).await,
        "delete" => twitch::cmd_delete(channel, args).await,
        "warn" => helix::cmd_warn(channel, args).await,
        "mod" => helix::cmd_mod(channel, args).await,
        "unmod" => helix::cmd_unmod(channel, args).await,
        "vip" => helix::cmd_vip(channel, args).await,
        "unvip" => helix::cmd_unvip(channel, args).await,
        "mods" => helix::cmd_mods(channel).await,
        "vips" => helix::cmd_vips(channel).await,
        "monitor" => helix::cmd_monitor(channel, args).await,
        "unmonitor" => helix::cmd_unmonitor(channel, args).await,
        "restrict" => helix::cmd_restrict(channel, args).await,
        "unrestrict" => helix::cmd_unrestrict(channel, args).await,
        "banid" => twitch::cmd_banid(channel, args).await,
        "block" => helix::cmd_block(args).await,
        "ignore" => helix::cmd_ignore(args).await,
        "unblock" => helix::cmd_unblock(args).await,
        "unignore" => helix::cmd_unignore(args).await,

        "emoteonly" => helix::cmd_emote_only(channel, true).await,
        "emoteonlyoff" => helix::cmd_emote_only(channel, false).await,
        "subscribers" => helix::cmd_subscribers_only(channel, true).await,
        "subscribersoff" => helix::cmd_subscribers_only(channel, false).await,
        "slow" => helix::cmd_slow_mode(channel, args).await,
        "slowoff" => helix::cmd_slow_mode_off(channel).await,
        "r9kbeta" | "r9k" | "uniquechat" => helix::cmd_unique_chat(channel, true).await,
        "r9kbetaoff" | "r9koff" | "uniquechatoff" => helix::cmd_unique_chat(channel, false).await,
        "followers" => helix::cmd_followers_only(channel, args).await,
        "followersoff" => helix::cmd_followers_only_off(channel).await,
        "clear" => helix::delete_chat_messages(channel, None).await,

        "announce" | "announceblue" | "announcegreen" | "announceorange" | "announcepurple" => {
            helix::cmd_announce(channel, &command, args).await
        }
        "shield" => helix::cmd_shield(channel).await,
        "shieldoff" => helix::cmd_shieldoff(channel).await,
        "shoutout" | "so" => helix::cmd_shoutout(channel, args).await,
        "clip" => helix::cmd_clip(channel).await,
        "color" => helix::cmd_color(args).await,
        "commercial" => helix::cmd_commercial(channel, args).await,
        "cancelprediction" => helix::cmd_cancelprediction(channel).await,
        "cancelpoll" => helix::cmd_cancelpoll(channel).await,
        "completeprediction" => helix::cmd_completeprediction(channel, args).await,
        "endpoll" => helix::cmd_endpoll(channel).await,
        "lockprediction" => helix::cmd_lockprediction(channel).await,
        "poll" => helix::cmd_poll(channel, args).await,
        "prediction" => helix::cmd_prediction(channel, args).await,
        "profile" => local::cmd_user(channel, args).await,
        "marker" => helix::cmd_marker(channel, args).await,
        "raid" => helix::cmd_raid(channel, args).await,
        "raw" => twitch::cmd_raw(args).await,
        "setgame" => helix::cmd_setgame(channel, args).await,
        "settitle" => helix::cmd_settitle(channel, args).await,
        "unraid" => helix::cmd_unraid(channel).await,
        "chatters" | "test-chatters" => helix::cmd_chatters(channel).await,

        "clearmessages" => local::cmd_clearmessages(channel).await,
        "copy" => local::cmd_copy(args).await,
        "debug-args" => local::cmd_debug_args().await,
        "debug-disable-logfile" => local::cmd_debug_disable_logfile().await,
        "debug-enable-logfile" => local::cmd_debug_enable_logfile(args).await,
        "debug-env" => local::cmd_debug_env().await,
        "debug-relaunch-with-logfile" => local::cmd_debug_relaunch_with_logfile(args).await,
        "fakemsg" => local::cmd_fakemsg(channel, args).await,
        "follow" => local::cmd_follow_removed().await,
        "automod" => local::cmd_automod().await,
        "lowtrust" => local::cmd_lowtrust(channel, args).await,
        "openurl" => local::cmd_openurl(args).await,
        "popout" => local::cmd_popout(channel, args).await,
        "popup" => local::cmd_popup(channel, args).await,
        "r" => local::cmd_reply_whisper(channel, args).await,
        "reply" => local::cmd_reply(channel, args).await,
        "requests" => local::cmd_requests(channel, args).await,
        "streamlink" => local::cmd_streamlink(channel, args).await,
        "uptime" => local::cmd_uptime(channel, args).await,
        "unfollow" => local::cmd_unfollow_removed().await,
        "unstable-set-user-color" => local::cmd_unstable_set_user_color(args).await,
        "user" => local::cmd_user(channel, args).await,
        "usercard" => local::cmd_usercard(channel, args).await,
        "w" | "whisper" => local::cmd_whisper(channel, args).await,

        "themefile" => local::cmd_themefile(args).await,
        "themefile-export" => local::cmd_themefile_export(args).await,

        "me" => CommandResult::NotACommand,

        _ => twitch::cmd_passthrough(channel, input).await,
    }
}

fn command_parts(without_slash: &str) -> (&str, &str) {
    let mut parts = without_slash.splitn(2, char::is_whitespace);
    (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("").trim(),
    )
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    #[test]
    fn command_parser_accepts_non_space_whitespace() {
        for input in [
            "/openurl https://example.com",
            "/openurl\thttps://example.com",
        ] {
            assert_eq!(
                super::command_parts(&input[1..]),
                ("openurl", "https://example.com")
            );
        }
    }
}
