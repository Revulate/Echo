//! The emote grid of both picker surfaces: loading and empty states, the
//! virtualized list, and its header, load-more and emote rows.

use super::core::{
    EMOTE_PICKER_ROW_HEIGHT, LoadingState, calculate_emote_dimensions, picker_display_url,
    picker_native_animated_enabled, picker_tooltip_text, provider_color,
};
use super::grid::{PickerHost, PickerShell};
use super::shared::{
    PickerButtonExt, VirtualRow, picker_accessible_label, picker_cell_id, picker_control_row,
    publish_visible_emote_rows,
};
use super::stability::{self, PickerCellVisualState};
use crate::backend::Emote;
use crate::backend::settings::EmotePickerAnimationMode;
use crate::cache::loader as image_cache;
use crate::components::overlays::global_tooltip::{
    clear_global_tooltip_if_source_in_window, show_text_tooltip,
};
use crate::components::ui::{ButtonExt, PressToClick};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROW_HEIGHT: f32 = EMOTE_PICKER_ROW_HEIGHT;

/// Element ids of one picker surface's grid.
pub(super) struct GridElementIds {
    pub(super) grid: &'static str,
    pub(super) list: &'static str,
    pub(super) header: &'static str,
    pub(super) header_row: &'static str,
    pub(super) load_more: &'static str,
    pub(super) load_more_button: &'static str,
    pub(super) load_more_row: &'static str,
    pub(super) emote_row: &'static str,
    pub(super) emote_cell: &'static str,
    pub(super) animated_emote: &'static str,
}

/// How a hover change that admits or stops a cell's animation reaches the screen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HoverRepaint {
    /// Notify the picker entity (records `picker.hover_animation`).
    NotifyPicker,
    /// Refresh the picker's window.
    RefreshWindow,
}

/// Styling of the name placeholder drawn while a cell's image is not ready.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaceholderStyle {
    /// Clip the name to the image box.
    Clipped,
    /// Pad the name horizontally.
    Padded,
}

/// Grid presentation that differs between the picker surfaces.
pub(super) struct GridStyle {
    pub(super) ids: GridElementIds,
    /// Give special (recent/favorite) category headers their own background
    /// and icon colour.
    pub(super) highlight_special_headers: bool,
    /// Show an icon above the empty-state message.
    pub(super) empty_state_icon: bool,
    pub(super) placeholder: PlaceholderStyle,
    pub(super) hover_repaint: HoverRepaint,
    /// Publish a viewport range even when it holds no emote cell (such as
    /// `uniform_list`'s single-row measuring pass).
    pub(super) track_ranges_without_emotes: bool,
    /// Feed the benchmark stability telemetry.
    pub(super) record_stability: bool,
    /// Time each `uniform_list` row-build callback, when the returned switch is on.
    pub(super) row_build_timing: Option<RowBuildTiming>,
}

#[derive(Clone, Copy)]
pub(super) struct RowBuildTiming {
    pub(super) enabled: fn() -> bool,
    pub(super) record: fn(Duration),
}

pub(super) fn visible_emote_cell_count(rows: &[VirtualRow], start: usize, end: usize) -> usize {
    let start = start.min(rows.len());
    let end = end.min(rows.len());
    rows[start..end]
        .iter()
        .map(|row| match row {
            VirtualRow::EmoteRow { emotes, .. } => emotes.len(),
            VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => 0,
        })
        .sum()
}

pub(super) fn visible_animated_emote_cell_count(
    rows: &[VirtualRow],
    start: usize,
    end: usize,
) -> usize {
    let start = start.min(rows.len());
    let end = end.min(rows.len());
    rows[start..end]
        .iter()
        .map(|row| match row {
            VirtualRow::EmoteRow { emotes, .. } => {
                emotes.iter().filter(|(emote, _)| emote.is_animated).count()
            }
            VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => 0,
        })
        .sum()
}

/// The grid element of the current tab, tracking the grid focus.
fn grid_container<S: PickerShell>(this: &S) -> Stateful<Div> {
    let grid = this.grid();
    div()
        .id(grid.host.style.ids.grid)
        .role(Role::Grid)
        .aria_label(format!("Emotes, {}", grid.core.active_tab.label()))
        .aria_row_count(grid.virtual_rows.len())
        .when_some(grid.grid_focus.as_ref(), |container, focus| {
            container.track_focus(focus)
        })
}

