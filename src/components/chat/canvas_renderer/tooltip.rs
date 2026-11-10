//! Tooltip state and rendering
//!
//! Shows tooltips on hover for:
//! - Emotes: enlarged preview + name
//! - Badges: name + description (e.g., "12 month subscriber")
//! - Usernames: login when the display name is localized
//! - URLs: full URL

use gpui::*;
use std::time::{Duration, Instant};

use super::layout::{
    BadgeTooltipMeta, EmoteTooltipMeta, HitboxKind, LayeredTooltipEmote, get_cached_color,
};
use crate::backend::irc::get_user_color;
use crate::theme::colors;

/// Delay before showing tooltip (milliseconds)
///
/// Set to zero for instantaneous hover tooltips.
const TOOLTIP_DELAY_MS: u64 = 0;
/// Panel padding, the emote-name row, and the two gaps around it. Keep this
/// conservative because GPUI resolves `text_xs` line height above its font size.
const EMOTE_TOOLTIP_FIXED_HEIGHT_PX: f32 = 50.0;
/// Badges render their 4x asset (72x72 for Twitch) at native pixel size in
/// the tooltip.
const BADGE_TOOLTIP_IMAGE_PX: f32 = 72.0;
/// One metadata row plus its inter-row gap at Echo's current tooltip typography.
const EMOTE_TOOLTIP_META_LINE_HEIGHT_PX: f32 = 19.0;
const MAX_LAYERED_TOOLTIP_ENTRIES: usize = 7;

/// How a tooltip preview treats animated media.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TooltipMotion {
    Static,
    Animated,
    /// An animated emote a display rule shows as its first frame.
    StillFrame,
}

impl TooltipMotion {
    fn for_emote(meta: Option<&EmoteTooltipMeta>) -> Self {
        match meta {
            Some(meta) if meta.animates() => Self::Animated,
            Some(meta) if meta.still_image => Self::StillFrame,
            _ => Self::Static,
        }
    }
}

pub(crate) fn visible_layered_tooltip_emotes(
    emotes: &[LayeredTooltipEmote],
) -> &[LayeredTooltipEmote] {
    // Reserve the final row for the overflow count.
    let limit = if emotes.len() > MAX_LAYERED_TOOLTIP_ENTRIES {
        MAX_LAYERED_TOOLTIP_ENTRIES - 1
    } else {
        MAX_LAYERED_TOOLTIP_ENTRIES
    };
    &emotes[..emotes.len().min(limit)]
}

/// Tooltip display state
#[derive(Clone, Debug, Default)]
pub struct TooltipState {
    /// Native window that owns the hovered surface.
    pub owner_window: Option<AnyWindowHandle>,
    /// Currently hovered hitbox kind
    pub hovered: Option<HitboxKind>,
    /// Screen position for tooltip popup
    pub position: Point<Pixels>,
    /// Time when hover started (for delay)
    pub hover_start: Option<Instant>,
    /// Whether tooltip is currently visible
    pub is_visible: bool,
}

impl TooltipState {
    /// Create new tooltip state
    pub fn new() -> Self {
        Self::default()
    }

    /// Update hover state from mouse position and hitboxes
    /// Returns true if the hovered element changed (for triggering repaints)
    pub fn update(
        &mut self,
        kind: Option<HitboxKind>,
        owner_window: AnyWindowHandle,
        position: Point<Pixels>,
    ) -> bool {
        match (&self.hovered, &kind) {
            (None, Some(_)) => {
                self.owner_window = Some(owner_window);
                self.hovered = kind;
                self.position = position;
                self.hover_start = Some(Instant::now());
                self.is_visible = TOOLTIP_DELAY_MS == 0;
                true
            }
            (Some(prev), Some(new))
                if self.owner_window != Some(owner_window) || !hitbox_kinds_equal(prev, new) =>
            {
                let was_visible = self.is_visible;
                self.owner_window = Some(owner_window);
                self.hovered = kind;
                self.position = position;
                self.hover_start = Some(Instant::now());
                self.is_visible = was_visible || TOOLTIP_DELAY_MS == 0;
                true
            }
            (Some(_), Some(_)) => {
                let payload_changed = self.hovered != kind;
                self.hovered = kind;
                self.owner_window = Some(owner_window);
                self.position = position;
                if let Some(start) = self.hover_start
                    && start.elapsed() >= Duration::from_millis(TOOLTIP_DELAY_MS)
                {
                    let was_visible = self.is_visible;
                    self.is_visible = true;
                    return payload_changed || !was_visible;
                }
                payload_changed
            }
            (Some(_), None) => {
                self.hovered = None;
                self.hover_start = None;
                self.is_visible = false;
                true
            }
            (None, None) => false,
        }
    }

