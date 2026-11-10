//! Followed channels that are live: the row data, display order, query
//! matching and state copy shared by the quick switcher and the empty-split
//! channel chooser.

use crate::fuzzy::FuzzyQuery;

/// One live stream of a channel the signed-in user follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowedLiveStream {
    pub login: String,
    pub display_name: String,
    pub title: String,
    /// Twitch category (`game_name`); empty when the stream has none.
    pub category: String,
    pub viewer_count: u32,
    /// Stream start as unix seconds; 0 when Twitch sent no usable time.
    pub started_at: i64,
}

/// Where the followed-live list stands for the current account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowedLiveStatus {
    /// Anonymous or signed out: there is no follow list to show.
    SignedOut,
    /// Signed in, but no answer for this account yet.
    Loading,
    /// The last answer for this account; the list may be empty.
    Ready,
    /// Twitch refused the request (401/403), typically a token from before
    /// Echo asked for `user:read:follows`.
    NeedsPermission,
    /// The last attempt failed for another reason and nothing is cached.
    Unavailable,
}

/// Line shown in place of the list while streamer mode hides it.
pub const STREAMER_MODE_NOTICE: &str = "Live channels hidden in streamer mode";

/// Sort streams for display: most viewers first, then by login.
pub fn sort_by_viewers(streams: &mut [FollowedLiveStream]) {
    streams.sort_by(|a, b| {
        b.viewer_count
            .cmp(&a.viewer_count)
            .then_with(|| a.login.cmp(&b.login))
    });
}

/// Indices into `streams` to show for `query`, in display order.
///
/// Streams whose login `exclude` accepts are skipped. An empty query keeps
/// every remaining stream, most viewers first. Otherwise a stream matches when
/// the query fuzzy-matches its login or display name, and the best match comes
/// first, with viewers then login breaking ties.
pub fn matching_streams(
    streams: &[FollowedLiveStream],
    query: &str,
    exclude: impl Fn(&str) -> bool,
) -> Vec<usize> {
    let query = query.trim().trim_start_matches(['#', '@']).trim();
    let by_viewers = |a: &FollowedLiveStream, b: &FollowedLiveStream| {
        b.viewer_count
            .cmp(&a.viewer_count)
            .then_with(|| a.login.cmp(&b.login))
    };

    if query.is_empty() {
        let mut indices: Vec<usize> = (0..streams.len())
            .filter(|&index| !exclude(&streams[index].login))
            .collect();
        indices.sort_by(|&a, &b| by_viewers(&streams[a], &streams[b]));
        return indices;
    }

    let fuzzy = FuzzyQuery::new(query);
    let mut scored: Vec<(usize, f64)> = streams
        .iter()
        .enumerate()
        .filter(|(_, stream)| !exclude(&stream.login))
        .filter_map(|(index, stream)| {
            let login = fuzzy.score(&stream.login, &stream.login.to_lowercase());
            let display = fuzzy.score(&stream.display_name, &stream.display_name.to_lowercase());
            let score = match (login, display) {
                (Some(a), Some(b)) => a.max(b),
                (Some(score), None) | (None, Some(score)) => score,
                (None, None) => return None,
            };
            Some((index, score))
        })
        .collect();
    scored.sort_by(|(a_index, a_score), (b_index, b_score)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| by_viewers(&streams[*a_index], &streams[*b_index]))
    });
    scored.into_iter().map(|(index, _)| index).collect()
}

/// The second row line: `category · title`, or whichever of them is set.
pub fn stream_summary(stream: &FollowedLiveStream) -> String {
    let category = stream.category.trim();
    let title = stream.title.trim();
    match (category.is_empty(), title.is_empty()) {
        (false, false) => format!("{category} · {title}"),
        (false, true) => category.to_string(),
        (true, false) => title.to_string(),
        (true, true) => String::new(),
    }
}

/// The quiet line under a list that shows only part of its matches.
pub fn more_hint(hidden_rows: usize) -> Option<String> {
    (hidden_rows > 0).then(|| format!("{hidden_rows} more — type to filter"))
}

/// The index of the previously selected row in `new`, so a refresh that
/// reorders or replaces rows keeps the same row highlighted when it is
/// still listed.
pub fn carry_selection<T: PartialEq>(
    old: &[T],
    selected: Option<usize>,
    new: &[T],
) -> Option<usize> {
    let row = old.get(selected?)?;
    new.iter().position(|candidate| candidate == row)
}

