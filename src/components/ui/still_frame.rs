//! Static first frames for emotes that a display rule shows still.
//!
//! GPUI's `img(url)` decodes and plays every frame of a GIF or WebP, so element
//! surfaces outside the chat canvas (thread viewer, user card, tooltips) cannot
//! use it for a still emote. This source shows only the first frame Echo's
//! loader decoded into its static layer, which is what the canvas paints.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, EntityId, ImageCacheError, ImageSource, SharedString, Window};

use crate::cache::loader::{self as image_cache, PreloadSource};

/// First re-check delay for a view showing a missing still frame; later
/// re-checks back off exponentially up to [`STILL_FRAME_POLL_MAX`].
const STILL_FRAME_POLL: Duration = Duration::from_millis(150);
const STILL_FRAME_POLL_MAX: Duration = Duration::from_millis(2_400);
/// Stop re-rendering a view for a frame that has not landed after this long.
/// The loader keeps retrying on its own backoff; the element shows its
/// fallback until something else re-renders the view.
const STILL_FRAME_GIVE_UP: Duration = Duration::from_secs(20);
/// Entries older than this are dropped whenever a new one is added, so views
/// that got their frame (or went away) never accumulate.
const STILL_FRAME_ENTRY_TTL: Duration = Duration::from_secs(60);

struct Recheck {
    first_seen: Instant,
    attempts: u32,
    pending: bool,
}

/// At most one pending re-check per view and URL, so a view showing several
/// missing frames, or rendering again before its re-check fires, never stacks
/// timers.
static RECHECKS: LazyLock<Mutex<HashMap<(EntityId, SharedString), Recheck>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// An image source showing only the static first frame of `url`.
///
/// While the frame is not resident it queues a poster (first-frame) decode and
/// re-renders the current view with backoff until the frame lands, for at most
/// [`STILL_FRAME_GIVE_UP`]; it never starts an animated decode. A failed load
/// that will not retry, or one still missing after the give-up window, shows
/// the element's fallback.
pub fn still_frame_source(url: SharedString, source: PreloadSource) -> ImageSource {
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        if let Some(image) = image_cache::get_cached_render_image(url.as_ref()) {
            return Some(Ok(image));
        }
        if matches!(
            image_cache::get_state(url.as_ref()),
            image_cache::ImageLoadState::Failed
        ) && !image_cache::should_retry(url.as_ref())
        {
            return Some(Err(ImageCacheError::Asset(
                "still frame unavailable".into(),
            )));
        }
        image_cache::trigger_animated_poster_preload_media_single_with_source(
            url.as_ref(),
            None,
            source,
        );

        let key = (window.current_view(), url.clone());
        let now = Instant::now();
        let delay = {
            let mut rechecks = RECHECKS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !rechecks.contains_key(&key) {
                rechecks.retain(|_, entry| {
                    now.duration_since(entry.first_seen) < STILL_FRAME_ENTRY_TTL
                });
            }
            let entry = rechecks.entry(key.clone()).or_insert(Recheck {
                first_seen: now,
                attempts: 0,
                pending: false,
            });
            if now.duration_since(entry.first_seen) >= STILL_FRAME_GIVE_UP {
                // Start a fresh window the next time something else renders it.
                rechecks.remove(&key);
                return Some(Err(ImageCacheError::Asset(
                    "still frame not loaded yet".into(),
                )));
            }
            if entry.pending {
                None
            } else {
                entry.pending = true;
                let delay = STILL_FRAME_POLL
                    .saturating_mul(1 << entry.attempts.min(4))
                    .min(STILL_FRAME_POLL_MAX);
                entry.attempts = entry.attempts.saturating_add(1);
                Some(delay)
            }
        };
        if let Some(delay) = delay {
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(delay).await;
                    if let Some(entry) = RECHECKS
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get_mut(&key)
                    {
                        entry.pending = false;
                    }
                    cx.update(|_, cx| cx.notify(key.0)).ok();
                })
                .detach();
        }
        None
    }))
}