    /// Whether `kind` is the element this state already tracks.
    pub fn is_hovering(&self, kind: Option<&HitboxKind>) -> bool {
        match (&self.hovered, kind) {
            (Some(hovered), Some(kind)) => hitbox_kinds_equal(hovered, kind),
            (None, None) => true,
            _ => false,
        }
    }

    /// Drop the hover target. Returns true when a real tooltip (not a plain
    /// message-background hover) was visible.
    pub fn hide(&mut self) -> bool {
        let was_showing_tooltip = self.should_show()
            && !matches!(self.hovered, Some(HitboxKind::MessageBackground { .. }));
        self.hovered = None;
        self.hover_start = None;
        self.is_visible = false;
        was_showing_tooltip
    }

    /// Check if tooltip should be shown (delay has passed)
    pub fn should_show(&self) -> bool {
        self.is_visible && self.owner_window.is_some() && self.hovered.is_some()
    }

    /// Remaining delay until the tooltip should become visible.
    ///
    /// Returns `None` when no tooltip is pending (not hovering or already visible).
    pub fn remaining_delay(&self) -> Option<Duration> {
        if self.is_visible || self.hovered.is_none() {
            return None;
        }
        let start = self.hover_start?;
        Some(Duration::from_millis(TOOLTIP_DELAY_MS).saturating_sub(start.elapsed()))
    }

    /// Re-check hover delay without requiring another mouse move event.
    ///
    /// Returns true when visibility changed (hidden -> visible).
    pub fn poll_delay_visibility(&mut self) -> bool {
        if self.is_visible || self.hovered.is_none() {
            return false;
        }
        if let Some(start) = self.hover_start
            && start.elapsed() >= Duration::from_millis(TOOLTIP_DELAY_MS)
        {
            self.is_visible = true;
            return true;
        }
        false
    }
}

/// Check if two hitbox kinds are the same (for hover tracking)
fn hitbox_kinds_equal(a: &HitboxKind, b: &HitboxKind) -> bool {
    match (a, b) {
        (HitboxKind::Emote { url: u1, .. }, HitboxKind::Emote { url: u2, .. }) => u1 == u2,
        (HitboxKind::LayeredEmote { emotes: e1 }, HitboxKind::LayeredEmote { emotes: e2 }) => {
            e1.len() == e2.len()
                && e1
                    .iter()
                    .zip(e2.iter())
                    .all(|(lhs, rhs)| lhs.url == rhs.url)
        }
        (HitboxKind::Badge { name: n1, .. }, HitboxKind::Badge { name: n2, .. }) => n1 == n2,
        (
            HitboxKind::Username {
                login: login1,
                display_name: display1,
                ..
            },
            HitboxKind::Username {
                login: login2,
                display_name: display2,
                ..
            },
        ) => login1 == login2 && display1 == display2,
        (HitboxKind::Url { url: u1, .. }, HitboxKind::Url { url: u2, .. }) => u1 == u2,
        (HitboxKind::Mention { username: u1 }, HitboxKind::Mention { username: u2 }) => u1 == u2,
        (HitboxKind::Emoji { emoji: e1 }, HitboxKind::Emoji { emoji: e2 }) => e1 == e2,
        (
            HitboxKind::MessageBackground { message_id: m1 },
            HitboxKind::MessageBackground { message_id: m2 },
        ) => m1 == m2,
        (
            HitboxKind::ReplyIndicator {
                parent_msg_id: p1, ..
            },
            HitboxKind::ReplyIndicator {
                parent_msg_id: p2, ..
            },
        ) => p1 == p2,
        (HitboxKind::EmbedPreview { url: u1, .. }, HitboxKind::EmbedPreview { url: u2, .. }) => {
            u1 == u2
        }
        (
            HitboxKind::MediaInspector {
                source_url: source1,
                ..
            },
            HitboxKind::MediaInspector {
                source_url: source2,
                ..
            },
        ) => source1 == source2,
        _ => false,
    }
}

