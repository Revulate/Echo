//! Paced reveal of live chat rows.
//!
//! Twitch's chat edge does not deliver a busy channel's messages one by one:
//! depending on load it sends a few every ~100 ms, or 60-85 at once every
//! ~3 s with nothing in between. Shown as they arrive, the second pattern makes
//! a busy chat jump in blocks instead of moving. The pacer holds the newest
//! rows of a bottom-following view and releases them at the channel's recent
//! arrival rate, so a burst plays out over the gap before the next one.
//!
//! The pacer only decides how many leading rows of the current message list
//! are visible. It never reorders, drops or edits rows; the caller shows that
//! prefix of its list. A steady stream releases immediately because its rows
//! accrue release allowance as fast as they arrive. No row is held longer
//! than [`MAX_HOLD`], and rows the caller marks urgent (the user's own
//! messages, moderation notices) release at once together with every row
//! before them, so order is kept.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest a row may be held back.
pub const MAX_HOLD: Duration = Duration::from_millis(2_500);
/// Window, ending at the newest arrival, over which the arrival rate is
/// measured. Spans at least one whole burst cycle (bursts were observed
/// every ~3 s), so the rate is the cycle's average.
pub const RATE_WINDOW: Duration = Duration::from_secs(6);
/// Arrivals this close to the newest one count as the same batch. A burst
/// reaches the view in a few pieces milliseconds apart; counting the earlier
/// pieces as past traffic would inflate the rate.
pub const BATCH_GAP: Duration = Duration::from_millis(50);
/// Shortest wake interval the pacer asks for, about one frame at 120-144 Hz.
pub const MIN_WAKE: Duration = Duration::from_millis(6);
/// Longest wake interval while rows are held, so a stale rate estimate is
/// corrected promptly.
pub const MAX_WAKE: Duration = Duration::from_millis(250);

/// What the caller should show after [`RevealPacer::pace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaceOutcome {
    /// Number of leading rows of the list that are visible.
    pub visible: usize,
    /// When to call `pace` again, if rows are still held.
    pub next_wake: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct RevealPacer {
    /// Id of the newest visible row. `None` until the first call, which
    /// shows the whole list (startup history is never paced).
    tail: Option<Arc<str>>,
    /// Arrival time of each held row, oldest first. Held rows are exactly the
    /// rows after `tail`.
    held: VecDeque<Instant>,
    /// Arrival times within `RATE_WINDOW`, for the release rate.
    arrivals: VecDeque<Instant>,
    /// Fractional rows already earned toward the next release.
    allowance: f64,
    last_tick: Option<Instant>,
}

