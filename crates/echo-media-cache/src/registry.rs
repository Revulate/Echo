//! Process-wide cache registries shared by every media cache layer.
//!
//! Two pieces of runtime state outlive any single layer: the deferred GPUI
//! sprite-atlas release queue, and the emote picker's published visible-media
//! sets that eviction must never reclaim. Both live here so the memory, frame,
//! and loader layers observe exactly one copy.

use gpui::{App, ImageId, RenderImage, Window};
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// GPU images whose final cache owner has been evicted.
///
/// Cache eviction can run on image-loader threads, while GPUI sprite-atlas
/// mutation must run through `App` on a window render thread. Hold the final
/// `Arc` here until either a cache owner reuses it or the next render drains it.
static PENDING_RENDER_IMAGE_DROPS: LazyLock<Mutex<HashMap<ImageId, PendingRenderImageDrop>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// GPU name-paint masks (`gpui::PaintMask` ids) whose cache owner was dropped,
/// released from every atlas with the image drops.
static PENDING_PAINT_MASK_DROPS: LazyLock<Mutex<Vec<u64>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static RENDER_IMAGE_DRAIN_BATCHES: AtomicU64 = AtomicU64::new(0);
static RENDER_IMAGES_DRAINED: AtomicU64 = AtomicU64::new(0);
static RENDER_IMAGE_BYTES_DRAINED: AtomicU64 = AtomicU64::new(0);

struct PendingRenderImageDrop {
    image: Arc<RenderImage>,
    queued_at: Instant,
    bytes: usize,
}

/// Decoded pixel bytes across every frame of `image`.
fn render_image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|frame| image.byte_len(frame))
        .sum()
}

pub fn queue_render_image_drop(image: Arc<RenderImage>) {
    let bytes = render_image_bytes(&image);
    PENDING_RENDER_IMAGE_DROPS.lock().insert(
        image.id,
        PendingRenderImageDrop {
            image,
            queued_at: Instant::now(),
            bytes,
        },
    );
}

/// Cancel a deferred atlas removal when an image is reused before the next
/// window render. This matters for the animation texture-deduplication cache,
/// whose weak reference can be upgraded while the pending-drop queue owns the
/// last strong reference.
pub fn cancel_render_image_drop(image_id: ImageId) {
    PENDING_RENDER_IMAGE_DROPS.lock().remove(&image_id);
}

/// Queue a GPU name-paint mask for removal from every sprite atlas.
pub fn queue_paint_mask_drop(mask_id: u64) {
    PENDING_PAINT_MASK_DROPS.lock().push(mask_id);
}

/// Number of final-owner image and paint-mask releases waiting for the next
/// window render.
pub fn pending_render_image_drop_count() -> usize {
    PENDING_RENDER_IMAGE_DROPS.lock().len() + PENDING_PAINT_MASK_DROPS.lock().len()
}

/// Final-owner image releases waiting for a window render.
#[derive(Clone, Copy, Debug, Default)]
pub struct PendingRenderImageDropStats {
    pub count: usize,
    /// Decoded pixel bytes still held for those images.
    pub bytes: usize,
    /// How long the oldest has waited.
    pub oldest_age: Duration,
}

/// Count, held bytes and oldest wait of the pending releases. Scans the
/// queue, so it is for periodic diagnostics only.
pub fn pending_render_image_drop_stats() -> PendingRenderImageDropStats {
    let pending = PENDING_RENDER_IMAGE_DROPS.lock();
    let now = Instant::now();
    pending.values().fold(
        PendingRenderImageDropStats {
            count: pending.len(),
            ..PendingRenderImageDropStats::default()
        },
        |mut stats, drop| {
            stats.bytes += drop.bytes;
            stats.oldest_age = stats
                .oldest_age
                .max(now.saturating_duration_since(drop.queued_at));
            stats
        },
    )
}

/// Remove evicted images from every GPUI sprite atlas.
///
/// The active window is temporarily absent from `App.windows` while rendering,
/// so it must be supplied explicitly to `drop_image`.
pub fn drain_render_image_drops(window: &mut Window, cx: &mut App) -> usize {
    let pending = std::mem::take(&mut *PENDING_RENDER_IMAGE_DROPS.lock());
    let count = pending.len();
    RENDER_IMAGE_DRAIN_BATCHES.fetch_add(1, Ordering::Relaxed);
    RENDER_IMAGES_DRAINED.fetch_add(count as u64, Ordering::Relaxed);
    for drop in pending.into_values() {
        RENDER_IMAGE_BYTES_DRAINED.fetch_add(drop.bytes as u64, Ordering::Relaxed);
        cx.drop_image(drop.image, Some(window));
    }
    let masks = std::mem::take(&mut *PENDING_PAINT_MASK_DROPS.lock());
    for mask_id in &masks {
        cx.drop_paint_mask(*mask_id, Some(window));
    }
    count + masks.len()
}

