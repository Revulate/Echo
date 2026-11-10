//! Process-wide "an image became ready" signal for overlay repaints.
//!
//! The image loader bumps one generation counter; each consumer remembers the
//! generation it last saw, so every consumer observes every signal no matter
//! which of them polls first.

use std::sync::atomic::{AtomicU64, Ordering};

static IMAGE_READY_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Who is asking; each consumer tracks its own last-seen generation.
#[derive(Clone, Copy, Debug)]
pub enum ImageReadyConsumer {
    /// The polling coordinator, which repaints the open pickers.
    App,
    InlinePicker,
    PopoutPicker,
}

static SEEN: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Record that an image finished loading.
pub fn notify_image_loaded() {
    IMAGE_READY_GENERATION.fetch_add(1, Ordering::Release);
    crate::managers::frame_rate_controller::record_image_ready();
}

/// Record that a refill window landed for an already-registered animation.
///
/// Overlays still repaint, but steady playback is not activity: it must not
/// reset the idle clock that `notify_image_loaded` resets.
pub fn notify_animation_refilled() {
    IMAGE_READY_GENERATION.fetch_add(1, Ordering::Release);
}

/// Whether an image became ready since `consumer` last asked.
///
/// Chat canvas refreshes are split-local via visible URL signatures; the app
/// consumer must not translate this global overlay signal into a root notify.
pub fn take_image_ready(consumer: ImageReadyConsumer) -> bool {
    let current = IMAGE_READY_GENERATION.load(Ordering::Acquire);
    SEEN[consumer as usize].swap(current, Ordering::AcqRel) != current
}
