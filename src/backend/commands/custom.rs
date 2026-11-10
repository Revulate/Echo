use echo_core::commands::match_trigger;

pub async fn expand_custom_command(channel: &str, input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    if let Some(command_name) = input
        .strip_prefix('/')
        .and_then(|value| value.split_whitespace().next())
        && super::is_builtin_command(command_name)
    {
        return None;
    }

    let commands = crate::backend::settings::get_custom_commands();
    let command = commands
        .iter()
        .filter(|command| command.enabled)
        .find_map(|command| match_trigger(command, input).map(|args| (command, args)))?;

    Some(resolve_placeholders(command.0.response.as_str(), channel, command.1).await)
}

async fn resolve_placeholders(template: &str, channel: &str, args: &str) -> String {
    let args: Vec<&str> = args.split_whitespace().collect();
    let mut output = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '{' {
            output.push(ch);
            continue;
        }

        if chars.peek() == Some(&'{') {
            chars.next();
            output.push('{');
            continue;
        }

        let mut key = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '}' {
                closed = true;
                break;
            }
            key.push(next);
        }

        if !closed {
            output.push('{');
            output.push_str(&key);
            break;
        }

        if let Some(value) = resolve_placeholder(&key, channel, &args).await {
            output.push_str(&value);
        } else {
            output.push('{');
            output.push_str(&key);
            output.push('}');
        }
    }

    output
}

async fn resolve_placeholder(key: &str, channel: &str, args: &[&str]) -> Option<String> {
    if let Some(index) = key.strip_suffix('+').and_then(parse_one_based_index) {
        return Some(args.get(index..).unwrap_or_default().join(" "));
    }

    if let Some(index) = parse_one_based_index(key) {
        return Some(args.get(index).copied().unwrap_or_default().to_string());
    }

    let (key, alt_text) = split_placeholder_key(key);
    match key {
        "channel.name" => Some(channel.trim_start_matches('#').to_string()),
        "channel" => Some(channel.trim_start_matches('#').to_string()),
        "channel.id" => Some(
            channel_id(channel)
                .await
                .unwrap_or_else(|| alt_text.to_string()),
        ),
        "my.name" => {
            Some(crate::backend::settings::get_username().unwrap_or_else(|| alt_text.to_string()))
        }
        "my.id" => Some(
            current_user_id()
                .await
                .unwrap_or_else(|| alt_text.to_string()),
        ),
        "stream.title" => Some(
            crate::backend::get_stream_info_async(channel)
                .await
                .map(|info| info.title)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| alt_text.to_string()),
        ),
        "stream.game" => Some(
            crate::backend::get_stream_info_async(channel)
                .await
                .and_then(|info| info.game_name)
                .filter(|game| !game.is_empty())
                .unwrap_or_else(|| alt_text.to_string()),
        ),
        _ => None,
    }
}

fn split_placeholder_key(key: &str) -> (&str, &str) {
    key.split_once(';').unwrap_or((key, ""))
}

fn parse_one_based_index(value: &str) -> Option<usize> {
    let parsed = value.parse::<usize>().ok()?;
    parsed.checked_sub(1)
}

async fn channel_id(channel: &str) -> Option<String> {
    let channel = channel.trim_start_matches('#');
    if let Some(id) = crate::backend::twitch_api::get_cached_channel_id(channel) {
        return Some(id);
    }
    crate::backend::twitch_api::get_user_id_cached(channel)
        .await
        .ok()
}

async fn current_user_id() -> Option<String> {
    let username = crate::backend::settings::get_username()?;
    crate::backend::twitch_api::get_user_id_cached(&username)
        .await
        .ok()
}
