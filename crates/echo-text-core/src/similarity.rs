//! Message similarity detection using Levenshtein distance.
//!
//! Implements duplicate/similar message detection with:
//! - Configurable similarity threshold
//! - Same-user-only mode
//! - Time-based filtering (max delay)
//! - Bounded lookback (max messages to check)

use serde::{Deserialize, Serialize};

/// Similarity detection settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SimilaritySettings {
    /// Enable similarity detection
    #[serde(default)]
    pub enabled: bool,

    /// Similarity threshold (0.0 to 1.0, default 0.9)
    /// 1.0 = exact match, 0.0 = completely different
    #[serde(default = "default_threshold")]
    pub threshold: f32,

    /// Hide similar messages completely (vs color them differently)
    #[serde(default)]
    pub hide_similar: bool,

    /// Color similar messages with accent tint
    #[serde(default = "default_true")]
    pub color_similar: bool,

    /// Only check messages from the same user
    #[serde(default)]
    pub same_user_only: bool,

    /// Maximum time between messages to check (seconds)
    #[serde(default = "default_max_delay")]
    pub max_delay_secs: u32,

    /// Maximum number of recent messages to check
    #[serde(default = "default_max_messages")]
    pub max_messages_to_check: u32,
}

fn default_threshold() -> f32 {
    0.9
}
fn default_true() -> bool {
    true
}
fn default_max_delay() -> u32 {
    5
}
fn default_max_messages() -> u32 {
    3
}

impl Default for SimilaritySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: default_threshold(),
            hide_similar: false,
            color_similar: true,
            same_user_only: false,
            max_delay_secs: default_max_delay(),
            max_messages_to_check: default_max_messages(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarityCandidate<'a> {
    pub content: &'a str,
    pub sender: &'a str,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarityMatch {
    pub score: f32,
}

/// Calculate the Levenshtein edit distance between two UTF-8 strings.
///
/// The comparison works on Unicode scalar values rather than bytes so common
/// non-ASCII chat text does not inflate distance by UTF-8 byte length.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();

    if a_chars.is_empty() {
        return b_chars.len();
    }
    if b_chars.is_empty() {
        return a_chars.len();
    }

    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0; b_chars.len() + 1];

    for (i, a_char) in a_chars.iter().enumerate() {
        current[0] = i + 1;
        for (j, b_char) in b_chars.iter().enumerate() {
            let substitution_cost = usize::from(a_char != b_char);
            current[j + 1] = (current[j] + 1)
                .min(previous[j + 1] + 1)
                .min(previous[j] + substitution_cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b_chars.len()]
}

/// Calculate normalized text similarity in the range 0.0..=1.0.
///
/// `1.0` means exact match, `0.0` means no overlap under edit distance.
pub fn calculate_similarity(a: &str, b: &str) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }

    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }

    let distance = levenshtein_distance(a, b);
    ((max_len - distance) as f32 / max_len as f32).clamp(0.0, 1.0)
}

pub fn detect_similarity<'a>(
    settings: &SimilaritySettings,
    content: &str,
    sender: &str,
    timestamp: i64,
    recent_messages: impl IntoIterator<Item = SimilarityCandidate<'a>>,
) -> Option<SimilarityMatch> {
    if !settings.enabled {
        return None;
    }

    let normalized_content = normalize_similarity_text(content);
    if normalized_content.is_empty() {
        return None;
    }

    let max_delay_ms = i64::from(settings.max_delay_secs) * 1000;
    for candidate in recent_messages {
        if settings.same_user_only && !candidate.sender.eq_ignore_ascii_case(sender) {
            continue;
        }

        let age_ms = timestamp.saturating_sub(candidate.timestamp);
        if age_ms < 0 || age_ms > max_delay_ms {
            continue;
        }

        let normalized_candidate = normalize_similarity_text(candidate.content);
        if normalized_candidate.is_empty() {
            continue;
        }

        let score = calculate_similarity(&normalized_content, &normalized_candidate);
        if score >= settings.threshold {
            return Some(SimilarityMatch { score });
        }
    }

    None
}

fn normalize_similarity_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut pending_space = false;

    for ch in value.chars() {
        if ch.is_whitespace() {
            if !normalized.is_empty() {
                pending_space = true;
            }
            continue;
        }

        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }

        for lower in ch.to_lowercase() {
            normalized.push(lower);
        }
    }

    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein_distance("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_one_char_diff() {
        assert_eq!(levenshtein_distance("hello", "helo"), 1);
        assert_eq!(levenshtein_distance("hello", "jello"), 1);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein_distance("", "hello"), 5);
        assert_eq!(levenshtein_distance("hello", ""), 5);
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    #[test]
    fn test_similarity_identical() {
        assert_eq!(calculate_similarity("hello", "hello"), 1.0);
    }

    #[test]
    fn test_similarity_high() {
        let sim = calculate_similarity("hello world", "helo world");
        assert!(sim > 0.9, "Expected >0.9, got {}", sim);
    }

    #[test]
    fn test_similarity_medium() {
        let sim = calculate_similarity("hello", "helo");
        assert!(sim > 0.75 && sim < 0.85, "Expected 0.75-0.85, got {}", sim);
    }

    #[test]
    fn test_similarity_low() {
        let sim = calculate_similarity("hello", "xyz");
        assert!(sim < 0.2, "Expected <0.2, got {}", sim);
    }

    #[test]
    fn test_similarity_empty() {
        assert_eq!(calculate_similarity("", ""), 1.0);
        assert_eq!(calculate_similarity("", "hello"), 0.0);
    }
}
