//! Parsing `/fakemsg` raw IRC lines into the system events they would raise.

use twitch_irc::message::{IRCMessage, IRCPrefix};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeIrcHighlight {
    None,
    Whisper,
    Subscription,
    Raid,
    BitsBadge,
    Announcement,
    WatchStreak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeIrcSystemEvent {
    pub text: String,
    pub highlight: FakeIrcHighlight,
}

pub fn parse_fake_irc_system_event(raw: &str, channel: &str) -> Option<FakeIrcSystemEvent> {
    let msg = IRCMessage::parse(raw).ok()?;
    fake_irc_system_event(&msg, channel)
}

pub fn fake_irc_system_event(msg: &IRCMessage, channel: &str) -> Option<FakeIrcSystemEvent> {
    match msg.command.as_str() {
        "NOTICE" => notice_event(msg),
        "USERNOTICE" => usernotice_event(msg),
        "CLEARCHAT" => clearchat_event(msg),
        "CLEARMSG" => clearmsg_event(msg),
        "ROOMSTATE" => roomstate_event(msg),
        "USERSTATE" => userstate_event(msg, false),
        "GLOBALUSERSTATE" => userstate_event(msg, true),
        "HOSTTARGET" => hosttarget_event(msg),
        "RECONNECT" => static_event("Twitch requested a reconnect"),
        "PING" => ping_pong_event("PING", msg),
        "PONG" => ping_pong_event("PONG", msg),
        "JOIN" => join_part_event(msg, channel, true),
        "PART" => join_part_event(msg, channel, false),
        "CAP" => cap_event(msg),
        "WHISPER" => whisper_event(msg),
        command if is_numeric_reply(command) => numeric_event(msg),
        _ => None,
    }
}

fn notice_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    Some(event(
        msg.params.last().filter(|value| !value.is_empty())?,
        FakeIrcHighlight::None,
    ))
}

fn usernotice_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let msg_id = tag_value(msg, "msg-id").unwrap_or_default();
    let system_message = match tag_value(msg, "system-msg").filter(|value| !value.is_empty()) {
        Some(message) => message,
        None if msg_id == "announcement" => {
            let content = msg.params.last().filter(|value| !value.is_empty())?;
            format!("Announcement: {content}")
        }
        None => msg
            .params
            .last()
            .filter(|value| !value.is_empty())
            .cloned()?,
    };

    let highlight = if is_subscription_usernotice(&msg_id) {
        FakeIrcHighlight::Subscription
    } else {
        match msg_id.as_str() {
            "raid" => FakeIrcHighlight::Raid,
            "bitsbadgetier" => FakeIrcHighlight::BitsBadge,
            "announcement" => FakeIrcHighlight::Announcement,
            _ if is_watch_streak_usernotice(msg, &msg_id) => FakeIrcHighlight::WatchStreak,
            _ => FakeIrcHighlight::None,
        }
    };

    Some(event(&unescape_irc_tag_value(&system_message), highlight))
}

fn is_subscription_usernotice(msg_id: &str) -> bool {
    matches!(
        msg_id,
        "sub"
            | "resub"
            | "subgift"
            | "anonsubgift"
            | "submysterygift"
            | "giftpaidupgrade"
            | "anongiftpaidupgrade"
            | "primepaidupgrade"
            | "communitypayforward"
            | "rewardgift"
            | "modiversary"
    )
}

fn is_watch_streak_usernotice(msg: &IRCMessage, msg_id: &str) -> bool {
    if matches!(msg_id, "watch-streak" | "watchstreak") {
        return true;
    }

    if msg_id == "viewermilestone" {
        return tag_value(msg, "msg-param-category")
            .as_deref()
            .is_some_and(|category| category.eq_ignore_ascii_case("watch-streak"))
            && tag_value(msg, "msg-param-value")
                .and_then(|value| value.parse::<u32>().ok())
                .is_some_and(|value| value > 0);
    }

    if tag_value(msg, "ritual-name")
        .as_deref()
        .is_some_and(is_watch_streak_label)
    {
        return true;
    }

    if tag_value(msg, "watch-streak-months")
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|months| months > 0)
    {
        return true;
    }

    tag_value(msg, "watch-streak")
        .as_deref()
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn is_watch_streak_label(value: &str) -> bool {
    value.eq_ignore_ascii_case("watch-streak") || value.eq_ignore_ascii_case("watchstreak")
}

fn clearchat_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let target = msg.params.get(1).map(|value| value.trim_start_matches('@'));
    let text = match target.filter(|value| !value.is_empty()) {
        Some(user) => {
            match tag_value(msg, "ban-duration").and_then(|value| value.parse::<u64>().ok()) {
                Some(seconds) => format!("{user} timed out for {seconds} seconds"),
                None => format!("{user} was permanently banned"),
            }
        }
        None => "Chat was cleared by a moderator".to_string(),
    };

    Some(event(&text, FakeIrcHighlight::None))
}