/// Cumulative `(drain_batches, images_drained)` since process start.
pub fn render_image_drain_totals() -> (u64, u64) {
    (
        RENDER_IMAGE_DRAIN_BATCHES.load(Ordering::Relaxed),
        RENDER_IMAGES_DRAINED.load(Ordering::Relaxed),
    )
}

/// Cumulative decoded pixel bytes released from the GPUI atlases by drains.
pub fn render_image_bytes_drained() -> u64 {
    RENDER_IMAGE_BYTES_DRAINED.load(Ordering::Relaxed)
}

/// Independent picker surfaces that can publish a visible decoded-media set.
///
/// Keeping the owners separate prevents an inline picker refresh from clearing
/// the popout's residency protection (or vice versa).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerViewport {
    Inline,
    Popout,
}

#[derive(Default)]
struct PickerVisibleMedia {
    /// Every URL currently needed to avoid placeholder churn, including static
    /// posters for animated cells.
    inline: HashSet<u64>,
    popout: HashSet<u64>,
    /// Full-animation variants eligible for decode/refill in each viewport.
    inline_animation: HashSet<u64>,
    popout_animation: HashSet<u64>,
    inline_published: bool,
    popout_published: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PickerVisibleMediaStats {
    pub inline_media: usize,
    pub popout_media: usize,
    pub inline_animations: usize,
    pub popout_animations: usize,
    pub inline_published: bool,
    pub popout_published: bool,
}

static PICKER_VISIBLE_MEDIA: LazyLock<RwLock<PickerVisibleMedia>> =
    LazyLock::new(|| RwLock::new(PickerVisibleMedia::default()));
static PICKER_VISIBILITY_GENERATION: AtomicU64 = AtomicU64::new(1);
static PICKER_VISIBILITY_CHANGES: LazyLock<watch::Sender<u64>> = LazyLock::new(|| {
    let (sender, _) = watch::channel(PICKER_VISIBILITY_GENERATION.load(Ordering::Acquire));
    sender
});

/// Publish a visibility mutation to picker decode tasks waiting for a scarce
/// full-decode permit. The write lock remains held by each caller while this is
/// sent, so a woken task's next snapshot cannot observe the new generation with
/// the old visible set.
fn publish_picker_visibility_change() {
    let generation = PICKER_VISIBILITY_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    PICKER_VISIBILITY_CHANGES.send_replace(generation);
}

fn picker_viewport_set(
    visible: &mut PickerVisibleMedia,
    viewport: PickerViewport,
) -> &mut HashSet<u64> {
    match viewport {
        PickerViewport::Inline => &mut visible.inline,
        PickerViewport::Popout => &mut visible.popout,
    }
}

fn picker_viewport_animation_set(
    visible: &mut PickerVisibleMedia,
    viewport: PickerViewport,
) -> &mut HashSet<u64> {
    match viewport {
        PickerViewport::Inline => &mut visible.inline_animation,
        PickerViewport::Popout => &mut visible.popout_animation,
    }
}

fn picker_viewport_published(
    visible: &mut PickerVisibleMedia,
    viewport: PickerViewport,
) -> &mut bool {
    match viewport {
        PickerViewport::Inline => &mut visible.inline_published,
        PickerViewport::Popout => &mut visible.popout_published,
    }
}

pub fn replace_picker_visible_hashes(
    viewport: PickerViewport,
    hashes: HashSet<u64>,
    animation_hashes: HashSet<u64>,
) {
    let mut visible = PICKER_VISIBLE_MEDIA.write();
    let unchanged = match viewport {
        PickerViewport::Inline => {
            visible.inline == hashes
                && visible.inline_animation == animation_hashes
                && visible.inline_published
        }
        PickerViewport::Popout => {
            visible.popout == hashes
                && visible.popout_animation == animation_hashes
                && visible.popout_published
        }
    };
    if unchanged {
        return;
    }
    *picker_viewport_set(&mut visible, viewport) = hashes;
    *picker_viewport_animation_set(&mut visible, viewport) = animation_hashes;
    *picker_viewport_published(&mut visible, viewport) = true;
    publish_picker_visibility_change();
}

pub fn clear_picker_visible_hashes(viewport: PickerViewport) {
    let mut visible = PICKER_VISIBLE_MEDIA.write();
    let unchanged = match viewport {
        PickerViewport::Inline => {
            visible.inline.is_empty()
                && visible.inline_animation.is_empty()
                && visible.inline_published
        }
        PickerViewport::Popout => {
            visible.popout.is_empty()
                && visible.popout_animation.is_empty()
                && visible.popout_published
        }
    };
    if unchanged {
        return;
    }
    picker_viewport_set(&mut visible, viewport).clear();
    picker_viewport_animation_set(&mut visible, viewport).clear();
    *picker_viewport_published(&mut visible, viewport) = true;
    publish_picker_visibility_change();
}

pub fn clear_all_picker_visible_hashes() {
    let mut visible = PICKER_VISIBLE_MEDIA.write();
    visible.inline.clear();
    visible.popout.clear();
    visible.inline_animation.clear();
    visible.popout_animation.clear();
    visible.inline_published = true;
    visible.popout_published = true;
    publish_picker_visibility_change();
}

/// Start a fresh picker-scope publication cycle.
///
/// Before the first viewport layout lands, decode admission may use the bounded
/// initial window. Once either viewport publishes—even an empty set—absence is
/// authoritative and stale picker work must be rejected.
pub fn reset_picker_visibility_publication() {
    let mut visible = PICKER_VISIBLE_MEDIA.write();
    visible.inline.clear();
    visible.popout.clear();
    visible.inline_animation.clear();
    visible.popout_animation.clear();
    visible.inline_published = false;
    visible.popout_published = false;
    publish_picker_visibility_change();
}

pub struct PickerVisibilitySnapshot {
    pub hashes: HashSet<u64>,
    pub published: bool,
    pub generation: u64,
}

/// Snapshot both the visible URL union and whether an authoritative viewport
/// publication has occurred in the current picker scope.
pub fn picker_visibility_snapshot() -> PickerVisibilitySnapshot {
    let visible = PICKER_VISIBLE_MEDIA.read();
    let mut hashes =
        HashSet::with_capacity(visible.inline_animation.len() + visible.popout_animation.len());
    hashes.extend(visible.inline_animation.iter().copied());
    hashes.extend(visible.popout_animation.iter().copied());
    PickerVisibilitySnapshot {
        hashes,
        published: visible.inline_published || visible.popout_published,
        generation: PICKER_VISIBILITY_GENERATION.load(Ordering::Acquire),
    }
}

/// Subscribe to picker viewport publications without polling. Receivers carry
/// the latest generation, so subscribing after a publication cannot miss the
/// cancellation fence.
pub fn subscribe_picker_visibility_changes() -> watch::Receiver<u64> {
    PICKER_VISIBILITY_CHANGES.subscribe()
}

/// Snapshot the union of media currently visible in either picker surface.
///
/// Cache eviction takes one snapshot per pressure pass instead of locking this
/// registry for every candidate.
pub fn picker_visible_hashes() -> HashSet<u64> {
    picker_visibility_snapshot().hashes
}

/// Snapshot every visible picker media URL, including static poster variants.
pub fn picker_visible_media_hashes() -> HashSet<u64> {
    let visible = PICKER_VISIBLE_MEDIA.read();
    let mut hashes = HashSet::with_capacity(visible.inline.len() + visible.popout.len());
    hashes.extend(visible.inline.iter().copied());
    hashes.extend(visible.popout.iter().copied());
    hashes
}

/// Counts for the opt-in process-memory diagnostic.
///
/// This deliberately reports each viewport separately so a picker window that
/// disappeared without releasing its residency ownership is visible in one log
/// line instead of being hidden by the union helpers used on hot paths.
pub fn picker_visible_media_stats() -> PickerVisibleMediaStats {
    let visible = PICKER_VISIBLE_MEDIA.read();
    PickerVisibleMediaStats {
        inline_media: visible.inline.len(),
        popout_media: visible.popout.len(),
        inline_animations: visible.inline_animation.len(),
        popout_animations: visible.popout_animation.len(),
        inline_published: visible.inline_published,
        popout_published: visible.popout_published,
    }
}

/// Fast exact-URL visibility probe for per-frame picker refill policy.
pub fn picker_url_hash_is_visible(hash: u64) -> bool {
    let visible = PICKER_VISIBLE_MEDIA.read();
    visible.inline_animation.contains(&hash) || visible.popout_animation.contains(&hash)
}

/// Run a short operation while picker visibility is stable.
///
/// Frame eviction uses this to revalidate a candidate and remove it under the
/// same registry read guard. A concurrent viewport publication therefore cannot
/// turn a candidate visible between the final check and removal.
pub fn with_picker_visible_hash_sets<R>(
    operation: impl FnOnce(&HashSet<u64>, &HashSet<u64>) -> R,
) -> R {
    let visible = PICKER_VISIBLE_MEDIA.read();
    operation(&visible.inline_animation, &visible.popout_animation)
}

/// Run a short operation while the complete picker media residency set is
/// stable, including static posters for animated cells.
///
/// Static-memory eviction uses this for its final candidate revalidation and
/// removal. Holding the read guard across both operations prevents a viewport
/// publication from making a poster visible between the check and removal.
pub fn with_picker_visible_media_hash_sets<R>(
    operation: impl FnOnce(&HashSet<u64>, &HashSet<u64>) -> R,
) -> R {
    let visible = PICKER_VISIBLE_MEDIA.read();
    operation(&visible.inline, &visible.popout)
}
