//! Pure message search query parsing and matching.
//!
//! This module is intentionally independent from Echo's chat model and GPUI.

use regex::{Regex, RegexBuilder};
use std::borrow::Cow;
use std::fmt;

pub trait SearchMessage {
    fn content(&self) -> &str;
    fn searchable_text(&self) -> Option<Cow<'_, str>> {
        None
    }
    fn sender(&self) -> &str;
    fn display_name(&self) -> &str;
    fn channel(&self) -> &str;
    fn has_link(&self) -> bool;
    fn has_emote(&self) -> bool {
        false
    }
    fn has_emoji(&self) -> bool {
        false
    }
    fn has_mention(&self) -> bool {
        false
    }
    fn has_any_badge(&self) -> bool {
        false
    }
    fn has_badge(&self, badge: &str) -> bool;
    fn has_subtier(&self, tier: &str) -> bool;
    fn has_type(&self, value: &str) -> bool;
}

#[derive(Clone, Debug)]
pub struct SearchQuery {
    terms: Vec<String>,
    filters: Vec<QueryFilter>,
    regex_terms: Vec<Regex>,
    whole_regex: Option<Regex>,
    case_sensitive: bool,
}

#[derive(Clone, Debug)]
struct QueryFilter {
    kind: FilterKind,
    values: Vec<String>,
    negated: bool,
}

#[derive(Clone, Debug)]
enum FilterKind {
    From,
    Has,
    In,
    Is,
    Regex(Regex),
    Badge,
    SubTier,
}

#[derive(Debug, Clone)]
pub enum SearchQueryError {
    InvalidRegex { pattern: String, error: String },
}

impl fmt::Display for SearchQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchQueryError::InvalidRegex { pattern, error } => {
                write!(f, "Invalid regex `{pattern}`: {error}")
            }
        }
    }
}

impl std::error::Error for SearchQueryError {}

pub fn compile_search_query(input: &str, use_regex: bool) -> Result<SearchQuery, SearchQueryError> {
    compile_search_query_with_case(input, use_regex, false)
}

pub fn compile_search_query_with_case(
    input: &str,
    use_regex: bool,
    case_sensitive: bool,
) -> Result<SearchQuery, SearchQueryError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(SearchQuery {
            terms: Vec::new(),
            filters: Vec::new(),
            regex_terms: Vec::new(),
            whole_regex: None,
            case_sensitive,
        });
    }

    let tokens = tokenize(trimmed);
    let mut raw_terms = Vec::new();
    let mut filters = Vec::new();

    for token in tokens {
        match parse_filter_token(&token, case_sensitive)? {
            Some(filter) => filters.push(filter),
            None => raw_terms.push(normalize_text_value(&token)),
        }
    }

    let whole_regex = if use_regex && filters.is_empty() {
        Some(compile_regex(trimmed, !case_sensitive)?)
    } else {
        None
    };

    let regex_terms = if use_regex && whole_regex.is_none() && !raw_terms.is_empty() {
        raw_terms
            .iter()
            .map(|term| compile_regex(term, !case_sensitive))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let terms = if use_regex {
        Vec::new()
    } else if case_sensitive {
        raw_terms
    } else {
        raw_terms
            .into_iter()
            .map(|term| term.to_ascii_lowercase())
            .collect()
    };

    Ok(SearchQuery {
        terms,
        filters,
        regex_terms,
        whole_regex,
        case_sensitive,
    })
}

impl SearchQuery {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.filters.is_empty()
            && self.regex_terms.is_empty()
            && self.whole_regex.is_none()
    }

    pub fn matches<M: SearchMessage + ?Sized>(&self, msg: &M) -> bool {
        if self.is_empty() {
            return true;
        }

        if let Some(regex) = &self.whole_regex
            && !regex_matches_message(regex, msg)
        {
            return false;
        }

        for term in &self.terms {
            if !message_contains_text(msg, term, self.case_sensitive) {
                return false;
            }
        }

        for regex in &self.regex_terms {
            if !regex_matches_message(regex, msg) {
                return false;
            }
        }

        for filter in &self.filters {
            let matched = filter_matches(filter, msg);
            if filter.negated == matched {
                return false;
            }
        }

        true
    }
}

