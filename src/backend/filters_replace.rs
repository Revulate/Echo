//! Ignore-phrase "replace" mode.
//!
//! An ignore phrase either blocks the whole message or REPLACES the matched
//! text with a user-provided string (censor slurs/spam while keeping the rest
//! of the message readable). Echo models the block mode as a `Hide` filter;
//! this module implements the replace mode.
//!
//! ## Where replacement happens
//! Replacement runs ONCE, at message BUILD time, BEFORE `parse_to_slots`, in the
//! PRIVMSG / whisper / API-history-recap paths. The rewritten text is stored as
//! `ChatMessage.content`, so every downstream consumer (tokenization, similarity
//! detection, configured highlight rules, search, local history persistence) sees
//! the replaced text. (Self-mention highlight is evaluated slightly earlier and reads
//! the pre-replacement text; that is a highlight/notification concern, not the
//! displayed message body.)
//!
//! ## Keeping Twitch emote positions correct
//! Twitch emote positions in the IRC `emotes` tag are CHAR offsets into the
//! original message text. Rewriting the content shifts those offsets. This module
//! remaps the parsed emote position map across the replacements:
//!   * an emote overlapping any replaced span is DROPPED,
//!   * an emote after a replacement is shifted by the cumulative char-length delta.
//!
//! The caller then re-serializes the remapped map back into a Twitch emotes tag
//! (`serialize_emote_map`) and stores it with `reply_emote_offset = 0`, so the
//! (content, tag) pair is self-consistent. Every later parse — the coalesced
//! channel retokenize and the local history-cache reload — re-parses that baked
//! pair and reproduces the same slots with NO replace-specific handling.

use regex::{Regex, RegexBuilder};
use rustc_hash::FxHashMap;
use std::sync::LazyLock;
use std::sync::RwLock;

use crate::backend::lock_utils::RwLockExt;
use crate::backend::settings::{self, FilterAction};

pub type TwitchEmotePositionMap = FxHashMap<usize, (String, usize)>;
pub type ReplacementResult = (String, TwitchEmotePositionMap);

/// A single compiled replace rule. Both literal-substring and regex patterns are
/// represented as a case-insensitive `Regex` (literal patterns are `regex::escape`d
/// first) so match offsets are always correct byte offsets into the original text.
struct CompiledReplaceRule {
    regex: Regex,
    replacement: String,
}

struct ReplaceCache {
    /// `settings::filters_version()` the compiled rules were built from. Starts at
    /// 0 while `filters_version()` starts at 1, so the first call always rebuilds.
    version: u64,
    rules: Vec<CompiledReplaceRule>,
}

static REPLACE_CACHE: LazyLock<RwLock<ReplaceCache>> = LazyLock::new(|| {
    RwLock::new(ReplaceCache {
        version: 0,
        rules: Vec::new(),
    })
});

/// Apply all enabled Replace-action filters to `content`, remapping `emotes`
/// (char-indexed Twitch emote positions) across the rewrites.
///
/// Returns `None` when there are no replace rules or nothing matched, so the hot
/// path stays allocation-free for the overwhelmingly common no-rule case.
pub fn apply_ignore_replacements(
    content: &str,
    emotes: &TwitchEmotePositionMap,
) -> Option<ReplacementResult> {
    if content.is_empty() {
        return None;
    }

    let current = settings::filters_version();

    {
        let cache = REPLACE_CACHE.read_or_recover();
        if cache.version == current {
            if cache.rules.is_empty() {
                return None;
            }
            return apply_replacements_with_rules(content, emotes, &cache.rules);
        }
    }

    {
        let mut cache = REPLACE_CACHE.write_or_recover();
        if cache.version != current {
            cache.rules = compile_replace_rules();
            cache.version = current;
        }
    }

    let cache = REPLACE_CACHE.read_or_recover();
    if cache.rules.is_empty() {
        return None;
    }
    apply_replacements_with_rules(content, emotes, &cache.rules)
}

