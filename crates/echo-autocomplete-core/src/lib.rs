//! GPUI-free autocomplete context, command argument, and replacement behavior.

use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

static COMMAND_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([A-Za-z0-9_-]*)$").unwrap());
static EMOTE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":(\w*)$").unwrap());
static MENTION_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@(\w*)$").unwrap());
static TAB_REPLACE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\w+)$").unwrap());

const COMMAND_CHANNEL_QUERY_PREFIX: &str = "channel:";
const COMMAND_MESSAGE_QUERY_PREFIX: &str = "message:";
const COMMAND_PREDICTION_OUTCOME_QUERY_PREFIX: &str = "prediction-outcome:";
const COMMAND_USER_QUERY_PREFIX: &str = "user:";
const COMMAND_VALUE_QUERY_PREFIX: &str = "value:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutocompleteType {
    Emote,
    Mention,
    Command,
    Tab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnterKeyDisposition {
    AcceptSelection,
    SendInput,
}

impl EnterKeyDisposition {
    pub fn should_send_after_accept(self, selection_accepted: bool) -> bool {
        matches!(self, Self::SendInput)
            || matches!(self, Self::AcceptSelection) && !selection_accepted
    }
}

/// Emote suggestions still show for a one-character `:` query, but Enter only
/// accepts one from the second character on, so `:D`/`:P` send as typed.
pub const MIN_EMOTE_ENTER_ACCEPT_QUERY_CHARS: usize = 2;

pub fn enter_key_disposition(
    autocomplete_open: bool,
    suggestion_count: usize,
    ac_type: AutocompleteType,
    query_chars: usize,
) -> EnterKeyDisposition {
    if autocomplete_open
        && suggestion_count > 0
        && (ac_type != AutocompleteType::Emote || query_chars >= MIN_EMOTE_ENTER_ACCEPT_QUERY_CHARS)
    {
        EnterKeyDisposition::AcceptSelection
    } else {
        EnterKeyDisposition::SendInput
    }
}

/// True when accepting `selection` would leave the typed completion unchanged.
/// Mention completion decorates a selected name with a trailing comma or space;
/// those decorations are not part of the typed fragment.
pub fn completion_matches_fragment(fragment: &str, selection: &str) -> bool {
    if fragment.is_empty() {
        return false;
    }

    let selection = selection.trim_end_matches([' ', ',']);
    !selection.is_empty() && selection.to_lowercase() == fragment.to_lowercase()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionContext {
    pub ac_type: AutocompleteType,
    pub query: String,
    pub replacement_range: Range<usize>,
    pub expected_fragment: String,
}

impl Default for CompletionContext {
    fn default() -> Self {
        Self {
            ac_type: AutocompleteType::Emote,
            query: String::new(),
            replacement_range: 0..0,
            expected_fragment: String::new(),
        }
    }
}

pub type CommandOptionSpec = (&'static str, &'static str);
pub type CommandValueSpec = (&'static str, &'static str);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCompletionKind {
    None,
    User,
    Channel,
    UserThenChannel,
    Values(&'static [CommandValueSpec]),
    MessageId,
    PredictionOutcome,
}

pub struct CommandDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub usage: Option<&'static str>,
    pub options: &'static [CommandOptionSpec],
    pub argument_completion: CommandCompletionKind,
}

pub type CommandLookup = fn(&str) -> Option<&'static CommandDescriptor>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandArgumentKind {
    Channel,
    MessageId,
    Option,
    PredictionOutcome,
    User,
    Value,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommandArgumentContext {
    pub command: String,
    pub prefix: String,
    pub kind: CommandArgumentKind,
}

impl CommandArgumentContext {
    pub fn query(&self) -> String {
        match self.kind {
            CommandArgumentKind::Channel => format!(
                "{} {}{}",
                self.command, COMMAND_CHANNEL_QUERY_PREFIX, self.prefix
            ),
            CommandArgumentKind::MessageId => format!(
                "{} {}{}",
                self.command, COMMAND_MESSAGE_QUERY_PREFIX, self.prefix
            ),
            CommandArgumentKind::Option => format!("{} {}", self.command, self.prefix),
            CommandArgumentKind::PredictionOutcome => format!(
                "{} {}{}",
                self.command, COMMAND_PREDICTION_OUTCOME_QUERY_PREFIX, self.prefix
            ),
            CommandArgumentKind::User => format!(
                "{} {}{}",
                self.command, COMMAND_USER_QUERY_PREFIX, self.prefix
            ),
            CommandArgumentKind::Value => format!(
                "{} {}{}",
                self.command, COMMAND_VALUE_QUERY_PREFIX, self.prefix
            ),
        }
    }
}

pub fn command_argument_kind_from_query(query: &str) -> Option<CommandArgumentKind> {
    let (_, argument_query) = query.split_once(' ')?;
    if argument_query.starts_with(COMMAND_CHANNEL_QUERY_PREFIX) {
        Some(CommandArgumentKind::Channel)
    } else if argument_query.starts_with(COMMAND_MESSAGE_QUERY_PREFIX) {
        Some(CommandArgumentKind::MessageId)
    } else if argument_query.starts_with(COMMAND_PREDICTION_OUTCOME_QUERY_PREFIX) {
        Some(CommandArgumentKind::PredictionOutcome)
    } else if argument_query.starts_with(COMMAND_USER_QUERY_PREFIX) {
        Some(CommandArgumentKind::User)
    } else if argument_query.starts_with(COMMAND_VALUE_QUERY_PREFIX) {
        Some(CommandArgumentKind::Value)
    } else if argument_query.starts_with('-') {
        Some(CommandArgumentKind::Option)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CompletionToken<'a> {
    pub before_cursor: &'a str,
    pub fragment: &'a str,
    pub start: usize,
    pub end: usize,
}

pub fn clamp_to_char_boundary(text: &str, cursor_position: usize) -> usize {
    text.floor_char_boundary(cursor_position)
}

pub fn scan_completion_token(text: &str, cursor_position: usize) -> CompletionToken<'_> {
    let cursor = clamp_to_char_boundary(text, cursor_position);
    let before_cursor = &text[..cursor];
    let start = before_cursor
        .char_indices()
        .rev()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let end = text[cursor..]
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(cursor + index))
        .unwrap_or(text.len());
    CompletionToken {
        before_cursor,
        fragment: &text[start..cursor],
        start,
        end,
    }
}

