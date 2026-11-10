//! Row-level signals of the two picker surfaces.

use super::shared::pack_visible_range;
use crate::backend::Emote;
use crate::backend::lock_utils::MutexExt;
use crate::cache::loader as image_cache;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const NO_HOVERED_EMOTE: usize = usize::MAX;

/// Row-level events raised from `uniform_list` callbacks, which cannot use
/// `cx.listener`, and drained by the owning picker's next render.
///
/// Clicks own the exact rendered emote instead of a flat-list index: searches,
/// provider publications and category changes can all reorder that list before
/// the next render consumes the click.
pub(super) struct PickerSignals {
    viewport: image_cache::PickerViewport,
    favorite_click: Mutex<Option<Arc<Emote>>>,
    header_click: Mutex<Option<String>>,
    load_more_click: AtomicBool,
    /// Set by every signal so an idle render locks none of the slots above.
    pending: AtomicBool,
    /// The visible row range, packed so a reader never sees a start/end pair
    /// from two different `uniform_list` callbacks.
    visible_range: AtomicU64,
    hovered_emote: AtomicUsize,
}

impl PickerSignals {
    pub(super) const fn new(viewport: image_cache::PickerViewport) -> Self {
        Self {
            viewport,
            favorite_click: Mutex::new(None),
            header_click: Mutex::new(None),
            load_more_click: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            visible_range: AtomicU64::new(0),
            hovered_emote: AtomicUsize::new(NO_HOVERED_EMOTE),
        }
    }

    pub(super) fn viewport(&self) -> image_cache::PickerViewport {
        self.viewport
    }

    pub(super) fn signal_visible_range(&self, start: usize, end: usize) {
        self.visible_range
            .store(pack_visible_range(start, end), Ordering::Relaxed);
    }

    pub(super) fn visible_range(&self) -> (usize, usize) {
        let packed = self.visible_range.load(Ordering::Relaxed);
        (((packed >> 32) as u32) as usize, (packed as u32) as usize)
    }

    /// Forget the viewport and the hover, and drop this surface's cache
    /// residency protection.
    pub(super) fn reset_visible_range(&self) {
        self.visible_range.store(0, Ordering::Relaxed);
        self.clear_hovered_emote();
        image_cache::clear_picker_visible_urls(self.viewport);
    }

    pub(super) fn hovered_emote(&self) -> Option<usize> {
        let index = self.hovered_emote.load(Ordering::Relaxed);
        (index != NO_HOVERED_EMOTE).then_some(index)
    }

    pub(super) fn clear_hovered_emote(&self) {
        self.hovered_emote
            .store(NO_HOVERED_EMOTE, Ordering::Relaxed);
    }

    /// Update the cell whose animation is admitted in Hover mode; returns
    /// whether it changed.
    ///
    /// A conditional clear prevents a delayed leave event from the previous
    /// cell from stopping the animation of the newly hovered cell.
    pub(super) fn signal_hovered_emote(
        &self,
        index: usize,
        is_animated: bool,
        hovered: bool,
    ) -> bool {
        if hovered {
            let next = if is_animated { index } else { NO_HOVERED_EMOTE };
            return self.hovered_emote.swap(next, Ordering::Relaxed) != next;
        }

        is_animated
            && self
                .hovered_emote
                .compare_exchange(
                    index,
                    NO_HOVERED_EMOTE,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
    }

    pub(super) fn mark_pending(&self) {
        self.pending.store(true, Ordering::SeqCst);
    }

    /// Consume the pending flag; `false` means no signal arrived since the last take.
    pub(super) fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::SeqCst)
    }

    pub(super) fn signal_favorite_click(&self, emote: Arc<Emote>) {
        *self.favorite_click.lock_or_recover() = Some(emote);
        self.mark_pending();
    }

    pub(super) fn signal_header_click(&self, header_id: String) {
        *self.header_click.lock_or_recover() = Some(header_id);
        self.mark_pending();
    }

    pub(super) fn signal_load_more_click(&self) {
        self.load_more_click.store(true, Ordering::SeqCst);
        self.mark_pending();
    }

    pub(super) fn take_favorite_click(&self) -> Option<Arc<Emote>> {
        self.favorite_click.lock_or_recover().take()
    }

    pub(super) fn take_header_click(&self) -> Option<String> {
        self.header_click.lock_or_recover().take()
    }

    pub(super) fn take_load_more_click(&self) -> bool {
        self.load_more_click.swap(false, Ordering::SeqCst)
    }

    pub(super) fn clear_pending_actions(&self) {
        *self.favorite_click.lock_or_recover() = None;
        *self.header_click.lock_or_recover() = None;
        self.load_more_click.store(false, Ordering::SeqCst);
        self.pending.store(false, Ordering::SeqCst);
    }
}

pub(super) static INLINE_SIGNALS: PickerSignals =
    PickerSignals::new(image_cache::PickerViewport::Inline);
pub(super) static POPOUT_SIGNALS: PickerSignals =
    PickerSignals::new(image_cache::PickerViewport::Popout);
