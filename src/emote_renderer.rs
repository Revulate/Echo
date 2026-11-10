//! Emote animation scheduling and shared frame state.
//!
//! A global clock keeps visible animations synchronized. Its interval is
//! selected dynamically by the scheduler rather than fixed per emote. The clock
//! itself is GPUI-free and lives in `echo_render_core::animation_clock`; this
//! module keeps the renderer-side census of which animated emotes are on screen.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{Level, span};

pub use echo_render_core::animation_clock::{
    animation_enabled, animation_stats, current_animation_frame, current_animation_interval_ms,
    current_fps, set_animation_enabled, set_animation_interval_ms, should_throttle_offscreen,
    tick_animation,
};

use crate::cache::frame_layer::AnimationMetadata;

/// One renderer's visible animated emotes from its latest render.
struct OwnerMetadata {
    owner: u64,
    /// Animation frame when this list was last rebuilt.
    frame: u64,
    metadata: Vec<Arc<AnimationMetadata>>,
}

/// Owner of registrations made outside a chat split's render (composer
/// previews, user cards). Cleared at every frame boundary, restarted on each
/// new animation frame, and stale like any owner once nothing registers.
const UNATTRIBUTED_OWNER: u64 = 0;

/// Bound on distinct unattributed registrations per animation frame.
const UNATTRIBUTED_CAP: usize = 256;

/// Scheduler interval refresh cadence, in frame boundaries.
const INTERVAL_REFRESH_FRAMES: u64 = 5;

