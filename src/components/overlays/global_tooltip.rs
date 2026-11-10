//! Global tooltip overlay manager/state.
//!
//! This provides one shared tooltip state while ensuring each tooltip is rendered only
//! above its owning native window and clamped to that window's viewport.

use crate::backend::StreamInfo;
use crate::backend::hotkeys;
use crate::backend::hotkeys::HotkeyAction;
use crate::components::chat::canvas_renderer::layout::HitboxKind;
use crate::components::chat::canvas_renderer::tooltip::visible_layered_tooltip_emotes;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

const TOOLTIP_OFFSET_X_PX: f32 = 12.0;
const TOOLTIP_OFFSET_Y_PX: f32 = 22.0;
const TOOLTIP_MARGIN_PX: f32 = 12.0;
const TOOLTIP_CONTENT_FADE_MS: u64 = 110;
/// A fresh show within this window is a cross-split/cross-element handoff, not
/// a genuine appearance, so it reuses the previous animation key.
const TOOLTIP_REFADE_GRACE_MS: u64 = 100;
/// Wake cadence of the animated-tooltip repaint loop. This only paces the
/// frame-change check; the repaint itself is gated on the displayed frame
/// actually advancing.
const TOOLTIP_ANIMATION_TICK_MS: u64 = 20;

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum GlobalTooltipKind {
    CanvasHitbox(HitboxKind),
    Text(String),
    HotkeyHint(HotkeyAction),
    StreamHeader {
        channel: String,
        stream_info: Option<StreamInfo>,
        hide_viewer_stats: bool,
    },
}

#[derive(Clone, Debug, Default)]
struct GlobalTooltipState {
    owner_window: Option<AnyWindowHandle>,
    source: Option<String>,
    kind: Option<GlobalTooltipKind>,
    position: Option<Point<Pixels>>,
    viewport_size: Option<Size<Pixels>>,
    content_generation: u64,
    last_cleared_at: Option<std::time::Instant>,
}

impl GlobalTooltipState {
    fn show(
        &mut self,
        owner_window: AnyWindowHandle,
        source: String,
        kind: GlobalTooltipKind,
        position: Point<Pixels>,
    ) {
        if self.kind.is_none() {
            let cleared_recently = self.last_cleared_at.is_some_and(|cleared_at| {
                cleared_at.elapsed() <= std::time::Duration::from_millis(TOOLTIP_REFADE_GRACE_MS)
            });
            let previous_generation = cleared_recently
                .then(|| GLOBAL_TOOLTIP_CONTENT_GENERATION.load(Ordering::Relaxed))
                .filter(|generation| *generation != 0);

            self.content_generation =
                previous_generation.unwrap_or_else(next_global_tooltip_content_generation);
        }

        self.owner_window = Some(owner_window);
        self.source = Some(source);
        self.kind = Some(kind);
        self.position = Some(position);
    }
}

static GLOBAL_TOOLTIP_STATE: LazyLock<Mutex<GlobalTooltipState>> =
    LazyLock::new(|| Mutex::new(GlobalTooltipState::default()));
static GLOBAL_TOOLTIP_CONTENT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Lock a transient tooltip mutex, discarding its contents if a panic poisoned
/// it.
///
/// Unlike `MutexExt::lock_or_recover`, which keeps the half-updated value and
/// leaves the mutex poisoned (warning on every later lock), tooltip state is
/// pure UI scratch: resetting it to `Default` only hides the current tooltip,
/// and clearing the poison keeps later frames from re-warning.
fn lock_or_reset<'a, T: Default>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            *guard = T::default();
            mutex.clear_poison();
            crate::log_ui!(warn, "Reset poisoned global tooltip {label}");
            guard
        }
    }
}

fn next_global_tooltip_content_generation() -> u64 {
    GLOBAL_TOOLTIP_CONTENT_GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
}

