//! Shared prefetch helpers for emote picker variants.
//!
//! Both inline and popout pickers use this to keep image loading behavior identical.

use super::core::{picker_display_url, picker_native_animated_enabled};
use crate::backend::Emote;
use crate::backend::settings::EmotePickerAnimationMode;
use crate::cache::loader as image_cache;
use crate::utils::env_usize;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

static TIMING_ENABLED: LazyLock<bool> =
    LazyLock::new(|| echo_core::env::flag("ECHO_PICKER_TIMING"));
static TIMING_EXACT_FRAMES: AtomicU64 = AtomicU64::new(0);
static TIMING_HELD_FRAMES: AtomicU64 = AtomicU64::new(0);

pub(super) fn take_frame_delivery_timing() -> (u64, u64) {
    (
        TIMING_EXACT_FRAMES.swap(0, Ordering::Relaxed),
        TIMING_HELD_FRAMES.swap(0, Ordering::Relaxed),
    )
}

const PICKER_STATIC_PREFETCH_EAGER_LIMIT_DEFAULT: usize = 256;
const PICKER_STATIC_PREFETCH_LAZY_LIMIT_DEFAULT: usize = 512;
const PICKER_ANIMATED_PREFETCH_LIMIT_DEFAULT: usize = 96;
const PICKER_COMPACT_ANIMATION_DENSITY_DEFAULT: usize = 256;
/// Rows to cover while `UniformList` establishes the actual visible range.
///
/// The render-time viewport prefetch immediately replaces this small bridge with
/// the measured visible range plus its existing scroll buffer.
pub const PICKER_INITIAL_PREFETCH_ROWS: usize = 8;
pub const PICKER_ANIMATED_PREFETCH_BATCH_LIMIT: usize = 96;

/// Distinguishes the small first-open bridge from a real measured viewport.
///
/// Keeping this in the throttle key lets the first viewport plan fill any
/// remaining visible media even when it overlaps the initial rows exactly.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerPrefetchPlanKind {
    Initial,
    Viewport,
}

impl PickerPrefetchPlanKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Viewport => "viewport",
        }
    }
}

pub struct PrefetchPlan {
    pub static_requests: Vec<image_cache::MediaRequest>,
    pub poster_requests: Vec<image_cache::MediaRequest>,
    pub animated_requests: Vec<image_cache::MediaRequest>,
}

impl PrefetchPlan {
    pub fn is_empty(&self) -> bool {
        self.static_requests.is_empty()
            && self.poster_requests.is_empty()
            && self.animated_requests.is_empty()
    }
}

/// Smaller provider variant used for exceptionally dense picker animation or
/// after the configured picker-quality decode has remained in flight unusually
/// long. Its scale-aware identity is distinct, so it cannot replace chat-quality
/// primary frames.
pub fn picker_animation_fallback_url<'a>(emote: &'a Emote, primary_url: &str) -> Option<&'a str> {
    emote
        .url_1x
        .as_deref()
        .filter(|url| !url.is_empty() && *url != primary_url)
}

pub fn picker_uses_compact_animation_variant(
    animated_cell_count: usize,
    animation_mode: EmotePickerAnimationMode,
) -> bool {
    let compact_animation_density = env_usize(
        "ECHO_PICKER_COMPACT_ANIMATION_DENSITY",
        PICKER_COMPACT_ANIMATION_DENSITY_DEFAULT,
    );
    animation_mode.animates_cell(false)
        && !picker_native_animated_enabled()
        && compact_animation_density > 0
        && animated_cell_count >= compact_animation_density
}

/// Select the single full-animation source for one cell.
///
/// Dense viewports prefer the compact provider variant; ordinary viewports
/// prefer the configured primary. Only a terminal decode/static-container
/// result promotes the alternate. This avoids alternating between two
/// independently evicted frame rings while still providing a real failover.
pub fn picker_active_animation_url<'a>(
    emote: &'a Emote,
    primary_url: &'a str,
    use_compact_animation: bool,
) -> &'a str {
    let compact_url = picker_animation_fallback_url(emote, primary_url);
    let preferred_url = if use_compact_animation {
        compact_url.unwrap_or(primary_url)
    } else {
        primary_url
    };
    let preferred_identity = crate::backend::types::media_source_id_for_url(emote, preferred_url);
    if image_cache::get_animated_cache_state_media(preferred_url, preferred_identity.as_ref())
        == image_cache::AnimatedCacheState::Failed
    {
        if use_compact_animation {
            primary_url
        } else {
            compact_url.unwrap_or(primary_url)
        }
    } else {
        preferred_url
    }
}