fn estimate_emote_tooltip_size(meta: Option<&EmoteTooltipMeta>) -> (f32, f32) {
    let (frame_w, frame_h) = emote_preview_frame_size(meta);
    let meta_lines = meta.map(estimated_emote_meta_line_count).unwrap_or(0) as f32;
    let total_h =
        frame_h + EMOTE_TOOLTIP_FIXED_HEIGHT_PX + (meta_lines * EMOTE_TOOLTIP_META_LINE_HEIGHT_PX);
    (frame_w + 20.0, total_h.clamp(140.0, 380.0))
}

/// Conservative tooltip size estimates used for viewport clamping.
///
/// GPUI doesn't expose laid-out child size before placement in this path, so we clamp using
/// bounded estimates based on tooltip type. Estimates intentionally err slightly large.
pub fn estimate_tooltip_size_for_hitbox(kind: &HitboxKind) -> (f32, f32) {
    match kind {
        HitboxKind::Emote { meta, .. } => estimate_emote_tooltip_size(meta.as_deref()),
        HitboxKind::LayeredEmote { emotes } => {
            if let Some(single) = emotes.first().filter(|_| emotes.len() == 1) {
                return estimate_emote_tooltip_size(single.meta.as_deref());
            }
            let visible = visible_layered_tooltip_emotes(emotes);
            let overflow_height = if visible.len() < emotes.len() {
                19.0
            } else {
                0.0
            };
            (
                360.0,
                (visible.len() as f32 * 58.0 + 24.0 + overflow_height).max(72.0),
            )
        }
        HitboxKind::Badge {
            description, meta, ..
        } => {
            let mut lines = 0.0_f32;
            if let Some(meta) = meta {
                lines += 1.0;
                if meta.tier_label.is_some() {
                    lines += 1.0;
                }
                if meta.is_animated {
                    lines += 1.0;
                }
            }
            if description.is_some() {
                lines += 1.0;
            }
            (
                240.0,
                BADGE_TOOLTIP_IMAGE_PX + EMOTE_TOOLTIP_FIXED_HEIGHT_PX + lines * 24.0,
            )
        }
        HitboxKind::Username { .. } => (240.0, 68.0),
        HitboxKind::Url { url, .. }
        | HitboxKind::EmbedPreview { url, .. }
        | HitboxKind::MediaInspector { url, .. } => {
            match crate::backend::bridge::get_embed_metadata(url) {
                Some(meta) => {
                    let thumb = url_tooltip_thumbnail_url(&meta).is_some();
                    (420.0, if thumb { 300.0 } else { 130.0 })
                }
                None => (420.0, 64.0),
            }
        }
        HitboxKind::Emoji { .. } => (96.0, 48.0),
        HitboxKind::ReplyIndicator { parent_preview, .. } => {
            if parent_preview.is_some() {
                (340.0, 112.0)
            } else {
                (260.0, 84.0)
            }
        }
        HitboxKind::MessageBackground { .. }
        | HitboxKind::Mention { .. }
        | HitboxKind::AutomodAction { .. }
        | HitboxKind::ModAction { .. } => (0.0, 0.0),
    }
}

pub fn render_tooltip_content_for_hitbox(hovered: &HitboxKind) -> Option<AnyElement> {
    let content: AnyElement = match hovered {
        HitboxKind::Emote { name, url, meta } => render_emote_tooltip(name, url, meta.as_deref()),
        HitboxKind::LayeredEmote { emotes } => render_layered_emote_tooltip(emotes),
        HitboxKind::Badge {
            name,
            url,
            description,
            meta,
        } => render_badge_tooltip(name, url, description.as_deref(), meta.as_deref()),
        HitboxKind::Username {
            login,
            display_name,
            color,
            ..
        } => {
            if display_name.eq_ignore_ascii_case(login) {
                return None;
            }
            render_username_tooltip(display_name, login, *color)
        }
        HitboxKind::Url { url, .. } => render_url_tooltip(url),
        HitboxKind::Mention { .. } | HitboxKind::MessageBackground { .. } => return None,
        HitboxKind::EmbedPreview { url, .. } => render_url_tooltip(url),
        HitboxKind::MediaInspector { url, .. } => render_url_tooltip(url),
        HitboxKind::ReplyIndicator {
            parent_display_name,
            parent_preview,
            ..
        } => render_reply_tooltip(parent_display_name, parent_preview.as_deref()),
        HitboxKind::Emoji { emoji } => render_emoji_tooltip(emoji),
        HitboxKind::AutomodAction { .. } | HitboxKind::ModAction { .. } => return None,
    };
    Some(content)
}