static GLOBAL_TOOLTIP_ANIMATION_WAKER_OWNER: LazyLock<Mutex<Option<AnyWindowHandle>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn show_canvas_hitbox_tooltip(
    owner_window: AnyWindowHandle,
    source: String,
    kind: HitboxKind,
    position: Point<Pixels>,
) {
    if matches!(kind, HitboxKind::MessageBackground { .. }) {
        clear_global_tooltip_if_source_in_window(owner_window, &source);
        return;
    }

    lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state").show(
        owner_window,
        source,
        GlobalTooltipKind::CanvasHitbox(kind),
        position,
    );
}

pub fn show_text_tooltip(
    owner_window: AnyWindowHandle,
    source: String,
    text: String,
    position: Point<Pixels>,
) {
    lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state").show(
        owner_window,
        source,
        GlobalTooltipKind::Text(text),
        position,
    );
}

pub fn show_hotkey_hint_tooltip(
    owner_window: AnyWindowHandle,
    source: String,
    action: HotkeyAction,
    position: Point<Pixels>,
) {
    lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state").show(
        owner_window,
        source,
        GlobalTooltipKind::HotkeyHint(action),
        position,
    );
}

pub fn show_stream_header_tooltip(
    owner_window: AnyWindowHandle,
    source: String,
    channel: String,
    stream_info: Option<StreamInfo>,
    hide_viewer_stats: bool,
    position: Point<Pixels>,
) {
    lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state").show(
        owner_window,
        source,
        GlobalTooltipKind::StreamHeader {
            channel,
            stream_info,
            hide_viewer_stats,
        },
        position,
    );
}

/// Keep a visible tooltip attached to the live pointer instead of the point at
/// which a wide header/button first received hover.
pub fn update_global_tooltip_position(
    owner_window: AnyWindowHandle,
    position: Point<Pixels>,
) -> bool {
    let mut state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
    if state.owner_window != Some(owner_window) || state.kind.is_none() {
        return false;
    }
    let changed = state.position.is_none_or(|previous| {
        (previous.x - position.x).abs() > px(0.5) || (previous.y - position.y).abs() > px(0.5)
    });
    if changed {
        state.position = Some(position);
    }
    changed
}

pub fn clear_global_tooltip_if_source(source: &str) {
    let mut state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
    if state.source.as_deref() == Some(source) {
        *state = GlobalTooltipState::default();
        state.last_cleared_at = Some(std::time::Instant::now());
    }
}

pub(super) fn clear_global_tooltip_if_source_prefix(prefix: &str) {
    let mut state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
    if state.source.as_deref().is_some_and(|source| {
        source == prefix
            || source
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with(':'))
    }) {
        *state = GlobalTooltipState::default();
        state.last_cleared_at = Some(std::time::Instant::now());
    }
}

/// Clear a tooltip only when both its source and native-window owner match.
/// Hover-leave callbacks use this so stale events from one window cannot clear
/// a newer tooltip displayed by another window.
pub fn clear_global_tooltip_if_source_in_window(owner_window: AnyWindowHandle, source: &str) {
    let mut state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
    if state.owner_window == Some(owner_window) && state.source.as_deref() == Some(source) {
        *state = GlobalTooltipState::default();
        state.last_cleared_at = Some(std::time::Instant::now());
    }
}

/// `on_hover` handler that shows `text` as the global tooltip for `source`
/// while hovered and clears it when the pointer leaves.
pub fn text_tooltip_on_hover(
    source: String,
    text: String,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    move |hovered, window, _app| {
        if *hovered {
            show_text_tooltip(
                window.window_handle(),
                source.clone(),
                text.clone(),
                window.mouse_position(),
            );
        } else {
            clear_global_tooltip_if_source_in_window(window.window_handle(), &source);
        }
    }
}

/// Clear the active tooltip regardless of which surface owns it.
///
/// Blocking menus call this while open so hover state from covered content
/// cannot paint above the foreground overlay.
pub fn clear_global_tooltip() {
    let mut state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
    *state = GlobalTooltipState::default();
    state.last_cleared_at = Some(std::time::Instant::now());
}

