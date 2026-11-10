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

/// How many rows a same-length snapshot dropped from the front of `order`:
/// `Some(n)` when `next` starts with `order[n..]` and `0 < n < next.len()`.
pub(super) fn same_count_rotation(
    order: &[SharedString],
    next: &[Arc<ChatMessage>],
) -> Option<usize> {
    let first_new_id = next.first()?.id.as_ref();
    let rotated = order.iter().position(|id| id.as_ref() == first_new_id)?;
    (rotated > 0 && rotated < next.len() && ids_align(&order[rotated..], next)).then_some(rotated)
}