/// Select one animated picker frame from a single authoritative source.
///
/// Only `active_url` may schedule refill work. A different already-resident URL
/// can still supply a current frame during hydration, but that read-only visual
/// fallback cannot grow a second decode/refill working set or cause alternating
/// source selection once the active source becomes renderable.
pub fn picker_current_animated_frame<'a>(
    _primary_url: &'a str,
    _fallback_url: Option<&'a str>,
    active_url: &'a str,
) -> Option<(
    Arc<gpui::RenderImage>,
    Arc<crate::cache::frame_layer::AnimationMetadata>,
    u16,
    &'a str,
)> {
    let query = if image_cache::picker_animation_url_is_visible(active_url) {
        image_cache::FrameQuery::Current
    } else {
        image_cache::FrameQuery::Resident
    };
    if let Some(image_cache::AnimatedFrame {
        image,
        metadata,
        index: frame_idx,
        exact,
    }) = image_cache::animated_frame(active_url, query)
    {
        if *TIMING_ENABLED {
            if exact {
                &TIMING_EXACT_FRAMES
            } else {
                &TIMING_HELD_FRAMES
            }
            .fetch_add(1, Ordering::Relaxed);
        }
        return Some((image, metadata, frame_idx, active_url));
    }
    None
}

pub fn build_prefetch_plan<'a, I>(
    emotes: I,
    animation_mode: EmotePickerAnimationMode,
) -> PrefetchPlan
where
    I: IntoIterator<Item = &'a Arc<Emote>>,
{
    let emotes: Vec<&Arc<Emote>> = emotes.into_iter().collect();
    let mut static_requests = Vec::with_capacity(128);
    let mut poster_requests = Vec::with_capacity(128);
    let mut animated_requests = Vec::with_capacity(64);
    let mut seen_urls: HashSet<&str> = HashSet::with_capacity(256);
    let native_animated = picker_native_animated_enabled();
    let use_compact_animations = picker_uses_compact_animation_variant(
        emotes.iter().filter(|emote| emote.is_animated).count(),
        animation_mode,
    );

    for emote in emotes {
        let primary_url = picker_display_url(emote);

        if emote.is_animated {
            if animation_mode.animates_cell(false) {
                if native_animated {
                    continue;
                }
                let url = picker_active_animation_url(emote, primary_url, use_compact_animations);
                if !seen_urls.insert(url) {
                    continue;
                }
                let request = crate::backend::types::media_request_for_url(emote, url);
                image_cache::register_picker_media_identity(url, request.identity.as_deref());
                let state =
                    image_cache::get_animated_cache_state_media(url, request.identity.as_deref());
                if state == image_cache::AnimatedCacheState::NotLoaded {
                    animated_requests.push(request);
                } else if state == image_cache::AnimatedCacheState::Failed
                    && image_cache::get_animated_poster_render_image(url).is_none()
                    && image_cache::get_cached_render_image(url).is_none()
                {
                    poster_requests.push(request);
                }
            } else {
                if !seen_urls.insert(primary_url) {
                    continue;
                }
                if image_cache::get_animated_poster_render_image(primary_url).is_some()
                    || image_cache::get_stable_animated_render_image(primary_url).is_some()
                {
                    continue;
                }
                poster_requests.push(crate::backend::types::media_request_for_url(
                    emote,
                    primary_url,
                ));
            }
        } else {
            if !seen_urls.insert(primary_url) {
                continue;
            }
            if image_cache::has_cached_render_image(primary_url) {
                continue;
            }
            static_requests.push(crate::backend::types::media_request_for_url(
                emote,
                primary_url,
            ));
        }
    }

    PrefetchPlan {
        static_requests,
        poster_requests,
        animated_requests,
    }
}

#[derive(Clone, Copy)]
enum BoundedStaticPrefetchKind {
    Static,
    AnimatedPoster,
}