fn parse_filter_token(
    token: &str,
    case_sensitive: bool,
) -> Result<Option<QueryFilter>, SearchQueryError> {
    let (negated, body) = token
        .strip_prefix('!')
        .or_else(|| token.strip_prefix('-'))
        .map(|body| (true, body))
        .unwrap_or((false, token));

    let Some((name, value)) = body.split_once(':') else {
        return Ok(None);
    };

    let name = name.to_ascii_lowercase();
    let values = split_filter_values(value);
    if values.is_empty() {
        return Ok(None);
    }

    let kind = match name.as_str() {
        "from" | "user" | "author" => FilterKind::From,
        "has" => FilterKind::Has,
        "in" | "channel" => FilterKind::In,
        "is" => FilterKind::Is,
        "regex" => {
            let pattern = normalize_text_value(value);
            FilterKind::Regex(compile_regex(&pattern, !case_sensitive)?)
        }
        "badge" | "badges" => FilterKind::Badge,
        "subtier" | "tier" => FilterKind::SubTier,
        _ => return Ok(None),
    };

    Ok(Some(QueryFilter {
        kind,
        values,
        negated,
    }))
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            if matches!(ch, '"' | '\\') {
                current.push(ch);
            } else {
                current.push('\\');
                current.push(ch);
            }
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn split_filter_values(value: &str) -> Vec<String> {
    let normalized = normalize_text_value(value);
    normalized
        .split(',')
        .map(normalize_text_value)
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_text_value(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn compile_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, SearchQueryError> {
    RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| SearchQueryError::InvalidRegex {
            pattern: pattern.to_string(),
            error: error.to_string(),
        })
}

fn filter_matches<M: SearchMessage + ?Sized>(filter: &QueryFilter, msg: &M) -> bool {
    match &filter.kind {
        FilterKind::From => any_value_matches(&filter.values, |value| {
            equals_ignore_ascii_case(msg.sender(), value)
                || equals_ignore_ascii_case(msg.display_name(), value)
        }),
        FilterKind::Has => any_value_matches(&filter.values, |value| match value {
            "badge" | "badges" => msg.has_any_badge(),
            "emote" | "emotes" => msg.has_emote(),
            "emoji" | "emojis" => msg.has_emoji(),
            "image" | "images" => msg.has_emote() || msg.has_emoji(),
            "link" | "links" | "url" => msg.has_link(),
            "mention" | "mentions" => msg.has_mention(),
            "reply" | "replies" => msg.has_type("reply"),
            _ => false,
        }),
        FilterKind::In => any_value_matches(&filter.values, |value| {
            channel_matches(msg.channel(), value)
        }),
        FilterKind::Is => any_value_matches(&filter.values, |value| msg.has_type(value)),
        FilterKind::Regex(regex) => regex_matches_message(regex, msg),
        FilterKind::Badge => any_value_matches(&filter.values, |value| msg.has_badge(value)),
        FilterKind::SubTier => any_value_matches(&filter.values, |value| msg.has_subtier(value)),
    }
}

fn any_value_matches(values: &[String], matches: impl FnMut(&str) -> bool) -> bool {
    values
        .iter()
        .map(|value| value.trim().trim_start_matches('@'))
        .any(matches)
}

fn channel_matches(channel: &str, value: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#');
    let value = value.trim().trim_start_matches('#');
    equals_ignore_ascii_case(channel, value)
}

#[cfg(test)]
fn badge_key_name_and_value(value: &str) -> (&str, Option<&str>) {
    value
        .split_once('/')
        .or_else(|| value.split_once(':'))
        .map(|(name, version)| (name, Some(version)))
        .unwrap_or((value, None))
}

#[cfg(test)]
fn is_lead_moderator_badge_name(value: &str) -> bool {
    equals_ignore_ascii_case(value, "lead_moderator")
        || equals_ignore_ascii_case(value, "lead-moderator")
}

#[cfg(test)]
fn badge_name_matches(left: &str, right: &str) -> bool {
    equals_ignore_ascii_case(left, right)
        || (is_lead_moderator_badge_name(left) && is_lead_moderator_badge_name(right))
}

#[cfg(test)]
fn badge_key_matches(stored: &str, expected: &str) -> bool {
    badge_name_matches(stored, expected)
        || badge_name_matches(badge_key_name_and_value(stored).0, expected)
}

fn message_contains_text<M: SearchMessage + ?Sized>(
    msg: &M,
    needle: &str,
    case_sensitive: bool,
) -> bool {
    let searchable_text = msg.searchable_text();
    let content = searchable_text.as_deref().unwrap_or_else(|| msg.content());
    if case_sensitive {
        content.contains(needle)
            || msg.sender().contains(needle)
            || msg.display_name().contains(needle)
    } else {
        contains_ignore_ascii_case(content, needle)
            || contains_ignore_ascii_case(msg.sender(), needle)
            || contains_ignore_ascii_case(msg.display_name(), needle)
    }
}

fn regex_matches_message<M: SearchMessage + ?Sized>(regex: &Regex, msg: &M) -> bool {
    let searchable_text = msg.searchable_text();
    let content = searchable_text.as_deref().unwrap_or_else(|| msg.content());
    regex.is_match(content) || regex.is_match(msg.sender()) || regex.is_match(msg.display_name())
}

#[inline]
#[doc(hidden)]
pub fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }

    if !haystack.is_ascii() || !needle_lower.is_ascii() {
        return haystack
            .to_lowercase()
            .contains(&needle_lower.to_lowercase());
    }

    let haystack = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    if needle.len() > haystack.len() {
        return false;
    }

    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.to_ascii_lowercase() == *right)
    })
}