/// Re-serialize a (remapped) emote position map into the IRC `emotes` tag format
/// (`id:start-end` per span, end inclusive, `/`-joined). Returns `None` for an
/// empty map. Output is sorted for determinism.
pub fn serialize_emote_map(emotes: &TwitchEmotePositionMap) -> Option<String> {
    if emotes.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = emotes
        .iter()
        .map(|(start, (id, end))| format!("{id}:{start}-{}", end.saturating_sub(1)))
        .collect();
    parts.sort();
    Some(parts.join("/"))
}

fn compile_replace_rules() -> Vec<CompiledReplaceRule> {
    settings::get_filters()
        .into_iter()
        .filter(|f| f.enabled && f.action == FilterAction::Replace)
        .filter_map(|f| {
            let pattern = f.expression.trim();
            if pattern.is_empty() {
                return None;
            }
            let regex = if f.pattern_is_regex {
                RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                    .ok()?
            } else {
                RegexBuilder::new(&regex::escape(pattern))
                    .case_insensitive(true)
                    .build()
                    .ok()?
            };
            Some(CompiledReplaceRule {
                regex,
                replacement: f.replacement.unwrap_or_default(),
            })
        })
        .collect()
}

/// Pure replace+remap over pre-compiled rules. Kept separate from cache handling
/// so the logic is easy to reason about in isolation.
fn apply_replacements_with_rules(
    content: &str,
    emotes: &TwitchEmotePositionMap,
    rules: &[CompiledReplaceRule],
) -> Option<ReplacementResult> {
    if rules.is_empty() {
        return None;
    }

    struct Span<'a> {
        start: usize,
        end: usize,
        repl: &'a str,
    }
    let mut spans: Vec<Span<'_>> = Vec::new();
    for rule in rules {
        for m in rule.regex.find_iter(content) {
            if m.start() == m.end() {
                continue;
            }
            spans.push(Span {
                start: m.start(),
                end: m.end(),
                repl: rule.replacement.as_str(),
            });
        }
    }
    if spans.is_empty() {
        return None;
    }

    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut kept: Vec<Span<'_>> = Vec::with_capacity(spans.len());
    let mut last_end = 0usize;
    for span in spans {
        if span.start >= last_end {
            last_end = span.end;
            kept.push(span);
        }
    }

    let mut replaced = String::with_capacity(content.len());
    let mut cursor = 0usize;
    for span in &kept {
        replaced.push_str(&content[cursor..span.start]);
        replaced.push_str(span.repl);
        cursor = span.end;
    }
    replaced.push_str(&content[cursor..]);

    struct CharSpan {
        char_start: usize,
        char_end: usize,
        delta: isize,
    }
    let char_spans: Vec<CharSpan> = kept
        .iter()
        .map(|span| {
            let char_start = content[..span.start].chars().count();
            let char_end = content[..span.end].chars().count();
            let orig_len = char_end - char_start;
            let repl_len = span.repl.chars().count();
            CharSpan {
                char_start,
                char_end,
                delta: repl_len as isize - orig_len as isize,
            }
        })
        .collect();

    let mut new_emotes =
        TwitchEmotePositionMap::with_capacity_and_hasher(emotes.len(), Default::default());
    for (start, value) in emotes.iter() {
        let start = *start;
        let end = value.1;
        let overlaps = char_spans
            .iter()
            .any(|s| start < s.char_end && s.char_start < end);
        if overlaps {
            continue;
        }
        let delta: isize = char_spans
            .iter()
            .filter(|s| s.char_end <= start)
            .map(|s| s.delta)
            .sum();
        let new_start = (start as isize + delta).max(0) as usize;
        let new_end = (end as isize + delta).max(0) as usize;
        if new_end > new_start {
            new_emotes.insert(new_start, (value.0.clone(), new_end));
        }
    }

    Some((replaced, new_emotes))
}
