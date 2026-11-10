use crate::message::{ChatMessage, MessageFlags, day_divider_id};
use crate::message_buffer::{HistoryMergeStrategy, merge_history_by_timestamp};
use chrono::{Local, NaiveDate, TimeZone};
use smallvec::SmallVec;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Bounded authoritative chat buffer and its published display projection.
///
/// Rows are shared `Arc<ChatMessage>`s because every publish hands splits a
/// new immutable row list; sharing makes that list one pointer per row instead
/// of a deep copy of the retained tail.
#[derive(Clone)]
pub struct ChatSnapshot {
    pub channel: String,
    message_capacity: usize,
    raw_messages: Arc<Vec<Arc<ChatMessage>>>,
    pub messages: Arc<Vec<Arc<ChatMessage>>>,
}

impl ChatSnapshot {
    pub fn new(channel: String, message_capacity: usize) -> Self {
        let empty = Arc::new(Vec::new());
        Self {
            channel,
            message_capacity: message_capacity.max(1),
            raw_messages: Arc::clone(&empty),
            messages: empty,
        }
    }

    pub fn replace_snapshot_from_backend(&mut self, messages: Vec<ChatMessage>) -> bool {
        let messages = Self::bounded_newest(messages, self.message_capacity);

        if self.raw_messages.len() == messages.len()
            && self
                .raw_messages
                .iter()
                .zip(messages.iter())
                .all(|(left, right)| left.id == right.id)
        {
            let changed_positions: Vec<usize> = self
                .raw_messages
                .iter()
                .zip(messages.iter())
                .enumerate()
                .filter(|(_, (left, right))| !messages_equivalent_for_snapshot(left, right))
                .map(|(idx, _)| idx)
                .collect();

            if changed_positions.is_empty() {
                return false;
            }

            let mut incoming = messages;
            let current = Arc::make_mut(&mut self.raw_messages);
            for idx in changed_positions {
                current[idx] = Arc::new(std::mem::take(&mut incoming[idx]));
            }
            self.publish_display_projection();
            return true;
        }

        if Self::message_snapshots_equivalent(&self.raw_messages, &messages) {
            return false;
        }
        self.set_messages_vec(messages.into_iter().map(Arc::new).collect());
        true
    }

    pub fn set_message_capacity(&mut self, target_capacity: usize) -> bool {
        let target_capacity = target_capacity.max(1);
        self.message_capacity = target_capacity;
        if self.raw_messages.len() <= target_capacity {
            return false;
        }

        let start = self.raw_messages.len() - target_capacity;
        self.set_messages_vec(self.raw_messages[start..].to_vec());
        true
    }

    /// Append live rows, skipping any whose id the snapshot's tail already
    /// holds. A backend refresh drained in the same pass as (or between the
    /// store and the enqueue of) a `NewMessage` already contains that row,
    /// and the refresh copy is at least as new. Returns whether a row was added.
    pub fn append_messages_to_snapshot(&mut self, mut new_messages: Vec<ChatMessage>) -> bool {
        self.drop_rows_already_in_tail(&mut new_messages);
        if new_messages.is_empty() {
            return false;
        }

        let capacity = self.message_capacity.max(1);
        if new_messages.len() >= capacity {
            self.set_messages_vec(
                Self::bounded_newest(new_messages, capacity)
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
            );
            return true;
        }

        // Splits hold the published list, so it is always rebuilt rather than
        // extended in place; with shared rows that is one pointer per row.
        let keep_existing = capacity
            .saturating_sub(new_messages.len())
            .min(self.raw_messages.len());
        let start = self.raw_messages.len() - keep_existing;
        let mut merged = Vec::with_capacity(keep_existing + new_messages.len());
        merged.extend(self.raw_messages[start..].iter().cloned());
        merged.extend(new_messages.into_iter().map(Arc::new));
        self.set_messages_vec(merged);
        true
    }

    fn drop_rows_already_in_tail(&self, new_messages: &mut Vec<ChatMessage>) {
        /// Rows past the incoming batch to search; a refresh that raced the
        /// batch placed its copies at the newest end.
        const TAIL_SLACK: usize = 64;
        if new_messages.is_empty() || self.raw_messages.is_empty() {
            return;
        }
        let window = (new_messages.len() + TAIL_SLACK).min(self.raw_messages.len());
        let tail = &self.raw_messages[self.raw_messages.len() - window..];
        let held: HashSet<&str> = tail
            .iter()
            .map(|message| message.id.as_ref())
            .filter(|id| !id.is_empty())
            .collect();
        if held.is_empty() {
            return;
        }
        new_messages.retain(|message| message.id.is_empty() || !held.contains(message.id.as_ref()));
    }