thread_local! {
    /// Per-split census. Splits keep their list until they render again, so
    /// a split whose view was cached this frame still counts, and the union
    /// covers every visible split rather than the one that rendered last.
    static VISIBLE_METADATA: RefCell<Vec<OwnerMetadata>> = const { RefCell::new(Vec::new()) };
    static CURRENT_OWNER: Cell<u64> = const { Cell::new(UNATTRIBUTED_OWNER) };
    /// Splits that began a render since the last frame boundary. A split
    /// rendering a second time marks the boundary.
    static FRAME_OWNERS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// Install the visible-animation census into the GPUI-free animation clock.
pub fn install_animation_clock_ports() {
    echo_render_core::animation_clock::install_visible_animation_count(visible_metadata_count);
}

/// A list is live while it is at most two animation frames old, so a split
/// that stopped rendering (hidden, minimized, scrolled away) drops out once
/// the clock moves on.
#[inline]
fn is_live(entry_frame: u64, current_frame: u64) -> bool {
    current_frame.saturating_sub(entry_frame) <= 2
}

fn owner_entry(entries: &mut Vec<OwnerMetadata>, owner: u64) -> &mut OwnerMetadata {
    let index = match entries.iter().position(|entry| entry.owner == owner) {
        Some(index) => index,
        None => {
            entries.push(OwnerMetadata {
                owner,
                frame: current_animation_frame(),
                metadata: Vec::new(),
            });
            entries.len() - 1
        }
    };
    &mut entries[index]
}

/// Register an animated emote's metadata as visible
/// Call this during rendering for each visible animated emote
/// PERF: Accepts Arc to avoid cloning Vec<u16> frame durations per-emote per-frame
pub fn register_visible_metadata(metadata: Arc<AnimationMetadata>) {
    if !animation_enabled() {
        return;
    }
    let owner = CURRENT_OWNER.with(Cell::get);
    VISIBLE_METADATA.with(|entries| {
        let mut entries = entries.borrow_mut();
        let entry = owner_entry(&mut entries, owner);
        if owner == UNATTRIBUTED_OWNER {
            let frame = current_animation_frame();
            if entry.frame != frame {
                entry.frame = frame;
                entry.metadata.clear();
            }
            if entry.metadata.len() >= UNATTRIBUTED_CAP
                || entry
                    .metadata
                    .iter()
                    .any(|known| Arc::ptr_eq(known, &metadata))
            {
                return;
            }
        }
        entry.metadata.push(metadata);
    });
}

/// Get the count of visible animated emotes across every split.
///
/// PERF: Ignores lists older than two animation frames. This enables
/// TrueIdle mode when no painting occurs (e.g., window minimized, all
/// animations scrolled off-screen, or no visible content).
pub fn visible_metadata_count() -> usize {
    if !animation_enabled() {
        return 0;
    }
    let current_frame = current_animation_frame();
    VISIBLE_METADATA.with(|entries| {
        entries
            .borrow()
            .iter()
            .filter(|entry| is_live(entry.frame, current_frame))
            .map(|entry| entry.metadata.len())
            .sum()
    })
}

/// Animated emotes `owner` registered during its latest render.
pub fn owner_visible_count(owner: u64) -> usize {
    VISIBLE_METADATA.with(|entries| {
        entries
            .borrow()
            .iter()
            .find(|entry| entry.owner == owner)
            .map_or(0, |entry| entry.metadata.len())
    })
}

/// Drop a split's census (split hidden or closed).
pub fn remove_owner(owner: u64) {
    VISIBLE_METADATA.with(|entries| entries.borrow_mut().retain(|entry| entry.owner != owner));
    FRAME_OWNERS.with(|owners| owners.borrow_mut().retain(|seen| *seen != owner));
}

/// Once per window frame: record render cadence, drop unattributed
/// registrations, and periodically retune the shared animation interval
/// from the union of every visible split's animated emotes.
fn begin_frame() {
    let _span = span!(Level::DEBUG, "emote_frame").entered();

    echo_render_core::animation_clock::record_frame_cadence();

    static FRAME_COUNTER: AtomicU64 = AtomicU64::new(0);
    let frame = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    if frame.is_multiple_of(120) {
        let stats = get_stats();
        crate::log_emotes!(
            debug,
            "Emote renderer frame stats: render_frame={}, animation_frame={}, visible_animated={}, animation_enabled={}, fps={:.1}, throttling={}",
            frame,
            stats.animation_frame,
            stats.visible_animated_emotes,
            stats.animation_enabled,
            stats.current_fps,
            stats.throttling_offscreen
        );
    }

    let current_frame = current_animation_frame();
    VISIBLE_METADATA.with(|entries| {
        let mut entries = entries.borrow_mut();
        entries.retain(|entry| entry.owner != UNATTRIBUTED_OWNER);
        if !frame.is_multiple_of(INTERVAL_REFRESH_FRAMES) {
            return;
        }
        let visible = entries
            .iter()
            .filter(|entry| is_live(entry.frame, current_frame))
            .flat_map(|entry| entry.metadata.iter());
        if visible.clone().next().is_some() {
            let interval =
                crate::managers::dynamic_tick_scheduler::update_from_visible_emotes(visible);
            set_animation_interval_ms(interval);
        }
    });
}

/// Call at the start of a chat split's render, before it registers its
/// visible animated emotes. `owner` is the split's stable entity id.
pub fn begin_split_frame(owner: u64) {
    let frame_boundary = FRAME_OWNERS.with(|owners| {
        let mut owners = owners.borrow_mut();
        let repeated = owners.contains(&owner);
        if repeated {
            owners.clear();
        }
        owners.push(owner);
        repeated
    });
    if frame_boundary {
        begin_frame();
    }

    VISIBLE_METADATA.with(|entries| {
        let mut entries = entries.borrow_mut();
        let entry = owner_entry(&mut entries, owner);
        entry.frame = current_animation_frame();
        entry.metadata.clear();
    });
    CURRENT_OWNER.with(|current| current.set(owner));
}

/// Call once the split has registered its animated emotes; later
/// registrations (composer, user card) are unattributed.
pub fn end_split_frame() {
    CURRENT_OWNER.with(|current| current.set(UNATTRIBUTED_OWNER));
}

/// Get comprehensive stats for debugging
pub fn get_stats() -> EmoteRenderStats {
    let (animation_frame, visible_animated_emotes, animation_enabled) = animation_stats();

    EmoteRenderStats {
        animation_frame,
        visible_animated_emotes,
        animation_enabled,
        current_fps: current_fps(),
        throttling_offscreen: should_throttle_offscreen(),
    }
}

/// Statistics about emote rendering
#[derive(Clone, Debug, Default)]
pub struct EmoteRenderStats {
    pub animation_frame: u64,
    pub visible_animated_emotes: u32,
    pub animation_enabled: bool,
    pub current_fps: f32,
    pub throttling_offscreen: bool,
}