/// Render emote tooltip: enlarged preview + name
fn render_emote_tooltip(name: &str, url: &str, meta: Option<&EmoteTooltipMeta>) -> AnyElement {
    let preview_url = emote_tooltip_preview_url(url, meta);
    let (preview_w, preview_h) = emote_preview_frame_size(meta);

    let mut container = tooltip_preview_header(
        px((preview_w + 20.0).max(240.0)),
        preview_w,
        preview_h,
        name,
        SharedString::from(format!("tooltip-emote:{}", name)),
        url,
        preview_url,
        TooltipMotion::for_emote(meta),
    );

    if let Some(meta) = meta {
        container = container.child(render_emote_tooltip_meta(meta));
    }

    container.into_any_element()
}

/// Centered tooltip panel with a framed image preview and a bold name line;
/// callers append their own meta lines.
#[allow(clippy::too_many_arguments)]
fn tooltip_preview_header(
    max_w: Pixels,
    preview_w: f32,
    preview_h: f32,
    name: &str,
    image_id: SharedString,
    url: &str,
    preview_url: SharedString,
    motion: TooltipMotion,
) -> Div {
    tooltip_panel_col(max_w)
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .w(px(preview_w))
                .h(px(preview_h))
                .flex()
                .items_center()
                .justify_center()
                .pt(px(2.0))
                .pb(px(1.0))
                .child(tooltip_image_with_cached_placeholder(
                    image_id,
                    url,
                    preview_url,
                    motion,
                )),
        )
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .text_center()
                .max_w(px(220.0))
                .child(SharedString::from(name)),
        )
}

/// Render a bounded preview of the emote stack.
fn render_layered_emote_tooltip(emotes: &[LayeredTooltipEmote]) -> AnyElement {
    if let Some(single) = emotes.first().filter(|_| emotes.len() == 1) {
        return render_emote_tooltip(&single.name, &single.url, single.meta.as_deref());
    }

    let mut container = tooltip_panel_col(px(360.0)).gap(px(6.0));

    let visible = visible_layered_tooltip_emotes(emotes);
    for emote in visible {
        let detail = emote.meta.as_ref().map(|meta| meta.source_label.clone());

        let mut text_col = div().min_w_0().flex().flex_col().gap(px(1.0)).child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .max_w(px(240.0))
                .child(emote.name.clone()),
        );
        if let Some(detail) = detail {
            text_col = text_col.child(render_meta_line_left(&detail));
        }

        container = container.child(
            div()
                .flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .size(px(30.0))
                        .child(tooltip_image_with_cached_placeholder(
                            SharedString::from(format!("tooltip-layered-emote:{}", emote.name)),
                            &emote.url,
                            emote_tooltip_preview_url(&emote.url, emote.meta.as_deref()),
                            TooltipMotion::for_emote(emote.meta.as_deref()),
                        )),
                )
                .child(text_col),
        );
    }

    let hidden = emotes.len() - visible.len();
    if hidden > 0 {
        container = container.child(render_meta_line_left(&format!("+{hidden} more")));
    }

    container.into_any_element()
}