    pub fn merge_history_into_snapshot(&mut self, incoming: Vec<ChatMessage>) -> bool {
        if incoming.is_empty() {
            if self.raw_messages.is_empty() {
                return false;
            }
            self.set_messages_vec(Vec::new());
            return true;
        }

        let mut existing: Vec<Arc<ChatMessage>> = self.raw_messages.iter().cloned().collect();
        if existing.is_empty() {
            self.set_messages_vec(
                Self::bounded_newest(incoming, self.message_capacity)
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
            );
            return !self.raw_messages.is_empty();
        }

        // `incoming` is the backend buffer, which re-stripes every row after a
        // history merge. Rows already held here adopt that parity, or an
        // inserted gap-fill row sits between two rows with stale stripes.
        let incoming_parity: HashMap<Arc<str>, bool> = incoming
            .iter()
            .map(|message| (Arc::clone(&message.id), message.flags.is_alternate_row))
            .collect();
        let mut parity_changed = false;
        for message in &mut existing {
            if let Some(&parity) = incoming_parity.get(message.id.as_ref())
                && message.flags.is_alternate_row != parity
            {
                Arc::make_mut(message).flags.is_alternate_row = parity;
                parity_changed = true;
            }
        }

        let merge = merge_history_by_timestamp(existing, incoming, Arc::new);
        if merge.strategy == HistoryMergeStrategy::NoNewRows {
            if parity_changed {
                self.set_messages_vec(merge.rows);
            }
            return parity_changed;
        }
        self.set_messages_vec(Self::bounded_newest(merge.rows, self.message_capacity));
        true
    }

    fn bounded_newest<T>(messages: Vec<T>, capacity: usize) -> Vec<T> {
        let start = messages.len().saturating_sub(capacity.max(1));
        messages.into_iter().skip(start).collect()
    }

    fn set_messages_vec(&mut self, mut messages: Vec<Arc<ChatMessage>>) {
        messages.shrink_to_fit();
        self.raw_messages = Arc::new(messages);
        self.publish_display_projection();
    }

    fn publish_display_projection(&mut self) {
        let boundaries = day_divider_boundaries(&self.raw_messages);
        if boundaries.is_empty() {
            self.messages = Arc::clone(&self.raw_messages);
            return;
        }

        let mut rows = Vec::with_capacity(self.raw_messages.len() + boundaries.len());
        let mut pending = boundaries.into_iter().peekable();
        for (idx, message) in self.raw_messages.iter().enumerate() {
            if let Some((boundary, date)) = pending.peek().copied()
                && boundary == idx
            {
                pending.next();
                rows.push(Arc::new(day_divider_row(&self.channel, date, message)));
            }
            rows.push(Arc::clone(message));
        }
        self.messages = Arc::new(rows);
    }

    fn message_snapshots_equivalent(
        current: &Arc<Vec<Arc<ChatMessage>>>,
        incoming: &[ChatMessage],
    ) -> bool {
        current.len() == incoming.len()
            && current
                .iter()
                .zip(incoming.iter())
                .all(|(left, right)| messages_equivalent_for_snapshot(left, right))
    }
}

#[derive(Clone, Copy)]
struct LocalDayWindow {
    day: NaiveDate,
    start_ms: i64,
    end_ms: i64,
}

impl LocalDayWindow {
    fn containing(timestamp_ms: i64) -> Option<Self> {
        let day = Local
            .timestamp_millis_opt(timestamp_ms)
            .single()?
            .date_naive();
        let bounds = local_midnight_ms(day)
            .zip(day.succ_opt().and_then(local_midnight_ms))
            .filter(|(start_ms, end_ms)| (*start_ms..*end_ms).contains(&timestamp_ms));

        Some(match bounds {
            Some((start_ms, end_ms)) => Self {
                day,
                start_ms,
                end_ms,
            },
            None => Self {
                day,
                start_ms: timestamp_ms,
                end_ms: timestamp_ms.saturating_add(1),
            },
        })
    }

    fn contains(&self, timestamp_ms: i64) -> bool {
        timestamp_ms >= self.start_ms && timestamp_ms < self.end_ms
    }
}