/// The grid: a loading state, an empty state, or the virtualized rows.
pub(super) fn render_grid<S: PickerShell>(this: &mut S, cx: &mut Context<S>) -> AnyElement {
    if let Some(focus) = this.grid().grid_focus.clone() {
        this.grid_mut().focus_order.include(focus);
    }
    let host = this.grid().host;

    if this.grid().core.get_loading_state() == LoadingState::Loading {
        let glyph = div()
            .text_2xl()
            .text_color(colors::accent_text())
            .child("⏳");
        let glyph: AnyElement = if cx.reduce_motion() {
            glyph.into_any_element()
        } else {
            glyph
                .with_animation(
                    "spinner",
                    Animation::new(Duration::from_secs(1))
                        .repeat()
                        .with_easing(pulsating_between(0.6, 1.0)),
                    |s, delta| s.opacity(delta),
                )
                .into_any_element()
        };
        return grid_container(this)
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(glyph)
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_muted())
                    .child("Loading emotes…"),
            )
            .into_any_element();
    }

    if this.grid().virtual_rows.is_empty() {
        let message = div().text_sm().text_color(colors::text_muted());
        let content = if host.style.empty_state_icon {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(div().text_xl().text_color(colors::text_muted()).child("😶"))
                .child(message.text_center().child(empty_message(this)))
                .into_any_element()
        } else {
            message.child(empty_message(this)).into_any_element()
        };
        return grid_container(this)
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(content)
            .into_any_element();
    }

    let list = emote_list(this, cx);
    grid_container(this)
        .flex_1()
        .p_2()
        .child(list)
        .into_any_element()
}

fn empty_message<S: PickerShell>(this: &S) -> SharedString {
    this.grid().core.empty_state_message().into()
}

/// The virtualized row list. Its row callback cannot borrow the picker, so
/// everything a row needs is captured by value.
fn emote_list<S: PickerShell>(this: &mut S, cx: &mut Context<S>) -> UniformList {
    let channel = this.accessible_channel().to_string();
    let grid = this.grid_mut();
    let host = grid.host;
    let virtual_rows = Arc::clone(&grid.virtual_rows);
    let row_focuses: Vec<Option<FocusHandle>> = virtual_rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            let key = match row {
                VirtualRow::Header(header) => format!("header-{}", header.id),
                VirtualRow::LoadMore { .. } => "load-more".to_string(),
                VirtualRow::EmoteRow { .. } => return None,
            };
            Some(
                grid.focus_order
                    .register_row(key, row_idx, &grid.scroll_handle, cx),
            )
        })
        .collect();
    let rows = RowContext {
        host,
        selected_idx: grid.selected_emote_index,
        render_version: grid.render_version,
        animation_mode: grid.animation_mode,
        channel,
        picker: cx.entity().downgrade(),
    };
    let row_count = virtual_rows.len();
    let timing = host
        .style
        .row_build_timing
        .filter(|timing| (timing.enabled)());

    uniform_list(
        (host.style.ids.list, rows.render_version),
        row_count,
        move |range: Range<usize>, window: &mut Window, _cx: &mut App| {
            let _scale_scope = crate::components::chat::canvas_renderer::layout_scale::enter(
                window.scale_factor(),
            );
            let timing_started = timing.map(|_| Instant::now());
            let observed_emote_cells =
                visible_emote_cell_count(virtual_rows.as_slice(), range.start, range.end);
            if host.style.track_ranges_without_emotes
                || (observed_emote_cells > 0 && range.end > range.start)
            {
                if host.signals.visible_range() != (range.start, range.end) {
                    publish_visible_emote_rows(
                        virtual_rows.as_slice(),
                        range.start,
                        range.end,
                        rows.animation_mode,
                        host.signals.viewport(),
                    );
                }
                host.signals.signal_visible_range(range.start, range.end);
            }
            if host.style.record_stability {
                stability::record_visible_range(range.start, range.end, observed_emote_cells);
            }
            let compact_animations = super::prefetch::picker_uses_compact_animation_variant(
                visible_animated_emote_cell_count(virtual_rows.as_slice(), range.start, range.end),
                rows.animation_mode,
            );
            let a11y_active = window.is_a11y_active();
            let built = range
                .map(|row_idx| {
                    rows.render(
                        &virtual_rows[row_idx],
                        row_idx,
                        row_focuses[row_idx].clone(),
                        compact_animations,
                        a11y_active,
                    )
                })
                .collect();
            if let (Some(timing), Some(started)) = (timing, timing_started) {
                (timing.record)(started.elapsed());
            }
            built
        },
    )
    .flex_1()
    .h_full()
    .track_scroll(&this.grid().scroll_handle)
}