/// Render badge tooltip: name + description
fn render_badge_tooltip(
    name: &str,
    url: &str,
    description: Option<&str>,
    meta: Option<&BadgeTooltipMeta>,
) -> AnyElement {
    let mut container = tooltip_preview_header(
        px(240.0),
        BADGE_TOOLTIP_IMAGE_PX,
        BADGE_TOOLTIP_IMAGE_PX,
        name,
        SharedString::from(format!("tooltip-badge:{}", name)),
        url,
        SharedString::from(url),
        if meta.is_some_and(|m| m.is_animated) {
            TooltipMotion::Animated
        } else {
            TooltipMotion::Static
        },
    );

    if let Some(desc) = description.filter(|d| *d != name) {
        container = container.child(render_meta_line_center(desc, 224.0));
    }

    if let Some(meta) = meta {
        container = container.child(render_meta_line_center(&meta.source_label, 224.0));
        if let Some(tier) = meta.tier_label.as_ref() {
            container = container.child(render_meta_line_center(tier, 224.0));
        }
        if meta.is_animated {
            container = container.child(render_meta_line_center("Animated", 224.0));
        }
    } else if description.is_none() {
        container = container.child(render_meta_line_center("Badge", 224.0));
    }

    container.into_any_element()
}

fn render_emote_tooltip_meta(meta: &EmoteTooltipMeta) -> AnyElement {
    let mut col = div()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(1.0))
        .pt(px(2.0))
        .child(render_meta_line_center(&meta.source_label, 280.0));
    if let Some(creator) = meta.creator_name.as_ref() {
        col = col.child(render_meta_line_center(&format!("By: {}", creator), 280.0));
    }
    if let Some(original) = meta.original_name.as_ref()
        && !original.is_empty()
    {
        col = col.child(render_meta_line_center(
            &format!("Original: {}", original),
            280.0,
        ));
    }
    if meta.is_animated {
        let label = if meta.still_image {
            "Animated, shown as a still image"
        } else {
            "Animated"
        };
        col = col.child(render_meta_line_center(label, 280.0));
    }
    if meta.is_zero_width {
        col = col.child(render_meta_line_center("Zero-width overlay", 280.0));
    }

    col.into_any_element()
}

fn emote_tooltip_preview_url(fallback_url: &str, meta: Option<&EmoteTooltipMeta>) -> SharedString {
    meta.and_then(|m| m.tooltip_preview_url.as_ref().cloned())
        .unwrap_or_else(|| SharedString::from(fallback_url))
}

/// Render a tooltip image with instant feedback from the shared decoded cache.
///
/// `display_url` is the URL chat already painted, so its decoded image is
/// almost always in the shared cache; `target_url` is the (usually larger)
/// preview fetched through GPUI's async image path. The cached chat-size image
/// is stacked underneath so something shows immediately, and the hi-res img()
/// paints over it once its fetch+decode completes. If the target itself is
/// already decoded in the shared cache it renders directly with no fetch.
fn tooltip_image_with_cached_placeholder(
    id: SharedString,
    display_url: &str,
    target_url: SharedString,
    motion: TooltipMotion,
) -> AnyElement {
    use crate::cache::loader as image_cache;

    if motion == TooltipMotion::StillFrame {
        // Only Echo's static layer: GPUI's `img(url)` would play a GIF/WebP.
        // Chat usually has the displayed copy resident; an emote shown as text
        // has neither, so the preview queues a first-frame decode and waits.
        let resolved = image_cache::get_cached_render_image(target_url.as_ref())
            .or_else(|| image_cache::get_cached_render_image(display_url));
        let placeholder = || {
            div()
                .size_full()
                .rounded(px(4.0))
                .bg(colors::bg_elevated())
                .into_any_element()
        };
        return match resolved {
            Some(render_image) => img(ImageSource::Render(render_image))
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => img(crate::components::ui::still_frame_source(
                target_url,
                image_cache::PreloadSource::TooltipVisible,
            ))
            .id(ElementId::Name(id))
            .size_full()
            .object_fit(ObjectFit::Contain)
            .with_loading(placeholder)
            .with_fallback(placeholder)
            .into_any_element(),
        };
    }

    if motion == TooltipMotion::Animated {
        image_cache::trigger_animated_decode_with_source(
            target_url.as_ref(),
            image_cache::PreloadSource::TooltipVisible,
        );
        // Same order as chat's direct paint: single-frame or over-limit
        // animations keep only their first frame in the static layer.
        let resolved = image_cache::get_current_animated_render_image(&target_url)
            .or_else(|| image_cache::get_current_animated_render_image(display_url))
            .or_else(|| image_cache::get_cached_render_image(target_url.as_ref()))
            .or_else(|| image_cache::get_cached_render_image(display_url));
        if let Some(render_image) = resolved {
            return img(ImageSource::Render(render_image))
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element();
        }
        return div()
            .size_full()
            .rounded(px(4.0))
            .bg(colors::bg_elevated())
            .into_any_element();
    }

    if let Some(render_image) = image_cache::get_cached_render_image(target_url.as_ref()) {
        return img(ImageSource::Render(render_image))
            .size_full()
            .object_fit(ObjectFit::Contain)
            .into_any_element();
    }

    let placeholder = image_cache::get_cached_render_image(display_url);

    let hi_res = img(target_url)
        .id(ElementId::Name(id))
        .size_full()
        .object_fit(ObjectFit::Contain);

    match placeholder {
        Some(render_image) => div()
            .relative()
            .size_full()
            .child(
                img(ImageSource::Render(render_image))
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            )
            .child(hi_res.absolute().inset_0())
            .into_any_element(),
        None => hi_res.into_any_element(),
    }
}

