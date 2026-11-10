//! Custom chat command model and trigger matching.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// Custom chat command
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomCommand {
    pub id: String,
    pub name: String,
    pub response: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Match a command trigger against raw input, returning the argument tail.
///
/// A custom command only fires when the trigger ends on a word boundary, so
/// `/high` must not run a `/hi` command.
pub fn match_trigger<'a>(command: &CustomCommand, input: &'a str) -> Option<&'a str> {
    let trigger = command.name.trim();
    if trigger.is_empty() || !input.starts_with(trigger) {
        return None;
    }

    let rest = &input[trigger.len()..];
    if rest.is_empty() {
        return Some("");
    }
    if rest.chars().next().is_some_and(char::is_whitespace) {
        return Some(rest.trim_start());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_requires_word_boundary() {
        let command = CustomCommand {
            id: "1".to_string(),
            name: "/hi".to_string(),
            response: "hello".to_string(),
            enabled: true,
        };

        assert_eq!(match_trigger(&command, "/hi there"), Some("there"));
        assert_eq!(match_trigger(&command, "/high"), None);
    }
}
