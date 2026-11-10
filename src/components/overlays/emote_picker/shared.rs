//! Row model, viewport publication, and load-more confirmation shared by the
//! inline and popout emote pickers.

use super::core::{
    CategoryHeader, picker_display_url, picker_native_animated_enabled, provider_display_name,
};
use crate::backend::settings::EmotePickerAnimationMode;
use crate::backend::{self, Emote, EmoteProvider};
use crate::cache::loader as image_cache;
use crate::components::overlays::traits::OverlayPointerIsolation;
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use echo_emote_core::EmoteScope;
use gpui::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub(super) trait PickerButtonExt: ButtonExt {
    fn on_picker_activate(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_accessible_activate(handler).tab_index(-1)
    }
}

impl<E: ButtonExt> PickerButtonExt for E {}

pub(super) fn picker_accessible_label(emote: &Emote, channel: &str) -> String {
    let provider = provider_display_name(&emote.provider);
    let scope = match emote.scope() {
        _ if emote.provider == EmoteProvider::Emoji => "",
        EmoteScope::Global => " global",
        EmoteScope::Personal => " personal",
        EmoteScope::Subscription(_) => " subscription",
        EmoteScope::Channel(channel) if channel.starts_with("twitch:") => " global",
        EmoteScope::Channel(_) => " channel",
    };
    let mut label = if emote.provider == EmoteProvider::Emoji {
        format!(
            "{}, {provider}",
            emote.original_name.as_deref().unwrap_or(&emote.name)
        )
    } else {
        format!("{}, {provider}{scope}", emote.name)
    };
    // The lightweight picker view omits provider flags; only trust a matching cached identity.
    if emote.provider != EmoteProvider::Emoji
        && backend::emotes::get_emote_fast_case_sensitive(channel, &emote.name).is_some_and(
            |metadata| {
                metadata.id == emote.id
                    && metadata.provider == emote.provider
                    && metadata.zero_width
            },
        )
    {
        label.push_str(", zero-width emote");
    }
    label
}

pub(super) fn picker_cell_id(emote: &Emote) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    emote.provider.hash(&mut hasher);
    emote.id.hash(&mut hasher);
    emote.name.hash(&mut hasher);
    emote.channel.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn picker_control_row(
    id: &'static str,
    row: usize,
    control: impl IntoElement,
) -> AnyElement {
    div()
        .id((id, row))
        .role(Role::Row)
        .aria_row_index(row + 1)
        .w_full()
        .h(px(super::core::EMOTE_PICKER_ROW_HEIGHT))
        .child(
            div()
                .id("control-cell")
                .role(Role::GridCell)
                .aria_column_index(1)
                .size_full()
                .child(control),
        )
        .into_any_element()
}

#[derive(Default)]
pub(super) struct PickerFocusOrder {
    handles: HashMap<String, FocusHandle>,
    order: Vec<(FocusHandle, Option<(UniformListScrollHandle, usize)>)>,
}

impl PickerFocusOrder {
    pub(super) fn begin(&mut self) {
        self.order.clear();
    }

    pub(super) fn include(&mut self, handle: FocusHandle) {
        if !self.order.iter().any(|(existing, _)| *existing == handle) {
            self.order.push((handle, None));
        }
    }

    pub(super) fn register(&mut self, key: impl Into<String>, cx: &mut App) -> FocusHandle {
        let handle = self
            .handles
            .entry(key.into())
            .or_insert_with(|| cx.focus_handle())
            .clone();
        self.include(handle.clone());
        handle
    }

    pub(super) fn register_row(
        &mut self,
        key: impl Into<String>,
        row: usize,
        scroll: &UniformListScrollHandle,
        cx: &mut App,
    ) -> FocusHandle {
        let handle = self.register(key, cx);
        if let Some((_, reveal)) = self.order.iter_mut().find(|(entry, _)| *entry == handle) {
            *reveal = Some((scroll.clone(), row));
        }
        handle
    }

    pub(super) fn cycle(&self, reverse: bool, window: &mut Window, cx: &mut App) {
        if self.order.is_empty() {
            return;
        }
        let current = self
            .order
            .iter()
            .position(|(handle, _)| handle.is_focused(window));
        let next = match (current, reverse) {
            (Some(0) | None, true) => self.order.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % self.order.len(),
            (None, false) => 0,
        };
        let (handle, reveal) = &self.order[next];
        if let Some((scroll, row)) = reveal {
            scroll.scroll_to_item(*row, ScrollStrategy::Nearest);
        }
        window.focus(handle, cx);
    }

    pub(super) fn ensure_focused(&self, window: &mut Window, cx: &mut App) {
        if !self
            .order
            .iter()
            .any(|(handle, _)| handle.is_focused(window))
        {
            self.cycle(false, window, cx);
        }
    }
}

/// A virtualized row in the emote picker grid
#[derive(Clone)]
pub(super) enum VirtualRow {
    /// Category header row
    Header(CategoryHeader),
    /// Row of emotes packed to fit current available row width
    EmoteRow {
        /// Emotes with pre-computed favorite status (emote, is_favorite)
        emotes: Vec<(Arc<Emote>, bool)>,
        base_index: usize,
    },
    /// Footer row that lets the user intentionally reveal more emotes.
    LoadMore { shown: usize, total: usize },
}

pub(super) fn pack_visible_range(start: usize, end: usize) -> u64 {
    let start = start.min(u32::MAX as usize) as u64;
    let end = end.min(u32::MAX as usize) as u64;
    (start << 32) | end
}