fn emote_preview_frame_size(meta: Option<&EmoteTooltipMeta>) -> (f32, f32) {
    let max_w: f32 = 236.0;
    let max_h: f32 = 142.0;
    let min_w: f32 = 96.0;
    let min_h: f32 = 72.0;

    let Some(meta) = meta else {
        return (220.0, 112.0);
    };
    let (Some(w), Some(h)) = (meta.native_width, meta.native_height) else {
        return (220.0, 112.0);
    };
    if w == 0 || h == 0 {
        return (220.0, 112.0);
    }

    let aspect = w as f32 / h as f32;
    if aspect >= 1.0 {
        let mut frame_w = max_w.min((max_h * aspect).round());
        let mut frame_h = (frame_w / aspect).round();
        if frame_h > max_h {
            frame_h = max_h;
            frame_w = (frame_h * aspect).round();
        }
        (frame_w.clamp(min_w, max_w), frame_h.clamp(min_h, max_h))
    } else {
        let mut frame_h = max_h.min((max_w / aspect).round());
        let mut frame_w = (frame_h * aspect).round();
        if frame_w > max_w {
            frame_w = max_w;
            frame_h = (frame_w / aspect).round();
        }
        (frame_w.clamp(min_w, max_w), frame_h.clamp(min_h, max_h))
    }
}

fn estimated_emote_meta_line_count(meta: &EmoteTooltipMeta) -> usize {
    let mut lines = 1usize;
    if meta.creator_name.is_some() {
        lines += 1;
    }
    if meta.original_name.as_deref().is_some_and(|s| !s.is_empty()) {
        lines += 1;
    }
    if meta.is_animated {
        lines += 1;
    }
    if meta.is_zero_width {
        lines += 1;
    }
    lines
}

fn render_meta_line_left(text: &str) -> AnyElement {
    div()
        .text_xs()
        .text_color(colors::text_primary().opacity(0.84))
        .max_w(px(280.0))
        .child(SharedString::from(text))
        .into_any_element()
}

fn render_meta_line_center(text: &str, max_w: f32) -> AnyElement {
    div()
        .text_xs()
        .text_color(colors::text_primary().opacity(0.86))
        .max_w(px(max_w))
        .text_center()
        .child(SharedString::from(text))
        .into_any_element()
}

fn tooltip_shell(max_w: Pixels) -> Div {
    div()
        .max_w(max_w)
        .bg(colors::bg_void().opacity(0.84))
        .border_1()
        .border_color(colors::text_primary().opacity(0.16))
        .rounded(px(4.0))
        .shadow_lg()
        .px(px(8.0))
        .py(px(6.0))
}

fn tooltip_panel_col(max_w: Pixels) -> Div {
    tooltip_shell(max_w).flex().flex_col()
}

/// Render username tooltip for a localized display name: name plus login.
fn render_username_tooltip(display_name: &str, login: &str, color: Option<Hsla>) -> AnyElement {
    let text_color = color.unwrap_or(colors::text_primary());

    div()
        .child(
            tooltip_panel_col(px(240.0))
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(text_color)
                        .max_w(px(220.0))
                        .child(display_name.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_secondary())
                        .max_w(px(220.0))
                        .child(format!("@{}", login)),
                ),
        )
        .into_any_element()
}

