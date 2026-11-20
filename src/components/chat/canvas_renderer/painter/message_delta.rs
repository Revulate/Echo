//! Pure message-buffer delta classification for `set_messages`.

use gpui::SharedString;
use std::sync::Arc;

use crate::backend::ChatMessage;

/// Minimum first snapshot size where estimated-height seeding is cheaper than a full rebuild.
const INITIAL_SNAPSHOT_FAST_PATH_MIN_MESSAGES: usize = 32;

pub(super) struct MessageDeltaPlan {
    pub(super) content_changed: bool,
    pub(super) count_increased: bool,
    pub(super) had_new_messages: bool,
    pub(super) added_count: usize,
    pub(super) old_order_matches_prefix: bool,
    pub(super) old_order_matches_suffix: bool,
    pub(super) old_order_middle_start: Option<usize>,
    pub(super) is_initial_snapshot_fast_path: bool,
}

impl MessageDeltaPlan {
    pub(super) fn classify(
        current_messages: &Arc<Vec<Arc<ChatMessage>>>,
        message_order: &[SharedString],
        height_tree_len: usize,
        next_messages: &Arc<Vec<Arc<ChatMessage>>>,
    ) -> Self {
        let old_count = current_messages.len();
        let new_count = next_messages.len();
        let content_changed = !Arc::ptr_eq(current_messages, next_messages);
        let count_increased = new_count > old_count;
        let had_new_messages = count_increased || (content_changed && new_count == old_count);
        let added_count = new_count.saturating_sub(old_count);
        let has_stable_existing_order = old_count > 0 && message_order.len() == old_count;

        let mut old_order_matches_prefix = false;
        let mut old_order_matches_suffix = false;
        let mut old_order_middle_start = None;

        if count_increased && has_stable_existing_order {
            old_order_matches_prefix = ids_align(message_order, next_messages);

            if !old_order_matches_prefix {
                old_order_matches_suffix = ids_align(message_order, &next_messages[added_count..]);

                if !old_order_matches_suffix {
                    old_order_middle_start = message_order.first().and_then(|first_old_id| {
                        next_messages
                            .iter()
                            .position(|new_msg| new_msg.id.as_ref() == first_old_id.as_ref())
                            .filter(|&start| {
                                start > 0
                                    && start + old_count <= new_count
                                    && ids_align(message_order, &next_messages[start..])
                            })
                    });
                }
            }
        }
        let is_initial_snapshot_fast_path = count_increased
            && old_count == 0
            && message_order.is_empty()
            && height_tree_len == 0
            && new_count >= INITIAL_SNAPSHOT_FAST_PATH_MIN_MESSAGES;

        Self {
            content_changed,
            count_increased,
            had_new_messages,
            added_count,
            old_order_matches_prefix,
            old_order_matches_suffix,
            old_order_middle_start,
            is_initial_snapshot_fast_path,
        }
    }
}

/// Whether `order` and the ids of `messages` agree over their common length.
pub(super) fn ids_align(order: &[SharedString], messages: &[Arc<ChatMessage>]) -> bool {
    order
        .iter()
        .zip(messages)
        .all(|(old_id, new_msg)| old_id.as_ref() == new_msg.id.as_ref())
}

/// A new snapshot that keeps the old rows from `dropped` on, in order, and
/// adds `appended` rows after them: a rotation at the buffer cap, rows held by
/// paced reveal leaving the front, or front rows removed by a moderation or
/// block (any of those with new rows released or arriving in the same sync),
/// or with nothing dropped a plain append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrontDrop {
    pub(super) dropped: usize,
    pub(super) appended: usize,
}

/// How `next` relates to the rows in `order` when it drops rows from the
/// front, keeps the rest and may append others: `next` starts with
/// `order[dropped..]` (ids, same order) and continues with `appended` other
/// rows, and it changes something (drops or appends at least one row).
/// `None` for every other shape, including a drop that keeps no row.
pub(super) fn front_drop(order: &[SharedString], next: &[Arc<ChatMessage>]) -> Option<FrontDrop> {
    let first_new_id = next.first()?.id.as_ref();
    let dropped = order.iter().position(|id| id.as_ref() == first_new_id)?;
    let kept = order.len() - dropped;
    (next.len() >= kept && (dropped > 0 || next.len() > kept) && ids_align(&order[dropped..], next))
        .then(|| FrontDrop {
            dropped,
            appended: next.len() - kept,
        })
}

