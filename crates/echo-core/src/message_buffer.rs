//! Circular buffer for chat messages with FIFO eviction
//!
//! Implements a bounded message buffer that automatically evicts
//! oldest messages when capacity is reached.
//!
use crate::message::ChatMessage;
use rustc_hash::FxHashMap;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

/// Buffer capacity used when the root has not installed its retention sizing.
///
/// Matches Echo's `config::MAX_CACHED_MESSAGES` so a standalone
/// `echo-core` build (crate tests, verification tools) retains the same number
/// of messages the application does.
pub const DEFAULT_MESSAGE_CAPACITY: usize = 1_000;

static PORTS: OnceLock<MessageBufferPorts> = OnceLock::new();

/// Root lookups the buffer cannot reach on its own.
///
/// Every port is optional: without an installed set the buffer keeps its
/// default capacity and simply skips the layout-cache and embed bookkeeping
/// that only exists inside the running application.
#[derive(Clone, Copy)]
pub struct MessageBufferPorts {
    /// Retention sizing for an active channel.
    pub default_capacity: fn() -> usize,
    /// Queue one evicted message id for deferred layout-cache reclaim.
    pub queue_evicted_id: fn(Arc<str>),
    /// Queue a batch of evicted message ids under a single queue lock.
    pub queue_evicted_ids: fn(Vec<Arc<str>>),
    /// Drop embed expansion state belonging to a removed message id.
    pub prune_message_id: fn(&str),
}

/// Install the root's message-buffer ports. Idempotent; the first call wins.
pub fn install_message_buffer_ports(ports: MessageBufferPorts) {
    let _ = PORTS.set(ports);
}

#[inline]
fn active_channel_capacity() -> usize {
    PORTS
        .get()
        .map_or(DEFAULT_MESSAGE_CAPACITY, |ports| (ports.default_capacity)())
}

#[inline]
fn queue_evicted_id(id: Arc<str>) {
    if let Some(ports) = PORTS.get() {
        (ports.queue_evicted_id)(id);
    }
}

#[inline]
fn queue_evicted_ids(ids: Vec<Arc<str>>) {
    if let Some(ports) = PORTS.get() {
        (ports.queue_evicted_ids)(ids);
    }
}

