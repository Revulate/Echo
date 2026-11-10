//! Observer drain recovery policy.
//!
//! When the background observer queue overflows, the drained event list is no
//! longer a complete ordered stream. The UI must recover from the backend
//! snapshot instead of applying the partial queue.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserverDrainAction {
    ApplyQueuedEvents,
    RefreshFromBackendSnapshot,
}

#[inline]
pub fn observer_drain_action(dropped_count: u64) -> ObserverDrainAction {
    if dropped_count > 0 {
        ObserverDrainAction::RefreshFromBackendSnapshot
    } else {
        ObserverDrainAction::ApplyQueuedEvents
    }
}