/// What every row of one list build shares.
struct RowContext<S: 'static> {
    host: &'static PickerHost,
    selected_idx: usize,
    render_version: u32,
    animation_mode: EmotePickerAnimationMode,
    channel: String,
    picker: WeakEntity<S>,
}

impl<S: PickerShell> RowContext<S> {
    fn render(
        &self,
        row: &VirtualRow,
        row_idx: usize,
        focus: Option<FocusHandle>,
        compact_animations: bool,
        a11y_active: bool,
    ) -> AnyElement {
        let ids = &self.host.style.ids;
        let signals = self.host.signals;
        match row {
            VirtualRow::Header(header) => {
                let header_id = header.id.clone();
                let expand_id = header.id.clone();
                let collapse_id = header.id.clone();
                let collapsed = header.is_collapsed;
                let highlight = self.host.style.highlight_special_headers && header.is_special;
                let control = div()
                    .id((ids.header, row_idx))
                    .role(Role::Button)
                    .aria_label(header.name.clone())
                    .aria_expanded(!header.is_collapsed)
                    .on_a11y_action(AccessibleAction::Expand, move |_, window, _| {
                        if collapsed {
                            signals.signal_header_click(expand_id.clone());
                            window.refresh();
                        }
                    })
                    .on_a11y_action(AccessibleAction::Collapse, move |_, window, _| {
                        if !collapsed {
                            signals.signal_header_click(collapse_id.clone());
                            window.refresh();
                        }
                    })
                    .when_some(focus, |row, focus| row.track_focus(&focus))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .when(highlight, |s| s.bg(colors::bg_plate()))
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_picker_activate(move |_, window, _| {
                        signals.signal_header_click(header_id.clone());
                        window.refresh();
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(if header.is_collapsed { "▶" } else { "▼" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(if let Some(ref p) = header.provider {
                                provider_color(Some(*p))
                            } else if highlight || !self.host.style.highlight_special_headers {
                                colors::accent()
                            } else {
                                colors::text_secondary()
                            })
                            .child(header.icon.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .child(header.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(header.count_str.clone()),
                    );
                picker_control_row(ids.header_row, row_idx, control)
            }
            VirtualRow::LoadMore { shown, total } => {
                let remaining = total.saturating_sub(*shown);
                let control = div()
                    .id((ids.load_more, row_idx))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(format!("Showing {} of {}", shown, total)),
                    )
                    .child(
                        div()
                            .id(ids.load_more_button)
                            .role(Role::Button)
                            .aria_label("Load more emotes")
                            .when_some(focus, |button, focus| button.track_focus(&focus))
                            .px_3()
                            .py_1()
                            .rounded(Radii::sm())
                            .border_1()
                            .border_color(colors::border())
                            .bg(colors::bg_surface())
                            .hover(|s| s.bg(colors::bg_hover()))
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .on_picker_activate(move |_, window, _| {
                                signals.signal_load_more_click();
                                window.refresh();
                            })
                            .child(if remaining > 0 {
                                format!("Load more ({remaining})")
                            } else {
                                "Load more".to_string()
                            }),
                    );
                picker_control_row(ids.load_more_row, row_idx, control)
            }
            VirtualRow::EmoteRow { emotes, base_index } => div()
                .id((ids.emote_row, row_idx))
                .role(Role::Row)
                .aria_row_index(row_idx + 1)
                .w_full()
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap_1()
                .children(
                    emotes
                        .iter()
                        .enumerate()
                        .map(|(column, (emote, is_favorite))| {
                            self.render_cell(
                                emote,
                                *is_favorite,
                                base_index + column,
                                column,
                                compact_animations,
                                a11y_active,
                            )
                        }),
                )
                .into_any_element(),
        }
    }

    fn render_cell(
        &self,
        emote: &Arc<Emote>,
        is_favorite: bool,
        emote_idx: usize,
        column: usize,
        compact_animations: bool,
        a11y_active: bool,
    ) -> AnyElement {
        let style = &self.host.style;
        let signals = self.host.signals;
        let animation_mode = self.animation_mode;
        let record_stability = style.record_stability;
        let is_selected = emote_idx == self.selected_idx;
        let emote_url = picker_display_url(emote).to_string();
        let fallback_animation_url =
            super::prefetch::picker_animation_fallback_url(emote, &emote_url).map(str::to_string);
        let active_animation_url =
            super::prefetch::picker_active_animation_url(emote, &emote_url, compact_animations)
                .to_string();
        let active_media_identity =
            crate::backend::types::media_source_id_for_url(emote, &active_animation_url);
        let (cell_width, cell_height, img_width, img_height) = calculate_emote_dimensions(emote);
        let is_animated = emote.is_animated;
        let should_animate =
            is_animated && animation_mode.animates_cell(signals.hovered_emote() == Some(emote_idx));
        let media_identity = crate::backend::types::media_source_id_for_url(emote, &emote_url);
        let tooltip_text = picker_tooltip_text(emote);
        let tooltip_source = format!(
            "{}:{}:{emote_idx}",
            self.host.tooltip_source, self.render_version
        );
        let selection_emote = Arc::clone(emote);
        let accessible_emote = Arc::clone(emote);
        let favorite_emote = Arc::clone(emote);
        let focus_emote = Arc::clone(emote);
        let selection_picker = self.picker.clone();
        let accessible_picker = self.picker.clone();
        let focus_picker = self.picker.clone();
        let hover_picker = self.picker.clone();

        let placeholder = || {
            if record_stability {
                stability::record_cell(emote_idx, PickerCellVisualState::Placeholder);
            }
            div()
                .w(px(img_width))
                .h(px(img_height))
                .rounded(Radii::sm())
                .bg(colors::bg_hover())
                .flex()
                .items_center()
                .justify_center()
                .map(|s| match style.placeholder {
                    PlaceholderStyle::Clipped => s.overflow_hidden(),
                    PlaceholderStyle::Padded => s.px_1(),
                })
                .text_xs()
                .text_color(colors::text_muted())
                .child(emote.name.to_string())
                .into_any_element()
        };
        let rendered = |element: AnyElement| {
            if record_stability {
                stability::record_cell(emote_idx, PickerCellVisualState::Rendered);
            }
            element
        };
        let image = |source: ImageSource| {
            rendered(
                img(source)
                    .w(px(img_width))
                    .h(px(img_height))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
            )
        };

        let content = if should_animate && picker_native_animated_enabled() {
            let stable_instance_id = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                emote_idx.hash(&mut hasher);
                emote_url.hash(&mut hasher);
                hasher.finish()
            };
            rendered(
                img(emote_url.clone())
                    .id((style.ids.animated_emote, stable_instance_id))
                    .w(px(img_width))
                    .h(px(img_height))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
            )
        } else if should_animate {
            match super::prefetch::picker_current_animated_frame(
                &emote_url,
                fallback_animation_url.as_deref(),
                &active_animation_url,
            ) {
                Some((render_image, _metadata, displayed_frame_index, active_url)) => {
                    if record_stability {
                        stability::record_animated_frame_source(
                            emote_idx,
                            active_url,
                            displayed_frame_index,
                        );
                    }
                    image(render_image.into())
                }
                _ => {
                    if animation_mode == EmotePickerAnimationMode::Hover {
                        image_cache::trigger_animated_decode_media(
                            &active_animation_url,
                            active_media_identity.clone(),
                            image_cache::PreloadSource::PickerCell,
                        );
                    }
                    match image_cache::get_animated_poster_render_image(&active_animation_url)
                        .or_else(|| {
                            image_cache::get_stable_animated_render_image(&active_animation_url)
                        })
                        .or_else(|| image_cache::get_cached_render_image(&active_animation_url))
                        .or_else(|| image_cache::get_animated_poster_render_image(&emote_url))
                        .or_else(|| image_cache::get_stable_animated_render_image(&emote_url))
                        .or_else(|| image_cache::get_cached_render_image(&emote_url))
                    {
                        Some(render_image) => image(render_image.into()),
                        _ => placeholder(),
                    }
                }
            }
        } else if is_animated {
            match image_cache::get_animated_poster_render_image(&emote_url)
                .or_else(|| image_cache::get_stable_animated_render_image(&emote_url))
            {
                Some(render_image) => image(render_image.into()),
                _ => {
                    if !image_cache::is_downloading(&emote_url) {
                        image_cache::trigger_animated_poster_preload_media_single(
                            &emote_url,
                            media_identity.clone(),
                            image_cache::PreloadSource::PickerCell,
                        );
                    }
                    match image_cache::get_animated_poster_render_image(&emote_url)
                        .or_else(|| image_cache::get_stable_animated_render_image(&emote_url))
                    {
                        Some(render_image) => image(render_image.into()),
                        _ => placeholder(),
                    }
                }
            }
        } else {
            match image_cache::get_cached_render_image(&emote_url) {
                Some(render_image) => image(render_image.into()),
                _ => {
                    if !image_cache::is_downloading(&emote_url) {
                        image_cache::trigger_preload_media_single(
                            &emote_url,
                            media_identity.clone(),
                            image_cache::PreloadSource::PickerCell,
                        );
                    }
                    placeholder()
                }
            }
        };

        div()
            .id((style.ids.emote_cell, picker_cell_id(emote)))
            .role(Role::GridCell)
            .aria_column_index(column + 1)
            .when(a11y_active, |cell| {
                cell.aria_label(picker_accessible_label(emote, &self.channel))
                    .aria_selected(is_selected)
                    .when(is_selected, |cell| cell.aria_active_descendant())
            })
            .on_a11y_action(AccessibleAction::Click, move |_, _, app| {
                accessible_picker
                    .update(app, |picker, cx| {
                        picker.activate_emote(Arc::clone(&accessible_emote), false, cx);
                    })
                    .ok();
            })
            .on_a11y_action(AccessibleAction::Focus, move |_, window, app| {
                let focus = focus_picker
                    .update(app, |picker, cx| {
                        let index = picker
                            .grid()
                            .visible_position(|emote| Arc::ptr_eq(emote, &focus_emote))?;
                        super::grid::apply_selection_move(picker, index, cx);
                        picker.grid().grid_focus.clone()
                    })
                    .ok()
                    .flatten();
                if let Some(focus) = focus {
                    window.focus(&focus, app);
                }
            })
            .w(px(cell_width))
            .h(px(cell_height))
            .rounded(Radii::sm())
            .cursor_pointer()
            .relative()
            .when(is_selected, |s| {
                s.bg(colors::bg_hover())
                    .border_1()
                    .border_color(colors::accent())
            })
            .hover(|s| s.bg(colors::bg_hover()))
            .on_activate_without_tab_stop(move |event, _, app| {
                let keep_open = event.modifiers().shift;
                selection_picker
                    .update(app, |picker, cx| {
                        picker.activate_emote(Arc::clone(&selection_emote), keep_open, cx);
                    })
                    .ok();
            })
            .on_right_click(move |_, window, _| {
                signals.signal_favorite_click(Arc::clone(&favorite_emote));
                window.refresh();
            })
            .on_hover({
                let hover_repaint = style.hover_repaint;
                move |hovered, window, app| {
                    if animation_mode == EmotePickerAnimationMode::Hover
                        && signals.signal_hovered_emote(emote_idx, is_animated, *hovered)
                    {
                        match hover_repaint {
                            HoverRepaint::NotifyPicker => {
                                hover_picker
                                    .update(app, |_, cx| {
                                        crate::backend::metrics::record_ui_notify_source(
                                            "picker.hover_animation",
                                        );
                                        cx.notify();
                                    })
                                    .ok();
                            }
                            HoverRepaint::RefreshWindow => window.refresh(),
                        }
                    }
                    if *hovered {
                        window.refresh();
                        show_text_tooltip(
                            window.window_handle(),
                            tooltip_source.clone(),
                            tooltip_text.clone(),
                            window.mouse_position(),
                        );
                    } else {
                        clear_global_tooltip_if_source_in_window(
                            window.window_handle(),
                            &tooltip_source,
                        );
                    }
                }
            })
            .flex()
            .items_center()
            .justify_center()
            .child(content)
            .when(is_favorite, |s| {
                s.child(
                    div()
                        .absolute()
                        .top(px(1.0))
                        .right(px(1.0))
                        .text_xs()
                        .text_color(colors::warning())
                        .child("♥"),
                )
            })
            .into_any_element()
    }
}
