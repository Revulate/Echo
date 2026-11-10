//! Moderation button configuration
//! Configurable buttons that appear on message hover when user is a moderator

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default)]
pub struct ModButtonContext<'a> {
    pub username: &'a str,
    pub user_display_name: &'a str,
    pub user_id: &'a str,
    pub message_id: &'a str,
    pub message_text: &'a str,
    pub channel: &'a str,
}

/// A configurable moderation button
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModButtonConfig {
    /// Display label (shown on hover tooltip)
    pub label: String,
    /// The command to execute (e.g., "/timeout {user} 60", "/ban {user}")
    /// Supports the placeholders
    /// {user.name}, {msg.id}, {msg.text}, and {channel.name}.
    /// Echo also supports legacy {user} and {msg_id} placeholders.
    pub command: String,
    /// Optional custom icon path (if None, uses default icon)
    pub icon: Option<String>,
    /// Whether this is a destructive action (shown in red)
    pub is_destructive: bool,
}

impl ModButtonConfig {
    /// Resolve the command by replacing placeholders
    pub fn resolve_command(&self, username: &str, message_id: &str) -> String {
        self.resolve_with_context(ModButtonContext {
            username,
            message_id,
            ..ModButtonContext::default()
        })
    }

    /// Resolve the command using the moderation button variables.
    pub fn resolve_for_context(
        &self,
        username: &str,
        message_id: &str,
        message_text: &str,
        channel: &str,
    ) -> String {
        self.resolve_with_context(ModButtonContext {
            username,
            message_id,
            message_text,
            channel,
            ..ModButtonContext::default()
        })
    }

    pub fn resolve_with_context(&self, context: ModButtonContext<'_>) -> String {
        self.command
            .replace("{user.name}", context.username)
            .replace("{user.login}", context.username)
            .replace("{user.display_name}", context.user_display_name)
            .replace("{user.id}", context.user_id)
            .replace("{msg.id}", context.message_id)
            .replace("{msg.text}", context.message_text)
            .replace("{message.id}", context.message_id)
            .replace("{message.text}", context.message_text)
            .replace("{channel.name}", context.channel)
            .replace("{channel.login}", context.channel)
            .replace("{user}", context.username)
            .replace("{msg_id}", context.message_id)
    }

    /// Return true when this button requires message-specific context that is
    /// unavailable from a plain user card.
    pub fn requires_message_context(&self) -> bool {
        self.command.contains("{msg.id}")
            || self.command.contains("{msg_id}")
            || self.command.contains("{msg.text}")
            || self.command.contains("{message.id}")
            || self.command.contains("{message.text}")
    }
}

/// Default mod button configuration
pub fn default_mod_buttons() -> Vec<ModButtonConfig> {
    let mut buttons = vec![ModButtonConfig {
        label: "Delete".to_string(),
        command: "/delete {msg.id}".to_string(),
        icon: None,
        is_destructive: false,
    }];

    buttons.extend(
        [
            ("1s", 1),
            ("10s", 10),
            ("30s", 30),
            ("1m", 60),
            ("5m", 300),
            ("10m", 600),
            ("1h", 3600),
            ("1d", 86400),
        ]
        .into_iter()
        .map(|(label, seconds)| ModButtonConfig {
            label: label.to_string(),
            command: format!("/timeout {{user.name}} {seconds}"),
            icon: None,
            is_destructive: false,
        }),
    );

    buttons.push(ModButtonConfig {
        label: "Ban".to_string(),
        command: "/ban {user.name}".to_string(),
        icon: None,
        is_destructive: true,
    });

    buttons
}