pub fn render_global_tooltip_overlay(
    owner_window: AnyWindowHandle,
    viewport_size: Size<Pixels>,
    live_mouse_position: Point<Pixels>,
) -> Option<AnyElement> {
    let snapshot = {
        let mut state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
        if state.owner_window != Some(owner_window) {
            return None;
        }
        let viewport_changed = state.viewport_size.is_some_and(|previous| {
            (previous.width - viewport_size.width).abs() > px(0.5)
                || (previous.height - viewport_size.height).abs() > px(0.5)
        });
        if viewport_changed {
            *state = GlobalTooltipState::default();
            state.last_cleared_at = Some(std::time::Instant::now());
            return None;
        }
        state.viewport_size = Some(viewport_size);
        state.clone()
    };
    let kind = snapshot.kind?;
    let position = live_mouse_position;

    let (content, estimated_w, estimated_h): (AnyElement, f32, f32) = match &kind {
        GlobalTooltipKind::CanvasHitbox(hitbox) => {
            let content =
                crate::components::chat::canvas_renderer::tooltip::render_tooltip_content_for_hitbox(
                    hitbox,
                )?;
            let (w, h) =
                crate::components::chat::canvas_renderer::tooltip::estimate_tooltip_size_for_hitbox(
                    hitbox,
                );
            (content, w, h)
        }
        GlobalTooltipKind::Text(text) => {
            let width = ((text.chars().count() as f32) * 7.0 + 16.0).clamp(90.0, 260.0);
            (render_text_tooltip(text), width, 30.0)
        }
        GlobalTooltipKind::HotkeyHint(action) => {
            let width = if let Some(binding) = hotkeys::get_binding(*action) {
                let binding_display = binding.to_display();
                ((binding_display.len() as f32) * 7.0 + 16.0).clamp(28.0, 140.0)
            } else {
                let label = action.display_name();
                ((label.chars().count() as f32) * 6.5 + 16.0).clamp(90.0, 260.0)
            };
            (
                crate::components::overlays::keyboard_hint_for_action(*action).into_any_element(),
                width,
                22.0,
            )
        }
        GlobalTooltipKind::StreamHeader {
            channel,
            stream_info,
            hide_viewer_stats,
        } => {
            let live = stream_info.as_ref().is_some_and(|info| info.is_live);
            (
                render_stream_header_tooltip(channel, stream_info.as_ref(), *hide_viewer_stats),
                340.0,
                if live { 330.0 } else { 110.0 },
            )
        }
    };

    let viewport_w: f32 = viewport_size.width.into();
    let viewport_h: f32 = viewport_size.height.into();
    let pos_x: f32 = position.x.into();
    let pos_y: f32 = position.y.into();
    let margin = TOOLTIP_MARGIN_PX;
    let initial_fit =
        crate::utils::fit_overlay_to_viewport(viewport_size, estimated_w, estimated_h, margin);
    let wrapped_height = if initial_fit.width > 0.0 && estimated_w > initial_fit.width {
        estimated_h * (estimated_w / initial_fit.width).ceil()
    } else {
        estimated_h
    };
    let fit =
        crate::utils::fit_overlay_to_viewport(viewport_size, estimated_w, wrapped_height, margin);
    let placed_w = fit.width;
    let placed_h = fit.height;
    let height_constrained = wrapped_height > placed_h + 0.5;

    let space_below = viewport_h - pos_y - TOOLTIP_OFFSET_Y_PX;
    let space_above = pos_y - TOOLTIP_OFFSET_Y_PX;

    let top = if space_below >= placed_h + fit.margin_y {
        pos_y + TOOLTIP_OFFSET_Y_PX
    } else if space_above >= placed_h + fit.margin_y {
        pos_y - TOOLTIP_OFFSET_Y_PX - placed_h
    } else if space_below >= space_above {
        pos_y + TOOLTIP_OFFSET_Y_PX
    } else {
        pos_y - TOOLTIP_OFFSET_Y_PX - placed_h
    };

    let space_right = viewport_w - pos_x - TOOLTIP_OFFSET_X_PX;

    let left = if space_right >= placed_w + fit.margin_x {
        pos_x + TOOLTIP_OFFSET_X_PX
    } else {
        pos_x - TOOLTIP_OFFSET_X_PX - placed_w
    };

    let clamped_left = crate::utils::clamp_overlay_axis(left, placed_w, viewport_w, fit.margin_x);
    let clamped_top = crate::utils::clamp_overlay_axis(top, placed_h, viewport_h, fit.margin_y);

    let tooltip = div()
        .id("global-tooltip-overlay")
        .absolute()
        .left(px(clamped_left))
        .top(px(clamped_top))
        .max_w(px(placed_w));

    let tooltip = if height_constrained {
        tooltip.h(px(placed_h)).overflow_y_scroll()
    } else {
        tooltip
    };

    let content = div().child(content).with_animation(
        ("global-tooltip-fade", snapshot.content_generation),
        Animation::new(std::time::Duration::from_millis(TOOLTIP_CONTENT_FADE_MS))
            .with_easing(ease_out_quint()),
        |content, delta| content.opacity(delta),
    );

    Some(tooltip.child(content).into_any_element())
}

