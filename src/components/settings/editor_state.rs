//! Fail-closed synchronization for transient settings-editor drafts.
//!
//! A poisoned editor may contain a partially applied mutation. Unlike runtime
//! queues and caches, retaining that inner value could persist unintended user
//! settings. Recovery therefore replaces the entire draft from an authoritative
//! snapshot, marks it for review, and rejects the event that discovered poison.

use std::sync::{Mutex, MutexGuard};

/// Blurring an untouched color input must retain inheritance after a reset.
pub(crate) fn color_input_should_commit(
    is_blur: bool,
    dirty: bool,
    draft_text: &str,
    input_text: &str,
) -> bool {
    !is_blur || dirty || draft_text != input_text
}

pub(crate) fn discard_invalid_color_input(dirty: &mut bool, invalid: &mut bool) {
    *dirty = false;
    *invalid = false;
}

#[derive(Clone)]
pub(crate) struct EditorSnapshot<T> {
    pub(crate) draft: T,
    pub(crate) generation: u64,
    pub(crate) needs_review: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorEditResult {
    Applied,
    ResetNeedsReview,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorCommit<T> {
    Ready(T),
    ResetNeedsReview,
    StaleRender,
}

struct EditorSlot<T> {
    draft: T,
    generation: u64,
    needs_review: bool,
}

pub(crate) struct EditorState<T> {
    label: &'static str,
    slot: Mutex<EditorSlot<T>>,
}

impl<T: Clone> EditorState<T> {
    pub(crate) fn new(label: &'static str, initial: T) -> Self {
        Self {
            label,
            slot: Mutex::new(EditorSlot {
                draft: initial,
                generation: 0,
                needs_review: false,
            }),
        }
    }

    fn lock_or_reset(
        &self,
        authoritative: impl FnOnce(&T) -> T,
    ) -> (MutexGuard<'_, EditorSlot<T>>, bool) {
        match self.slot.lock() {
            Ok(slot) => (slot, false),
            Err(poisoned) => {
                let slot = poisoned.into_inner();
                let previous_generation = slot.generation;
                let previous_draft = slot.draft.clone();
                drop(slot);

                let draft = authoritative(&previous_draft);
                let mut slot = match self.slot.lock() {
                    Ok(slot) => slot,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if slot.generation != previous_generation || !self.slot.is_poisoned() {
                    return (slot, true);
                }

                let generation = slot.generation.wrapping_add(1);
                *slot = EditorSlot {
                    draft,
                    generation,
                    needs_review: true,
                };
                self.slot.clear_poison();
                crate::log_ui!(warn, editor = self.label, "Reset poisoned settings editor");
                (slot, true)
            }
        }
    }

    pub(crate) fn snapshot_or_reset(
        &self,
        authoritative: impl FnOnce(&T) -> T,
    ) -> EditorSnapshot<T> {
        let (slot, _) = self.lock_or_reset(authoritative);
        EditorSnapshot {
            draft: slot.draft.clone(),
            generation: slot.generation,
            needs_review: slot.needs_review,
        }
    }

    pub(crate) fn sync_and_snapshot(
        &self,
        authoritative: impl FnOnce(&T) -> T,
        sync: impl FnOnce(&mut T) -> bool,
    ) -> EditorSnapshot<T> {
        let (mut slot, recovered) = self.lock_or_reset(authoritative);
        if !recovered && sync(&mut slot.draft) {
            slot.generation = slot.generation.wrapping_add(1);
        }
        EditorSnapshot {
            draft: slot.draft.clone(),
            generation: slot.generation,
            needs_review: slot.needs_review,
        }
    }

    pub(crate) fn apply_user_edit(
        &self,
        authoritative: impl FnOnce(&T) -> T,
        edit: impl FnOnce(&mut T),
    ) -> EditorEditResult {
        let (mut slot, recovered) = self.lock_or_reset(authoritative);
        if recovered {
            return EditorEditResult::ResetNeedsReview;
        }
        edit(&mut slot.draft);
        slot.generation = slot.generation.wrapping_add(1);
        slot.needs_review = false;
        EditorEditResult::Applied
    }

    /// Apply an explicit backend-authoritative action. If this call discovers
    /// poison, it first rebuilds the complete draft and then applies the known
    /// authoritative result instead of dropping the action like a user edit.
    pub(crate) fn apply_authoritative_update(
        &self,
        authoritative: impl FnOnce(&T) -> T,
        update: impl FnOnce(&mut T),
    ) {
        let (mut slot, _) = self.lock_or_reset(authoritative);
        update(&mut slot.draft);
        slot.generation = slot.generation.wrapping_add(1);
        slot.needs_review = false;
    }

    pub(crate) fn prepare_commit(
        &self,
        expected_generation: u64,
        authoritative: impl FnOnce(&T) -> T,
    ) -> EditorCommit<T> {
        let (slot, recovered) = self.lock_or_reset(authoritative);
        if recovered || slot.needs_review {
            return EditorCommit::ResetNeedsReview;
        }
        if slot.generation != expected_generation {
            return EditorCommit::StaleRender;
        }
        EditorCommit::Ready(slot.draft.clone())
    }

    /// [`Self::prepare_commit`] that yields the draft only when it is ready.
    pub(crate) fn prepare_ready(
        &self,
        expected_generation: u64,
        authoritative: impl FnOnce(&T) -> T,
    ) -> Option<T> {
        match self.prepare_commit(expected_generation, authoritative) {
            EditorCommit::Ready(draft) => Some(draft),
            EditorCommit::ResetNeedsReview | EditorCommit::StaleRender => None,
        }
    }

    pub(crate) fn replace_from_authority(&self, draft: T) {
        let mut slot = match self.slot.lock() {
            Ok(slot) => slot,
            Err(poisoned) => {
                let slot = poisoned.into_inner();
                self.slot.clear_poison();
                crate::log_ui!(
                    warn,
                    editor = self.label,
                    "Replaced poisoned settings editor"
                );
                slot
            }
        };
        let generation = slot.generation.wrapping_add(1);
        *slot = EditorSlot {
            draft,
            generation,
            needs_review: false,
        };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_color_change_then_two_blurs_keeps_inheritance_and_allows_sync() {
        let canonical = "#FFFFFF";
        let mut dirty = true;
        let mut invalid = true;
        assert!(super::color_input_should_commit(true, dirty, "#GG", "#GG"));
        super::discard_invalid_color_input(&mut dirty, &mut invalid);
        assert!(!super::color_input_should_commit(
            true, dirty, canonical, canonical
        ));
        assert!(!super::color_input_should_commit(
            true, dirty, canonical, canonical
        ));
        assert!(!dirty && !invalid);
    }
    #[test]
    fn inherited_color_reset_survives_untouched_blur() {
        assert!(!super::color_input_should_commit(
            true, false, "#FFFFFF", "#FFFFFF"
        ));
        assert!(super::color_input_should_commit(
            true, true, "#FFFFFF", "#FFFFFF"
        ));
        assert!(super::color_input_should_commit(
            true, false, "#FFFFFF", "#AAAAAA"
        ));
        assert!(super::color_input_should_commit(
            false, false, "#FFFFFF", "#FFFFFF"
        ));
    }
    use super::*;
    use core::prelude::v1::test;
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Draft(String);

    fn poison(state: &Arc<EditorState<Draft>>) {
        let state = state.clone();
        let result = std::thread::spawn(move || {
            let mut slot = state.slot.lock().unwrap();
            slot.draft.0.push_str(" partial");
            panic!("poison editor slot");
        })
        .join();
        assert!(result.is_err());
    }

    #[test]
    fn poison_discards_partial_draft_and_blocks_commit() {
        let state = Arc::new(EditorState::new("test", Draft("initial".into())));
        poison(&state);

        let snapshot = state.snapshot_or_reset(|_| Draft("authoritative".into()));
        assert_eq!(snapshot.draft, Draft("authoritative".into()));
        assert!(snapshot.needs_review);
        assert_eq!(
            state.prepare_commit(snapshot.generation, |_| Draft("authoritative".into())),
            EditorCommit::ResetNeedsReview
        );
    }

    #[test]
    fn event_that_discovers_poison_is_not_replayed() {
        let state = Arc::new(EditorState::new("test", Draft("initial".into())));
        poison(&state);

        assert_eq!(
            state.apply_user_edit(
                |_| Draft("authoritative".into()),
                |draft| draft.0.push_str(" event")
            ),
            EditorEditResult::ResetNeedsReview
        );
        assert_eq!(
            state.snapshot_or_reset(|_| Draft("unused".into())).draft,
            Draft("authoritative".into())
        );

        assert_eq!(
            state.apply_user_edit(
                |_| Draft("unused".into()),
                |draft| draft.0.push_str(" fresh")
            ),
            EditorEditResult::Applied
        );
        let snapshot = state.snapshot_or_reset(|_| Draft("unused".into()));
        assert_eq!(snapshot.draft, Draft("authoritative fresh".into()));
        assert!(!snapshot.needs_review);
    }

    #[test]
    fn stale_render_cannot_commit_and_authoritative_replace_is_ready() {
        let state = EditorState::new("test", Draft("initial".into()));
        let stale = state.snapshot_or_reset(|_| Draft("unused".into()));
        state.apply_user_edit(
            |_| Draft("unused".into()),
            |draft| draft.0.push_str(" newer"),
        );
        assert_eq!(
            state.prepare_commit(stale.generation, |_| Draft("unused".into())),
            EditorCommit::StaleRender
        );

        state.replace_from_authority(Draft("saved".into()));
        let current = state.snapshot_or_reset(|_| Draft("unused".into()));
        assert_eq!(current.draft, Draft("saved".into()));
        assert!(!current.needs_review);
        assert_eq!(
            state.prepare_commit(current.generation, |_| Draft("unused".into())),
            EditorCommit::Ready(Draft("saved".into()))
        );
    }

    #[test]
    fn authoritative_action_applies_after_poison_recovery() {
        let state = Arc::new(EditorState::new("test", Draft("initial".into())));
        poison(&state);

        state.apply_authoritative_update(
            |_| Draft("authoritative".into()),
            |draft| draft.0.push_str(" action"),
        );
        let snapshot = state.snapshot_or_reset(|_| Draft("unused".into()));
        assert_eq!(snapshot.draft, Draft("authoritative action".into()));
        assert!(!snapshot.needs_review);
    }
}
