//! Order of the open channels in the quick switcher.

use crate::fuzzy::fuzzy_score;
use std::cmp::Ordering;

/// An open channel as the quick switcher ranks it.
#[derive(Clone, Copy, Debug)]
pub struct QuickSwitchCandidate<'a> {
    pub channel: &'a str,
    pub mention_count: usize,
    pub is_live: bool,
}

/// Indices of `candidates` in display order.
///
/// An empty query lists every channel; otherwise only fuzzy matches are
/// listed, best match first. Ties go to more unread mentions, then the channel
/// switched to most recently (`recent` is newest first, compared
/// case-insensitively), then live channels, then the channel name.
pub fn rank_channels(
    query: &str,
    candidates: &[QuickSwitchCandidate<'_>],
    recent: &[impl AsRef<str>],
) -> Vec<usize> {
    let recent_rank = |channel: &str| {
        recent
            .iter()
            .position(|recent| recent.as_ref().eq_ignore_ascii_case(channel))
            .unwrap_or(usize::MAX)
    };
    let tie_break = |a: &QuickSwitchCandidate<'_>, b: &QuickSwitchCandidate<'_>| {
        b.mention_count
            .cmp(&a.mention_count)
            .then_with(|| recent_rank(a.channel).cmp(&recent_rank(b.channel)))
            .then_with(|| b.is_live.cmp(&a.is_live))
            .then_with(|| a.channel.cmp(b.channel))
    };

    if query.is_empty() {
        let mut ordered: Vec<usize> = (0..candidates.len()).collect();
        ordered.sort_by(|&a, &b| tie_break(&candidates[a], &candidates[b]));
        return ordered;
    }

    let mut matches: Vec<(usize, f64)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, candidate)| {
            fuzzy_score(query, candidate.channel).map(|score| (idx, score))
        })
        .collect();
    matches.sort_by(|&(a, a_score), &(b, b_score)| {
        b_score
            .partial_cmp(&a_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| tie_break(&candidates[a], &candidates[b]))
    });
    matches.into_iter().map(|(idx, _score)| idx).collect()
}
