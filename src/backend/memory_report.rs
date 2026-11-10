//! Process memory breakdown diagnostic.
//!
//! Answers "what is actually holding the RSS", which the individual subsystem
//! budgets cannot: each one knows its own ceiling, but nothing compares the sum
//! of them against what the OS reports for the process. The gap between those
//! two numbers is the interesting part, so it is reported explicitly rather than
//! left implicit.
//!
//! Enable with `ECHO_MEM_REPORT_SECS=<seconds>` (0 or unset disables). Output
//! goes to the `perf` log category, so pair it with
//! `ECHO_LOG_FILE=1 ECHO_LOG=perf`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes per resident layout-cache entry.
///
/// A typical row: the 768 B struct (`cached_layout_types_stay_compact` in
/// echo-chat-render) plus exact-size lists of about 6 segments (184 B), 2-3
/// text spans (792 B), 10 hitboxes (112 B) and 4 image commands (64 B). This is
/// an estimate, not accounting: those lists vary with message length, so treat
/// the layout line as approximate. It was 12,856 B while the lists were inline.
const LAYOUT_ENTRY_BYTES_ESTIMATE: usize = 5_300;

static LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);
fn interval_secs() -> u64 {
    std::env::var("ECHO_MEM_REPORT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Log a breakdown if the configured interval has elapsed. Cheap no-op when the
/// diagnostic is disabled, so it is safe to call from a periodic slot.
pub fn maybe_log_memory_breakdown() {
    let interval = interval_secs();
    if interval == 0 {
        return;
    }
    let now = crate::utils::unix_now_ms();
    let last = LAST_REPORT_MS.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < interval * 1000 {
        return;
    }
    LAST_REPORT_MS.store(now, Ordering::Relaxed);
    log_memory_breakdown();
}

/// Log one memory breakdown immediately.
pub fn log_memory_breakdown() {
    let (static_b, frames_b, meta_b, anim_src_b, media_budget) =
        crate::cache::loader::decoded_media_residency();
    let crate::cache::loader::PickerMediaDiagnostics {
        picker_scopes,
        picker_touched_urls,
        picker_touched_keys,
        picker_visible,
        hot_frames,
        hot_frame_urls,
        picker_protected_frames,
        picker_protected_bytes,
        hot_frames_max_per_url,
        hot_frame_urls_over_limit,
        hot_render_images,
        hot_frame_entry_limit,
        hot_frame_entries_over_limit,
    } = crate::cache::loader::picker_media_diagnostics();
    // Hot frames release their CPU pixels once uploaded, so `frames_b` (the
    // budget's charge, which tracks atlas residency) is not RAM; count the
    // pixels they still hold instead.
    let frame_pixels_b = crate::cache::loader::hot_frame_resident_pixel_bytes();
    let anim_src_disk_b = crate::cache::loader::decoded_animation_source_disk_bytes();
    let animation_frame_rebuilds = crate::cache::loader::animation_frame_rebuilds();
    let decoded_total = static_b + frame_pixels_b + meta_b + anim_src_b;

    let (layout_entries, layout_capacity) =
        crate::components::chat::canvas_renderer::global_layout_cache::GLOBAL_LAYOUT_CACHE
            .occupancy();
    let layout_est = layout_entries * LAYOUT_ENTRY_BYTES_ESTIMATE;
    let (
        name_paint_composites,
        name_paint_composite_bytes,
        name_paint_composite_budget,
        name_paint_composite_retirements,
    ) = crate::components::chat::canvas_renderer::seventv_paint::composite_cache_diagnostics();
    let pending_render_image_drops = crate::cache::pending_render_image_drop_stats();
    let render_image_bytes_drained = crate::cache::render_image_bytes_drained();
    let (render_image_drain_batches, render_images_drained) =
        crate::cache::render_image_drain_totals();
    let hot_frame_render_images_created =
        crate::cache::frame_layer::hot_frame_render_images_created();
    let full_cycle_demotions = crate::cache::frame_layer::full_cycle_demotions();
    let (animation_frame_decompressions, animation_refill_windows, animation_refill_frames) =
        crate::cache::loader::animation_refill_totals();
    let uri_images = crate::cache::uri_layer::stats();

    let (channels, messages, message_bytes) = message_buffer_bytes();
    let (emote_channels, emote_entries, emote_bytes) = emote_map_bytes();

    let (ws_bytes, private_bytes) = process_memory();
    let (_alloc_rss, alloc_peak_rss, alloc_commit, alloc_peak_commit) = allocator_memory();
    let accounted =
        decoded_total + layout_est + message_bytes + emote_bytes + name_paint_composite_bytes;

    let alloc_retained = alloc_commit.saturating_sub(ws_bytes);

    crate::log_perf!(
        info,
        ws_mb = ws_bytes / 1_048_576,
        private_mb = private_bytes / 1_048_576,
        peak_ws_mb = alloc_peak_rss / 1_048_576,
        commit_mb = alloc_commit / 1_048_576,
        peak_commit_mb = alloc_peak_commit / 1_048_576,
        committed_not_resident_mb = alloc_retained / 1_048_576,
        message_bytes_mb = message_bytes / 1_048_576,
        emote_bytes_mb = emote_bytes / 1_048_576,
        decoded_static_mb = static_b / 1_048_576,
        decoded_frames_mb = frames_b / 1_048_576,
        frame_pixels_resident_mb = frame_pixels_b / 1_048_576,
        decoded_meta_mb = meta_b / 1_048_576,
        anim_sources_mb = anim_src_b / 1_048_576,
        anim_sources_disk_mb = anim_src_disk_b / 1_048_576,
        decoded_total_mb = decoded_total / 1_048_576,
        decoded_budget_mb = media_budget / 1_048_576,
        picker_scopes,
        picker_touched_urls,
        picker_touched_keys,
        picker_inline_media = picker_visible.inline_media,
        picker_popout_media = picker_visible.popout_media,
        picker_inline_animations = picker_visible.inline_animations,
        picker_popout_animations = picker_visible.popout_animations,
        picker_inline_published = picker_visible.inline_published,
        picker_popout_published = picker_visible.popout_published,
        hot_frames,
        hot_frame_urls,
        hot_frames_max_per_url,
        hot_frame_urls_over_limit,
        hot_render_images,
        hot_frame_entry_limit,
        hot_frame_entries_over_limit,
        picker_protected_frames,
        picker_protected_mb = picker_protected_bytes / 1_048_576,
        name_paint_composites,
        name_paint_composite_mb = name_paint_composite_bytes / 1_048_576,
        name_paint_composite_budget_mb = name_paint_composite_budget / 1_048_576,
        name_paint_composite_retirements,
        pending_render_image_drops = pending_render_image_drops.count,
        pending_render_image_drop_kb = pending_render_image_drops.bytes / 1024,
        pending_render_image_drop_oldest_ms = pending_render_image_drops.oldest_age.as_millis(),
        render_image_drain_batches,
        render_images_drained,
        render_image_drained_mb = render_image_bytes_drained / 1_048_576,
        hot_frame_render_images_created,
        full_cycle_demotions,
        animation_frame_decompressions,
        animation_frame_rebuilds,
        animation_refill_windows,
        animation_refill_frames,
        gpui_uri_entries = uri_images.resident_entries,
        gpui_uri_bytes_mb = uri_images.resident_bytes / 1_048_576,
        gpui_uri_inflight = uri_images.inflight,
        gpui_uri_hits = uri_images.hits,
        gpui_uri_misses = uri_images.misses,
        gpui_uri_evictions = uri_images.evictions,
        gpui_uri_retirements = uri_images.retirements,
        gpui_uri_failures = uri_images.failures,
        gpui_uri_failed_entries = uri_images.failed_entries,
        gpui_uri_rejections = uri_images.rejections,
        gpui_uri_waiter_rejections = uri_images.waiter_rejections,
        gpui_uri_pinned_entries = uri_images.pinned_entries,
        gpui_uri_pinned_mb = uri_images.pinned_bytes / 1_048_576,
        gpui_uri_late_completion_discards = uri_images.late_completion_discards,
        gpui_uri_pending_retirements = uri_images.pending_retirements,
        gpui_uri_tracked_windows = uri_images.tracked_windows,
        gpui_uri_visible_references = uri_images.visible_references,
        gpui_uri_window_cap_rejections = uri_images.window_cap_rejections,
        gpui_uri_visible_cap_rejections = uri_images.visible_cap_rejections,
        layout_entries,
        layout_capacity,
        layout_est_mb = layout_est / 1_048_576,
        channels,
        messages,
        emote_channels,
        emote_entries,
        accounted_mb = accounted / 1_048_576,
        unaccounted_mb = ws_bytes.saturating_sub(accounted) / 1_048_576,
        "memory breakdown"
    );
}

/// Retained chat rows: `(channels, messages, bytes)`.
///
/// Walks every buffered message and sums the struct plus the heap behind its
/// `Arc<str>` fields, badges and slots. Shared `Arc`s are counted once per
/// referencing message, so this is an upper bound on unique bytes -- interned
/// senders and repeated emote names inflate it.
fn message_buffer_bytes() -> (usize, usize, usize) {
    let mut channels = 0usize;
    let mut messages = 0usize;
    let mut bytes = 0usize;
    for entry in crate::backend::irc::CHAT_MESSAGES.iter() {
        channels += 1;
        let buffer = entry.value();
        messages += buffer.len();
        for msg in buffer.iter() {
            bytes += std::mem::size_of::<crate::backend::models::ChatMessage>();
            bytes += msg.id.len()
                + msg.channel.len()
                + msg.sender.len()
                + msg.display_name.len()
                + msg.content.len();
            bytes += msg.color.as_ref().map_or(0, |c| c.len());
            bytes += msg.twitch_emotes_tag.as_ref().map_or(0, |t| t.len());
            bytes += msg.user_id.as_ref().map_or(0, |u| u.len());
            bytes += msg.badges.iter().map(|b| b.len()).sum::<usize>();
            bytes += msg.slots.len() * std::mem::size_of::<crate::backend::slots::MessageSlot>();
        }
    }
    (channels, messages, bytes)
}

/// Emote lookup metadata: `(channels, entries, bytes)`.
///
/// Counts the per-channel maps' key strings and the `Emote` records behind each
/// `Arc`. The same `Arc<Emote>` appears in several channel maps when providers
/// overlap, so this too is an upper bound.
fn emote_map_bytes() -> (usize, usize, usize) {
    let maps = crate::backend::emotes::EMOTE_LOOKUP_MAPS.load();
    let mut channels = 0usize;
    let mut entries = 0usize;
    let mut bytes = 0usize;
    for (channel, publication) in maps.iter() {
        let map = publication.lookup_snapshot();
        if map.is_empty() {
            continue;
        }
        channels += 1;
        bytes += channel.len();
        entries += map.len();
        for (name, emote) in map.iter() {
            bytes += name.len();
            bytes += std::mem::size_of_val(emote.as_ref());
            bytes += emote.name.len() + emote.id.len() + emote.url.len();
        }
    }
    (channels, entries, bytes)
}

/// `(current_rss, peak_rss, current_commit, peak_commit)` as mimalloc reports them.
///
/// IMPORTANT: on Windows `mi_process_info` reports PROCESS-level counters, not
/// allocator-heap-only ones -- `current_rss` comes back identical to the working
/// set and `current_commit` identical to private bytes. So this cannot be used
/// to separate the Rust heap from GPU/driver memory; subtracting it from the
/// process working set yields a meaningless zero. It is still worth reporting
/// for `peak_*` (high-water marks the OS sampler would miss between polls) and
/// for `commit - rss`, which is real: committed pages that are not resident.
#[cfg(any(windows, target_os = "linux"))]
fn allocator_memory() -> (usize, usize, usize, usize) {
    let mut elapsed = 0usize;
    let mut user = 0usize;
    let mut system = 0usize;
    let mut current_rss = 0usize;
    let mut peak_rss = 0usize;
    let mut current_commit = 0usize;
    let mut peak_commit = 0usize;
    let mut page_faults = 0usize;
    // SAFETY: mi_process_info only writes through the out-pointers supplied here
    // and is documented as callable from any thread.
    unsafe {
        libmimalloc_sys::mi_process_info(
            &mut elapsed,
            &mut user,
            &mut system,
            &mut current_rss,
            &mut peak_rss,
            &mut current_commit,
            &mut peak_commit,
            &mut page_faults,
        );
    }
    (current_rss, peak_rss, current_commit, peak_commit)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn allocator_memory() -> (usize, usize, usize, usize) {
    (0, 0, 0, 0)
}

/// Resident and virtual bytes for this process.
///
/// Refreshes only this pid: a full process-table refresh would be far more
/// expensive than the diagnostic is worth.
fn process_memory() -> (usize, usize) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let Ok(mut sys) = System::new() else {
        return (0, 0);
    };
    let me = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[me]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(me)
        .map(|p| (p.memory() as usize, p.virtual_memory() as usize))
        .unwrap_or((0, 0))
}