fn url_tooltip_thumbnail_url(meta: &crate::backend::link_types::EmbedMetadata) -> Option<&str> {
    if crate::backend::streamer_mode::should_hide_link_thumbnails() {
        return None;
    }
    let url = meta.thumbnail_url.as_deref()?;
    crate::backend::embed_thumbnails::should_render_thumbnail(
        crate::backend::settings::get_embed_thumbnail_mode(),
        Some(url),
    )
    .then_some(url)
}

/// Render URL tooltip: metadata preview plus full URL
fn render_url_tooltip(url: &str) -> AnyElement {
    use crate::backend::link_types::{format_content_length, get_domain, short_content_type_label};

    let meta = if crate::backend::upload_history::is_known_upload_deletion_url(url) {
        None
    } else {
        crate::backend::bridge::get_embed_metadata(url)
    };
    let host = get_domain(url);

    let mut panel = tooltip_panel_col(px(400.0)).gap(px(3.0));

    if crate::utils::is_url_opened(url) {
        panel = panel.child(render_meta_line_left("Opened in Echo this session"));
    }

    if let Some(meta) = meta.as_ref() {
        if let Some(thumb) = url_tooltip_thumbnail_url(meta) {
            panel = panel.child(
                div()
                    .w(px(320.0))
                    .max_h(px(180.0))
                    .rounded(px(4.0))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(SharedString::from(thumb))
                            .max_w(px(320.0))
                            .max_h(px(180.0))
                            .object_fit(ObjectFit::Contain),
                    ),
            );
        }

        let type_label = meta
            .content_type
            .as_deref()
            .and_then(short_content_type_label);
        let header = meta.title.clone().unwrap_or_else(|| match &type_label {
            Some(type_label) => format!("{} - {}", type_label, host),
            None => host.clone(),
        });
        panel = panel.child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .max_w(px(400.0))
                .child(header),
        );

        let info = match (&type_label, meta.content_length) {
            (Some(type_label), Some(length))
                if meta
                    .content_type
                    .as_deref()
                    .is_some_and(|content_type| content_type.starts_with("image/")) =>
            {
                Some(format!(
                    "View {} image ({}) on {}",
                    type_label,
                    format_content_length(length),
                    host
                ))
            }
            (Some(type_label), None)
                if meta
                    .content_type
                    .as_deref()
                    .is_some_and(|content_type| content_type.starts_with("image/")) =>
            {
                Some(format!("View {} image on {}", type_label, host))
            }
            _ => meta.subtitle.clone().filter(|subtitle| *subtitle != host),
        };
        if let Some(info) = info {
            panel = panel.child(render_meta_line_left(&info));
        }
    }

    panel
        .child(
            div()
                .text_xs()
                .text_color(colors::link())
                .max_w(px(400.0))
                .child(SharedString::from(url)),
        )
        .into_any_element()
}

/// Render emoji tooltip: shows the emoji character
fn render_emoji_tooltip(emoji: &str) -> AnyElement {
    tooltip_panel_col(px(96.0))
        .items_center()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child(SharedString::from(emoji)),
        )
        .into_any_element()
}

/// Render reply indicator tooltip: shows parent username and preview
fn render_reply_tooltip(parent_display_name: &str, parent_preview: Option<&str>) -> AnyElement {
    let parent_color = get_user_color(parent_display_name)
        .map(|hex| get_cached_color(&hex))
        .unwrap_or(colors::text_secondary());

    let mut tooltip = tooltip_panel_col(px(300.0)).gap_1().child(
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child("Replying to"),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(parent_color)
                    .child(format!("@{}", parent_display_name)),
            ),
    );

    if let Some(preview) = parent_preview {
        tooltip = tooltip.child(
            div()
                .text_xs()
                .text_color(colors::text_secondary())
                .max_w(px(280.0))
                .child(SharedString::from(preview)),
        );
    }

    tooltip
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .mt_1()
                .child("Click to jump • Right-click for thread"),
        )
        .into_any_element()
}