/// Hash the animation frame one tooltip image would paint right now.
///
/// Returns false when the painted frame cannot be proven: the preview is still
/// decoding, or its exact frame is not resident and the cache would substitute
/// a held fallback we cannot predict from here. Callers must then repaint
/// unconditionally instead of latching onto a signature that never advances.
fn hash_displayed_animation_frame(
    hasher: &mut std::collections::hash_map::DefaultHasher,
    display_url: &str,
    preview_url: Option<&str>,
    time_ms: u64,
) -> bool {
    use crate::cache::loader as image_cache;
    use std::hash::Hash;

    let target = preview_url.unwrap_or(display_url);
    let resolved = match image_cache::get_animation_metadata(target) {
        Some(metadata) => Some((target, metadata)),
        None if target != display_url => {
            image_cache::get_animation_metadata(display_url).map(|meta| (display_url, meta))
        }
        None => None,
    };
    let Some((url, metadata)) = resolved else {
        // No animation registered: the tooltip paints the static first-frame
        // fallback, which never advances. A later registration changes the
        // signature, so the waker still repaints then.
        if let Some(static_url) = [target, display_url]
            .into_iter()
            .find(|url| image_cache::has_cached_render_image(url))
        {
            static_url.hash(hasher);
            u16::MAX.hash(hasher);
            return true;
        }
        // Failed decodes paint the placeholder until a retry changes state.
        if [target, display_url].into_iter().all(|url| {
            matches!(
                image_cache::get_animated_cache_state(url),
                image_cache::AnimatedCacheState::Failed
            )
        }) {
            target.hash(hasher);
            (u16::MAX - 1).hash(hasher);
            return true;
        }
        return false;
    };
    let frame_idx = metadata.frame_index_at(time_ms);
    if !image_cache::has_animated_frame(url, frame_idx) {
        return false;
    }
    url.hash(hasher);
    frame_idx.hash(hasher);
    true
}