fn execute_bounded_static_prefetch(
    mut requests: Vec<image_cache::MediaRequest>,
    kind: BoundedStaticPrefetchKind,
) {
    if requests.is_empty() {
        return;
    }

    let total = requests.len();
    let eager_limit = env_usize(
        "ECHO_PICKER_STATIC_PREFETCH_EAGER_LIMIT",
        PICKER_STATIC_PREFETCH_EAGER_LIMIT_DEFAULT,
    );
    let lazy_limit = env_usize(
        "ECHO_PICKER_STATIC_PREFETCH_LAZY_LIMIT",
        PICKER_STATIC_PREFETCH_LAZY_LIMIT_DEFAULT,
    );

    let mut lazy_requests = if requests.len() > eager_limit {
        requests.split_off(eager_limit)
    } else {
        Vec::new()
    };
    let skipped_by_limit = lazy_requests.len().saturating_sub(lazy_limit);
    if lazy_requests.len() > lazy_limit {
        lazy_requests.truncate(lazy_limit);
    }

    if !requests.is_empty() {
        let eager_count = requests.len();
        match kind {
            BoundedStaticPrefetchKind::Static => image_cache::trigger_preload_media(
                requests,
                image_cache::PreloadSource::PickerVisible,
            ),
            BoundedStaticPrefetchKind::AnimatedPoster => {
                image_cache::trigger_animated_poster_preload_media(
                    requests,
                    image_cache::PreloadSource::PickerVisible,
                )
            }
        };
        crate::backend::metrics::record_emote_stage(
            match kind {
                BoundedStaticPrefetchKind::Static => "picker.prefetch.static_eager",
                BoundedStaticPrefetchKind::AnimatedPoster => "picker.prefetch.poster_eager",
            },
            std::time::Duration::ZERO,
            eager_count,
        );
    }

    if !lazy_requests.is_empty() {
        let lazy_count = lazy_requests.len();
        match kind {
            BoundedStaticPrefetchKind::Static => image_cache::trigger_lazy_preload_media(
                lazy_requests,
                image_cache::PreloadSource::PickerVisible,
            ),
            BoundedStaticPrefetchKind::AnimatedPoster => {
                image_cache::trigger_lazy_animated_poster_preload_media(
                    lazy_requests,
                    image_cache::PreloadSource::PickerVisible,
                )
            }
        };
        crate::backend::metrics::record_emote_stage(
            match kind {
                BoundedStaticPrefetchKind::Static => "picker.prefetch.static_lazy_overflow",
                BoundedStaticPrefetchKind::AnimatedPoster => "picker.prefetch.poster_lazy_overflow",
            },
            std::time::Duration::ZERO,
            lazy_count,
        );
    }

    if skipped_by_limit > 0 {
        crate::backend::metrics::record_emote_stage(
            match kind {
                BoundedStaticPrefetchKind::Static => "picker.prefetch.static_skipped_limit",
                BoundedStaticPrefetchKind::AnimatedPoster => "picker.prefetch.poster_skipped_limit",
            },
            std::time::Duration::ZERO,
            skipped_by_limit,
        );
    }

    crate::log_cache!(
        trace,
        total,
        eager_limit,
        lazy_limit,
        skipped_by_limit,
        poster = matches!(kind, BoundedStaticPrefetchKind::AnimatedPoster),
        "Picker static/poster prefetch bounded"
    );
}

pub fn execute_prefetch(plan: PrefetchPlan, max_animated_per_batch: usize) {
    execute_bounded_static_prefetch(plan.static_requests, BoundedStaticPrefetchKind::Static);
    execute_bounded_static_prefetch(
        plan.poster_requests,
        BoundedStaticPrefetchKind::AnimatedPoster,
    );

    if max_animated_per_batch == 0 {
        return;
    }

    if picker_native_animated_enabled() {
        return;
    }

    if plan.animated_requests.is_empty() {
        return;
    }

    let requested = plan.animated_requests.len();
    let configured_limit = env_usize(
        "ECHO_PICKER_ANIMATED_PREFETCH_LIMIT",
        PICKER_ANIMATED_PREFETCH_LIMIT_DEFAULT,
    );
    if configured_limit == 0 {
        crate::backend::metrics::record_emote_stage(
            "picker.prefetch.animated_disabled",
            std::time::Duration::ZERO,
            requested,
        );
        return;
    }
    let decode_limit = configured_limit.min(max_animated_per_batch);
    let skipped_by_limit = requested.saturating_sub(decode_limit);

    for request in plan.animated_requests.into_iter().take(decode_limit) {
        image_cache::trigger_animated_decode_request(
            request,
            image_cache::PreloadSource::PickerVisible,
        );
    }

    crate::backend::metrics::record_emote_stage(
        "picker.prefetch.animated_decode",
        std::time::Duration::ZERO,
        requested.saturating_sub(skipped_by_limit),
    );
    if skipped_by_limit > 0 {
        crate::backend::metrics::record_emote_stage(
            "picker.prefetch.animated_skipped_limit",
            std::time::Duration::ZERO,
            skipped_by_limit,
        );
    }
    crate::log_cache!(
        trace,
        requested,
        decode_limit,
        skipped_by_limit,
        "Picker animated prefetch queued"
    );
}
