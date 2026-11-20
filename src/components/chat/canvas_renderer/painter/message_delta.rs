//! Pure message-buffer delta classification for `set_messages`.

use gpui::SharedString;
use std::sync::Arc;

use crate::backend::ChatMessage;

/// Minimum first snapshot size where estimated-height seeding is cheaper than a full rebuild.
const INITIAL_SNAPSHOT_FAST_PATH_MIN_MESSAGES: usize = 32;

/// The model update `apply_messages` makes for the next snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateKind {
    /// The same list, or a same-length one with nothing to apply.
    Unchanged,
    /// The same ids in the same order, some rows replaced in place.
    SameOrder,
    /// The current rows followed by new ones.
    Append,
    /// The first snapshot of a split, large enough to seed with estimates.
    InitialSnapshot,
    /// The current rows, contiguous, from `existing_start` on, with rows
    /// before them and possibly after them (a history merge).
    InsertAround { existing_start: usize },
    /// Any other shape: a front drop with or without an append when the
    /// model can apply one, otherwise a rebuild.
    Reshape(Reshape),
}

/// Which kind of change led to [`UpdateKind::Reshape`], for its log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reshape {
    Grew,
    SameCount,
    Shrank,
}

pub(super) struct MessageDeltaPlan {
    pub(super) kind: UpdateKind,
    pub(super) content_changed: bool,
    pub(super) count_increased: bool,
    pub(super) had_new_messages: bool,
    pub(super) old_order_matches_prefix: bool,
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

        let kind = if count_increased {
            if old_order_matches_prefix {
                UpdateKind::Append
            } else if is_initial_snapshot_fast_path {
                UpdateKind::InitialSnapshot
            } else if old_order_matches_suffix {
                UpdateKind::InsertAround {
                    existing_start: added_count,
                }
            } else if let Some(existing_start) = old_order_middle_start {
                UpdateKind::InsertAround { existing_start }
            } else {
                UpdateKind::Reshape(Reshape::Grew)
            }
        } else if new_count == old_count && content_changed && new_count > 0 {
            if message_order.len() == new_count && ids_align(message_order, next_messages) {
                UpdateKind::SameOrder
            } else {
                UpdateKind::Reshape(Reshape::SameCount)
            }
        } else if new_count < old_count {
            UpdateKind::Reshape(Reshape::Shrank)
        } else {
            UpdateKind::Unchanged
        };