fn clearmsg_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let login = tag_value(msg, "login").unwrap_or_else(|| "unknown".to_string());
    let message_id = tag_value(msg, "target-msg-id");
    let text = match message_id {
        Some(id) => format!("Message from {login} was deleted (id: {id})"),
        None => format!("Message from {login} was deleted"),
    };

    Some(event(&text, FakeIrcHighlight::None))
}

fn roomstate_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let mut changes = Vec::new();
    for key in [
        "emote-only",
        "followers-only",
        "r9k",
        "rituals",
        "slow",
        "subs-only",
    ] {
        if let Some(value) = tag_value(msg, key) {
            changes.push(format!("{key}={value}"));
        }
    }

    let text = if changes.is_empty() {
        "Room state updated".to_string()
    } else {
        format!("Room state updated: {}", changes.join(", "))
    };

    Some(event(&text, FakeIrcHighlight::None))
}

fn userstate_event(msg: &IRCMessage, global: bool) -> Option<FakeIrcSystemEvent> {
    let display_name = tag_value(msg, "display-name").unwrap_or_else(|| "current user".to_string());
    let text = if global {
        format!("Global user state updated for {display_name}")
    } else {
        format!("User state updated for {display_name}")
    };
    Some(event(&text, FakeIrcHighlight::None))
}

fn hosttarget_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let payload = msg.params.get(1).or_else(|| msg.params.last())?;
    let mut parts = payload.split_whitespace();
    let target = parts.next().unwrap_or("-");
    let viewers = parts.next();

    let text = if target == "-" {
        "Exited host mode".to_string()
    } else if let Some(viewers) = viewers {
        format!("Now hosting {target} for {viewers} viewers")
    } else {
        format!("Now hosting {target}")
    };

    Some(event(&text, FakeIrcHighlight::None))
}

fn ping_pong_event(command: &str, msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let server = msg
        .params
        .last()
        .map(|value| value.as_str())
        .unwrap_or("server");
    Some(event(
        &format!("Received server {command} from {server}"),
        FakeIrcHighlight::None,
    ))
}

fn join_part_event(
    msg: &IRCMessage,
    fallback_channel: &str,
    joined: bool,
) -> Option<FakeIrcSystemEvent> {
    let nick = prefix_nick(msg)?;
    let channel = msg
        .params
        .iter()
        .find_map(|param| param.strip_prefix('#'))
        .unwrap_or(fallback_channel);
    let action = if joined { "joined" } else { "left" };
    Some(event(
        &format!("{nick} {action} #{channel}"),
        FakeIrcHighlight::None,
    ))
}

fn cap_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let action = msg.params.get(1).or_else(|| msg.params.first())?;
    let capabilities = msg.params.last().filter(|value| *value != action);
    let text = match capabilities {
        Some(capabilities) if !capabilities.is_empty() => {
            format!("Capability {action}: {capabilities}")
        }
        _ => format!("Capability {action}"),
    };
    Some(event(&text, FakeIrcHighlight::None))
}

fn whisper_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let sender = tag_value(msg, "display-name")
        .or_else(|| prefix_nick(msg).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let text = msg.params.last().filter(|value| !value.is_empty())?;
    Some(event(
        &format!("Whisper from {sender}: {text}"),
        FakeIrcHighlight::Whisper,
    ))
}

fn numeric_event(msg: &IRCMessage) -> Option<FakeIrcSystemEvent> {
    let text = msg
        .params
        .iter()
        .filter(|value| !value.is_empty() && value.as_str() != "*")
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    Some(event(
        &format!("IRC {}: {text}", msg.command),
        FakeIrcHighlight::None,
    ))
}

fn static_event(text: &str) -> Option<FakeIrcSystemEvent> {
    Some(event(text, FakeIrcHighlight::None))
}

fn event(text: &str, highlight: FakeIrcHighlight) -> FakeIrcSystemEvent {
    FakeIrcSystemEvent {
        text: text.to_string(),
        highlight,
    }
}

fn is_numeric_reply(command: &str) -> bool {
    command.len() == 3 && command.bytes().all(|byte| byte.is_ascii_digit())
}

fn tag_value(msg: &IRCMessage, key: &str) -> Option<String> {
    msg.tags.0.get(key).cloned()
}

fn prefix_nick(msg: &IRCMessage) -> Option<&str> {
    match msg.prefix.as_ref()? {
        IRCPrefix::Full { nick, .. } => Some(nick.as_str()),
        IRCPrefix::HostOnly { host } => Some(host.as_str()),
    }
}

fn unescape_irc_tag_value(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('s') => output.push(' '),
            Some(':') => output.push(';'),
            Some('\\') => output.push('\\'),
            Some('r') => output.push('\r'),
            Some('n') => output.push('\n'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}