impl RevealPacer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of rows currently held back.
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Show every row and forget held state (pacing off, view not following
    /// live, hidden tab, or a list the pacer cannot relate to the last one).
    pub fn reveal_all<T>(&mut self, rows: &[T], id: impl Fn(&T) -> &Arc<str>) -> usize {
        self.tail = rows.last().map(|row| Arc::clone(id(row)));
        self.held.clear();
        self.allowance = self.allowance.min(1.0);
        rows.len()
    }

    /// Decide how many leading rows of `rows` are visible at `now`.
    ///
    /// `id` returns a row's stable id; `urgent` marks rows that must show at
    /// once (with everything before them).
    pub fn pace<T>(
        &mut self,
        rows: &[T],
        now: Instant,
        id: impl Fn(&T) -> &Arc<str>,
        urgent: impl Fn(&T) -> bool,
    ) -> PaceOutcome {
        let dt = self
            .last_tick
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last))
            .min(MAX_HOLD);
        self.last_tick = Some(now);

        let Some(tail_index) = self.tail.as_ref().and_then(|tail| {
            rows.iter()
                .rposition(|row| id(row).as_ref() == tail.as_ref())
        }) else {
            return PaceOutcome {
                visible: self.reveal_all(rows, &id),
                next_wake: None,
            };
        };

        // Allowance only builds while rows are held; an idle gap earns at
        // most the next row, or a burst after a pause would show at once.
        let was_holding = !self.held.is_empty();

        // Rows after the tail are held. New ones arrived now; rows that
        // vanished after the tail (rare in-place removals) drop their oldest
        // stamps so the counts stay aligned.
        let after = rows.len() - tail_index - 1;
        while self.held.len() > after {
            self.held.pop_front();
        }
        while self.held.len() < after {
            self.held.push_back(now);
            self.arrivals.push_back(now);
        }
        // The window ends at the newest arrival, not at `now`, so the rate
        // holds steady while a burst drains instead of falling to zero once
        // the previous burst ages out.
        if let Some(&newest) = self.arrivals.back() {
            while self
                .arrivals
                .front()
                .is_some_and(|&at| newest.saturating_duration_since(at) > RATE_WINDOW)
            {
                self.arrivals.pop_front();
            }
        }

        let mut visible = tail_index + 1;
        if let Some(last_urgent) = rows[visible..].iter().rposition(&urgent) {
            self.release(last_urgent + 1);
            visible += last_urgent + 1;
        }

        // Drain at the measured rate, but fast enough that the oldest held
        // row is out by MAX_HOLD (a flat floor would let the tail of a long
        // burst go overdue and dump at once). With no rate yet, after a quiet
        // spell or at startup, there is nothing to pace against: show it all.
        let measured = self.rate();
        if measured <= 0.0 {
            let held = self.held.len();
            self.release(held);
            self.allowance = self.allowance.min(1.0);
            return self.finish(rows, visible + held, &id, None);
        }
        let remaining = self.held.front().map_or(MAX_HOLD, |&oldest| {
            MAX_HOLD.saturating_sub(now.saturating_duration_since(oldest))
        });
        let rate = measured.max(self.held.len() as f64 / remaining.as_secs_f64().max(0.05));
        if was_holding {
            self.allowance += rate * dt.as_secs_f64();
        } else {
            self.allowance = (self.allowance + rate * dt.as_secs_f64()).min(1.0);
        }
        if self.held.is_empty() {
            self.allowance = self.allowance.min(1.0);
            return self.finish(rows, visible, &id, None);
        }
        let mut release = (self.allowance.floor() as usize).min(self.held.len());
        let overdue = self
            .held
            .iter()
            .take_while(|&&at| now.saturating_duration_since(at) >= MAX_HOLD)
            .count();
        release = release.max(overdue);
        self.allowance = (self.allowance - release as f64).max(0.0);
        self.release(release);
        visible += release;

        let next_wake = if self.held.is_empty() {
            None
        } else {
            let until_allowance = Duration::from_secs_f64(((1.0 - self.allowance) / rate).max(0.0));
            let until_overdue =
                MAX_HOLD.saturating_sub(now.saturating_duration_since(self.held[0]));
            Some(until_allowance.min(until_overdue).clamp(MIN_WAKE, MAX_WAKE))
        };
        self.finish(rows, visible, &id, next_wake)
    }

    /// Rows per second, from the arrivals before the newest batch (see
    /// [`BATCH_GAP`]) over the
    /// time they took to arrive. Counting the batch that just landed against
    /// the gap before it would double a bursty channel's rate and play each
    /// burst out in half the gap. Zero until two batches half a second apart
    /// have been seen; the MAX_HOLD floor drains rows meanwhile.
    fn rate(&self) -> f64 {
        let (Some(&oldest), Some(&newest)) = (self.arrivals.front(), self.arrivals.back()) else {
            return 0.0;
        };
        let span = newest.saturating_duration_since(oldest);
        if span < Duration::from_millis(500) {
            return 0.0;
        }
        let before_newest = self
            .arrivals
            .iter()
            .take_while(|&&at| newest.saturating_duration_since(at) > BATCH_GAP)
            .count();
        before_newest as f64 / span.as_secs_f64()
    }

    fn release(&mut self, count: usize) {
        for _ in 0..count {
            self.held.pop_front();
        }
    }

    fn finish<T>(
        &mut self,
        rows: &[T],
        visible: usize,
        id: impl Fn(&T) -> &Arc<str>,
        next_wake: Option<Duration>,
    ) -> PaceOutcome {
        self.tail = visible
            .checked_sub(1)
            .and_then(|index| rows.get(index))
            .map(|row| Arc::clone(id(row)));
        PaceOutcome { visible, next_wake }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Row {
        id: Arc<str>,
        urgent: bool,
    }

    fn rows(range: std::ops::Range<usize>) -> Vec<Row> {
        range
            .map(|n| Row {
                id: Arc::from(n.to_string()),
                urgent: false,
            })
            .collect()
    }

    fn pace(pacer: &mut RevealPacer, list: &[Row], now: Instant) -> PaceOutcome {
        pacer.pace(list, now, |row| &row.id, |row| row.urgent)
    }

    #[test]
    fn first_list_and_unrelated_lists_show_everything() {
        let mut pacer = RevealPacer::new();
        let t0 = Instant::now();
        assert_eq!(pace(&mut pacer, &rows(0..800), t0).visible, 800);
        // The tail is gone (a replaced list): show it all.
        assert_eq!(pace(&mut pacer, &rows(900..950), t0).visible, 50);
    }

    #[test]
    fn steady_stream_shows_without_delay() {
        let mut pacer = RevealPacer::new();
        let t0 = Instant::now();
        let mut list = rows(0..100);
        pace(&mut pacer, &list, t0);
        // Two rows every 100 ms for 10 s, like Twitch's quiet-load cadence.
        let mut max_held = 0;
        for step in 1..=100 {
            let n = 100 + step * 2;
            list = rows(0..n);
            let now = t0 + Duration::from_millis(100 * step as u64);
            let outcome = pace(&mut pacer, &list, now);
            max_held = max_held.max(n - outcome.visible);
        }
        // About two arrival groups (~200 ms): the cost of evening them out.
        assert!(max_held <= 4, "held {max_held} rows of a steady stream");
    }

    #[test]
    fn burst_is_spread_over_the_gap_and_never_held_past_the_cap() {
        let mut pacer = RevealPacer::new();
        let t0 = Instant::now();
        pace(&mut pacer, &rows(0..10), t0);
        // 70 rows land at once every 3 s.
        let mut total = 10;
        let mut reveal_times = Vec::new();
        for cycle in 0..4u64 {
            let burst_at = t0 + Duration::from_secs(3 * cycle + 1);
            total += 70;
            let list = rows(0..total);
            let mut now = burst_at;
            let mut visible = pace(&mut pacer, &list, now).visible;
            let mut shown_at = vec![burst_at; visible.saturating_sub(total - 70)];
            while visible < total {
                let outcome = pace(&mut pacer, &list, now);
                visible = outcome.visible;
                while shown_at.len() < visible - (total - 70) {
                    shown_at.push(now);
                }
                let Some(wake) = outcome.next_wake else { break };
                now += wake;
            }
            assert_eq!(visible, total, "cycle {cycle} did not drain");
            let hold = now.duration_since(burst_at);
            assert!(hold <= MAX_HOLD + MAX_WAKE, "held {hold:?}");
            if cycle == 3 {
                reveal_times = shown_at;
            }
        }
        // Once the rate is known, the burst is not shown in one block: no
        // instant reveals more than a handful of rows.
        let mut per_instant = std::collections::HashMap::new();
        for at in &reveal_times {
            *per_instant.entry(*at).or_insert(0) += 1;
        }
        assert!(per_instant.values().all(|&n| n <= 3), "{per_instant:?}");
        let spread = reveal_times.last().unwrap().duration_since(reveal_times[0]);
        assert!(spread >= Duration::from_millis(1_500), "spread {spread:?}");
    }

    /// A pacer that has seen rows arrive at 10/s and holds nothing; returns
    /// the time to continue from. Its list is `rows(0..n)`.
    fn warmed(n: usize) -> (RevealPacer, Instant) {
        let mut pacer = RevealPacer::new();
        let t0 = Instant::now();
        pace(&mut pacer, &rows(0..n - 20), t0);
        pace(&mut pacer, &rows(0..n - 10), t0 + Duration::from_secs(1));
        pace(&mut pacer, &rows(0..n), t0 + Duration::from_secs(2));
        let now = t0 + Duration::from_secs(5);
        assert_eq!(pace(&mut pacer, &rows(0..n), now).visible, n);
        (pacer, now)
    }

    #[test]
    fn without_a_measured_rate_rows_show_at_once() {
        let mut pacer = RevealPacer::new();
        let t0 = Instant::now();
        pace(&mut pacer, &rows(0..10), t0);
        // A burst after a quiet start: nothing to pace against yet.
        assert_eq!(
            pace(&mut pacer, &rows(0..80), t0 + Duration::from_millis(5)).visible,
            80
        );
    }

    #[test]
    fn urgent_row_releases_itself_and_everything_before_it() {
        let (mut pacer, now) = warmed(30);
        let mut list = rows(0..100);
        let held = pace(&mut pacer, &list, now + Duration::from_millis(10));
        assert!(held.visible < 100);
        list[90].urgent = true;
        let outcome = pace(&mut pacer, &list, now + Duration::from_millis(20));
        assert!(outcome.visible >= 91);
    }

    #[test]
    fn rotation_keeps_the_tail_by_id() {
        let (mut pacer, now) = warmed(1000);
        // A full buffer: each arrival evicts the oldest row.
        let list = rows(50..1050);
        let outcome = pace(&mut pacer, &list, now + Duration::from_millis(5));
        let tail_index = list
            .iter()
            .position(|row| row.id.as_ref() == "999")
            .unwrap();
        assert!(outcome.visible > tail_index);
        assert!(outcome.visible < list.len());
    }

    #[test]
    fn reveal_all_clears_held_rows() {
        let (mut pacer, now) = warmed(30);
        pace(&mut pacer, &rows(0..90), now + Duration::from_millis(5));
        assert!(pacer.held() > 0);
        assert_eq!(pacer.reveal_all(&rows(0..90), |row| &row.id), 90);
        assert_eq!(pacer.held(), 0);
    }
}
