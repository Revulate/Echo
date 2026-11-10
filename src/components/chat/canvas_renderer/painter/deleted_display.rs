//! Per-split presentation of moderator-removed rows.
//!
//! The backend keeps every removed row intact and only marks it deleted. Each
//! split projects the shared snapshot through its own [`DeletedMessageMode`]
//! (the global setting unless the split overrides it), so one channel can fade
//! its removed rows in one split and hide them in another without either
//! touching the other's layouts: a presentation change swaps in a derived row
//! whose content fingerprint differs, and the shared layout cache keeps the
//! two shapes apart.

use super::*;

use crate::backend::deleted_message_display::DELETED_MESSAGE_PLACEHOLDER;
use crate::backend::settings::DeletedMessageMode;
use crate::backend::slots::{MessageSlot, compute_structure_hash};
use echo_core::message::DeletedPresentation;
use rustc_hash::FxHashSet;

pub(super) struct DeletedDisplayState {
    mode_override: Option<DeletedMessageMode>,
    /// Collapsed rows the user expanded; they keep the original text.
    revealed: FxHashSet<Arc<str>>,
    reveal_epoch: u64,
    /// The shared snapshot as the split received it.
    source: Arc<Vec<Arc<ChatMessage>>>,
    applied: Option<AppliedProjection>,
    derived: FxHashMap<Arc<str>, DerivedRow>,
}

struct AppliedProjection {
    mode: DeletedMessageMode,
    reveal_epoch: u64,
    source: Arc<Vec<Arc<ChatMessage>>>,
    output: Arc<Vec<Arc<ChatMessage>>>,
}

/// A deleted row as one split paints it, kept so an unchanged row keeps its
/// pointer identity from frame to frame.
struct DerivedRow {
    original: Arc<ChatMessage>,
    /// `None` shows the row as if it were never removed.
    presentation: Option<DeletedPresentation>,
    message: Arc<ChatMessage>,
}

impl DeletedDisplayState {
    pub(super) fn initial() -> Self {
        Self {
            mode_override: None,
            revealed: FxHashSet::default(),
            reveal_epoch: 0,
            source: Arc::new(Vec::new()),
            applied: None,
            derived: FxHashMap::default(),
        }
    }

    fn project(&mut self, mode: DeletedMessageMode) -> Arc<Vec<Arc<ChatMessage>>> {
        if let Some(applied) = &self.applied
            && applied.mode == mode
            && applied.reveal_epoch == self.reveal_epoch
            && Arc::ptr_eq(&applied.source, &self.source)
        {
            return Arc::clone(&applied.output);
        }

        let output = if self.source.iter().any(|msg| msg.flags.is_deleted) {
            self.project_deleted(mode)
        } else {
            self.derived.clear();
            Arc::clone(&self.source)
        };
        self.applied = Some(AppliedProjection {
            mode,
            reveal_epoch: self.reveal_epoch,
            source: Arc::clone(&self.source),
            output: Arc::clone(&output),
        });
        output
    }

    fn project_deleted(&mut self, mode: DeletedMessageMode) -> Arc<Vec<Arc<ChatMessage>>> {
        let mut previous = std::mem::take(&mut self.derived);
        let mut output = Vec::with_capacity(self.source.len());
        for msg in self.source.iter() {
            if !msg.flags.is_deleted {
                output.push(Arc::clone(msg));
                continue;
            }
            let presentation = match mode {
                DeletedMessageMode::Dim => {
                    output.push(Arc::clone(msg));
                    continue;
                }
                DeletedMessageMode::Hide => continue,
                DeletedMessageMode::Strikethrough => Some(DeletedPresentation::Struck),
                DeletedMessageMode::Disable => None,
                DeletedMessageMode::Replace => {
                    if self.revealed.contains(&msg.id) {
                        output.push(Arc::clone(msg));
                        continue;
                    }
                    Some(DeletedPresentation::Collapsed)
                }
            };
            let row = match previous.remove(&msg.id) {
                Some(row)
                    if Arc::ptr_eq(&row.original, msg) && row.presentation == presentation =>
                {
                    row
                }
                _ => DerivedRow {
                    original: Arc::clone(msg),
                    presentation,
                    message: Arc::new(derive_row(msg, presentation)),
                },
            };
            output.push(Arc::clone(&row.message));
            self.derived.insert(Arc::clone(&msg.id), row);
        }
        Arc::new(output)
    }
}

