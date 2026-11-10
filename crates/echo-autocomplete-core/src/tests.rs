use super::*;

const POLL_OPTIONS: &[CommandOptionSpec] = &[("--duration, -d", "Poll duration")];
const COLOR_VALUES: &[CommandValueSpec] = &[("blue", "Blue")];
const COMMERCIAL_VALUES: &[CommandValueSpec] = &[("60", "60 seconds")];
const COMMANDS: &[CommandDescriptor] = &[
    CommandDescriptor {
        name: "timeout",
        description: "Timeout a user",
        usage: Some("/timeout <user> [seconds] [reason]"),
        options: &[],
        argument_completion: CommandCompletionKind::User,
    },
    CommandDescriptor {
        name: "untimeout",
        description: "Remove timeout from user",
        usage: Some("/untimeout <user>"),
        options: &[],
        argument_completion: CommandCompletionKind::User,
    },
    CommandDescriptor {
        name: "popup",
        description: "Open a channel popup",
        usage: None,
        options: &[],
        argument_completion: CommandCompletionKind::Channel,
    },
    CommandDescriptor {
        name: "color",
        description: "Set color",
        usage: None,
        options: &[],
        argument_completion: CommandCompletionKind::Values(COLOR_VALUES),
    },
    CommandDescriptor {
        name: "delete",
        description: "Delete message",
        usage: None,
        options: &[],
        argument_completion: CommandCompletionKind::MessageId,
    },
    CommandDescriptor {
        name: "completeprediction",
        description: "Complete prediction",
        usage: None,
        options: &[],
        argument_completion: CommandCompletionKind::PredictionOutcome,
    },
    CommandDescriptor {
        name: "user",
        description: "Open usercard",
        usage: None,
        options: &[],
        argument_completion: CommandCompletionKind::UserThenChannel,
    },
    CommandDescriptor {
        name: "poll",
        description: "Create poll",
        usage: None,
        options: POLL_OPTIONS,
        argument_completion: CommandCompletionKind::None,
    },
    CommandDescriptor {
        name: "commercial",
        description: "Run commercial",
        usage: None,
        options: &[],
        argument_completion: CommandCompletionKind::Values(COMMERCIAL_VALUES),
    },
];

fn command_entry(name: &str) -> Option<&'static CommandDescriptor> {
    COMMANDS
        .iter()
        .find(|command| command.name.eq_ignore_ascii_case(name))
}

#[test]
fn replace_emote_keeps_unicode_prefix() {
    let input = "hello 😀 :ka";
    let out = replace_with_selection(
        input,
        input.len(),
        AutocompleteType::Emote,
        "Kappa",
        command_entry,
    );
    assert_eq!(out, "hello 😀 Kappa ");
}

#[test]
fn replace_mention_works_at_middle_cursor() {
    let input = "ping @te now";
    let cursor = "ping @te".len();
    let out = replace_with_selection(
        input,
        cursor,
        AutocompleteType::Mention,
        "@tester",
        command_entry,
    );
    assert_eq!(out, "ping @tester  now");
}

#[test]
fn replace_command_treats_selection_as_literal() {
    let input = "/ti";
    let out = replace_with_selection(
        input,
        input.len(),
        AutocompleteType::Command,
        "$1literal",
        command_entry,
    );
    assert_eq!(out, "$1literal ");
}

#[test]
fn replace_hyphenated_command_keeps_command_completion_active() {
    let input = "/clear-chat";
    let out = replace_with_selection(
        input,
        input.len(),
        AutocompleteType::Command,
        "/clear-chat-history",
        command_entry,
    );
    assert_eq!(out, "/clear-chat-history ");
}

#[test]
fn command_argument_context_detects_poll_option_prefix() {
    let context = command_argument_context("/poll --d", command_entry).unwrap();
    assert_eq!(context.command, "poll");
    assert_eq!(context.prefix, "--d");
    assert_eq!(context.kind, CommandArgumentKind::Option);
}

#[test]
fn command_argument_context_detects_user_prefix() {
    let context = command_argument_context("/timeout @te", command_entry).unwrap();
    assert_eq!(context.command, "timeout");
    assert_eq!(context.prefix, "@te");
    assert_eq!(context.kind, CommandArgumentKind::User);
}

#[test]
fn fully_typed_untimeout_argument_remains_a_user_completion() {
    let context = command_argument_context("/untimeout Melonbot_", command_entry).unwrap();
    assert_eq!(context.command, "untimeout");
    assert_eq!(context.prefix, "Melonbot_");
    assert_eq!(context.kind, CommandArgumentKind::User);
    assert_eq!(context.query(), "untimeout user:Melonbot_");
    assert_eq!(
        command_argument_kind_from_query(&context.query()),
        Some(CommandArgumentKind::User)
    );
}

#[test]
fn command_argument_context_detects_channel_prefix() {
    let context = command_argument_context("/popup #fo", command_entry).unwrap();
    assert_eq!(context.command, "popup");
    assert_eq!(context.prefix, "#fo");
    assert_eq!(context.kind, CommandArgumentKind::Channel);
}

#[test]
fn command_argument_context_detects_value_prefix() {
    let context = command_argument_context("/color blue", command_entry).unwrap();
    assert_eq!(context.command, "color");
    assert_eq!(context.prefix, "blue");
    assert_eq!(context.kind, CommandArgumentKind::Value);
}

#[test]
fn command_argument_context_detects_message_id_prefix() {
    let context = command_argument_context("/delete abc", command_entry).unwrap();
    assert_eq!(context.command, "delete");
    assert_eq!(context.prefix, "abc");
    assert_eq!(context.kind, CommandArgumentKind::MessageId);
}

