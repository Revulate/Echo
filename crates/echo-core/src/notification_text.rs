//! User-facing text truncation shared by notifications and UI previews.

use std::borrow::Cow;

/// Truncate `message` to at most `max_len` characters, ending in "…" when cut.
///
/// Counting characters rather than bytes keeps multi-byte chat content from
/// being cut mid-codepoint.
pub fn truncate_message(message: &str, max_len: usize) -> String {
    if message.len() <= max_len || message.chars().count() <= max_len {
        return message.to_string();
    }

    let truncated: String = message.chars().take(max_len.saturating_sub(1)).collect();
    format!("{}…", truncated.trim_end())
}

/// Escape text for a freedesktop notification body. Servers that advertise
/// `body-markup` (KDE Plasma, GNOME) parse the body as a small HTML subset, so
/// chat text such as `<3` or `a & b` would otherwise vanish or render as tags.
pub fn escape_notification_markup(text: &str) -> Cow<'_, str> {
    if !text.contains(['&', '<', '>']) {
        return Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(ch),
        }
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_message() {
        assert_eq!(truncate_message("hello", 10), "hello");
        assert_eq!(truncate_message("hello world", 8), "hello w…");
        assert_eq!(truncate_message("hello world", 7), "hello…");
    }

    #[test]
    fn escape_notification_markup_escapes_only_markup_characters() {
        assert!(matches!(
            escape_notification_markup("plain chat text"),
            Cow::Borrowed("plain chat text")
        ));
        assert_eq!(
            escape_notification_markup("<3 you & me > them"),
            "&lt;3 you &amp; me &gt; them"
        );
        assert_eq!(
            escape_notification_markup("<b>not bold</b> &amp;"),
            "&lt;b&gt;not bold&lt;/b&gt; &amp;amp;"
        );
        assert_eq!(escape_notification_markup("émoji 😀 <"), "émoji 😀 &lt;");
    }
}