pub(super) fn publish_visible_emote_rows(
    virtual_rows: &[VirtualRow],
    start_row: usize,
    end_row: usize,
    animation_mode: EmotePickerAnimationMode,
    viewport: image_cache::PickerViewport,
) -> bool {
    let start_row = start_row.min(virtual_rows.len());
    let end_row = end_row.min(virtual_rows.len());
    if end_row <= start_row {
        image_cache::clear_picker_visible_urls(viewport);
        return false;
    }

    let visible_emotes: Vec<&Arc<Emote>> = virtual_rows[start_row..end_row]
        .iter()
        .filter_map(|row| match row {
            VirtualRow::EmoteRow { emotes, .. } => Some(emotes.iter()),
            VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
        })
        .flatten()
        .map(|(emote, _)| emote)
        .collect();
    let compact_animations = super::prefetch::picker_uses_compact_animation_variant(
        visible_emotes
            .iter()
            .filter(|emote| emote.is_animated)
            .count(),
        animation_mode,
    );
    let mut media_urls = Vec::with_capacity(visible_emotes.len() * 2);
    let mut animation_urls = Vec::with_capacity(visible_emotes.len());
    for emote in visible_emotes {
        let primary_url = picker_display_url(emote);
        media_urls.push(primary_url);
        if emote.is_animated {
            let fallback_url = super::prefetch::picker_animation_fallback_url(emote, primary_url);
            if let Some(fallback_url) = fallback_url {
                media_urls.push(fallback_url);
            }
            if !picker_native_animated_enabled() {
                match animation_mode {
                    EmotePickerAnimationMode::Always => {
                        animation_urls.push(super::prefetch::picker_active_animation_url(
                            emote,
                            primary_url,
                            compact_animations,
                        ));
                    }
                    EmotePickerAnimationMode::Hover => {
                        animation_urls.push(super::prefetch::picker_active_animation_url(
                            emote,
                            primary_url,
                            false,
                        ));
                    }
                    EmotePickerAnimationMode::Never => {}
                }
            }
        }
    }
    image_cache::publish_picker_visible_urls(viewport, media_urls, animation_urls);
    true
}

/// Modal confirmation shown before revealing the full emote list.
pub(super) fn render_load_more_warning<V: 'static>(
    id: &'static str,
    hide_next_time: bool,
    cancel: fn(&mut V, &mut Context<V>),
    toggle_hide: fn(&mut V, &mut Context<V>),
    confirm: fn(&mut V, &mut Context<V>),
    focus_order: &mut PickerFocusOrder,
    cx: &mut Context<V>,
) -> impl IntoElement + use<V> {
    focus_order.begin();
    let toggle_focus = focus_order.register("warning-toggle", cx);
    let cancel_focus = focus_order.register("warning-cancel", cx);
    let confirm_focus = focus_order.register("warning-confirm", cx);
    div()
        .id(id)
        .absolute()
        .inset_0()
        .block_overlay_pointer_passthrough()
        .bg(colors::backdrop())
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _window, cx| {
                cancel(this, cx);
            }),
        )
        .child(
            div()
                .w(px(360.0))
                .bg(colors::bg_elevated())
                .rounded(Radii::lg())
                .border_1()
                .border_color(colors::border())
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .px_4()
                        .pt_4()
                        .pb_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(22.0))
                                .rounded(Radii::full())
                                .bg(colors::warning().opacity(0.18))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .font_weight(FontWeight::BOLD)
                                .text_color(colors::warning())
                                .child("!"),
                        )
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child("Load more emotes"),
                        ),
                )
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child(
                            "Loading all emotes may take up significant resources while the picker is open. Are you sure?",
                        ),
                )
                .child(
                    div()
                        .id("load-all-warning-dont-show")
                        .role(Role::Button)
                        .aria_label("Do not show this warning again")
                        .track_focus(&toggle_focus)
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .on_picker_activate(
                            cx.listener(move |this, _, _window, cx| {
                                toggle_hide(this, cx);
                            }),
                        )
                        .child(
                            div()
                                .size(px(16.0))
                                .rounded(Radii::sm())
                                .border_1()
                                .border_color(if hide_next_time {
                                    colors::accent()
                                } else {
                                    colors::border()
                                })
                                .bg(if hide_next_time {
                                    colors::accent()
                                } else {
                                    colors::bg_surface()
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .text_color(colors::text_on_accent_contrast())
                                .child(if hide_next_time { "✓" } else { "" }),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .child("Do not show this warning again"),
                        ),
                )
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("load-all-warning-cancel")
                                .role(Role::Button)
                                .aria_label("Cancel loading more emotes")
                                .track_focus(&cancel_focus)
                                .px_3()
                                .py_2()
                                .min_w(px(76.0))
                                .rounded(Radii::sm())
                                .bg(colors::bg_hover())
                                .hover(|s| s.bg(colors::pressed_surface()))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .on_picker_activate(
                                    cx.listener(move |this, _, _window, cx| {
                                        cancel(this, cx);
                                    }),
                                )
                                .child("Cancel"),
                        )
                        .child(
                            div()
                                .id("load-all-warning-confirm")
                                .role(Role::Button)
                                .aria_label("Load more emotes")
                                .track_focus(&confirm_focus)
                                .px_3()
                                .py_2()
                                .min_w(px(96.0))
                                .rounded(Radii::sm())
                                .bg(colors::accent())
                                .hover(|s| s.bg(colors::accent_hover()))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_on_accent_contrast())
                                .on_picker_activate(
                                    cx.listener(move |this, _, _window, cx| {
                                        confirm(this, cx);
                                    }),
                                )
                                .child("Load more"),
                        ),
                ),
        )
}