#[inline]
fn prune_message_id(id: &str) {
    if let Some(ports) = PORTS.get() {
        (ports.prune_message_id)(id);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageReconcileAction {
    Keep,
    Mutated,
    Remove,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageReconcileResult {
    pub removed: usize,
    pub mutated: usize,
}

/// Circular buffer for messages with automatic FIFO eviction
///
#[derive(Clone, Debug)]
pub struct MessageBuffer {
    /// Internal storage using VecDeque for efficient push/pop at both ends
    messages: VecDeque<ChatMessage>,
    /// Maximum capacity before eviction
    capacity: usize,
    /// Version counter - increments on every mutation
    version: u64,
    /// Cached observer snapshot, valid while `version` matches.
    ///
    /// PERF: observer events (history load, gap fill, retokenize) each need
    /// a snapshot; the cached `Arc` makes repeat snapshots of an unchanged
    /// buffer free instead of deep-cloning up to 10k `ChatMessage`s per
    /// event. The extra retention is negligible because observers hold the
    /// latest snapshot `Arc` anyway.
    snapshot: Option<(u64, Arc<Vec<ChatMessage>>)>,
    /// Zebra parity for the next pushed message. Stored on the buffer (and
    /// persisted onto each message's flags) so front-pruning at capacity does
    /// not flip the parity of every remaining row.
    next_alternate: bool,
    /// How many buffered rows carry each id.
    ///
    /// PERF: every live PRIVMSG asks whether its id is already buffered (a
    /// history merge can land the same row first). Without the index that
    /// question walks every resident row and misses almost always. Counts
    /// rather than a set because nothing forbids a caller from pushing the
    /// same id twice; removing one copy must not hide the other.
    id_counts: FxHashMap<Arc<str>, u32>,
}

impl MessageBuffer {
    /// Create a new message buffer with the given capacity
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            // Retention is a limit, not a reason to allocate an entire quiet
            // channel's scrollback before any messages arrive.
            messages: VecDeque::new(),
            capacity,
            version: 0,
            snapshot: None,
            next_alternate: false,
            id_counts: FxHashMap::default(),
        }
    }

    fn index_id(&mut self, id: &Arc<str>) {
        *self.id_counts.entry(Arc::clone(id)).or_insert(0) += 1;
    }

    fn unindex_id(&mut self, id: &str) {
        if let Some(count) = self.id_counts.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                self.id_counts.remove(id);
            }
        }
    }

    /// Rebuild the id index after a bulk change to `messages`.
    fn rebuild_id_index(&mut self) {
        self.id_counts.clear();
        for message in &self.messages {
            *self.id_counts.entry(Arc::clone(&message.id)).or_insert(0) += 1;
        }
        self.id_counts.shrink_to(self.messages.len());
    }

    /// Create a buffer with default capacity from config
    pub fn with_default_capacity() -> Self {
        Self::new(active_channel_capacity())
    }

    /// Invalidate the cached snapshot (called on mutation)
    #[inline]
    fn invalidate_cache(&mut self) {
        self.version = self.version.wrapping_add(1);
        self.snapshot = None;
    }

    /// Get an `Arc` snapshot of all messages, reusing the cached one when the
    /// buffer has not changed since the last call.
    pub fn snapshot(&mut self) -> Arc<Vec<ChatMessage>> {
        if let Some((version, snapshot)) = self.snapshot.as_ref()
            && *version == self.version
        {
            return Arc::clone(snapshot);
        }
        let snapshot: Arc<Vec<ChatMessage>> = Arc::new(self.messages.iter().cloned().collect());
        self.snapshot = Some((self.version, Arc::clone(&snapshot)));
        snapshot
    }

    /// Reassign zebra parity across the whole buffer after a merge or replace
    /// changed message order. Anchored at the newest message's existing flag
    /// so the visible bottom rows keep their parity; older rows alternate
    /// backward from it. Also re-anchors the parity used by the next `push`.
    fn reassign_alternate_rows(&mut self) {
        let Some(anchor) = self.messages.back().map(|m| m.flags.is_alternate_row) else {
            self.next_alternate = false;
            return;
        };
        let mut parity = anchor;
        for message in self.messages.iter_mut().rev() {
            message.flags.is_alternate_row = parity;
            parity = !parity;
        }
        self.next_alternate = !anchor;
    }

    /// Add a single message to the buffer
    ///
    /// If at capacity, the oldest message is evicted.
    #[inline]
    pub fn push(&mut self, mut message: ChatMessage) {
        message.flags.is_alternate_row = self.next_alternate;
        self.next_alternate = !self.next_alternate;
        if self.messages.len() >= self.capacity
            && let Some(evicted) = self.messages.pop_front()
        {
            self.unindex_id(evicted.id.as_ref());
            // Hand the dropped id to the layout-cache reclaim queue so its
            // stale layouts are freed on the next maintenance pass (deferred
            // to keep this hot path off the global cache's full-map scan).
            queue_evicted_id(evicted.id);
        } else if self.messages.len() == self.messages.capacity() {
            self.grow_storage();
        }
        self.index_id(&message.id);
        self.messages.push_back(message);
        self.invalidate_cache();
    }

    /// Push a clone of `message` and stamp the zebra parity it was given onto
    /// the caller's copy too.
    ///
    /// Live rows reach the UI as the copy broadcast in `NewMessage`, which the
    /// chat snapshot appends as-is. Without the stamp that copy keeps its
    /// default `false` and every live row draws the same background until a
    /// full refresh from this buffer re-reads the real parity.
    pub fn push_stamped(&mut self, message: &mut ChatMessage) {
        message.flags.is_alternate_row = self.next_alternate;
        self.push(message.clone());
    }

    #[cold]
    #[inline(never)]
    fn grow_storage(&mut self) {
        // Keep allocation off the steady-state insertion path. Quiet channels
        // start with 32 rows; busier channels reserve their full retention in
        // one step to avoid repeated reallocations during bursts/history fill.
        let target = if self.messages.is_empty() {
            self.capacity.min(32)
        } else {
            self.capacity
        };
        self.messages.reserve_exact(target - self.messages.len());
    }

    /// Replace all messages (used when syncing from backend)
    ///
    /// PERF: Uses atomic swap instead of clear-then-rebuild to avoid race
    /// conditions where renders could see an empty buffer mid-replacement.
    pub fn replace_all(&mut self, messages: Vec<ChatMessage>) {
        // Build new VecDeque first, then swap atomically (no observable empty state)
        let start = if messages.len() > self.capacity {
            messages.len() - self.capacity
        } else {
            0
        };
        let mut new_messages = VecDeque::with_capacity(self.capacity.min(messages.len()));
        new_messages.extend(messages.into_iter().skip(start));

        let retained_ids: HashSet<&str> = new_messages
            .iter()
            .map(|message| message.id.as_ref())
            .collect();
        queue_evicted_ids(
            self.messages
                .iter()
                .filter(|message| !retained_ids.contains(message.id.as_ref()))
                .map(|message| message.id.clone())
                .collect(),
        );

        // Atomic swap - messages is never empty between states
        self.messages = new_messages;
        self.rebuild_id_index();
        self.reassign_alternate_rows();
        self.invalidate_cache();
    }

    /// Merge incoming messages with existing buffer, deduplicating by ID
    /// This is safer than replace_all when history may race with new messages.
    ///
    /// PERF: Uses O(N) two-pointer merge when possible, falling back to O(N log N)
    /// sort only when history overlaps with existing messages.
    ///
    /// IMPORTANT: We drain FIRST, then build the dedup set. This ensures the dedup set
    /// matches exactly what we drained, avoiding race conditions where messages could
    /// arrive between building the ID set and draining.
    pub fn merge_history(&mut self, incoming: Vec<ChatMessage>) {
        if incoming.is_empty() {
            return;
        }
        self.merge_history_rows(incoming);
        self.rebuild_id_index();
    }

    fn merge_history_rows(&mut self, incoming: Vec<ChatMessage>) {
        use std::collections::HashSet;

        let incoming_count = incoming.len();

        // Step 1: Drain existing messages FIRST
        let existing: Vec<ChatMessage> = self.messages.drain(..).collect();
        let existing_count = existing.len();

        // Early return: if no existing messages, just use incoming
        if existing.is_empty() {
            let start = incoming_count.saturating_sub(self.capacity);
            self.messages = incoming.into_iter().skip(start).collect();
            self.reassign_alternate_rows();
            self.invalidate_cache();
            crate::log_irc!(
                debug,
                "merge_history: no existing, using {} incoming messages",
                self.messages.len()
            );
            return;
        }

        // Get timestamp bounds for fast-path detection
        let existing_earliest = existing.first().map(|m| m.timestamp).unwrap_or(i64::MAX);
        let existing_latest = existing.last().map(|m| m.timestamp).unwrap_or(i64::MIN);
        let incoming_earliest = incoming
            .iter()
            .map(|m| m.timestamp)
            .min()
            .unwrap_or(i64::MAX);
        let incoming_latest = incoming
            .iter()
            .map(|m| m.timestamp)
            .max()
            .unwrap_or(i64::MIN);

        // Build dedup set from existing
        let existing_ids: HashSet<&str> = existing.iter().map(|m| m.id.as_ref()).collect();

        // Filter incoming to only new messages, also dropping duplicate IDs that
        // appear more than once within the same incoming payload (Arc<str> clone
        // is a cheap refcount bump, not an allocation).
        let mut seen_incoming_ids: HashSet<Arc<str>> = HashSet::new();
        let new_incoming: Vec<ChatMessage> = incoming
            .into_iter()
            .filter(|m| {
                !existing_ids.contains(m.id.as_ref()) && seen_incoming_ids.insert(m.id.clone())
            })
            .collect();
        let new_from_incoming = new_incoming.len();

        if new_incoming.is_empty() {
            // All incoming were duplicates, restore existing
            self.messages = existing.into_iter().collect();
            self.invalidate_cache();
            crate::log_irc!(
                debug,
                "merge_history: all {} incoming were duplicates, keeping {} existing",
                incoming_count,
                self.messages.len()
            );
            return;
        }

        // FAST PATH: History is purely older than existing (most common case)
        // This is O(N) - just prepend history to existing
        if incoming_latest < existing_earliest {
            let mut merged = new_incoming;
            merged.extend(existing);
            // Truncate from front to respect capacity
            let start = merged.len().saturating_sub(self.capacity);
            self.messages = merged.into_iter().skip(start).collect();
            self.reassign_alternate_rows();
            self.invalidate_cache();
            crate::log_irc!(
                debug,
                "merge_history (fast-path prepend): existing={}, new_unique={}, final={}",
                existing_count,
                new_from_incoming,
                self.messages.len()
            );
            return;
        }

        // FAST PATH: History is purely newer than existing (rare but possible)
        // This is O(N) - just append history to existing
        if incoming_earliest > existing_latest {
            let mut merged = existing;
            merged.extend(new_incoming);
            // Truncate from front to respect capacity
            let start = merged.len().saturating_sub(self.capacity);
            self.messages = merged.into_iter().skip(start).collect();
            self.reassign_alternate_rows();
            self.invalidate_cache();
            crate::log_irc!(
                debug,
                "merge_history (fast-path append): existing={}, new_unique={}, final={}",
                existing_count,
                new_from_incoming,
                self.messages.len()
            );
            return;
        }

        // SLOW PATH: Overlapping timestamps - need O(N) two-pointer merge
        // Both existing and incoming should be mostly sorted, but we sort incoming
        // to guarantee it (API returns in order, but we filter which may not preserve order)
        let mut sorted_incoming = new_incoming;
        sorted_incoming.sort_by_key(|m| m.timestamp);

        // O(N) two-pointer merge
        let mut merged = Vec::with_capacity(existing.len() + sorted_incoming.len());
        let mut i = 0; // existing index
        let mut j = 0; // incoming index

        while i < existing.len() && j < sorted_incoming.len() {
            if existing[i].timestamp <= sorted_incoming[j].timestamp {
                merged.push(existing[i].clone());
                i += 1;
            } else {
                merged.push(sorted_incoming[j].clone());
                j += 1;
            }
        }

        // Append remaining from whichever has leftovers
        while i < existing.len() {
            merged.push(existing[i].clone());
            i += 1;
        }
        while j < sorted_incoming.len() {
            merged.push(sorted_incoming[j].clone());
            j += 1;
        }

        // Truncate from front to respect capacity
        let start = merged.len().saturating_sub(self.capacity);
        self.messages = merged.into_iter().skip(start).collect();
        self.reassign_alternate_rows();
        self.invalidate_cache();

        crate::log_irc!(
            debug,
            "merge_history (two-pointer merge): existing={}, incoming={}, new_unique={}, final={}",
            existing_count,
            incoming_count,
            new_from_incoming,
            self.messages.len()
        );
    }

    /// Get number of messages
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the buffer has no messages
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Iterate over all messages
    pub fn iter(&self) -> impl Iterator<Item = &ChatMessage> {
        self.messages.iter()
    }

    /// Iterate over the newest messages first, capped by `limit`.
    pub fn iter_recent(&self, limit: usize) -> impl Iterator<Item = &ChatMessage> {
        self.messages.iter().rev().take(limit)
    }

    /// Get index of message by ID
    pub fn index_of(&self, id: &str) -> Option<usize> {
        if !self.id_counts.contains_key(id) {
            return None;
        }
        self.messages.iter().position(|m| m.id.as_ref() == id)
    }

    /// Get index of message by ID, scanning from the newest end.
    ///
    /// PERF: Live messages sit at the tail of a buffer that can hold 10k
    /// entries, so the per-live-message retokenize lookup should not walk the
    /// whole history first.
    pub fn rindex_of(&self, id: &str) -> Option<usize> {
        if !self.id_counts.contains_key(id) {
            return None;
        }
        self.messages.iter().rposition(|m| m.id.as_ref() == id)
    }

    /// Update a message at a specific index (for re-tokenization)
    ///
    /// Returns true if the update was successful (index valid and ID matches).
    /// This is used when emotes load and messages need to be re-tokenized.
    pub fn update_at(&mut self, index: usize, updated_msg: ChatMessage) -> bool {
        if let Some(existing) = self.messages.get_mut(index)
            && existing.id == updated_msg.id
        {
            // Parity belongs to the buffer position, and `updated_msg` may come
            // from a snapshot taken before a merge or prune re-striped the rows.
            let alternate = existing.flags.is_alternate_row;
            *existing = updated_msg;
            existing.flags.is_alternate_row = alternate;
            self.invalidate_cache();
            return true;
        }
        false
    }

    /// Mutate a message by ID.
    pub fn update_by_id<F>(&mut self, id: &str, update: F) -> bool
    where
        F: FnOnce(&mut ChatMessage),
    {
        if !self.id_counts.contains_key(id) {
            return false;
        }
        // Rows updated by id are almost always recent, so search from the
        // newest end.
        let Some(existing) = self.messages.iter_mut().rev().find(|m| m.id.as_ref() == id) else {
            return false;
        };
        update(existing);
        if existing.id.as_ref() != id {
            let new_id = Arc::clone(&existing.id);
            self.unindex_id(id);
            self.index_id(&new_id);
        }
        self.invalidate_cache();
        true
    }

    /// Remove a message by ID.
    pub fn remove_by_id(&mut self, id: &str) -> bool {
        if let Some(index) = self.index_of(id) {
            self.messages.remove(index);
            self.unindex_id(id);
            prune_message_id(id);
            self.reassign_alternate_rows();
            self.invalidate_cache();
            return true;
        }
        false
    }

    /// Retain messages matching `keep`, returning the number removed.
    ///
    /// Bulk policy changes (for example, an authenticated Twitch block-list
    /// refresh) use this to prune rows in one mutation and one observer
    /// snapshot instead of repeatedly scanning and invalidating the buffer.
    pub fn retain<F>(&mut self, mut keep: F) -> usize
    where
        F: FnMut(&ChatMessage) -> bool,
    {
        let mut evicted_ids = Vec::new();
        self.messages.retain(|message| {
            let retained = keep(message);
            if !retained {
                evicted_ids.push(message.id.clone());
            }
            retained
        });
        let removed = evicted_ids.len();
        if removed != 0 {
            for id in &evicted_ids {
                self.unindex_id(id.as_ref());
            }
            // Hold the global eviction queue lock once for the whole policy
            // prune instead of once per removed message.
            queue_evicted_ids(evicted_ids);
            self.reassign_alternate_rows();
            self.invalidate_cache();
        }
        removed
    }

    /// Reconcile rows in one mutation-aware pass. Policy changes use this to
    /// remove blocked senders and redact reply previews without cloning the
    /// complete buffer or rescanning by ID.
    pub fn reconcile<F>(&mut self, mut reconcile: F) -> MessageReconcileResult
    where
        F: FnMut(&mut ChatMessage) -> MessageReconcileAction,
    {
        let mut evicted_ids = Vec::new();
        let mut result = MessageReconcileResult::default();
        self.messages
            .retain_mut(|message| match reconcile(message) {
                MessageReconcileAction::Keep => true,
                MessageReconcileAction::Mutated => {
                    result.mutated += 1;
                    true
                }
                MessageReconcileAction::Remove => {
                    result.removed += 1;
                    evicted_ids.push(message.id.clone());
                    false
                }
            });
        if result.removed != 0 {
            queue_evicted_ids(evicted_ids);
            self.reassign_alternate_rows();
        }
        if result.removed != 0 || result.mutated != 0 {
            // A mutation may rewrite a row's id; this is a rare policy path.
            self.rebuild_id_index();
            self.invalidate_cache();
        }
        result
    }

    /// Change the buffer capacity, evicting oldest messages when shrinking.
    ///
    /// Returns true when visible message contents changed. Capacity increases do
    /// not invalidate snapshots because the current message sequence is
    /// unchanged. Storage grows on insertion, not when the limit increases.
    pub fn set_capacity(&mut self, target_capacity: usize) -> bool {
        let target_capacity = target_capacity.max(1);
        let old_capacity = self.capacity;
        self.capacity = target_capacity;

        if self.messages.len() > target_capacity {
            let to_remove = self.messages.len() - target_capacity;
            // Queue the dropped ids so their layouts are reclaimed from the
            // global layout cache on the next maintenance pass.
            let evicted: Vec<Arc<str>> = self
                .messages
                .drain(0..to_remove)
                .map(|message| message.id)
                .collect();
            for id in &evicted {
                self.unindex_id(id.as_ref());
            }
            queue_evicted_ids(evicted);
            self.messages.shrink_to_fit();
            self.invalidate_cache();
            return true;
        }

        if target_capacity < old_capacity {
            self.messages.shrink_to_fit();
        }

        false
    }

    /// Get the current capacity of the buffer
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get the latest message ID (most recent message)
    pub fn latest_id(&self) -> Option<&str> {
        self.messages.back().map(|m| m.id.as_ref())
    }

    /// Whether a message with `id` is currently buffered.
    pub fn contains_id(&self, id: &str) -> bool {
        self.id_counts.contains_key(id)
    }

    /// Get a message by index
    pub fn get(&self, index: usize) -> Option<&ChatMessage> {
        self.messages.get(index)
    }
}