/// Displayed-frame signature of the animated tooltip owned by `owner_window`.
///
/// Outer `None`: this window has no animated tooltip, so its waker must stop.
/// Inner `None`: the painted frame is not provable yet (still decoding), so the
/// caller must repaint unconditionally until it resolves.
fn animated_tooltip_frame_signature(owner_window: AnyWindowHandle) -> Option<Option<u64>> {
    use std::hash::Hasher;

    let state = lock_or_reset(&GLOBAL_TOOLTIP_STATE, "state");
    if state.owner_window != Some(owner_window) {
        return None;
    }

    let time_ms = crate::cache::frame_layer::current_animation_time_ms();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let provable = match state.kind.as_ref()? {
        GlobalTooltipKind::CanvasHitbox(HitboxKind::Emote { url, meta, .. }) => {
            let meta = meta.as_ref().filter(|meta| meta.animates())?;
            hash_displayed_animation_frame(
                &mut hasher,
                url,
                meta.tooltip_preview_url.as_deref(),
                time_ms,
            )
        }
        GlobalTooltipKind::CanvasHitbox(HitboxKind::LayeredEmote { emotes }) => {
            let mut any_animated = false;
            let mut provable = true;
            for emote in visible_layered_tooltip_emotes(emotes) {
                let Some(meta) = emote.meta.as_ref().filter(|meta| meta.animates()) else {
                    continue;
                };
                any_animated = true;
                provable &= hash_displayed_animation_frame(
                    &mut hasher,
                    &emote.url,
                    meta.tooltip_preview_url.as_deref(),
                    time_ms,
                );
            }
            if !any_animated {
                return None;
            }
            provable
        }
        GlobalTooltipKind::CanvasHitbox(HitboxKind::Badge { url, meta, .. }) => {
            meta.as_ref().filter(|meta| meta.is_animated)?;
            hash_displayed_animation_frame(&mut hasher, url, None, time_ms)
        }
        _ => return None,
    };

    Some(provable.then(|| hasher.finish()))
}

/// The app-level tooltip is not owned by the split-local canvas animation
/// waker. While it displays animated media, give only that overlay a bounded
/// repaint loop and stop immediately when the tooltip closes.
///
/// The notify is gated on the tooltip's displayed frame actually advancing.
/// The wake cadence tracks the fastest animation Echo supports, which is much
/// faster than a typical emote advances, and every notify here invalidates the
/// whole owner window (GPUI always re-renders an uncached window root), so an
/// ungated loop repaints the window many times per visible frame change.
pub fn ensure_global_tooltip_animation_waker<T: 'static>(
    owner_window: AnyWindowHandle,
    cx: &mut Context<T>,
) {
    if !crate::emote_renderer::animation_enabled() {
        return;
    }
    if animated_tooltip_frame_signature(owner_window).is_none() {
        return;
    }

    let mut waker_owner = lock_or_reset(&GLOBAL_TOOLTIP_ANIMATION_WAKER_OWNER, "waker owner");
    if *waker_owner == Some(owner_window) {
        return;
    }
    *waker_owner = Some(owner_window);
    drop(waker_owner);

    let entity = cx.entity().downgrade();
    cx.spawn(async move |_this, cx| {
        let mut last_frame_signature: Option<u64> = None;
        loop {
            cx.background_executor()
                .timer(crate::managers::dynamic_tick_scheduler::aligned_tick_delay(
                    TOOLTIP_ANIMATION_TICK_MS,
                ))
                .await;
            let still_owns_waker =
                *lock_or_reset(&GLOBAL_TOOLTIP_ANIMATION_WAKER_OWNER, "waker owner")
                    == Some(owner_window);
            if !crate::emote_renderer::animation_enabled() {
                clear_animation_waker_owner_if_matches(owner_window);
                break;
            }
            let signature = animated_tooltip_frame_signature(owner_window);
            let Some(signature) = signature.filter(|_| still_owns_waker) else {
                clear_animation_waker_owner_if_matches(owner_window);
                break;
            };

            match signature {
                Some(signature) if last_frame_signature == Some(signature) => {
                    crate::backend::metrics::record_emote_event(
                        "tooltip.animation_frame_unchanged_skipped",
                    );
                    continue;
                }
                signature => last_frame_signature = signature,
            }

            crate::backend::metrics::record_ui_notify_source("tooltip.animation_waker");
            if entity.update(cx, |_this, cx| cx.notify()).is_err() {
                clear_animation_waker_owner_if_matches(owner_window);
                break;
            }
        }
    })
    .detach();
}

