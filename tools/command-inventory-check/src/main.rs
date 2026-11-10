use std::collections::BTreeSet;

const COMMANDS_MOD: &str = include_str!("../../../src/backend/commands/mod.rs");

const POLL_OPTIONS: &[&str] = &["--title", "--choice", "--duration", "--points"];
const PREDICTION_OPTIONS: &[&str] = &["--title", "--choice", "--duration"];
const COMPLETE_PREDICTION_OPTIONS: &[&str] = &["--choice", "--index"];
const SUSPICIOUS_USER_COMMANDS: &[&str] = &[
    "automod",
    "lowtrust",
    "monitor",
    "restrict",
    "unmonitor",
    "unrestrict",
];
const LOCAL_COMMANDS: &[&str] = &[
    "clearmessages",
    "copy",
    "debug-args",
    "debug-disable-logfile",
    "debug-enable-logfile",
    "debug-env",
    "debug-relaunch-with-logfile",
    "fakemsg",
    "follow",
    "openurl",
    "popout",
    "popup",
    "r",
    "reply",
    "requests",
    "streamlink",
    "themefile",
    "themefile-export",
    "uptime",
    "user",
    "usercard",
    "w",
];

fn main() {
    let specs = command_specs();
    let dispatch = dispatch_commands();

    let missing_specs: Vec<_> = dispatch.difference(&specs).cloned().collect();
    assert!(
        missing_specs.is_empty(),
        "wired commands missing autocomplete specs: {missing_specs:?}"
    );

    for command in SUSPICIOUS_USER_COMMANDS {
        assert!(
            specs.contains(*command),
            "missing suspicious-user spec: {command}"
        );
        assert!(
            dispatch.contains(*command),
            "missing suspicious-user dispatch: {command}"
        );
    }

    for command in LOCAL_COMMANDS {
        assert!(
            specs.contains(*command),
            "missing local command spec: {command}"
        );
        assert!(
            dispatch.contains(*command),
            "missing local command dispatch: {command}"
        );
    }

    assert_option_block("POLL_COMMAND_OPTIONS", POLL_OPTIONS);
    assert_option_block("PREDICTION_COMMAND_OPTIONS", PREDICTION_OPTIONS);
    assert_option_block(
        "COMPLETE_PREDICTION_COMMAND_OPTIONS",
        COMPLETE_PREDICTION_OPTIONS,
    );
    assert_contains(
        r#"command_spec!(
        "completeprediction""#,
        "completeprediction spec",
    );
    assert_contains(
        "argument_completion: CommandArgumentCompletion::PredictionOutcome",
        "completeprediction outcome completion",
    );

    println!("kind,name,value,checksum");
    print_set_row("command_autocomplete_spec_count", &specs);
    print_set_row("command_dispatch_arm_count", &dispatch);
    print_fixture(
        "command_dispatch_specs_missing",
        &missing_specs.join("|"),
        "",
    );
    print_fixture(
        "command_poll_option_inventory",
        &option_summary("POLL_COMMAND_OPTIONS", POLL_OPTIONS),
        "--title|--choice|--duration|--points",
    );
    print_fixture(
        "command_prediction_option_inventory",
        &option_summary("PREDICTION_COMMAND_OPTIONS", PREDICTION_OPTIONS),
        "--title|--choice|--duration",
    );
    print_fixture(
        "command_completeprediction_option_inventory",
        &option_summary(
            "COMPLETE_PREDICTION_COMMAND_OPTIONS",
            COMPLETE_PREDICTION_OPTIONS,
        ),
        "--choice|--index",
    );
    print_fixture(
        "command_suspicious_user_inventory",
        &presence_summary(&specs, &dispatch, SUSPICIOUS_USER_COMMANDS),
        "automod:spec+dispatch|lowtrust:spec+dispatch|monitor:spec+dispatch|restrict:spec+dispatch|unmonitor:spec+dispatch|unrestrict:spec+dispatch",
    );
    print_fixture(
        "command_local_inventory",
        &presence_summary(&specs, &dispatch, LOCAL_COMMANDS),
        "clearmessages:spec+dispatch|copy:spec+dispatch|debug-args:spec+dispatch|debug-disable-logfile:spec+dispatch|debug-enable-logfile:spec+dispatch|debug-env:spec+dispatch|debug-relaunch-with-logfile:spec+dispatch|fakemsg:spec+dispatch|follow:spec+dispatch|openurl:spec+dispatch|popout:spec+dispatch|popup:spec+dispatch|r:spec+dispatch|reply:spec+dispatch|requests:spec+dispatch|streamlink:spec+dispatch|themefile:spec+dispatch|themefile-export:spec+dispatch|uptime:spec+dispatch|user:spec+dispatch|usercard:spec+dispatch|w:spec+dispatch",
    );
}