pub fn completion_context(
    text: &str,
    ac_type: AutocompleteType,
    query: String,
    replacement_range: Range<usize>,
) -> Option<CompletionContext> {
    let expected_fragment = text.get(replacement_range.clone())?.to_string();
    Some(CompletionContext {
        ac_type,
        query,
        replacement_range,
        expected_fragment,
    })
}

pub fn is_command_root_query(query: &str) -> bool {
    query
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub fn detect_tab_context(text: &str, cursor_position: usize) -> Option<CompletionContext> {
    let token = scan_completion_token(text, cursor_position);
    if token.fragment.chars().count() < 2
        || token
            .fragment
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, ':' | '/' | '@'))
    {
        return None;
    }
    completion_context(
        text,
        AutocompleteType::Tab,
        token.fragment.to_string(),
        token.start..token.end,
    )
}

pub fn command_subtitle(command: &CommandDescriptor, show_descriptions: bool) -> Option<String> {
    if !show_descriptions {
        return None;
    }
    Some(match command.usage {
        Some(usage) => format!("Usage: {usage} - {}", command.description),
        None => command.description.to_string(),
    })
}

pub fn command_argument_context(
    before_cursor: &str,
    lookup: CommandLookup,
) -> Option<CommandArgumentContext> {
    let after_slash = before_cursor.strip_prefix('/')?;
    let command_end = after_slash.find(char::is_whitespace)?;
    let command = after_slash[..command_end].to_ascii_lowercase();
    let entry = lookup(&command)?;

    if entry.argument_completion == CommandCompletionKind::PredictionOutcome
        && let Some(prefix) =
            command_prediction_outcome_argument(after_slash, command_end, before_cursor)
    {
        return Some(CommandArgumentContext {
            command,
            prefix,
            kind: CommandArgumentKind::PredictionOutcome,
        });
    }
    if !entry.options.is_empty()
        && let Some(prefix) = command_option_argument_prefix(before_cursor)
    {
        return Some(CommandArgumentContext {
            command,
            prefix,
            kind: CommandArgumentKind::Option,
        });
    }

    let context = match entry.argument_completion {
        CommandCompletionKind::User => {
            command_first_argument_prefix(after_slash, command_end, before_cursor)
                .map(|prefix| (CommandArgumentKind::User, prefix))
        }
        CommandCompletionKind::Channel => {
            command_first_argument_prefix(after_slash, command_end, before_cursor)
                .map(|prefix| (CommandArgumentKind::Channel, prefix))
        }
        CommandCompletionKind::UserThenChannel => {
            command_user_then_channel_argument(after_slash, command_end, before_cursor)
        }
        CommandCompletionKind::Values(_) => {
            command_first_argument_prefix(after_slash, command_end, before_cursor)
                .map(|prefix| (CommandArgumentKind::Value, prefix))
        }
        CommandCompletionKind::MessageId => {
            command_first_argument_prefix(after_slash, command_end, before_cursor)
                .map(|prefix| (CommandArgumentKind::MessageId, prefix))
        }
        CommandCompletionKind::PredictionOutcome | CommandCompletionKind::None => None,
    }?;
    Some(CommandArgumentContext {
        command,
        kind: context.0,
        prefix: context.1,
    })
}