/// The line to show in the Live now area instead of (or with no) rows.
///
/// `live_total` counts every live followed stream for the account and `shown`
/// the rows left after excluding open channels and applying `query_active`.
/// Returns `None` when rows are shown and nothing else needs saying.
pub fn notice(
    status: FollowedLiveStatus,
    streamer_mode: bool,
    live_total: usize,
    shown: usize,
    query_active: bool,
) -> Option<&'static str> {
    if streamer_mode {
        return Some(STREAMER_MODE_NOTICE);
    }
    match status {
        FollowedLiveStatus::SignedOut => {
            return Some("Sign in to see followed channels that are live");
        }
        FollowedLiveStatus::NeedsPermission => {
            return Some("Sign in again to let Echo see who you follow");
        }
        _ if shown > 0 => return None,
        FollowedLiveStatus::Loading => return Some("Checking who's live…"),
        FollowedLiveStatus::Unavailable => return Some("Couldn't check who's live right now"),
        FollowedLiveStatus::Ready => {}
    }
    Some(if live_total == 0 {
        "No followed channels are live"
    } else if query_active {
        "No live followed channels match"
    } else {
        "Every live followed channel is already open"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(login: &str, display_name: &str, viewers: u32) -> FollowedLiveStream {
        FollowedLiveStream {
            login: login.to_string(),
            display_name: display_name.to_string(),
            title: String::new(),
            category: String::new(),
            viewer_count: viewers,
            started_at: 0,
        }
    }

    #[test]
    fn followed_live_sort_puts_most_viewers_first_then_login() {
        let mut streams = vec![
            stream("beta", "Beta", 10),
            stream("alpha", "Alpha", 10),
            stream("gamma", "Gamma", 500),
        ];
        sort_by_viewers(&mut streams);
        let logins: Vec<_> = streams.iter().map(|s| s.login.as_str()).collect();
        assert_eq!(logins, ["gamma", "alpha", "beta"]);
    }

    #[test]
    fn followed_live_empty_query_skips_excluded_and_orders_by_viewers() {
        let streams = vec![
            stream("small", "Small", 3),
            stream("open", "Open", 9_000),
            stream("big", "Big", 4_000),
        ];
        let shown = matching_streams(&streams, "  ", |login| login == "open");
        assert_eq!(shown, [2, 0]);
    }

    #[test]
    fn followed_live_query_matches_login_or_display_name() {
        let streams = vec![
            stream("xqc", "xQc", 50_000),
            stream("kaicenat", "KaiCenat", 80_000),
            stream("ja_streamer", "日本語", 100),
        ];
        assert_eq!(matching_streams(&streams, "#XQ", |_| false), [0]);
        assert_eq!(matching_streams(&streams, "日本", |_| false), [2]);
        assert!(matching_streams(&streams, "zzz", |_| false).is_empty());
        assert!(matching_streams(&streams, "xqc", |login| login == "xqc").is_empty());
    }

    #[test]
    fn followed_live_query_ranks_better_match_before_viewers() {
        let streams = vec![
            stream("summit1g", "summit1g", 90_000),
            stream("sum", "Sum", 10),
        ];
        assert_eq!(matching_streams(&streams, "sum", |_| false), [1, 0]);
    }

    #[test]
    fn followed_live_summary_joins_category_and_title() {
        let mut row = stream("a", "A", 1);
        assert_eq!(stream_summary(&row), "");
        row.title = " Ranked grind ".to_string();
        assert_eq!(stream_summary(&row), "Ranked grind");
        row.category = "Valorant".to_string();
        assert_eq!(stream_summary(&row), "Valorant · Ranked grind");
        row.title.clear();
        assert_eq!(stream_summary(&row), "Valorant");
    }

    #[test]
    fn followed_live_more_hint_only_for_hidden_rows() {
        assert_eq!(more_hint(0), None);
        assert_eq!(more_hint(12).as_deref(), Some("12 more — type to filter"));
    }

    #[test]
    fn followed_live_carry_selection_follows_the_row() {
        let old = ["a", "b", "c"];
        assert_eq!(carry_selection(&old, Some(1), &["c", "b"]), Some(1));
        assert_eq!(carry_selection(&old, Some(0), &["c", "b"]), None);
        assert_eq!(carry_selection(&old, None, &old), None);
        assert_eq!(carry_selection(&old, Some(7), &old), None);
    }

    #[test]
    fn followed_live_notice_follows_state_precedence() {
        use FollowedLiveStatus::*;
        assert_eq!(notice(Ready, true, 4, 4, false), Some(STREAMER_MODE_NOTICE));
        assert_eq!(
            notice(SignedOut, false, 0, 0, false),
            Some("Sign in to see followed channels that are live")
        );
        assert_eq!(
            notice(NeedsPermission, false, 0, 0, false),
            Some("Sign in again to let Echo see who you follow")
        );
        assert_eq!(
            notice(Loading, false, 0, 0, false),
            Some("Checking who's live…")
        );
        assert_eq!(
            notice(Unavailable, false, 0, 0, false),
            Some("Couldn't check who's live right now")
        );
        assert_eq!(
            notice(Ready, false, 0, 0, false),
            Some("No followed channels are live")
        );
        assert_eq!(
            notice(Ready, false, 3, 0, true),
            Some("No live followed channels match")
        );
        assert_eq!(
            notice(Ready, false, 3, 0, false),
            Some("Every live followed channel is already open")
        );
        assert_eq!(notice(Ready, false, 3, 2, false), None);
    }
}