impl Default for MessageBuffer {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

impl From<Vec<ChatMessage>> for MessageBuffer {
    fn from(messages: Vec<ChatMessage>) -> Self {
        let mut buffer = Self::with_default_capacity();
        buffer.replace_all(messages);
        buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_message(id: &str) -> ChatMessage {
        ChatMessage {
            id: Arc::from(id),
            ..Default::default()
        }
    }

    #[test]
    fn test_push_and_eviction() {
        let mut buffer = MessageBuffer::new(3);

        buffer.push(make_test_message("1"));
        buffer.push(make_test_message("2"));
        buffer.push(make_test_message("3"));
        assert_eq!(buffer.len(), 3);

        // Should evict oldest
        buffer.push(make_test_message("4"));
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.get(0).unwrap().id.as_ref(), "2");
        assert_eq!(buffer.get(2).unwrap().id.as_ref(), "4");
    }

    #[test]
    fn push_stamped_gives_the_broadcast_copy_the_stored_parity() {
        let mut buffer = MessageBuffer::new(10);
        let mut stamped = Vec::new();
        for id in ["1", "2", "3", "4"] {
            let mut message = make_test_message(id);
            buffer.push_stamped(&mut message);
            stamped.push(message.flags.is_alternate_row);
        }
        let stored: Vec<bool> = buffer.iter().map(|m| m.flags.is_alternate_row).collect();
        assert_eq!(stamped, stored);
        assert_eq!(stamped, [false, true, false, true]);
    }

