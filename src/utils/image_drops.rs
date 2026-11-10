//! Coalesced release of evicted GPU images.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, Window};

use crate::managers::frame_rate_controller::{RenderMode, current_mode_cached};

const DRAIN_INTERVAL: Duration = Duration::from_millis(250);
/// Drain spacing while the user is typing, dragging or scrolling: a drain whose
/// tiles are still on screen forces an uncached redraw, which would land on
/// exactly the frames they are waiting for. Holding only delays freeing evicted
/// atlas tiles; `DRAIN_NOW_COUNT` still bounds the backlog.
const INTERACTIVE_DRAIN_INTERVAL: Duration = Duration::from_secs(2);
const DRAIN_NOW_COUNT: usize = 512;

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
static LAST_DRAIN_MS: AtomicU64 = AtomicU64::new(0);
static WAKE_SCHEDULED: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

/// How much longer a drain of `pending` images should wait, or `None` to drain
/// now.
fn drain_delay(pending: usize) -> Option<Duration> {
    if pending >= DRAIN_NOW_COUNT {
        return None;
    }
    let interval = if matches!(current_mode_cached(), RenderMode::Interactive) {
        INTERACTIVE_DRAIN_INTERVAL
    } else {
        DRAIN_INTERVAL
    };
    let since_last =
        Duration::from_millis(now_ms().saturating_sub(LAST_DRAIN_MS.load(Ordering::Relaxed)));
    (since_last < interval).then(|| interval - since_last)
}

/// Release queued sprite-atlas images at most every 250 ms (2 s while input is
/// active). A drain scans each window's last scene for the released tiles and
/// redraws it without cached views only when one is still drawn, so bursts
/// share one drain; a large backlog drains at once.
pub fn drain_render_image_drops_coalesced<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    let pending = crate::cache::pending_render_image_drop_count();
    if pending == 0 {
        return;
    }
    let Some(wait) = drain_delay(pending) else {
        LAST_DRAIN_MS.store(now_ms(), Ordering::Relaxed);
        window.defer(cx, |window, cx| {
            crate::cache::drain_render_image_drops(window, cx);
        });
        return;
    };
    if WAKE_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    cx.spawn_in(window, async move |_, cx| {
        // Re-check at least every `DRAIN_INTERVAL`: input starting after this
        // was scheduled should hold the drain, and input ending should let a
        // held drain go at the idle cadence.
        let mut wait = wait;
        loop {
            cx.background_executor()
                .timer(wait.min(DRAIN_INTERVAL))
                .await;
            match drain_delay(crate::cache::pending_render_image_drop_count()) {
                Some(remaining) => wait = remaining,
                None => break,
            }
        }
        WAKE_SCHEDULED.store(false, Ordering::Release);
        let _ = cx.update(|window, cx| {
            LAST_DRAIN_MS.store(now_ms(), Ordering::Relaxed);
            crate::cache::drain_render_image_drops(window, cx);
        });
    })
    .detach();
}