fn local_midnight_ms(day: NaiveDate) -> Option<i64> {
    let midnight = day.and_hms_opt(0, 0, 0)?;
    let mapped = Local.from_local_datetime(&midnight);
    mapped
        .earliest()
        .or_else(|| mapped.latest())
        .map(|local| local.timestamp_millis())
}

fn timestamp_millis(timestamp: i64) -> i64 {
    if timestamp > 4_102_444_800 {
        timestamp
    } else {
        timestamp * 1000
    }
}

fn day_divider_boundaries(messages: &[Arc<ChatMessage>]) -> SmallVec<[(usize, NaiveDate); 4]> {
    let mut boundaries = SmallVec::new();
    let mut current: Option<LocalDayWindow> = None;

    for (idx, message) in messages.iter().enumerate() {
        let timestamp_ms = timestamp_millis(message.timestamp);
        let window = match current {
            Some(window) if window.contains(timestamp_ms) => window,
            _ => match LocalDayWindow::containing(timestamp_ms) {
                Some(window) => window,
                None => {
                    current = None;
                    continue;
                }
            },
        };

        if current.is_some_and(|previous| previous.day != window.day) {
            boundaries.push((idx, window.day));
        }
        current = Some(window);
    }
    boundaries
}

fn day_divider_row(channel: &str, date: NaiveDate, next: &ChatMessage) -> ChatMessage {
    let date_iso = date.format("%Y-%m-%d").to_string();
    let mut hasher = DefaultHasher::new();
    "day_divider".hash(&mut hasher);
    date_iso.hash(&mut hasher);

    ChatMessage {
        id: Arc::from(day_divider_id(channel, &date_iso, &next.id).as_str()),
        channel: Arc::clone(&next.channel),
        timestamp: next.timestamp,
        structure_hash: hasher.finish(),
        flags: MessageFlags {
            is_system: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn messages_equivalent_for_snapshot(left: &ChatMessage, right: &ChatMessage) -> bool {
    left.id == right.id
        && left.channel == right.channel
        && left.sender == right.sender
        && left.display_name == right.display_name
        && left.content == right.content
        && left.color == right.color
        && left.badges == right.badges
        && left.timestamp == right.timestamp
        && left.structure_hash == right.structure_hash
        && Arc::ptr_eq(&left.slots, &right.slots)
        && left.reply == right.reply
        && left.flags == right.flags
        && left.twitch_emotes_tag == right.twitch_emotes_tag
        && left.user_id == right.user_id
        && left.reply_emote_offset == right.reply_emote_offset
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_MS: i64 = 1_700_000_000_000;
    const DAY_MS: i64 = 86_400_000;

    fn state() -> ChatSnapshot {
        ChatSnapshot::new("test".to_string(), usize::MAX)
    }

    fn make_message(id: &str, timestamp: i64) -> ChatMessage {
        ChatMessage {
            id: Arc::from(id),
            timestamp,
            ..Default::default()
        }
    }

    fn row_ids(messages: &[Arc<ChatMessage>]) -> Vec<&str> {
        messages.iter().map(|message| message.id.as_ref()).collect()
    }

    #[test]
    fn append_snapshot_retains_newest_capacity() {
        let mut state = state();
        state.set_message_capacity(3);
        state.append_messages_to_snapshot(vec![
            make_message("1", 1),
            make_message("2", 2),
            make_message("3", 3),
        ]);
        state.append_messages_to_snapshot(vec![make_message("4", 4)]);
        assert_eq!(row_ids(&state.messages), ["2", "3", "4"]);
    }

    #[test]
    fn append_skips_rows_a_refresh_already_installed() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![make_message("1", 1)]);
        // A retokenize refresh drained before NewMessage("2") already holds "2".
        state.replace_snapshot_from_backend(vec![make_message("1", 1), make_message("2", 2)]);

        assert!(!state.append_messages_to_snapshot(vec![make_message("2", 2)]));
        assert!(
            state.append_messages_to_snapshot(vec![make_message("2", 2), make_message("3", 3)])
        );
        assert_eq!(row_ids(&state.messages), ["1", "2", "3"]);
    }

    #[test]
    fn append_keeps_rows_without_ids() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![make_message("", 1)]);
        assert!(state.append_messages_to_snapshot(vec![make_message("", 2)]));
        assert_eq!(state.messages.len(), 2);
    }

    #[test]
    fn history_snapshot_prepends_and_trims() {
        let mut state = state();
        state.set_message_capacity(4);
        state.append_messages_to_snapshot(vec![make_message("3", 3), make_message("4", 4)]);
        state.merge_history_into_snapshot(vec![make_message("1", 1), make_message("2", 2)]);
        assert_eq!(row_ids(&state.messages), ["1", "2", "3", "4"]);
    }

    #[test]
    fn history_snapshot_deduplicates_overlap() {
        let mut state = state();
        state.set_message_capacity(5);
        state.append_messages_to_snapshot(vec![
            make_message("2", 2),
            make_message("3", 3),
            make_message("4", 4),
        ]);
        state.merge_history_into_snapshot(vec![
            make_message("1", 1),
            make_message("2", 2),
            make_message("3", 3),
        ]);
        assert_eq!(row_ids(&state.messages), ["1", "2", "3", "4"]);
    }

    #[test]
    fn history_snapshot_restripes_rows_already_held() {
        let mut state = state();
        let mut a = make_message("a", 1);
        let mut b = make_message("b", 3);
        b.flags.is_alternate_row = true;
        state.append_messages_to_snapshot(vec![a.clone(), b.clone()]);
        // The backend buffer after gap-filling "x", re-striped from the newest row.
        a.flags.is_alternate_row = true;
        state.merge_history_into_snapshot(vec![a, make_message("x", 2), b]);
        let stripes: Vec<(&str, bool)> = state
            .messages
            .iter()
            .map(|message| (message.id.as_ref(), message.flags.is_alternate_row))
            .collect();
        assert_eq!(stripes, [("a", true), ("x", false), ("b", true)]);
    }

    #[test]
    fn history_snapshot_reports_parity_only_changes() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![make_message("a", 1)]);
        let mut restriped = make_message("a", 1);
        restriped.flags.is_alternate_row = true;
        assert!(state.merge_history_into_snapshot(vec![restriped]));
        assert!(state.messages[0].flags.is_alternate_row);
    }

    #[test]
    fn set_message_capacity_trims_oldest_snapshot_entries() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![
            make_message("1", 1),
            make_message("2", 2),
            make_message("3", 3),
        ]);
        assert!(state.set_message_capacity(2));
        assert_eq!(row_ids(&state.messages), ["2", "3"]);
    }

    #[test]
    fn same_day_projection_shares_the_buffer_allocation() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![
            make_message("1", BASE_MS),
            make_message("2", BASE_MS + 1_000),
        ]);
        assert!(Arc::ptr_eq(&state.messages, &state.raw_messages));
        assert_eq!(row_ids(&state.messages), ["1", "2"]);
    }

    #[test]
    fn no_divider_before_the_oldest_known_message() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![make_message("1", BASE_MS)]);
        assert_eq!(row_ids(&state.messages), ["1"]);
    }

    #[test]
    fn day_divider_precedes_first_message_of_a_new_local_date() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![
            make_message("1", BASE_MS),
            make_message("2", BASE_MS + DAY_MS),
        ]);
        assert_eq!(row_ids(&state.raw_messages), ["1", "2"]);
        assert_eq!(state.messages.len(), 3);
        let divider = &state.messages[1];
        assert!(divider.flags.is_system);
        assert!(divider.content.is_empty());
        assert!(divider.reply.is_none());
        assert_eq!(divider.timestamp, BASE_MS + DAY_MS);
        let date_iso = divider
            .day_divider_date()
            .expect("divider carries its date");
        assert_eq!(
            divider.id.as_ref(),
            format!("__echo_day__:test:{date_iso}:2")
        );
        assert_eq!(row_ids(&state.messages)[0], "1");
        assert_eq!(row_ids(&state.messages)[2], "2");
    }

    #[test]
    fn backfilled_history_places_the_divider_chronologically() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![make_message("2", BASE_MS + DAY_MS)]);
        assert_eq!(state.messages.len(), 1);
        state.merge_history_into_snapshot(vec![make_message("1", BASE_MS)]);
        assert_eq!(state.messages.len(), 3);
        assert_eq!(row_ids(&state.messages)[0], "1");
        assert!(state.messages[1].day_divider_date().is_some());
        assert_eq!(row_ids(&state.messages)[2], "2");
    }

    #[test]
    fn evicting_the_earlier_date_drops_its_divider() {
        let mut state = state();
        state.append_messages_to_snapshot(vec![
            make_message("1", BASE_MS),
            make_message("2", BASE_MS + DAY_MS),
        ]);
        assert_eq!(state.messages.len(), 3);
        assert!(state.set_message_capacity(1));
        assert_eq!(row_ids(&state.messages), ["2"]);
    }
}