pub fn command_argument_token_range(
    before_cursor: &str,
    lookup: CommandLookup,
) -> Option<(usize, usize)> {
    let context = command_argument_context(before_cursor, lookup)?;
    if context.prefix.is_empty() {
        return Some((before_cursor.len(), before_cursor.len()));
    }
    before_cursor.ends_with(&context.prefix).then_some((
        before_cursor.len() - context.prefix.len(),
        before_cursor.len(),
    ))
}

pub fn replace_with_selection(
    text: &str,
    cursor_position: usize,
    ac_type: AutocompleteType,
    selection: &str,
    lookup: CommandLookup,
) -> String {
    let cursor = clamp_to_char_boundary(text, cursor_position);
    let before_cursor = &text[..cursor];
    let after_cursor = &text[cursor..];
    let pattern = match ac_type {
        AutocompleteType::Emote => &*EMOTE_REGEX,
        AutocompleteType::Mention => &*MENTION_REGEX,
        AutocompleteType::Command => {
            if let Some((start, end)) = command_argument_token_range(before_cursor, lookup) {
                let mut output = String::with_capacity(text.len() + selection.len() + 1);
                output.push_str(&before_cursor[..start]);
                output.push_str(selection);
                output.push(' ');
                output.push_str(&before_cursor[end..]);
                output.push_str(after_cursor);
                return output;
            }
            &*COMMAND_REGEX
        }
        AutocompleteType::Tab => &*TAB_REPLACE_REGEX,
    };
    if let Some(matched) = pattern.find(before_cursor) {
        let mut output = String::with_capacity(text.len() + selection.len() + 1);
        output.push_str(&before_cursor[..matched.start()]);
        output.push_str(selection);
        output.push(' ');
        output.push_str(&before_cursor[matched.end()..]);
        output.push_str(after_cursor);
        return output;
    }
    text.to_string()
}

pub fn command_argument_value_for_replacement(value: &str) -> String {
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\\')
    {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

fn command_prediction_outcome_argument(
    after_slash: &str,
    command_end: usize,
    before_cursor: &str,
) -> Option<String> {
    let arg_tail = after_slash[command_end..].trim_start();
    if arg_tail.is_empty() {
        return None;
    }
    let ends_with_whitespace = before_cursor
        .chars()
        .last()
        .is_some_and(|ch| ch.is_whitespace());
    let mut parts = arg_tail.split_whitespace();
    let option = parts.next()?;
    if option != "--choice" && option != "-c" {
        return None;
    }
    match (parts.next(), parts.next(), ends_with_whitespace) {
        (None, None, true) => Some(String::new()),
        (Some(prefix), None, false) => Some(prefix.to_string()),
        _ => None,
    }
}

fn command_option_argument_prefix(before_cursor: &str) -> Option<String> {
    let current_token = current_token_before_cursor(before_cursor);
    if current_token.is_empty() || current_token.starts_with("--") || current_token == "-" {
        Some(current_token.to_string())
    } else {
        None
    }
}

fn command_first_argument_prefix(
    after_slash: &str,
    command_end: usize,
    before_cursor: &str,
) -> Option<String> {
    let arg_tail = after_slash[command_end..].trim_start();
    if arg_tail.is_empty() {
        return Some(String::new());
    }
    if before_cursor
        .chars()
        .last()
        .is_some_and(|ch| ch.is_whitespace())
    {
        return None;
    }
    let mut parts = arg_tail.split_whitespace();
    let first_argument = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(first_argument.to_string())
}

fn command_user_then_channel_argument(
    after_slash: &str,
    command_end: usize,
    before_cursor: &str,
) -> Option<(CommandArgumentKind, String)> {
    let arg_tail = after_slash[command_end..].trim_start();
    if arg_tail.is_empty() {
        return Some((CommandArgumentKind::User, String::new()));
    }
    let ends_with_whitespace = before_cursor
        .chars()
        .last()
        .is_some_and(|ch| ch.is_whitespace());
    let mut parts = arg_tail.split_whitespace();
    let first_argument = parts.next()?;
    let second_argument = parts.next();
    if parts.next().is_some() {
        return None;
    }
    match (second_argument, ends_with_whitespace) {
        (None, false) => Some((CommandArgumentKind::User, first_argument.to_string())),
        (None, true) => Some((CommandArgumentKind::Channel, String::new())),
        (Some(second), false) => Some((CommandArgumentKind::Channel, second.to_string())),
        (Some(_), true) => None,
    }
}

fn current_token_before_cursor(before_cursor: &str) -> &str {
    if before_cursor
        .chars()
        .last()
        .is_some_and(|ch| ch.is_whitespace())
    {
        ""
    } else {
        before_cursor.split_whitespace().last().unwrap_or("")
    }
}

#[cfg(test)]
mod tests;