#[test]
fn command_argument_context_detects_prediction_outcome_prefix() {
    let context =
        command_argument_context("/completeprediction --choice blu", command_entry).unwrap();
    assert_eq!(context.command, "completeprediction");
    assert_eq!(context.prefix, "blu");
    assert_eq!(context.kind, CommandArgumentKind::PredictionOutcome);
}

#[test]
fn command_argument_context_detects_empty_prediction_outcome_prefix() {
    let context = command_argument_context("/completeprediction --choice ", command_entry).unwrap();
    assert_eq!(context.command, "completeprediction");
    assert_eq!(context.prefix, "");
    assert_eq!(context.kind, CommandArgumentKind::PredictionOutcome);
}

#[test]
fn command_argument_context_detects_second_channel_prefix() {
    let context = command_argument_context("/user tester #fo", command_entry).unwrap();
    assert_eq!(context.command, "user");
    assert_eq!(context.prefix, "#fo");
    assert_eq!(context.kind, CommandArgumentKind::Channel);
}

#[test]
fn command_argument_context_detects_empty_second_channel_prefix() {
    let context = command_argument_context("/user tester ", command_entry).unwrap();
    assert_eq!(context.command, "user");
    assert_eq!(context.prefix, "");
    assert_eq!(context.kind, CommandArgumentKind::Channel);
}

#[test]
fn command_argument_context_ignores_later_user_command_args() {
    assert!(command_argument_context("/timeout tester ", command_entry).is_none());
}

fn replace_command(input: &str, selection: &str) -> String {
    replace_with_selection(
        input,
        input.len(),
        AutocompleteType::Command,
        selection,
        command_entry,
    )
}

#[test]
fn replace_command_argument_option_keeps_existing_command() {
    assert_eq!(
        replace_command("/poll --d", "--duration"),
        "/poll --duration "
    );
}

#[test]
fn replace_command_argument_user_keeps_existing_command() {
    assert_eq!(
        replace_command("/timeout @te", "tester"),
        "/timeout tester "
    );
}

#[test]
fn replace_command_argument_channel_keeps_existing_command() {
    assert_eq!(replace_command("/popup #fo", "forsen"), "/popup forsen ");
}

#[test]
fn replace_command_argument_value_keeps_existing_command() {
    assert_eq!(replace_command("/commercial 6", "60"), "/commercial 60 ");
}

#[test]
fn replace_command_argument_message_id_keeps_existing_command() {
    assert_eq!(
        replace_command("/delete abc", "abc-def"),
        "/delete abc-def "
    );
}

#[test]
fn replace_command_argument_prediction_outcome_quotes_spaces() {
    assert_eq!(
        replace_command("/completeprediction --choice blu", "\"Blue Team\""),
        "/completeprediction --choice \"Blue Team\" "
    );
}

#[test]
fn replace_second_command_argument_channel_keeps_user() {
    assert_eq!(
        replace_command("/user tester #fo", "forsen"),
        "/user tester forsen "
    );
}

#[test]
fn replace_tab_keeps_suffix_text() {
    let input = "hello ka!";
    let cursor = "hello ka".len();
    let out = replace_with_selection(input, cursor, AutocompleteType::Tab, "Kappa", command_entry);
    assert_eq!(out, "hello Kappa !");
}

#[test]
fn command_subtitle_includes_usage_when_available() {
    let timeout = command_entry("timeout").unwrap();
    assert_eq!(
        command_subtitle(timeout, true).as_deref(),
        Some("Usage: /timeout <user> [seconds] [reason] - Timeout a user")
    );
}

#[test]
fn command_subtitle_respects_description_setting() {
    let timeout = command_entry("timeout").unwrap();
    assert_eq!(command_subtitle(timeout, false), None);
}

#[test]
fn enter_sends_when_open_autocomplete_has_no_suggestions() {
    let disposition = enter_key_disposition(true, 0, AutocompleteType::Emote, 3);
    assert_eq!(disposition, EnterKeyDisposition::SendInput);
    assert!(disposition.should_send_after_accept(false));
}

#[test]
fn enter_accepts_a_current_autocomplete_selection_without_sending() {
    let disposition = enter_key_disposition(true, 1, AutocompleteType::Emote, 2);
    assert_eq!(disposition, EnterKeyDisposition::AcceptSelection);
    assert!(!disposition.should_send_after_accept(true));
}

#[test]
fn enter_sends_one_character_colon_queries_as_typed() {
    let disposition = enter_key_disposition(true, 3, AutocompleteType::Emote, 1);
    assert_eq!(disposition, EnterKeyDisposition::SendInput);
    let mention = enter_key_disposition(true, 3, AutocompleteType::Mention, 1);
    assert_eq!(mention, EnterKeyDisposition::AcceptSelection);
}

#[test]
fn enter_falls_back_to_send_when_deferred_autocomplete_acceptance_is_stale() {
    let disposition = enter_key_disposition(true, 1, AutocompleteType::Command, 0);
    assert!(disposition.should_send_after_accept(false));
}

#[test]
fn completion_match_ignores_trailing_mention_decoration() {
    assert!(completion_matches_fragment("@Bob", "@bob, "));
    assert!(completion_matches_fragment("clear", "CLEAR "));
    assert!(completion_matches_fragment("/clear", "/CLEAR "));
    assert!(completion_matches_fragment("bob", "BOB "));
    assert!(completion_matches_fragment("@Änne", "@änne, "));
    assert!(!completion_matches_fragment("bob", "bobby, "));
    assert!(!completion_matches_fragment("@bob", "@bobby123, "));
    assert!(!completion_matches_fragment("", ", "));
}