        Self {
            kind,
            content_changed,
            count_increased,
            had_new_messages,
            old_order_matches_prefix,
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

/// Index of the first of `next`'s last rows that `known` does not hold: the
/// rows that arrived after every row the split already had. `None` when the
/// last row is known.
pub(super) fn first_trailing_new_row(
    next: &[Arc<ChatMessage>],
    known: impl Fn(&str) -> bool,
) -> Option<usize> {
    let start = next
        .iter()
        .rposition(|message| known(message.id.as_ref()))
        .map_or(0, |index| index + 1);
    (start < next.len()).then_some(start)
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
    use super::super::model::MessageListModel;
    use super::super::state::{LayoutRuntimeState, PreloadState};
    use super::*;
    use crate::components::chat::chat_view::shown_rows_are_current;
    use core::prelude::v1::test;
    use echo_chat_render::layout::message_content_fingerprint;
    use echo_core::chat_snapshot::ChatSnapshot;
    use echo_core::reveal_pacer::RevealPacer;
    use rustc_hash::FxHashMap;
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
    fn trailing_new_rows_start_after_the_last_known_row() {
        let known = |id: &str| ["a", "b", "c"].contains(&id);
        assert_eq!(
            first_trailing_new_row(&rows(&["a", "x", "c", "y", "z"]), known),
            Some(3)
        );
        assert_eq!(first_trailing_new_row(&rows(&["b", "x", "c"]), known), None);
        assert_eq!(first_trailing_new_row(&rows(&["x", "y"]), known), Some(0));
        assert_eq!(first_trailing_new_row(&rows(&[]), known), None);
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

    /// The height a row lays out to: it depends on the id and on what layout
    /// reads from the row, so a row replaced in place can change height.
    fn true_height(message: &ChatMessage) -> f32 {
        let hash = message.id.bytes().fold(7u32, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
        (hash % 40 + 12) as f32
            + (message.content.len() % 4) as f32 * 7.0
            + if message.flags.is_deleted { 3.0 } else { 0.0 }
    }

    /// A model built from scratch with every row at its true height: what a
    /// split converges to once nothing is pending or stale.
    fn rebuilt(next: &Rows) -> MessageListModel {
        let mut model = MessageListModel::initial();
        model.messages = next.clone();
        let heights: Vec<f32> = next.iter().map(|message| true_height(message)).collect();
        model.reset_rows(next, &heights);
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
                expected.get_message_index(&message.id),
                "{context}: index of row {index}"
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
    fn the_model_refuses_a_front_drop_outside_its_guards() {
        let base: Rows = Arc::new(rows(&["a", "b", "c", "d"]));
        let fresh = || rebuilt(&base);

        let mut model = fresh();
        let appended = rows(&["e", "f"]);
        assert!(model.drop_front_and_append(1, &appended, 0));
        let next: Rows = Arc::new(base[1..].iter().cloned().chain(appended).collect());
        for message in &next[3..] {
            model.height_tree.push(true_height(message));
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

    /// The reviewer's case: two appends in one frame, then a history merge
    /// that both prepends and appends (a gap fill), before the frame renders.
    /// The merge must add its appended rows to the rows already waiting for
    /// heights rather than replace their count.
    #[test]
    fn a_history_merge_after_two_appends_in_one_frame_keeps_the_waiting_rows() {
        let base: Rows = Arc::new(rows(&["a", "b", "c"]));
        let mut model = rebuilt(&base);
        let mut pending = 0usize;
        let classify = |model: &MessageListModel, next: &Rows| {
            MessageDeltaPlan::classify(
                &model.messages,
                &model.message_order,
                model.height_tree.len(),
                next,
            )
        };

        let a: Rows = Arc::new(rows(&["a", "b", "c", "d"]));
        assert_eq!(classify(&model, &a).kind, UpdateKind::Append);
        model.append_rows(&a, &mut pending);
        model.messages = a.clone();

        let b: Rows = Arc::new(a.iter().cloned().chain(rows(&["e"])).collect());
        assert_eq!(
            classify(&model, &b).kind,
            UpdateKind::Reshape(Reshape::Grew)
        );
        let front = model.front_drop_shape(&b, &mut pending).expect("an append");
        assert_eq!(model.apply_front_drop(&b, front, &mut pending), Some(false));
        model.messages = b.clone();
        assert_eq!(pending, 2, "d and e wait for heights");

        let c: Rows = Arc::new(
            rows(&["h"])
                .into_iter()
                .chain(b.iter().cloned())
                .chain(rows(&["f"]))
                .collect(),
        );
        let UpdateKind::InsertAround { existing_start } = classify(&model, &c).kind else {
            panic!("a history merge around the rows");
        };
        let heights: Vec<f32> = c[..existing_start].iter().map(|m| true_height(m)).collect();
        model.insert_around(&c, existing_start, &heights, &mut pending);
        model.messages = c.clone();
        assert_eq!(pending, 3, "d, e and f wait for heights");

        // update_layouts' pending-row pass.
        let start = model.message_order.len() - pending;
        for message in &c[start..] {
            model.height_tree.push(true_height(message));
        }
        model.sync_total_height();
        assert_same_model(&model, &rebuilt(&c), "after the pending-row pass");
    }

    /// Row-index state a split keeps beside its model, standing in for the
    /// prepared paint rows, the preload and layout ranges and the
    /// stale-layout cursor: each records the row it named when set.
    #[derive(Default)]
    struct IndexState {
        prepared: Vec<(usize, SharedString)>,
        /// The last row of each range when it was set.
        range_ends: Vec<SharedString>,
        stale_cursor_row: Option<SharedString>,
    }

    /// One split's model, synced through the same `MessageDeltaPlan`
    /// decision and `MessageListModel` steps `apply_messages` uses, with
    /// `update_layouts` modelled around a layout cache of true heights.
    struct Split {
        model: MessageListModel,
        pending: usize,
        needs_layout_rebuild: bool,
        stale: bool,
        width_refresh: bool,
        /// Laid-out rows: id to (content fingerprint, height).
        layouts: FxHashMap<Arc<str>, (u64, f32)>,
        /// Rows whose async layout was requested (history priming).
        primed: Vec<Arc<str>>,
        preload: PreloadState,
        runtime: LayoutRuntimeState,
        index_state: IndexState,
        front_drops: usize,
        rebuilds: usize,
        caught_up: usize,
        merges: usize,
    }

    const ESTIMATED_HEIGHT: f32 = 42.0;

    impl Split {
        fn new() -> Self {
            Self {
                model: MessageListModel::initial(),
                pending: 0,
                needs_layout_rebuild: false,
                stale: false,
                width_refresh: false,
                layouts: FxHashMap::default(),
                primed: Vec::new(),
                preload: PreloadState::initial(),
                runtime: LayoutRuntimeState::initial(800.0, 1.0),
                index_state: IndexState::default(),
                front_drops: 0,
                rebuilds: 0,
                caught_up: 0,
                merges: 0,
            }
        }

        fn lay_out(&mut self, message: &ChatMessage) -> f32 {
            let height = true_height(message);
            self.layouts.insert(
                Arc::clone(&message.id),
                (message_content_fingerprint(message), height),
            );
            height
        }

        /// `cached_or_estimated_height`: a cached layout's height whether or
        /// not it is stale for the row, else the estimate.
        fn cached_or_estimated(&self, rows: &[Arc<ChatMessage>]) -> Vec<f32> {
            rows.iter()
                .map(|message| {
                    self.layouts
                        .get(&message.id)
                        .map_or(ESTIMATED_HEIGHT, |&(_, height)| height)
                })
                .collect()
        }

        fn rebuild(&mut self, next: &Rows) {
            let heights = self.cached_or_estimated(next);
            self.model.reset_rows(next, &heights);
            self.pending = 0;
            self.needs_layout_rebuild = false;
            self.stale = true;
            self.width_refresh = true;
            self.preload.reset_index_ranges();
            self.index_state.prepared.clear();
            self.index_state.stale_cursor_row = None;
            self.rebuilds += 1;
        }

        /// `mark_stale`, which restarts the stale-layout scan.
        fn mark_stale(&mut self) {
            self.stale = true;
            self.runtime.reset_stale_scan();
            self.index_state.stale_cursor_row = None;
        }

        fn front_dropped(&mut self, dropped: usize) {
            let state = &mut self.index_state;
            state.prepared = remap_after_front_drop(&state.prepared, dropped, |row| &mut row.0);
            if self.runtime.stale_layout_refresh_cursor < dropped {
                state.stale_cursor_row = None;
            }
            self.preload.shift_after_front_drop(dropped);
            self.runtime.shift_stale_scan_after_front_drop(dropped);
        }

        /// `apply_messages`' model work.
        fn sync(&mut self, next: &Rows) {
            if Arc::ptr_eq(&self.model.messages, next) {
                return;
            }
            let plan = MessageDeltaPlan::classify(
                &self.model.messages,
                &self.model.message_order,
                self.model.height_tree.len(),
                next,
            );
            let (old_count, new_count) = (self.model.messages.len(), next.len());
            let mut pending = self.pending;
            match plan.kind {
                UpdateKind::Unchanged => {}
                UpdateKind::SameOrder => {
                    self.model.rebuild_message_index_from_order();
                    self.mark_stale();
                }
                UpdateKind::Append => {
                    if self.model.kept_rows_changed(0, &next[..old_count]) {
                        self.mark_stale();
                    }
                    self.model.append_rows(next, &mut pending);
                    self.preload.note_new_rows(old_count);
                }
                UpdateKind::InitialSnapshot => {
                    self.rebuild(next);
                    pending = 0;
                    self.primed
                        .extend(next.iter().rev().take(192).map(|m| Arc::clone(&m.id)));
                }
                UpdateKind::InsertAround { existing_start } => {
                    let heights = self.cached_or_estimated(&next[..existing_start]);
                    self.model
                        .insert_around(next, existing_start, &heights, &mut pending);
                    self.stale = true;
                    self.preload.reset_index_ranges();
                    self.index_state.prepared.clear();
                    self.index_state.stale_cursor_row = None;
                    self.primed.extend(
                        next[..existing_start]
                            .iter()
                            .rev()
                            .take(192)
                            .map(|m| Arc::clone(&m.id)),
                    );
                    self.merges += 1;
                }
                UpdateKind::Reshape(_) => {
                    let before = pending;
                    let front = self.model.front_drop_shape(next, &mut pending);
                    self.caught_up += usize::from(pending > before);
                    let applied = front.and_then(|front| {
                        self.model
                            .apply_front_drop(next, front, &mut pending)
                            .map(|kept_changed| (front, kept_changed))
                    });
                    match applied {
                        Some((front, kept_changed)) => {
                            if kept_changed {
                                self.mark_stale();
                            }
                            if front.dropped > 0 {
                                self.front_dropped(front.dropped);
                            }
                            if front.appended > 0 {
                                self.preload.note_new_rows(new_count - front.appended);
                            }
                            self.front_drops += 1;
                        }
                        None => {
                            self.rebuild(next);
                            pending = 0;
                        }
                    }
                }
            }
            self.pending = pending;
            self.model.messages = next.clone();
        }

        /// `queue_layout_rebuild` (a width-bucket change), which drops the
        /// pending count, or `invalidate_local_layouts`, which keeps it.
        fn queue_layout_rebuild(&mut self, drop_pending: bool) {
            self.needs_layout_rebuild = true;
            self.stale = true;
            if drop_pending {
                self.pending = 0;
            }
        }

        /// `update_layouts`: the queued-rebuild check, the pending-row pass
        /// (which returns), the append pass (which returns), the full
        /// rebuild of a mismatched order, then the stale refresh; async
        /// layouts land by id.
        fn render(&mut self) {
            let messages = self.model.messages.clone();
            let n = messages.len();
            if n == 0 {
                // Nothing left to lay out or refresh.
                self.model.message_order.clear();
                self.model.height_tree.clear();
                self.model.sync_total_height();
                self.pending = 0;
                self.stale = false;
                self.width_refresh = false;
                return;
            }
            if std::mem::take(&mut self.needs_layout_rebuild) {
                if self.model.order_and_heights_match(self.pending) {
                    self.width_refresh = true;
                } else {
                    self.model.message_order.clear();
                    self.model.height_tree.clear();
                    self.pending = 0;
                }
            }
            for id in std::mem::take(&mut self.primed) {
                if let Some(index) = self.model.get_message_index(&id)
                    && index < self.model.height_tree.len()
                {
                    let height = self.lay_out(&messages[index]);
                    self.model.height_tree.update(index, height);
                }
            }
            if self.pending > 0 {
                let start = self.model.message_order.len() - self.pending;
                for message in &messages[start..] {
                    let height = self.lay_out(message);
                    self.model.height_tree.push(height);
                }
                self.pending = 0;
            } else if !self.model.message_order.is_empty()
                && self.model.message_order.len() < n
                && self.model.message_order[0].as_ref() == messages[0].id.as_ref()
            {
                for message in &messages[self.model.message_order.len()..] {
                    self.model
                        .message_order
                        .push(SharedString::from(message.id.clone()));
                    let height = self.lay_out(message);
                    self.model.height_tree.push(height);
                }
            } else if self.model.message_order.len() != n || self.model.height_tree.len() != n {
                let heights: Vec<f32> = messages.iter().map(|m| self.lay_out(m)).collect();
                self.model.reset_rows(&messages, &heights);
                self.stale = false;
                self.width_refresh = false;
            } else if self.stale || self.width_refresh {
                for (index, message) in messages.iter().enumerate() {
                    let fingerprint = message_content_fingerprint(message);
                    let height = match self.layouts.get(&message.id) {
                        Some(&(cached, height)) if cached == fingerprint => {
                            if !self.width_refresh {
                                continue;
                            }
                            height
                        }
                        None if !self.width_refresh => continue,
                        _ => self.lay_out(message),
                    };
                    self.model.height_tree.update(index, height);
                }
                self.stale = false;
                self.width_refresh = false;
            }
            self.model.sync_total_height();
            // Nothing a render leaves is short of heights for the rows the
            // order holds.
            assert_eq!(
                self.model.height_tree.len(),
                self.model.message_order.len(),
                "heights after a render"
            );
            self.check_index_state();
        }

        /// Every piece of index state still names the row it named.
        fn check_index_state(&mut self) {
            let order = &self.model.message_order;
            for (index, id) in &self.index_state.prepared {
                assert_eq!(order.get(*index), Some(id), "prepared row {index}");
            }
            let ranges = [
                self.preload.emote_scan.range(),
                self.preload.embed_scan.range(),
                self.preload.last_layout_image_preload_range,
                self.preload.last_layout_lookahead_range,
                self.preload.last_layout_landing_range,
            ];
            for (range, end_row) in ranges.iter().zip(&self.index_state.range_ends) {
                if let Some((start, end)) = *range {
                    assert!(start < end && end <= order.len(), "range {start}..{end}");
                    assert_eq!(&order[end - 1], end_row, "last row of {start}..{end}");
                }
            }
            if let Some(row) = &self.index_state.stale_cursor_row {
                let cursor = self.runtime.stale_layout_refresh_cursor;
                assert_eq!(order.get(cursor), Some(row), "stale-scan cursor");
            }
            // Set them again on the rows this render shows.
            let len = order.len();
            self.index_state.prepared = (len.saturating_sub(20)..len)
                .map(|index| (index, order[index].clone()))
                .collect();
            let range = (len.saturating_sub(25), len);
            let now = Instant::now();
            self.preload
                .emote_scan
                .rows_to_scan(range, 0, Duration::ZERO, 64, now);
            self.preload
                .embed_scan
                .rows_to_scan(range, 0, Duration::ZERO, 48, now);
            self.preload.last_layout_image_preload_range = Some(range);
            self.preload.last_layout_lookahead_range = Some(range);
            self.preload.last_layout_landing_range = Some(range);
            self.index_state.range_ends = if len > 0 {
                vec![order[len - 1].clone(); 5]
            } else {
                Vec::new()
            };
            let cursor = len.saturating_sub(30);
            self.runtime.stale_layout_refresh_cursor = cursor;
            self.index_state.stale_cursor_row = order.get(cursor).cloned();
        }

        /// Render until nothing is pending or stale, then compare with a
        /// model built from scratch.
        fn settle(&mut self, context: &str) {
            for _ in 0..4 {
                self.render();
            }
            assert!(self.pending == 0 && !self.stale && !self.width_refresh);
            assert_same_model(&self.model, &rebuilt(&self.model.messages), context);
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
        /// The split hides deleted rows (its deleted-message mode).
        hide_deleted: bool,
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
                hide_deleted: false,
            }
        }

        fn live_row(&mut self, prefix: &str) -> ChatMessage {
            self.next_id += 1;
            self.clock_ms += 50;
            ChatMessage {
                id: Arc::from(format!("{prefix}{}", self.next_id)),
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
            let batch = (0..count).map(|_| self.live_row("m")).collect();
            self.snapshot.append_messages_to_snapshot(batch);
        }

        /// What `ChatView::paced_messages` and the split's deleted-row
        /// projection hand the canvas.
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
            let next = if self.hide_deleted && next.iter().any(|m| m.flags.is_deleted) {
                Arc::new(
                    next.iter()
                        .filter(|m| !m.flags.is_deleted)
                        .cloned()
                        .collect(),
                )
            } else {
                next
            };
            self.split.sync(&next);
            if render {
                self.split.render();
            }
        }

        fn sync(&mut self, render: bool) {
            self.sync_with(render, true);
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
        channel.split.settle("warmed");
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
            channel.split.settle("cap");
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
        let mut totals = [0usize; 5];
        let mut caught_up_when_shown = 0usize;
        for seed in 0..48 {
            let mut rng = Rng(seed);
            // Most seeds run at a small cap; some stay below it, where a
            // history merge keeps its older rows.
            let capacity = if seed % 4 == 1 {
                10_000
            } else {
                30 + rng.below(30) as usize
            };
            let mut channel = warmed(capacity);
            channel.hide_deleted = seed % 3 == 0;
            // A hidden split takes many syncs without a frame.
            let mut hidden_for = 0u32;
            // A hidden split skips the syncs and takes the latest snapshot,
            // all rows revealed, when it is shown.
            let mut skipped_for = 0u32;
            for step in 0..400 {
                match rng.below(24) {
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
                                0 => row.content = Arc::from("e".repeat(1 + step % 5)),
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
                        // History merge: an older row, a gap fill, and
                        // sometimes newer rows.
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
                        if backend.len() > 3 && rng.below(2) == 0 {
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
                        for _ in 0..rng.below(3) {
                            backend.push(channel.live_row("n"));
                        }
                        channel.snapshot.merge_history_into_snapshot(&backend);
                    }
                    15 if rng.below(10) == 0 => {
                        channel.snapshot.replace_snapshot_from_backend(Vec::new());
                    }
                    16 => {
                        // An optimistic local echo, later replaced in place
                        // by the row with its real id.
                        let echo = channel.live_row("local-");
                        channel.snapshot.append_messages_to_snapshot(vec![echo]);
                    }
                    17 => {
                        let mut backend = channel.backend_rows();
                        if let Some(row) = backend
                            .iter_mut()
                            .rev()
                            .find(|row| row.id.starts_with("local-"))
                        {
                            row.id = Arc::from(row.id.replacen("local-", "real-", 1));
                            channel.snapshot.replace_snapshot_from_backend(backend);
                        }
                    }
                    18 => {
                        // A duplicated id.
                        let mut backend = channel.backend_rows();
                        if let Some(source) =
                            backend.get(rng.below(backend.len().max(1) as u64) as usize)
                        {
                            let mut copy = source.clone();
                            copy.timestamp = channel.clock_ms;
                            backend.push(copy);
                            channel.snapshot.replace_snapshot_from_backend(backend);
                        }
                    }
                    19 => channel.split.queue_layout_rebuild(rng.below(2) == 0),
                    20 => hidden_for = 10 + rng.below(30) as u32,
                    22 if skipped_for == 0 => skipped_for = 5 + rng.below(60) as u32,
                    21 => {
                        // Two appends, then a merge with an older and a newer
                        // row, all before a frame.
                        for _ in 0..2 {
                            channel.append(1);
                            channel.tick(&mut rng);
                            channel.sync_with(false, false);
                        }
                        let mut backend = channel.backend_rows();
                        let first = backend
                            .first()
                            .map_or(channel.clock_ms, |row| row.timestamp);
                        channel.next_id += 1;
                        backend.insert(
                            0,
                            ChatMessage {
                                id: Arc::from(format!("h{}", channel.next_id)),
                                timestamp: first - 1_000,
                                ..Default::default()
                            },
                        );
                        backend.push(channel.live_row("n"));
                        channel.snapshot.merge_history_into_snapshot(&backend);
                    }
                    _ => {}
                }
                channel.tick(&mut rng);
                if skipped_for > 0 {
                    skipped_for -= 1;
                    if skipped_for == 0 {
                        channel.sync_with(true, false);
                        channel
                            .split
                            .settle(&format!("seed {seed} step {step} shown"));
                        caught_up_when_shown += 1;
                    }
                    continue;
                }
                let render = hidden_for == 0 && rng.below(4) != 0;
                hidden_for = hidden_for.saturating_sub(1);
                // Several syncs can land before a frame renders; now and
                // then the split stops following live for a sync.
                channel.sync_with(render, rng.below(25) != 0);
                if hidden_for == 0 && rng.below(5) == 0 {
                    channel.tick(&mut rng);
                    channel.sync(true);
                }
                if hidden_for == 0 && rng.below(8) == 0 {
                    channel.split.settle(&format!("seed {seed} step {step}"));
                }
            }
            channel.sync(true);
            channel.split.settle(&format!("seed {seed} end"));
            let split = &channel.split;
            for (total, count) in totals.iter_mut().zip([
                split.front_drops,
                channel.reused,
                split.caught_up,
                split.merges,
                split.rebuilds,
            ]) {
                *total += count;
            }
        }
        let [front_drops, reused, caught_up, merges, _rebuilds] = totals;
        assert!(front_drops > 1_000, "{front_drops}");
        assert!(reused > 100, "{reused}");
        assert!(caught_up > 10, "{caught_up}");
        assert!(merges > 50, "{merges}");
        assert!(caught_up_when_shown > 100, "{caught_up_when_shown}");
    }
}