/// The row a split paints for `msg` under `presentation`.
fn derive_row(msg: &ChatMessage, presentation: Option<DeletedPresentation>) -> ChatMessage {
    let mut row = msg.clone();
    match presentation {
        None => row.flags.is_deleted = false,
        Some(DeletedPresentation::Collapsed) => {
            let slots = vec![MessageSlot::text(DELETED_MESSAGE_PLACEHOLDER)];
            row.structure_hash = compute_structure_hash(&slots);
            row.slots = slots.into();
            row.content = Arc::from(DELETED_MESSAGE_PLACEHOLDER);
            row.twitch_emotes_tag = None;
            row.twitch_gifs_tag = None;
            row.reply_emote_offset = 0;
            row.flags.deleted_presentation = DeletedPresentation::Collapsed;
        }
        Some(presentation) => row.flags.deleted_presentation = presentation,
    }
    row
}

impl CanvasMessageList {
    /// The shared snapshot this split last received, before its deleted-row
    /// presentation was applied.
    #[inline]
    pub fn source_messages(&self) -> &Arc<Vec<Arc<ChatMessage>>> {
        &self.deleted_display.source
    }

    /// This split's own deleted-row mode, if it overrides the global setting.
    #[inline]
    pub fn deleted_mode_override(&self) -> Option<DeletedMessageMode> {
        self.deleted_display.mode_override
    }

    /// Override (or with `None`, follow) the global deleted-row mode for this
    /// split. Returns whether the effective presentation changed.
    pub fn set_deleted_mode_override(&mut self, mode: Option<DeletedMessageMode>) -> bool {
        if self.deleted_display.mode_override == mode {
            return false;
        }
        let before = self.effective_deleted_mode();
        self.deleted_display.mode_override = mode;
        // A new mode starts from collapsed rows again.
        if !self.deleted_display.revealed.is_empty() {
            self.deleted_display.revealed.clear();
            self.deleted_display.reveal_epoch += 1;
        }
        self.reproject_deleted_messages();
        before != self.effective_deleted_mode()
    }

    #[inline]
    pub(super) fn effective_deleted_mode(&self) -> DeletedMessageMode {
        self.deleted_display
            .mode_override
            .unwrap_or_else(settings::get_deleted_message_mode)
    }

    /// Project the stored snapshot again when the global setting moved under a
    /// split that follows it. Cheap enough for every frame.
    pub(super) fn sync_deleted_display(&mut self) {
        let mode = self.effective_deleted_mode();
        if self
            .deleted_display
            .applied
            .as_ref()
            .is_some_and(|applied| applied.mode != mode)
        {
            self.reproject_deleted_messages();
        }
    }

    pub(super) fn is_collapsed_deleted(&self, message_id: &str) -> bool {
        self.get_message(message_id).is_some_and(|msg| {
            msg.flags.is_deleted && msg.flags.deleted_presentation == DeletedPresentation::Collapsed
        })
    }

    /// Expand a collapsed deleted row to its original text.
    pub(super) fn reveal_deleted_message(&mut self, message_id: &str) -> bool {
        let Some(original) = self.original_message(message_id) else {
            return false;
        };
        if !self
            .deleted_display
            .revealed
            .insert(Arc::clone(&original.id))
        {
            return false;
        }
        self.deleted_display.reveal_epoch += 1;
        self.reproject_deleted_messages();
        true
    }

    /// The row as the backend stored it, for copy and reply actions that must
    /// not see a collapsed stand-in.
    pub(in crate::components::chat::canvas_renderer) fn original_message(
        &self,
        message_id: &str,
    ) -> Option<Arc<ChatMessage>> {
        match self.deleted_display.derived.get(message_id) {
            Some(row) => Some(Arc::clone(&row.original)),
            None => self
                .message_model
                .get_message_index(message_id)
                .and_then(|idx| self.message_model.messages.get(idx).cloned()),
        }
    }

    pub(super) fn set_source_messages(&mut self, source: Arc<Vec<Arc<ChatMessage>>>) {
        self.deleted_display.source = source;
        let projected = self.project_deleted_messages();
        self.apply_messages(projected);
    }

    fn project_deleted_messages(&mut self) -> Arc<Vec<Arc<ChatMessage>>> {
        let mode = self.effective_deleted_mode();
        self.deleted_display.project(mode)
    }

    fn reproject_deleted_messages(&mut self) {
        let projected = self.project_deleted_messages();
        if !Arc::ptr_eq(&projected, &self.message_model.messages) {
            self.apply_messages(projected);
        }
    }
}