/// The rows of `rows`, ordered by row index, that survive dropping the first
/// `dropped` rows of the list, with their indices shifted to match.
pub(super) fn remap_after_front_drop<T: Clone>(
    rows: &[T],
    dropped: usize,
    index: impl Fn(&mut T) -> &mut usize,
) -> Vec<T> {
    rows.iter()
        .filter_map(|row| {
            let mut row = row.clone();
            let shifted = index(&mut row).checked_sub(dropped)?;
            *index(&mut row) = shifted;
            Some(row)
        })
        .collect()
}

/// A row-index range after dropping the first `dropped` rows: the part that
/// survives, shifted to match, or `None` when nothing of it survives.
pub(super) fn range_after_front_drop(
    (start, end): (usize, usize),
    dropped: usize,
) -> Option<(usize, usize)> {
    (end > dropped).then(|| (start.saturating_sub(dropped), end - dropped))
}

#[cfg(test)]
mod tests {
    use super::super::CanvasMessageList;
    use super::super::model::MessageListModel;
    use super::*;
    use crate::components::chat::chat_view::shown_rows_are_current;
    use core::prelude::v1::test;
    use echo_core::chat_snapshot::ChatSnapshot;
    use echo_core::reveal_pacer::RevealPacer;
    use std::time::{Duration, Instant};

    type Rows = Arc<Vec<Arc<ChatMessage>>>;

    fn row(id: &str) -> Arc<ChatMessage> {
        Arc::new(ChatMessage {
            id: Arc::from(id),
            ..Default::default()
        })
    }

    fn ids(ids: &[&str]) -> Vec<SharedString> {
        ids.iter().map(|id| SharedString::from(*id)).collect()
    }

    fn rows(ids: &[&str]) -> Vec<Arc<ChatMessage>> {
        ids.iter().map(|id| row(id)).collect()
    }

    #[test]
    fn front_drop_reads_only_a_front_drop_with_an_optional_append() {
        let order = ids(&["a", "b", "c", "d"]);
        let shape = |dropped, appended| Some(FrontDrop { dropped, appended });
        for (next, expected, why) in [
            (vec!["b", "c", "d"], shape(1, 0), "front drop"),
            (vec!["c", "d", "e", "f"], shape(2, 2), "rotation"),
            (
                vec!["b", "c", "d", "e", "f"],
                shape(1, 2),
                "drop with a longer append",
            ),
            (vec!["d", "e"], shape(3, 1), "keeps one row"),
            (vec!["d"], shape(3, 0), "keeps only the last row"),
            (vec!["a", "b", "c", "d", "e"], shape(0, 1), "plain append"),
            (vec!["a", "b", "c", "d"], None, "same rows"),
            (vec!["e", "f"], None, "keeps no row"),
            (vec!["b", "d"], None, "drop plus a removal in the middle"),
            (vec!["b", "c"], None, "drop plus a removal at the end"),
            (vec!["c", "b", "d"], None, "reorder"),
            (vec!["x", "b", "c", "d"], None, "prepend"),
            (vec![], None, "clear"),
        ] {
            assert_eq!(front_drop(&order, &rows(&next)), expected, "{why}");
        }
        assert_eq!(front_drop(&[], &rows(&["a"])), None, "empty order");
    }

