//! OAuth token normalization and error-message sanitizing.
//!
//! Twitch returns access tokens under several textual prefixes and reports
//! failures in bodies that can echo a credential back. Normalizing and
//! redacting both stay independent of the transport that produced them.

use serde::Deserialize;

pub fn normalize_oauth_token(token: &str) -> &str {
    let trimmed = token.trim();
    trimmed
        .strip_prefix("oauth:")
        .unwrap_or_else(|| trimmed.strip_prefix("Bearer ").unwrap_or(trimmed))
}

#[derive(Deserialize, Debug)]
struct TwitchOAuthErrorBody {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub fn sanitized_oauth_error(body: String) -> String {
    let message = serde_json::from_str::<TwitchOAuthErrorBody>(&body)
        .ok()
        .and_then(|error| error.message.or(error.error))
        .unwrap_or(body);
    redact_token_like_values(&message)
}

fn redact_token_like_values(value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| {
            let trimmed = part.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
            if trimmed.len() >= 24
                && trimmed
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            {
                part.replace(trimmed, "<redacted>")
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_oauth_token_prefixes() {
        assert_eq!(normalize_oauth_token("oauth:abc"), "abc");
        assert_eq!(normalize_oauth_token("Bearer def"), "def");
        assert_eq!(normalize_oauth_token(" ghi "), "ghi");
    }

    #[test]
    fn sanitized_oauth_error_prefers_json_message() {
        let message = sanitized_oauth_error(
            r#"{"error":"invalid_grant","message":"Invalid refresh token"}"#.to_owned(),
        );

        assert_eq!(message, "Invalid refresh token");
    }

    #[test]
    fn sanitized_oauth_error_redacts_token_like_values() {
        let message = sanitized_oauth_error(
            r#"{"message":"invalid refresh abcdefghijklmnopqrstuvwxyz123456"}"#.to_owned(),
        );

        assert_eq!(message, "invalid refresh <redacted>");
    }
}