#[inline]
#[doc(hidden)]
pub fn equals_ignore_ascii_case(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestMessage {
        channel: &'static str,
        sender: &'static str,
        display_name: &'static str,
        content: &'static str,
        has_link: bool,
        has_emote: bool,
        has_emoji: bool,
        has_mention: bool,
        badges: Vec<&'static str>,
        types: Vec<&'static str>,
    }

    impl SearchMessage for TestMessage {
        fn content(&self) -> &str {
            self.content
        }

        fn sender(&self) -> &str {
            self.sender
        }

        fn display_name(&self) -> &str {
            self.display_name
        }

        fn channel(&self) -> &str {
            self.channel
        }

        fn has_link(&self) -> bool {
            self.has_link
        }

        fn has_emote(&self) -> bool {
            self.has_emote
        }

        fn has_emoji(&self) -> bool {
            self.has_emoji
        }

        fn has_mention(&self) -> bool {
            self.has_mention
        }

        fn has_any_badge(&self) -> bool {
            !self.badges.is_empty()
        }

        fn has_badge(&self, badge: &str) -> bool {
            self.badges
                .iter()
                .any(|value| badge_key_matches(value, badge))
        }

        fn has_subtier(&self, tier: &str) -> bool {
            self.badges.iter().any(|value| {
                let (name, Some(stored_tier)) = badge_key_name_and_value(value) else {
                    return false;
                };
                equals_ignore_ascii_case(name, "subscriber")
                    && (equals_ignore_ascii_case(stored_tier, tier)
                        || match tier {
                            "1" => stored_tier.starts_with('1'),
                            "2" => stored_tier.starts_with('2'),
                            "3" => stored_tier.starts_with('3'),
                            _ => false,
                        })
            })
        }

        fn has_type(&self, value: &str) -> bool {
            self.types
                .iter()
                .any(|stored| equals_ignore_ascii_case(stored, value))
        }
    }

    fn message(content: &'static str) -> TestMessage {
        TestMessage {
            channel: "alice",
            sender: "bob",
            display_name: "Bob",
            content,
            badges: vec!["moderator:1", "subscriber:3"],
            ..TestMessage::default()
        }
    }

    #[test]
    fn parses_has_and_from_filters_with_free_text() {
        let msg = TestMessage {
            has_link: true,
            ..message("merge this https://example.com")
        };

        let query = compile_search_query("has:link from:bob merge", false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn supports_negated_filters() {
        let msg = message("merge this");
        let query = compile_search_query("!from:carol merge", false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn supports_regex_filter() {
        let msg = message("gachibass");
        let query = compile_search_query(r#"regex:"^gachi\w+$""#, false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn supports_reply_type() {
        let msg = TestMessage {
            types: vec!["reply"],
            ..message("reply body")
        };
        let query = compile_search_query("is:reply body", false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn supports_hash_prefixed_channel_filters() {
        let msg = message("channel scoped");
        let query = compile_search_query("in:#alice scoped", false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn supports_has_content_filters() {
        let msg = TestMessage {
            has_emote: true,
            has_mention: true,
            ..message("@alice Kappa")
        };
        let query = compile_search_query("has:emote has:mention has:badge", false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn defaults_to_case_insensitive_text_terms() {
        let msg = message("MixedCase Body");
        let query = compile_search_query("mixedcase", false).unwrap();

        assert!(query.matches(&msg));
    }

    #[test]
    fn supports_case_sensitive_text_terms() {
        let msg = message("MixedCase Body");
        let query = compile_search_query_with_case("mixedcase", false, true).unwrap();

        assert!(!query.matches(&msg));
    }

    #[test]
    fn supports_case_sensitive_regex() {
        let msg = message("MixedCase Body");
        let query = compile_search_query_with_case("mixedcase", true, true).unwrap();

        assert!(!query.matches(&msg));
    }
}