fn clear_animation_waker_owner_if_matches(owner_window: AnyWindowHandle) {
    let mut current_owner = lock_or_reset(&GLOBAL_TOOLTIP_ANIMATION_WAKER_OWNER, "waker owner");
    if *current_owner == Some(owner_window) {
        *current_owner = None;
    }
}

fn render_stream_header_tooltip(
    channel: &str,
    stream_info: Option<&StreamInfo>,
    hide_viewer_stats: bool,
) -> AnyElement {
    use crate::components::chat::chat_ui::{format_uptime, format_viewer_count};

    let channel_label = channel.trim_start_matches('#').to_string();
    let live = stream_info.is_some_and(|info| info.is_live);
    let mut panel = div()
        .w(px(340.0))
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .rounded(Radii::md())
        .shadow_lg()
        .p(px(Spacing::SM_PLUS))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(Spacing::XS_PLUS));

    if live && !crate::bench::ui_fixture::is_fixture_channel(channel) {
        let normalized_channel = echo_core::channel_identity::normalize_channel_key(channel);
        let cache_bucket = crate::utils::unix_now_secs() / 180;
        let thumbnail_url = format!(
            "https://static-cdn.jtvnw.net/previews-ttv/live_user_{normalized_channel}-320x180.jpg?echo={cache_bucket}"
        );
        panel = panel.child(
            div()
                .w(px(320.0))
                .h(px(180.0))
                .rounded(Radii::sm())
                .overflow_hidden()
                .bg(colors::bg_void())
                .child(
                    img(SharedString::from(thumbnail_url))
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                ),
        );
    }

    let title = stream_info
        .map(|info| info.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or(&channel_label);
    panel = panel
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .text_center()
                .max_w(px(316.0))
                .child(title.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(if live {
                    colors::signal_red()
                } else {
                    colors::text_secondary()
                })
                .child(if live {
                    format!("{channel_label} • Live")
                } else {
                    format!("{channel_label} • Offline")
                }),
        );

    if let Some(info) = stream_info {
        if let Some(game) = info
            .game_name
            .as_deref()
            .map(str::trim)
            .filter(|game| !game.is_empty())
        {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(colors::text_secondary())
                    .text_center()
                    .child(game.to_string()),
            );
        }
        if live {
            let status = if hide_viewer_stats {
                if info.started_at > 0 {
                    format!(
                        "Live for {} • viewer count hidden",
                        format_uptime(info.started_at)
                    )
                } else {
                    "Live • viewer count hidden".to_string()
                }
            } else {
                let viewers = format_viewer_count(info.viewer_count);
                let noun = if info.viewer_count == 1 {
                    "viewer"
                } else {
                    "viewers"
                };
                if info.started_at > 0 {
                    format!(
                        "Live for {} with {viewers} {noun}",
                        format_uptime(info.started_at)
                    )
                } else {
                    format!("Live with {viewers} {noun}")
                }
            };
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(colors::text_primary().opacity(0.88))
                    .text_center()
                    .child(status),
            );
        }
    }

    panel.into_any_element()
}

fn render_text_tooltip(text: &str) -> AnyElement {
    div()
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .rounded(Radii::sm())
        .shadow_lg()
        .px(px(Spacing::XS_PLUS))
        .py(px(Spacing::XS))
        .child(
            div()
                .text_size(px(FontSize::XS))
                .text_color(colors::text_primary())
                .whitespace_normal()
                .child(text.to_string()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::lock_or_reset;
    use core::prelude::v1::test;
    use std::sync::{Arc, Mutex};

    #[test]
    fn poisoned_transient_state_is_reset_and_unpoisoned() {
        let state = Arc::new(Mutex::new(0usize));
        let poisoned_state = state.clone();
        let result = std::thread::spawn(move || {
            let mut guard = poisoned_state.lock().unwrap();
            *guard = 42;
            panic!("poison tooltip test state");
        })
        .join();
        assert!(result.is_err());

        *lock_or_reset(&state, "test state") = 7;
        assert_eq!(*state.lock().unwrap(), 7);
    }
}
