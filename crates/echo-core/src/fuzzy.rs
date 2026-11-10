//! Fuzzy search utilities using a simplified fzy-style algorithm.
//! Based on https://github.com/jhawthorn/fzy.js/
//!
//! This provides intelligent matching for emote and username autocomplete,
//! with bonuses for word boundaries, consecutive matches, and CamelCase.

// Scoring constants
const SCORE_GAP_LEADING: f64 = -0.005;
const SCORE_GAP_INNER: f64 = -0.01;
const SCORE_MATCH_CONSECUTIVE: f64 = 1.0;
const SCORE_MATCH_WORD: f64 = 0.8; // Word boundary (start, after digit, CamelCase)
const SCORE_MATCH_CAPITAL: f64 = 0.7; // CamelCase transition

/// Whether a non-leading character starts a word in an emote or identifier.
fn is_word_boundary(prev: Option<char>, curr: Option<char>) -> bool {
    let (Some(prev), Some(curr)) = (prev, curr) else {
        return false;
    };

    // After digit (e.g., "pog2" -> "pog2Win")
    if prev.is_ascii_digit() && !curr.is_ascii_digit() {
        return true;
    }

    // CamelCase transition (lowercase to uppercase)
    prev.is_lowercase() && curr.is_uppercase()
}

/// A query prepared once and scored against many targets.
#[derive(Clone, Debug)]
pub struct FuzzyQuery {
    lower: String,
    chars: Vec<char>,
}

impl FuzzyQuery {
    pub fn new(query: &str) -> Self {
        let lower = query.to_lowercase();
        let chars = lower.chars().collect();
        Self { lower, chars }
    }

    /// Score `target`, whose `to_lowercase()` the caller already holds in
    /// `target_lower`. Same result as [`fuzzy_score`], without allocating.
    pub fn score(&self, target: &str, target_lower: &str) -> Option<f64> {
        let mut score = 0.0;
        let mut qi = 0;
        let mut prev_pos: Option<usize> = None;
        let mut original = target.chars();
        let mut prev_original: Option<char> = None;

        for (pos, tc) in target_lower.chars().enumerate() {
            let current_original = original.next();
            if qi < self.chars.len() && tc == self.chars[qi] {
                qi += 1;
                let boundary = if pos == 0 {
                    true
                } else {
                    is_word_boundary(prev_original, current_original)
                };
                let capital = current_original.is_some_and(char::is_uppercase);

                match prev_pos {
                    None => {
                        score += pos as f64 * SCORE_GAP_LEADING;
                    }
                    Some(prev) => {
                        let gap = pos - prev - 1;
                        if gap > 0 {
                            score += gap as f64 * SCORE_GAP_INNER;
                        }
                    }
                }

                if prev_pos.is_some_and(|prev| pos == prev + 1) {
                    score += SCORE_MATCH_CONSECUTIVE;
                } else if boundary {
                    score += SCORE_MATCH_WORD;
                } else if capital {
                    score += SCORE_MATCH_CAPITAL;
                } else {
                    score += 0.5;
                }

                prev_pos = Some(pos);
            }
            prev_original = current_original;
        }
        if qi != self.chars.len() {
            return None;
        }

        // Bonus for shorter names (prefer concise matches)
        score -= target.len() as f64 * 0.001;

        if self.lower == target_lower {
            score += 100.0;
        } else if target_lower.starts_with(&self.lower) {
            score += 10.0;
        }

        Some(score)
    }
}

/// Compute fuzzy match score between a query and target string.
///
/// Returns a score (higher is better) or None if no match.
///
/// # Examples
/// ```
/// use echo_core::fuzzy::fuzzy_score;
/// assert!(fuzzy_score("pog", "PogChamp").is_some());
/// assert!(fuzzy_score("xyz", "PogChamp").is_none());
/// ```
pub fn fuzzy_score(query: &str, target: &str) -> Option<f64> {
    FuzzyQuery::new(query).score(target, &target.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let score = fuzzy_score("pog", "pog").unwrap();
        assert!(score > 50.0); // Exact match bonus
    }

    #[test]
    fn test_prefix_match() {
        let score = fuzzy_score("pog", "PogChamp").unwrap();
        assert!(score > 5.0); // Prefix match bonus
    }

    #[test]
    fn test_no_match() {
        assert!(fuzzy_score("xyz", "PogChamp").is_none());
    }

    #[test]
    fn test_camel_case() {
        // Should match on word boundary
        let score = fuzzy_score("pc", "PogChamp").unwrap();
        assert!(score > 0.0);
    }

    #[test]
    fn test_consecutive_bonus() {
        // "pog" in "PogChamp" has consecutive matches
        let score1 = fuzzy_score("pog", "PogChamp").unwrap();
        // "pcm" has gaps
        let score2 = fuzzy_score("pcm", "PogChamp").unwrap();
        assert!(score1 > score2);
    }
}