fn command_specs() -> BTreeSet<String> {
    let mut specs = BTreeSet::new();
    let mut cursor = COMMANDS_MOD;
    while let Some(start) = cursor.find("command_spec!(") {
        cursor = &cursor[start + "command_spec!(".len()..];
        let end = cursor
            .find("),")
            .or_else(|| cursor.find(")"))
            .expect("command_spec macro call should close");
        let macro_body = &cursor[..end];
        let literals = string_literals(macro_body);
        let name = literals
            .first()
            .expect("command_spec macro should start with command name");
        specs.insert(name.to_string());
        cursor = &cursor[end..];
    }
    specs
}

fn dispatch_commands() -> BTreeSet<String> {
    let Some(match_start) = COMMANDS_MOD.find("match command.as_str()") else {
        panic!("could not find command dispatch match");
    };
    let dispatch_body = &COMMANDS_MOD[match_start..];
    let Some(match_end) = dispatch_body.find("_ =>") else {
        panic!("could not find command passthrough arm");
    };
    dispatch_body[..match_end]
        .lines()
        .filter_map(|line| line.split_once("=>").map(|(patterns, _)| patterns))
        .flat_map(string_literals)
        .map(str::to_string)
        .collect()
}

fn assert_option_block(block_name: &str, required_options: &[&str]) {
    let block = option_block(block_name);
    for option in required_options {
        assert!(
            block.contains(&format!(r#""{option}"#)),
            "{block_name} missing {option}"
        );
    }
}

fn option_summary(block_name: &str, required_options: &[&str]) -> String {
    let block = option_block(block_name);
    required_options
        .iter()
        .filter(|option| block.contains(&format!(r#""{option}"#)))
        .copied()
        .collect::<Vec<_>>()
        .join("|")
}

fn option_block(block_name: &str) -> &'static str {
    let start = COMMANDS_MOD
        .find(block_name)
        .unwrap_or_else(|| panic!("missing option block {block_name}"));
    let tail = &COMMANDS_MOD[start..];
    let end = tail
        .find("];")
        .unwrap_or_else(|| panic!("unterminated option block {block_name}"));
    &tail[..end]
}

fn presence_summary(
    specs: &BTreeSet<String>,
    dispatch: &BTreeSet<String>,
    commands: &[&str],
) -> String {
    commands
        .iter()
        .map(|command| {
            let spec = if specs.contains(*command) {
                "spec"
            } else {
                "missing-spec"
            };
            let route = if dispatch.contains(*command) {
                "dispatch"
            } else {
                "missing-dispatch"
            };
            format!("{command}:{spec}+{route}")
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn assert_contains(needle: &str, label: &str) {
    assert!(
        COMMANDS_MOD.contains(needle),
        "missing expected source marker for {label}"
    );
}

fn string_literals(source: &str) -> Vec<&str> {
    let mut literals = Vec::new();
    let mut start = None;
    let mut escaped = false;

    for (index, ch) in source.char_indices() {
        if let Some(literal_start) = start {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                literals.push(&source[literal_start..index]);
                start = None;
            }
        } else if ch == '"' {
            start = Some(index + ch.len_utf8());
        }
    }

    literals
}

fn print_set_row(name: &str, values: &BTreeSet<String>) {
    let joined = values.iter().cloned().collect::<Vec<_>>().join("|");
    println!("fixture,{name},{},{}", values.len(), checksum(&joined));
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn checksum(value: &str) -> u64 {
    value
        .bytes()
        .fold(0u64, |sum, byte| sum.wrapping_add(byte as u64))
}