    #[test]
    fn edits_and_removals_keep_strict_alternation() {
        let mut buffer = MessageBuffer::new(10);
        for id in ["1", "2", "3", "4"] {
            buffer.push(make_test_message(id));
        }
        // A retokenized copy from an older snapshot must not restore its parity.
        let mut stale = make_test_message("2");
        stale.flags.is_alternate_row = false;
        assert!(buffer.update_at(1, stale));
        assert!(buffer.get(1).unwrap().flags.is_alternate_row);

        assert!(buffer.remove_by_id("3"));
        let stripes: Vec<bool> = buffer.iter().map(|m| m.flags.is_alternate_row).collect();
        assert!(stripes.windows(2).all(|pair| pair[0] != pair[1]));
        assert_eq!(stripes.last(), Some(&true));
    }

    fn assert_index_matches(buffer: &MessageBuffer) {
        let mut expected: FxHashMap<Arc<str>, u32> = FxHashMap::default();
        for message in buffer.iter() {
            *expected.entry(Arc::clone(&message.id)).or_insert(0) += 1;
        }
        assert_eq!(buffer.id_counts, expected);
    }

    #[test]
    fn id_index_follows_every_mutation() {
        let mut buffer = MessageBuffer::new(4);
        for id in ["1", "2", "3", "4", "5"] {
            buffer.push(make_test_message(id));
        }
        assert_index_matches(&buffer);
        assert!(!buffer.contains_id("1"));
        assert!(buffer.contains_id("5"));
        assert_eq!(buffer.index_of("1"), None);

        // A duplicate push keeps both copies findable until both are gone.
        buffer.push(make_test_message("5"));
        assert_index_matches(&buffer);
        assert!(buffer.remove_by_id("5"));
        assert!(buffer.contains_id("5"));
        assert_index_matches(&buffer);

        // An update that rewrites the id moves the index entry.
        assert!(buffer.update_by_id("4", |row| row.id = Arc::from("real-4")));
        assert!(!buffer.contains_id("4"));
        assert!(buffer.contains_id("real-4"));
        assert!(!buffer.update_by_id("missing", |_| {}));
        assert_index_matches(&buffer);

        buffer.merge_history(vec![make_test_message("0"), make_test_message("3")]);
        assert_index_matches(&buffer);

        assert_eq!(buffer.retain(|row| row.id.as_ref() != "3"), 1);
        assert_index_matches(&buffer);

        buffer.reconcile(|row| {
            if row.id.as_ref() == "real-4" {
                row.id = Arc::from("4");
                MessageReconcileAction::Mutated
            } else {
                MessageReconcileAction::Keep
            }
        });
        assert!(buffer.contains_id("4"));
        assert_index_matches(&buffer);

        buffer.set_capacity(1);
        assert_eq!(buffer.len(), 1);
        assert_index_matches(&buffer);

        // Capacity is now 1, so the replacement keeps only the newest row.
        buffer.replace_all(vec![make_test_message("a"), make_test_message("b")]);
        assert_index_matches(&buffer);
        assert!(!buffer.contains_id("a"));
        assert_eq!(buffer.rindex_of("b"), Some(0));
    }

    #[test]
    fn test_latest_id() {
        let mut buffer = MessageBuffer::new(10);
        assert!(buffer.latest_id().is_none());

        buffer.push(make_test_message("msg1"));
        assert_eq!(buffer.latest_id(), Some("msg1"));

        buffer.push(make_test_message("msg2"));
        assert_eq!(buffer.latest_id(), Some("msg2"));
    }
}