    fn height_of(id: &str) -> f32 {
        let hash = id.bytes().fold(7u32, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
        (hash % 40 + 12) as f32
    }

    /// The model `rebuild_message_model_from` produces, with each row's
    /// height a function of its id.
    fn rebuilt(next: &Rows) -> MessageListModel {
        let mut model = MessageListModel::initial();
        for (index, message) in next.iter().enumerate() {
            let id = SharedString::from(message.id.clone());
            model.message_order.push(id.clone());
            model.insert_message_index(id, index);
            model.height_tree.push(height_of(&message.id));
        }
        model.sync_total_height();
        model.messages = next.clone();
        model
    }

    fn assert_same_model(model: &MessageListModel, expected: &MessageListModel, context: &str) {
        assert!(
            Arc::ptr_eq(&model.messages, &expected.messages),
            "{context}"
        );
        assert_eq!(model.message_order, expected.message_order, "{context}");
        assert_eq!(
            model.height_tree.len(),
            expected.height_tree.len(),
            "{context}"
        );
        for (index, message) in expected.messages.iter().enumerate() {
            assert_eq!(
                model.get_message_index(&message.id),
                Some(index),
                "{context}"
            );
            assert_eq!(
                model.height_tree.height_at(index),
                expected.height_tree.height_at(index),
                "{context}: height of row {index}"
            );
        }
        assert_eq!(
            model.message_index.len(),
            expected.message_index.len(),
            "{context}: no stale index entries"
        );
        assert!(
            (model.total_height - expected.total_height).abs() < 0.01,
            "{context}"
        );
    }

    #[test]
    fn the_model_refuses_a_front_drop_it_cannot_prove() {
        let base: Rows = Arc::new(rows(&["a", "b", "c", "d"]));
        let fresh = || rebuilt(&base);

        let mut model = fresh();
        let appended = rows(&["e", "f"]);
        assert!(model.drop_front_and_append(1, &appended, 0));
        let next: Rows = Arc::new(base[1..].iter().cloned().chain(appended).collect());
        for message in &next[3..] {
            model.height_tree.push(height_of(&message.id));
        }
        model.sync_total_height();
        model.messages = next.clone();
        assert_same_model(&model, &rebuilt(&next), "accepted drop");

        let refused = |mut model: MessageListModel, dropped, appended: &[&str], pending, why| {
            let order = model.message_order.clone();
            let heights = model.height_tree.len();
            assert!(
                !model.drop_front_and_append(dropped, &rows(appended), pending),
                "{why}"
            );
            assert_eq!(model.message_order, order, "{why}: unchanged");
            assert_eq!(model.height_tree.len(), heights, "{why}: unchanged");
        };
        refused(fresh(), 0, &[], 0, "nothing dropped or appended");
        refused(fresh(), 4, &["e"], 0, "nothing kept");
        refused(fresh(), 1, &["c"], 0, "appended id already shown");
        refused(fresh(), 1, &["a"], 0, "appended id is a dropped one");
        refused(fresh(), 1, &["e"], 1, "heights and pending rows disagree");
        let mut pending = fresh();
        pending.height_tree.clear();
        pending.height_tree.push(1.0);
        refused(
            pending,
            2,
            &["e"],
            3,
            "a dropped row's height is still pending",
        );
        let mut repeated = fresh();
        repeated.message_order[2] = SharedString::from("a");
        repeated.rebuild_message_index_from_order();
        refused(repeated, 1, &["e"], 0, "a kept row repeats a dropped id");
        let mut lagging = fresh();
        lagging.message_order.pop();
        lagging.height_tree.clear();
        for _ in 0..3 {
            lagging.height_tree.push(1.0);
        }
        refused(lagging, 1, &["e"], 0, "order behind the rows");
    }

    /// One split's model, synced the way `apply_messages` and the
    /// `update_layouts` passes do it, with heights a function of the id.
    struct Split {
        model: MessageListModel,
        pending: usize,
        front_drops: usize,
        rebuilds: usize,
        caught_up: usize,
        /// Stand-ins for the prepared paint rows (index, id) and the emote
        /// preload range of the last frame, remapped like the real ones.
        prepared: Vec<(usize, SharedString)>,
        preload_range: Option<(usize, usize)>,
    }

    impl Split {
        fn new() -> Self {
            Self {
                model: MessageListModel::initial(),
                pending: 0,
                front_drops: 0,
                rebuilds: 0,
                caught_up: 0,
                prepared: Vec::new(),
                preload_range: None,
            }
        }

        /// `apply_messages`' model work for the shapes it tells apart; the
        /// prepend paths are folded into the rebuild, which they equal.
        fn sync(&mut self, next: &Rows) {
            if Arc::ptr_eq(&self.model.messages, next) {
                return;
            }
            let model = &mut self.model;
            let plan = MessageDeltaPlan::classify(
                &model.messages,
                &model.message_order,
                model.height_tree.len(),
                next,
            );
            let (old, new) = (model.messages.len(), next.len());
            let lagging_order_ok = !(plan.count_increased && plan.old_order_matches_prefix)
                && !(new == old
                    && model.message_order.len() == new
                    && ids_align(&model.message_order, next));
            if lagging_order_ok {
                let added = model.complete_lagging_order(self.pending);
                self.pending += added;
                self.caught_up += usize::from(added > 0);
            }
            let front = (model.message_order.len() == old)
                .then(|| front_drop(&model.message_order, next))
                .flatten()
                .filter(|front| {
                    front.dropped <= CanvasMessageList::incremental_rotation_limit(new)
                        || front.dropped * 2 <= new
                });
            if plan.count_increased && plan.old_order_matches_prefix {
                for (index, message) in next.iter().enumerate().skip(old) {
                    let id = SharedString::from(message.id.clone());
                    if self.pending > 0 {
                        model.message_order.push(id.clone());
                    }
                    model.insert_message_index(id, index);
                }
                if self.pending > 0 {
                    self.pending += new - old;
                }
            } else if new == old
                && model.message_order.len() == new
                && ids_align(&model.message_order, next)
            {
                model.rebuild_message_index_from_order();
            } else if let Some(front) = front
                && model.drop_front_and_append(
                    front.dropped,
                    &next[new - front.appended..],
                    self.pending,
                )
            {
                self.pending += front.appended;
                self.front_drops += 1;
                self.prepared =
                    remap_after_front_drop(&self.prepared, front.dropped, |row| &mut row.0);
                self.preload_range = self
                    .preload_range
                    .and_then(|range| range_after_front_drop(range, front.dropped));
            } else {
                *model = rebuilt(next);
                self.pending = 0;
                self.rebuilds += 1;
                self.prepared.clear();
                self.preload_range = None;
            }
            self.model.messages = next.clone();
        }

        /// `update_layouts`' pending-row and append passes, or its rebuild
        /// when the order no longer matches.
        fn render(&mut self) {
            let model = &mut self.model;
            let messages = model.messages.clone();
            if self.pending > 0 {
                let start = model.message_order.len() - self.pending;
                for message in &messages[start..] {
                    model.height_tree.push(height_of(&message.id));
                }
                self.pending = 0;
                // The real pass returns here; rows a prefix append left out
                // of the order would wait for the next frame.
                model.sync_total_height();
                return;
            } else if model.message_order.len() < messages.len()
                && model.message_order.first().map(|id| id.as_ref())
                    == messages.first().map(|message| message.id.as_ref())
            {
                for message in &messages[model.message_order.len()..] {
                    model
                        .message_order
                        .push(SharedString::from(message.id.clone()));
                    model.height_tree.push(height_of(&message.id));
                }
            } else if model.message_order.len() != messages.len()
                || model.height_tree.len() != messages.len()
            {
                *model = rebuilt(&messages);
            }
            model.sync_total_height();
            // Every prepared row kept across front drops still names its
            // own row, and the kept preload range lies within the rows.
            for (index, id) in &self.prepared {
                assert_eq!(
                    model.message_order.get(*index),
                    Some(id),
                    "prepared row {index}"
                );
            }
            if let Some((start, end)) = self.preload_range {
                assert!(start <= end && end <= model.message_order.len());
            }
            let len = model.message_order.len();
            self.prepared = (len.saturating_sub(20)..len)
                .map(|index| (index, model.message_order[index].clone()))
                .collect();
            self.preload_range = Some((len.saturating_sub(25), len));
        }
    }

    /// Splitmix64, for repeatable sequences without a dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound.max(1)
        }
    }

    const DAY_MS: i64 = 86_400_000;

    /// A channel's snapshot, a split following live with paced reveal, and
    /// the publishes that reach it.
    struct Channel {
        snapshot: ChatSnapshot,
        pacer: RevealPacer,
        split: Split,
        now: Instant,
        clock_ms: i64,
        next_id: u64,
        reused: usize,
    }

    impl Channel {
        fn new(capacity: usize) -> Self {
            Self {
                snapshot: ChatSnapshot::new("test".to_string(), capacity),
                pacer: RevealPacer::new(),
                split: Split::new(),
                now: Instant::now(),
                clock_ms: 1_700_000_000_000,
                next_id: 0,
                reused: 0,
            }
        }

        fn live_row(&mut self) -> ChatMessage {
            self.next_id += 1;
            self.clock_ms += 50;
            ChatMessage {
                id: Arc::from(format!("m{}", self.next_id)),
                timestamp: self.clock_ms,
                content: Arc::from("hello"),
                ..Default::default()
            }
        }

        fn backend_rows(&self) -> Vec<ChatMessage> {
            self.snapshot
                .messages
                .iter()
                .filter(|message| message.day_divider_date().is_none())
                .map(|message| (**message).clone())
                .collect()
        }

        fn append(&mut self, count: usize) {
            let batch = (0..count).map(|_| self.live_row()).collect();
            self.snapshot.append_messages_to_snapshot(batch);
        }

        /// What `ChatView::paced_messages` hands the canvas: the pacer's
        /// visible prefix, keeping the shown list when its rows are the same.
        fn sync(&mut self, render: bool) {
            self.sync_with(render, true);
        }

        /// `sync`, or with `paced` false what a split that stopped following
        /// live (scrolled up, paused) or was hidden gets: every row.
        fn sync_with(&mut self, render: bool, paced: bool) {
            let source = self.snapshot.messages.clone();
            let visible = if paced {
                self.pacer
                    .pace(
                        source.as_slice(),
                        self.now,
                        |message| &message.id,
                        |message| message.flags.is_system,
                    )
                    .visible
            } else {
                self.pacer
                    .reveal_all(source.as_slice(), |message| &message.id)
            };
            let shown = self.split.model.messages.clone();
            let next = if visible >= source.len() {
                source.clone()
            } else if shown_rows_are_current(&shown, &source[..visible]) {
                self.reused += 1;
                shown
            } else {
                Arc::new(source[..visible].to_vec())
            };
            assert_eq!(next.len(), visible.min(source.len()));
            assert!(
                next.iter()
                    .zip(source.iter())
                    .all(|(shown, source)| Arc::ptr_eq(shown, source)),
                "a split shows exactly the visible rows of the snapshot"
            );
            self.split.sync(&next);
            if render {
                self.split.render();
                assert_same_model(&self.split.model, &rebuilt(&next), "after render");
            }
        }

        fn tick(&mut self, rng: &mut Rng) {
            self.now += Duration::from_millis(5 + rng.below(80));
        }
    }

    /// Fill to `capacity` with a steady stream the pacer measures a rate
    /// from, rendering each sync.
    fn warmed(capacity: usize) -> Channel {
        let mut channel = Channel::new(capacity);
        for _ in 0..capacity.min(200) + 40 {
            channel.append(1);
            channel.now += Duration::from_millis(100);
            channel.sync(true);
        }
        channel
    }

    #[test]
    fn held_arrivals_at_the_cap_take_the_incremental_path() {
        let mut channel = warmed(60);
        assert_eq!(channel.snapshot.messages.len(), 60);
        let rebuilds = channel.split.rebuilds;
        let mut rng = Rng(7);
        for _ in 0..30 {
            // A burst the pacer holds, then single arrivals while it drains.
            channel.append(12);
            channel.now += Duration::from_millis(2);
            channel.sync(true);
            for _ in 0..8 {
                channel.append(1);
                channel.tick(&mut rng);
                channel.sync(rng.below(3) != 0);
            }
            channel.now += Duration::from_millis(3_000);
            channel.sync(true);
        }
        assert_eq!(channel.split.rebuilds, rebuilds, "no rebuild at the cap");
        assert!(
            channel.split.front_drops > 200,
            "{}",
            channel.split.front_drops
        );

        // Below the cap a held arrival changes nothing the split shows.
        let mut channel = warmed(10_000);
        let reused = channel.reused;
        channel.append(12);
        channel.now += Duration::from_millis(2);
        channel.sync(true);
        let shown = channel.split.model.messages.clone();
        channel.append(1);
        channel.now += Duration::from_millis(2);
        channel.sync(true);
        assert!(channel.reused > reused);
        assert!(Arc::ptr_eq(&channel.split.model.messages, &shown));
    }

    #[test]
    fn paced_syncs_match_a_full_rebuild_for_every_publish_sequence() {
        let mut front_drops = 0;
        let mut reused = 0;
        let mut caught_up = 0;
        for seed in 0..40 {
            let mut rng = Rng(seed);
            let mut channel = warmed(30 + rng.below(30) as usize);
            for _ in 0..400 {
                match rng.below(20) {
                    0..=7 => {
                        let count = 1 + rng.below(4) as usize;
                        channel.append(count);
                    }
                    8 => channel.append(8 + rng.below(8) as usize),
                    9 | 10 => {
                        // Retokenize, delete or restripe a row in place.
                        let mut backend = channel.backend_rows();
                        if !backend.is_empty() {
                            let index = rng.below(backend.len() as u64) as usize;
                            let change = rng.below(3);
                            let row = &mut backend[index];
                            match change {
                                0 => row.content = Arc::from(format!("edit {}", rng.next())),
                                1 => row.flags.is_deleted = true,
                                _ => row.flags.is_alternate_row = !row.flags.is_alternate_row,
                            }
                            channel.snapshot.replace_snapshot_from_backend(backend);
                        }
                    }
                    11 => {
                        // A block or moderation commit removing front rows.
                        let mut backend = channel.backend_rows();
                        let removed = (1 + rng.below(4) as usize).min(backend.len());
                        backend.drain(..removed);
                        channel.snapshot.replace_snapshot_from_backend(backend);
                    }
                    12 => {
                        let mut backend = channel.backend_rows();
                        if backend.len() > 2 {
                            let index = 1 + rng.below(backend.len() as u64 - 2) as usize;
                            backend.remove(index);
                            channel.snapshot.replace_snapshot_from_backend(backend);
                        }
                    }
                    13 => {
                        // Day rollover.
                        channel.clock_ms += DAY_MS;
                        channel.append(1 + rng.below(3) as usize);
                    }
                    14 => {
                        // History merge: an older row and a gap fill.
                        let mut backend = channel.backend_rows();
                        let first = backend
                            .first()
                            .map_or(channel.clock_ms, |row| row.timestamp);
                        channel.next_id += 1;
                        backend.insert(
                            0,
                            ChatMessage {
                                id: Arc::from(format!("h{}", channel.next_id)),
                                timestamp: first - 1_000 - DAY_MS * rng.below(2) as i64,
                                ..Default::default()
                            },
                        );
                        if backend.len() > 3 {
                            let index = 2 + rng.below(backend.len() as u64 - 2) as usize;
                            channel.next_id += 1;
                            let timestamp = backend[index - 1].timestamp;
                            backend.insert(
                                index,
                                ChatMessage {
                                    id: Arc::from(format!("g{}", channel.next_id)),
                                    timestamp,
                                    ..Default::default()
                                },
                            );
                        }
                        channel.snapshot.merge_history_into_snapshot(&backend);
                    }
                    15 if rng.below(10) == 0 => {
                        channel.snapshot.replace_snapshot_from_backend(Vec::new());
                    }
                    _ => {}
                }
                channel.tick(&mut rng);
                // Several syncs can land before a frame renders; now and
                // then the split stops following live for a sync.
                channel.sync_with(rng.below(4) != 0, rng.below(25) != 0);
                if rng.below(5) == 0 {
                    channel.tick(&mut rng);
                    channel.sync(true);
                }
            }
            channel.sync(true);
            front_drops += channel.split.front_drops;
            reused += channel.reused;
            caught_up += channel.split.caught_up;
        }
        assert!(front_drops > 1_000, "{front_drops}");
        assert!(reused > 100, "{reused}");
        assert!(caught_up > 10, "{caught_up}");
    }
}
