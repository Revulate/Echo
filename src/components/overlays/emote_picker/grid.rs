//! Grid, selection, search and prefetch state shared by the inline and popout
//! emote pickers.
//!
//! `PickerGrid` methods take no `Context`: the shells decide when to notify.

use super::core::{
    EmotePickerCore, EmoteTab, PackedGridNavigation, emote_selection_identity_key,
    pack_virtual_rows, picker_page_emote_limit,
};
use super::prefetch::{
    PICKER_ANIMATED_PREFETCH_BATCH_LIMIT, PICKER_INITIAL_PREFETCH_ROWS, PickerPrefetchPlanKind,
    build_prefetch_plan, execute_prefetch,
};
use super::rows::GridStyle;
use super::shared::{PickerButtonExt, PickerFocusOrder, VirtualRow, publish_visible_emote_rows};
use super::signals::PickerSignals;
use crate::backend::Emote;
use crate::backend::settings::EmotePickerAnimationMode;
use crate::cache::loader as image_cache;
use crate::components::overlays::global_tooltip::clear_global_tooltip_if_source_prefix;
use crate::components::overlays::image_ready::{ImageReadyConsumer, take_image_ready};
use crate::components::ui::input::{Input, InputEvent, InputState, apply_standard_input_chrome};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Search debounce delay in milliseconds.
pub(super) const SEARCH_DEBOUNCE_MS: u64 = 150;
/// Approximate horizontal space not available to emote cells (padding + scrollbar).
pub(super) const ROW_WIDTH_OVERHEAD: f32 = 24.0;
/// Minimum width used for row-packing safety.
pub(super) const MIN_ROW_WIDTH: f32 = 120.0;
/// Keep policy/viewport changes responsive even when every visible animation has
/// unusually long frames. This matches the dynamic scheduler's safety ceiling.
pub(super) const PICKER_ANIMATION_WAKE_MAX_MS: u64 = 100;
/// Fast poll interval while picker metadata is actively changing.
const UPDATE_CHECK_INTERVAL_MIN_MS: u64 = 120;
/// Slow poll interval once picker metadata is stable.
const UPDATE_CHECK_INTERVAL_MAX_MS: u64 = 1200;
/// Rows prefetched above and below the visible viewport.
const VIEWPORT_BUFFER_ROWS: usize = 6;
/// A repeated prefetch of the same rows is skipped for this long.
const PREFETCH_REPEAT_THROTTLE: Duration = Duration::from_millis(700);

/// Everything that differs between the picker surfaces apart from each
/// shell's own chrome and lifecycle.
pub(super) struct PickerHost {
    pub(super) signals: &'static PickerSignals,
    /// Prefix of the global-tooltip source keys of this surface's cells.
    pub(super) tooltip_source: &'static str,
    pub(super) metrics: PickerMetricNames,
    /// The initial prefetch also requests the first rows' animations in
    /// Always mode.
    pub(super) initial_prefetch_animations: bool,
    /// Prefetch the first rows after every tab, search, favorite or category
    /// change instead of leaving it to the next viewport publication.
    pub(super) eager_prefetch: bool,
    /// Also log row rebuilds to the `perf` target.
    pub(super) log_rebuild_perf: bool,
    pub(super) log_prefetch: fn(&PrefetchReport),
    pub(super) style: GridStyle,
    /// Category tabs in display order.
    pub(super) tabs: fn() -> &'static [EmoteTab],
    pub(super) tab_bar: TabBarStyle,
    pub(super) search: SearchStyle,
    pub(super) image_ready: ImageReadyConsumer,
    /// Repaint as soon as an image is ready, not only after the settle delay.
    pub(super) repaint_on_image_ready: bool,
}

/// Metric stage names of one picker surface.
pub(super) struct PickerMetricNames {
    pub(super) rebuild_rows: &'static str,
    pub(super) initial_prefetch_plan: &'static str,
    pub(super) viewport_prefetch_plan: &'static str,
    pub(super) search_navigation_flush: &'static str,
}

/// What one prefetch pass requested, for the host's diagnostic log.
pub(super) struct PrefetchReport {
    pub(super) kind: PickerPrefetchPlanKind,
    pub(super) rows: (usize, usize),
    pub(super) exact_animated_rows: (usize, usize),
    pub(super) total_rows: usize,
    pub(super) emotes: usize,
    pub(super) static_requests: usize,
    pub(super) poster_requests: usize,
    pub(super) animated_requests: usize,
    pub(super) elapsed: Duration,
}

impl PrefetchReport {
    pub(super) fn requests(&self) -> usize {
        self.static_requests + self.poster_requests + self.animated_requests
    }
}

pub(super) type PrefetchRangeKey = (PickerPrefetchPlanKind, usize, usize, usize, usize);

/// Grid, selection, search and prefetch state of one picker surface.
pub(super) struct PickerGrid {
    pub(super) host: &'static PickerHost,
    pub(super) core: EmotePickerCore,
    /// Search input state (created on first render).
    pub(super) search_input: Option<Entity<InputState>>,
    /// Focus target for keyboard grid navigation after leaving search editing.
    pub(super) grid_focus: Option<FocusHandle>,
    pub(super) focus_order: PickerFocusOrder,
    pub(super) subscriptions: Vec<Subscription>,
    /// Enter is down; suppresses key repeat until the key is released.
    pub(super) enter_down: bool,
    /// Invalidates delayed search applications after newer input/context changes.
    pub(super) search_revision: u64,
    pub(super) pending_grid_focus: bool,
    /// Virtualized rows for efficient rendering.
    pub(super) virtual_rows: Arc<Vec<VirtualRow>>,
    pub(super) packed_navigation: PackedGridNavigation,
    pub(super) scroll_handle: UniformListScrollHandle,
    pub(super) selected_emote_index: usize,
    pub(super) selected_emote_identity: Option<String>,
    pub(super) last_prefetch_time: Instant,
    /// Scale of the window the picker last rendered in, for prefetches
    /// started outside its render.
    pub(super) window_scale: f32,
    pub(super) last_prefetch_range: Option<PrefetchRangeKey>,
    /// Last exact viewport published to cache residency protection.
    pub(super) last_published_visible_range: Option<(u32, usize, usize, u64)>,
    pub(super) last_update_check: Instant,
    /// Adaptive poll interval for metadata checks.
    pub(super) update_check_interval: Duration,
    /// Last image-ready repaint (throttles repaints while images stream in).
    pub(super) last_refresh: Instant,
    /// Width hint used by row packing.
    pub(super) row_width_hint: f32,
    /// Incremented on every row rebuild to force a `uniform_list` refresh.
    pub(super) render_version: u32,
    /// Saved policy controlling which cells actively advance animation.
    pub(super) animation_mode: EmotePickerAnimationMode,
    /// Number of emotes currently allowed into the rendered grid.
    pub(super) visible_emote_limit: usize,
    /// Number of emotes represented in the rendered grid.
    pub(super) visible_emote_count: usize,
    /// Number of emotes available in the current tab/search result.
    pub(super) total_available_emotes: usize,
    /// Whether the load-more resource warning is open.
    pub(super) show_load_more_warning: bool,
    /// Local pending value for the "do not show again" toggle.
    pub(super) hide_load_more_warning_next_time: bool,
}

impl PickerGrid {
    pub(super) fn new(
        host: &'static PickerHost,
        core: EmotePickerCore,
        row_width_hint: f32,
        animation_mode: EmotePickerAnimationMode,
    ) -> Self {
        Self {
            host,
            core,
            search_input: None,
            grid_focus: None,
            focus_order: PickerFocusOrder::default(),
            subscriptions: Vec::new(),
            enter_down: false,
            search_revision: 0,
            pending_grid_focus: false,
            virtual_rows: Arc::new(Vec::new()),
            packed_navigation: PackedGridNavigation::default(),
            scroll_handle: UniformListScrollHandle::new(),
            selected_emote_index: 0,
            selected_emote_identity: None,
            last_prefetch_time: Instant::now(),
            window_scale: 0.0,
            last_prefetch_range: None,
            last_published_visible_range: None,
            last_update_check: Instant::now(),
            update_check_interval: Duration::from_millis(UPDATE_CHECK_INTERVAL_MIN_MS),
            last_refresh: Instant::now(),
            row_width_hint,
            render_version: 0,
            animation_mode,
            visible_emote_limit: picker_page_emote_limit(),
            visible_emote_count: 0,
            total_available_emotes: 0,
            show_load_more_warning: false,
            hide_load_more_warning_next_time:
                crate::backend::settings::get_hide_emote_picker_load_more_warning(),
        }
    }

    pub(super) fn reset_visible_emote_limit(&mut self) {
        self.visible_emote_limit = picker_page_emote_limit();
        self.show_load_more_warning = false;
        self.hide_load_more_warning_next_time =
            crate::backend::settings::get_hide_emote_picker_load_more_warning();
    }

    pub(super) fn request_load_more(&mut self) {
        if crate::backend::settings::get_hide_emote_picker_load_more_warning() {
            self.confirm_load_more();
            return;
        }
        self.hide_load_more_warning_next_time = false;
        self.show_load_more_warning = true;
    }

    pub(super) fn confirm_load_more(&mut self) {
        if self.hide_load_more_warning_next_time {
            crate::backend::settings::set_hide_emote_picker_load_more_warning(true);
        }
        self.visible_emote_limit = self.total_available_emotes.max(picker_page_emote_limit());
        self.show_load_more_warning = false;
        self.pending_grid_focus = true;
        self.rebuild_virtual_rows();
        self.prefetch_initial();
    }

    pub(super) fn cancel_load_more(&mut self) {
        self.show_load_more_warning = false;
        self.pending_grid_focus = true;
    }

    pub(super) fn toggle_hide_load_more_warning(&mut self) {
        self.hide_load_more_warning_next_time = !self.hide_load_more_warning_next_time;
    }

    fn sync_selected_identity(&mut self) {
        self.selected_emote_identity = self
            .selected_emote()
            .map(|emote| emote_selection_identity_key(&emote));
    }

    fn scroll_selection_into_view(&self) {
        if let Some(row_index) = self
            .packed_navigation
            .virtual_row_for_index(self.selected_emote_index)
        {
            self.scroll_handle
                .scroll_to_item(row_index, ScrollStrategy::Nearest);
        }
    }

    pub(super) fn reset_selection_to_first(&mut self) {
        self.select_index(0);
    }

    /// Select flat index `index` and scroll it into view.
    pub(super) fn select_index(&mut self, index: usize) {
        self.selected_emote_index = index;
        self.sync_selected_identity();
        self.scroll_selection_into_view();
    }

    /// Select flat index `next`; returns whether the selection changed.
    pub(super) fn apply_selection_move(&mut self, next: usize) -> bool {
        if next == self.selected_emote_index {
            return false;
        }
        self.select_index(next);
        true
    }

    pub(super) fn move_selection_vertical(&mut self, direction: isize) -> bool {
        self.packed_navigation
            .vertical_neighbor(self.selected_emote_index, direction)
            .is_some_and(|next| self.apply_selection_move(next))
    }

    pub(super) fn move_selection_horizontal(&mut self, direction: isize) -> bool {
        if self.visible_emote_count == 0 {
            return false;
        }
        let next = self
            .selected_emote_index
            .saturating_add_signed(direction)
            .min(self.visible_emote_count - 1);
        self.apply_selection_move(next)
    }

    /// The selected emote, when the selection is inside the rendered grid.
    pub(super) fn selected_emote(&self) -> Option<Arc<Emote>> {
        self.core
            .flat_emotes
            .get(self.selected_emote_index)
            .filter(|_| self.selected_emote_index < self.visible_emote_count)
            .cloned()
    }

    /// Flat index of the first rendered emote matching `predicate`.
    pub(super) fn visible_position(
        &self,
        predicate: impl Fn(&Arc<Emote>) -> bool,
    ) -> Option<usize> {
        self.core
            .flat_emotes
            .iter()
            .take(self.visible_emote_count)
            .position(predicate)
    }

    /// Rebuild the virtualized row list from the core's grid items, keeping
    /// the selected emote selected when it is still shown.
    pub(super) fn rebuild_virtual_rows(&mut self) {
        clear_global_tooltip_if_source_prefix(self.host.tooltip_source);
        self.host.signals.reset_visible_range();
        self.last_prefetch_range = None;
        self.last_published_visible_range = None;
        let started_at = Instant::now();
        let previous_index = self.selected_emote_index;
        let selected_identity = self.selected_emote_identity.clone();
        let (rows, shown, total_available, packed_navigation) = pack_virtual_rows(
            &self.core.grid_items,
            &self.core.flat_emotes,
            self.visible_emote_limit,
            self.row_width_hint,
            |emote| self.core.is_favorite(emote),
            VirtualRow::Header,
            |emotes, base_index| VirtualRow::EmoteRow { emotes, base_index },
            |shown, total| VirtualRow::LoadMore { shown, total },
        );
        let elapsed = started_at.elapsed();
        crate::backend::metrics::record_emote_stage(
            self.host.metrics.rebuild_rows,
            elapsed,
            rows.len(),
        );
        if self.host.log_rebuild_perf && crate::log::enabled("perf") {
            crate::log_perf!(
                debug,
                stage = self.host.metrics.rebuild_rows,
                elapsed_ms = elapsed.as_millis(),
                rows = rows.len(),
                row_width_hint = self.row_width_hint,
                "Popout virtual rows rebuilt"
            );
        }
        self.visible_emote_count = shown;
        self.total_available_emotes = total_available;
        if self.visible_emote_count == 0 {
            self.selected_emote_index = 0;
            self.selected_emote_identity = None;
        } else {
            self.selected_emote_index = selected_identity
                .as_deref()
                .and_then(|identity| {
                    self.visible_position(|emote| emote_selection_identity_key(emote) == identity)
                })
                .unwrap_or_else(|| previous_index.min(self.visible_emote_count - 1));
        }
        self.virtual_rows = Arc::new(rows);
        self.packed_navigation = packed_navigation;
        self.sync_selected_identity();
        self.scroll_selection_into_view();
        self.render_version = self.render_version.wrapping_add(1);
    }

    /// Publish the exact viewport `start_row..end_row` to cache residency
    /// protection, unless it was already published unchanged.
    fn publish_visible_emotes(&mut self, start_row: usize, end_row: usize) {
        let start_row = start_row.min(self.virtual_rows.len());
        let end_row = end_row.min(self.virtual_rows.len());
        let animation_state_revision = image_cache::animated_cache_state_revision();
        let key = (
            self.render_version,
            start_row,
            end_row,
            animation_state_revision,
        );
        if self.last_published_visible_range == Some(key) {
            return;
        }

        let published = publish_visible_emote_rows(
            self.virtual_rows.as_slice(),
            start_row,
            end_row,
            self.animation_mode,
            self.host.signals.viewport(),
        );
        self.last_published_visible_range = published.then_some(key);
    }

    /// Adopt the saved animation policy; returns whether it changed.
    pub(super) fn set_animation_mode(&mut self, animation_mode: EmotePickerAnimationMode) -> bool {
        if animation_mode == self.animation_mode {
            return false;
        }
        self.animation_mode = animation_mode;
        self.host.signals.clear_hovered_emote();
        self.last_prefetch_range = None;
        self.last_published_visible_range = None;
        true
    }

    /// Repack rows for a new available row width (ignores changes under 8 px).
    pub(super) fn set_row_width_hint(&mut self, row_width: f32) {
        if (row_width - self.row_width_hint).abs() >= 8.0 {
            self.row_width_hint = row_width;
            self.rebuild_virtual_rows();
            self.prefetch_initial();
        }
    }

    /// Pick up published emote changes; returns whether the catalog changed.
    pub(super) fn refresh_emote_catalog(&mut self) -> bool {
        if !self.core.check_for_emote_updates() {
            return false;
        }
        self.rebuild_virtual_rows();
        self.prefetch_initial();
        true
    }

    /// Poll for published emote changes when the adaptive interval is due:
    /// fast while metadata changes, backing off while it is stable. Returns
    /// whether the catalog changed.
    pub(super) fn poll_emote_catalog(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_update_check) < self.update_check_interval {
            return false;
        }
        self.last_update_check = now;
        let updated = self.refresh_emote_catalog();
        self.update_check_interval = if updated {
            Duration::from_millis(UPDATE_CHECK_INTERVAL_MIN_MS)
        } else {
            Duration::from_millis(
                (self.update_check_interval.as_millis() as u64 * 2)
                    .min(UPDATE_CHECK_INTERVAL_MAX_MS),
            )
        };
        updated
    }

    fn prefetch_after_view_change(&mut self) {
        if self.host.eager_prefetch {
            self.prefetch_initial();
        }
    }

    pub(super) fn select_tab(&mut self, tab: EmoteTab) {
        self.core.select_tab(tab);
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        self.prefetch_after_view_change();
    }

    pub(super) fn toggle_favorite(&mut self, emote: &Emote) {
        self.core.toggle_favorite(emote);
        self.rebuild_virtual_rows();
        self.prefetch_after_view_change();
    }

    /// Collapse or expand a category and keep its header in view.
    pub(super) fn toggle_category(&mut self, category_id: &str) {
        self.core.toggle_category(category_id);
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        if let Some(row) = self
            .virtual_rows
            .iter()
            .position(|row| matches!(row, VirtualRow::Header(header) if header.id == category_id))
        {
            self.scroll_handle
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.prefetch_after_view_change();
    }

    /// Bump the search revision so a pending debounced search is dropped.
    pub(super) fn invalidate_search(&mut self) -> u64 {
        self.search_revision = self.search_revision.wrapping_add(1);
        self.search_revision
    }

    /// Apply `query` and restart the grid at its first page and first emote.
    pub(super) fn set_search_query(&mut self, query: String) {
        self.core.set_search(query);
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        self.prefetch_after_view_change();
    }

    /// Apply the field's text now instead of after the debounce, so keyboard
    /// navigation acts on the rows the user sees; returns whether it changed.
    pub(super) fn flush_search_for_navigation(&mut self, cx: &App) -> bool {
        let query = self
            .search_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| self.core.search_query.clone());
        self.invalidate_search();
        if self.core.search_query == query {
            return false;
        }
        self.set_search_query(query);
        crate::backend::metrics::record_ui_notify_source(self.host.metrics.search_navigation_flush);
        true
    }

    /// Whether the field or the applied query holds search text.
    pub(super) fn has_search(&self, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|input| !input.read(cx).value().is_empty())
            || !self.core.search_query.is_empty()
    }

    fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window))
    }

    /// The focused search field holds an uncommitted IME composition, which
    /// owns Escape.
    pub(super) fn search_ime_composing(&self, window: &Window, cx: &App) -> bool {
        self.search_input.as_ref().is_some_and(|input| {
            let input = input.read(cx);
            input.focus_handle().is_focused(window) && input.marked_range().is_some()
        })
    }

    /// Publish the signalled viewport and prefetch it plus a buffer of rows.
    pub(super) fn publish_and_prefetch_viewport(&mut self) {
        let (start, end) = self.host.signals.visible_range();
        self.publish_visible_emotes(start, end);
        if end > start {
            let buffered_start = start.saturating_sub(VIEWPORT_BUFFER_ROWS);
            let buffered_end = (end + VIEWPORT_BUFFER_ROWS).min(self.virtual_rows.len());
            self.prefetch_rows(
                buffered_start,
                buffered_end,
                Some((start, end)),
                PickerPrefetchPlanKind::Viewport,
            );
        }
    }

    /// Prefetch the first rows of the grid.
    pub(super) fn prefetch_initial(&mut self) {
        self.prefetch_rows(
            0,
            PICKER_INITIAL_PREFETCH_ROWS,
            self.host
                .initial_prefetch_animations
                .then_some((0, PICKER_INITIAL_PREFETCH_ROWS)),
            PickerPrefetchPlanKind::Initial,
        );
    }

    /// Prefetch the stills of rows `start_row..end_row`, and in Always mode
    /// the animations of the emotes in `exact_animated_rows`: the rows actually
    /// on screen, so buffer rows never start animation decodes.
    pub(super) fn prefetch_rows(
        &mut self,
        start_row: usize,
        end_row: usize,
        exact_animated_rows: Option<(usize, usize)>,
        kind: PickerPrefetchPlanKind,
    ) {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(self.window_scale);
        let started_at = Instant::now();

        let row_count = self.virtual_rows.len();
        let start_row = start_row.min(row_count);
        let end_row = end_row.min(row_count);
        let (exact_animated_start, exact_animated_end) = exact_animated_rows
            .map(|(start, end)| (start.min(row_count), end.min(row_count)))
            .filter(|(start, end)| end > start)
            .unwrap_or((0, 0));
        let range_key = (
            kind,
            start_row,
            end_row,
            exact_animated_start,
            exact_animated_end,
        );
        let now = Instant::now();
        if Some(range_key) == self.last_prefetch_range
            && now.duration_since(self.last_prefetch_time) < PREFETCH_REPEAT_THROTTLE
        {
            return;
        }
        self.last_prefetch_time = now;
        self.last_prefetch_range = Some(range_key);

        let animate_exact = self.animation_mode == EmotePickerAnimationMode::Always;
        let mut buffered_emotes: Vec<Arc<Emote>> = Vec::with_capacity(220);
        let mut exact_animated_emotes: Vec<Arc<Emote>> = Vec::with_capacity(96);
        for row_idx in start_row..end_row {
            if let Some(VirtualRow::EmoteRow { emotes, .. }) = self.virtual_rows.get(row_idx) {
                for (emote, _) in emotes {
                    buffered_emotes.push(Arc::clone(emote));
                    if animate_exact
                        && emote.is_animated
                        && (exact_animated_start..exact_animated_end).contains(&row_idx)
                    {
                        exact_animated_emotes.push(Arc::clone(emote));
                    }
                }
            }
        }

        // Buffer rows only fetch stills: in Always mode the exact rows alone
        // request animations, and the other modes never animate an unhovered
        // cell.
        let buffered_animation_mode = if animate_exact {
            EmotePickerAnimationMode::Never
        } else {
            self.animation_mode
        };
        let mut plan = build_prefetch_plan(buffered_emotes.iter(), buffered_animation_mode);
        if animate_exact {
            let mut exact_plan = build_prefetch_plan(
                exact_animated_emotes.iter(),
                EmotePickerAnimationMode::Always,
            );
            plan.animated_requests
                .append(&mut exact_plan.animated_requests);
        }
        let plan_metric = match kind {
            PickerPrefetchPlanKind::Initial => self.host.metrics.initial_prefetch_plan,
            PickerPrefetchPlanKind::Viewport => self.host.metrics.viewport_prefetch_plan,
        };
        let report = PrefetchReport {
            kind,
            rows: (start_row, end_row),
            exact_animated_rows: (exact_animated_start, exact_animated_end),
            total_rows: row_count,
            emotes: buffered_emotes.len(),
            static_requests: plan.static_requests.len(),
            poster_requests: plan.poster_requests.len(),
            animated_requests: plan.animated_requests.len(),
            elapsed: started_at.elapsed(),
        };
        crate::backend::metrics::record_emote_stage(plan_metric, report.elapsed, report.requests());
        (self.host.log_prefetch)(&report);
        if !plan.is_empty() {
            execute_prefetch(plan, PICKER_ANIMATED_PREFETCH_BATCH_LIMIT);
        }
    }
}

/// A picker entity that embeds a [`PickerGrid`].
pub(super) trait PickerShell: 'static + Sized {
    fn grid(&self) -> &PickerGrid;
    fn grid_mut(&mut self) -> &mut PickerGrid;
    /// Insert or send `emote`. `keep_open` (Shift) is honoured only by the
    /// inline picker, which otherwise closes.
    fn activate_emote(&mut self, emote: Arc<Emote>, keep_open: bool, cx: &mut Context<Self>);
    /// The channel named in each cell's accessible label.
    fn accessible_channel(&self) -> &str;
    /// Empty the search field and the applied query, keeping focus in the field.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>);
    /// Escape with nothing left to cancel or clear.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>);
    /// The search field's text changed to `text`.
    fn search_changed(&mut self, text: String, cx: &mut Context<Self>);
    /// Whether Enter in the search field keeps the picker open.
    fn search_enter_keeps_open(&self) -> bool;
}

/// Create the search field on first render and focus it.
pub(super) fn ensure_search_input<S: PickerShell>(
    this: &mut S,
    window: &mut Window,
    cx: &mut Context<S>,
) {
    if this.grid().search_input.is_some() {
        return;
    }
    let search_input = cx.new(|cx| InputState::new(window, cx).with_placeholder("Search emotes…"));
    let subscription = cx.subscribe(
        &search_input,
        |this: &mut S, _entity, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                if let Some(input) = &this.grid().search_input {
                    let text = input.read(cx).value().to_string();
                    this.search_changed(text, cx);
                }
            }
            InputEvent::PressEnter => {
                flush_search_for_navigation(this, cx);
                let keep_open = this.search_enter_keeps_open();
                confirm_selection(this, keep_open, cx);
            }
            InputEvent::Blur => this.grid_mut().enter_down = false,
            InputEvent::Focus => {}
        },
    );
    let focus_handle = search_input.read(cx).focus_handle().clone();
    let grid = this.grid_mut();
    grid.search_input = Some(search_input);
    grid.subscriptions.push(subscription);
    window.focus(&focus_handle, cx);
}

pub(super) fn request_load_more<S: PickerShell>(this: &mut S, cx: &mut Context<S>) {
    this.grid_mut().request_load_more();
    cx.notify();
}

pub(super) fn confirm_load_more<S: PickerShell>(this: &mut S, cx: &mut Context<S>) {
    this.grid_mut().confirm_load_more();
    cx.notify();
}

pub(super) fn cancel_load_more<S: PickerShell>(this: &mut S, cx: &mut Context<S>) {
    this.grid_mut().cancel_load_more();
    cx.notify();
}

pub(super) fn toggle_hide_load_more_warning<S: PickerShell>(this: &mut S, cx: &mut Context<S>) {
    this.grid_mut().toggle_hide_load_more_warning();
    cx.notify();
}

pub(super) fn select_tab<S: PickerShell>(this: &mut S, tab: EmoteTab, cx: &mut Context<S>) {
    this.grid_mut().select_tab(tab);
    cx.notify();
}

pub(super) fn toggle_favorite<S: PickerShell>(this: &mut S, emote: &Emote, cx: &mut Context<S>) {
    this.grid_mut().toggle_favorite(emote);
    cx.notify();
}

pub(super) fn toggle_category<S: PickerShell>(
    this: &mut S,
    category_id: &str,
    cx: &mut Context<S>,
) {
    this.grid_mut().toggle_category(category_id);
    cx.notify();
}

/// Apply the field's pending text before keyboard navigation.
pub(super) fn flush_search_for_navigation<S: PickerShell>(this: &mut S, cx: &mut Context<S>) {
    if this.grid_mut().flush_search_for_navigation(cx) {
        cx.notify();
    }
}

pub(super) fn apply_selection_move<S: PickerShell>(this: &mut S, next: usize, cx: &mut Context<S>) {
    if this.grid_mut().apply_selection_move(next) {
        cx.notify();
    }
}

pub(super) fn move_selection_vertical<S: PickerShell>(
    this: &mut S,
    direction: isize,
    cx: &mut Context<S>,
) {
    if this.grid_mut().move_selection_vertical(direction) {
        cx.notify();
    }
}

pub(super) fn move_selection_horizontal<S: PickerShell>(
    this: &mut S,
    direction: isize,
    cx: &mut Context<S>,
) {
    if this.grid_mut().move_selection_horizontal(direction) {
        cx.notify();
    }
}

/// Apply the favorite, header and load-more clicks rows signalled since the
/// last render. Callers gate this on `PickerSignals::take_pending`.
pub(super) fn apply_pending_row_actions<S: PickerShell>(this: &mut S, cx: &mut Context<S>) {
    let signals = this.grid().host.signals;
    if let Some(emote) = signals.take_favorite_click() {
        toggle_favorite(this, &emote, cx);
    }
    if let Some(header_id) = signals.take_header_click() {
        toggle_category(this, &header_id, cx);
    }
    if signals.take_load_more_click() {
        request_load_more(this, cx);
    }
}

/// Repaint for images that became ready, at most every 100 ms, and once more
/// after a 100 ms settle so a burst of loads lands in one frame.
pub(super) fn repaint_for_ready_images<S: PickerShell>(
    this: &mut S,
    now: Instant,
    cx: &mut Context<S>,
) {
    let grid = this.grid_mut();
    if now.duration_since(grid.last_refresh) < Duration::from_millis(100)
        || !take_image_ready(grid.host.image_ready)
    {
        return;
    }
    grid.last_refresh = now;
    if grid.host.repaint_on_image_ready {
        cx.notify();
    }
    cx.spawn(async move |this, cx| {
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
        this.update(cx, |_, cx| cx.notify()).ok();
    })
    .detach();
}

/// Activate the selected emote, if the selection is inside the rendered grid.
pub(super) fn confirm_selection<S: PickerShell>(
    this: &mut S,
    keep_open: bool,
    cx: &mut Context<S>,
) {
    if let Some(emote) = this.grid().selected_emote() {
        this.activate_emote(emote, keep_open, cx);
    }
}

/// Escape: cancel the load-more warning, else clear the search, else dismiss
/// the picker.
pub(super) fn escape<S: PickerShell>(this: &mut S, window: &mut Window, cx: &mut Context<S>) {
    let grid = this.grid();
    if grid.show_load_more_warning {
        cancel_load_more(this, cx);
    } else if grid.has_search(cx) {
        this.clear_search(window, cx);
    } else {
        this.dismiss(window, cx);
    }
}

/// The `Escape` action, unless the search field's IME composition owns it.
pub(super) fn capture_escape<S: PickerShell>(
    this: &mut S,
    window: &mut Window,
    cx: &mut Context<S>,
) {
    if this.grid().search_ime_composing(window, cx) {
        return;
    }
    escape(this, window, cx);
    cx.stop_propagation();
}

/// Keyboard navigation for the whole picker, captured at its root.
pub(super) fn handle_grid_key<S: PickerShell>(
    this: &mut S,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<S>,
) {
    let key = event.keystroke.key.as_str();
    let modifiers = &event.keystroke.modifiers;
    let control = modifiers.control;
    if key == "enter" {
        this.grid_mut().enter_down = true;
    }
    let grid = this.grid();
    let search_focused = grid.search_focused(window, cx);
    let grid_focused = grid
        .grid_focus
        .as_ref()
        .is_some_and(|focus| focus.is_focused(window));
    // The load-more warning is modal: only its focus cycle and Escape pass.
    if grid.show_load_more_warning && !matches!(key, "tab" | "escape") {
        return;
    }
    let tabs = (grid.host.tabs)();
    let handled = match key {
        "up" | "down" if grid_focused || search_focused => {
            flush_search_for_navigation(this, cx);
            move_selection_vertical(this, if key == "up" { -1 } else { 1 }, cx);
            true
        }
        "left" | "right" if grid_focused => {
            move_selection_horizontal(this, if key == "left" { -1 } else { 1 }, cx);
            true
        }
        "home" | "end" if grid_focused => {
            let grid = this.grid();
            let next = if control {
                grid.visible_emote_count
                    .checked_sub(1)
                    .map(|last| if key == "end" { last } else { 0 })
            } else {
                grid.packed_navigation
                    .row_edge(grid.selected_emote_index, key == "end")
            };
            if let Some(next) = next {
                apply_selection_move(this, next, cx);
            }
            true
        }
        "pageup" | "pagedown" if grid_focused => {
            let grid = this.grid();
            let (start, end) = grid.host.signals.visible_range();
            if let Some(next) = grid.packed_navigation.page_neighbor(
                grid.selected_emote_index,
                if key == "pageup" { -1 } else { 1 },
                end.saturating_sub(start).max(1),
            ) {
                apply_selection_move(this, next, cx);
            }
            true
        }
        "tab" if control => {
            let current = tabs
                .iter()
                .position(|&tab| tab == this.grid().core.active_tab)
                .unwrap_or(0);
            let next = if modifiers.shift {
                current.checked_sub(1).unwrap_or(tabs.len() - 1)
            } else {
                (current + 1) % tabs.len()
            };
            select_tab(this, tabs[next], cx);
            true
        }
        "tab" => {
            flush_search_for_navigation(this, cx);
            this.grid().focus_order.cycle(modifiers.shift, window, cx);
            true
        }
        "enter" | "space" if grid_focused => {
            if !event.is_held {
                flush_search_for_navigation(this, cx);
                confirm_selection(this, modifiers.shift, cx);
            }
            true
        }
        "escape" if !this.grid().search_ime_composing(window, cx) => {
            escape(this, window, cx);
            true
        }
        "f" if control && (grid_focused || search_focused) => {
            flush_search_for_navigation(this, cx);
            if let Some(emote) = this.grid().selected_emote() {
                toggle_favorite(this, &emote, cx);
            }
            true
        }
        key if control => match key
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| tabs.get(index).copied())
        {
            Some(tab) => {
                select_tab(this, tab, cx);
                true
            }
            None => false,
        },
        _ if grid_focused && crate::event_handlers::is_typed_text(event) => {
            if let Some(text) = event
                .keystroke
                .key_char
                .as_deref()
                .filter(|text| !text.chars().any(char::is_control))
                && let Some(input) = this.grid().search_input.clone()
            {
                let focus = input.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                input.update(cx, |input, cx| input.insert(text, window, cx));
                true
            } else {
                false
            }
        }
        _ => false,
    };
    if handled {
        cx.stop_propagation();
    }
}

/// Tab-bar presentation that differs between the picker surfaces.
pub(super) struct TabBarStyle {
    pub(super) bar_id: &'static str,
    pub(super) tab_id: &'static str,
    pub(super) height: f32,
    /// Narrower horizontal tab padding.
    pub(super) compact: bool,
}

/// The category tab bar.
pub(super) fn render_tab_bar<S: PickerShell>(this: &mut S, cx: &mut Context<S>) -> Stateful<Div> {
    let grid = this.grid_mut();
    let host = grid.host;
    let style = &host.tab_bar;
    let active_tab = grid.core.active_tab;
    let tabs = (host.tabs)();
    let tab_focuses: Vec<_> = tabs
        .iter()
        .map(|tab| {
            grid.focus_order
                .register(format!("tab-{}", *tab as usize), cx)
        })
        .collect();

    div()
        .id(style.bar_id)
        .role(Role::TabList)
        .aria_label("Emote categories")
        .aria_orientation(Orientation::Horizontal)
        .w_full()
        .h(px(style.height))
        .bg(colors::bg_surface())
        .border_b_1()
        .border_color(colors::border())
        .overflow_x_scroll()
        .overflow_y_hidden()
        .child(
            div()
                .h_full()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .children(tabs.iter().zip(tab_focuses.iter()).map(|(&tab, focus)| {
                    let is_selected = tab == active_tab;

                    div()
                        .id((style.tab_id, tab as usize))
                        .role(Role::Tab)
                        .aria_label(tab.label())
                        .aria_selected(is_selected)
                        .track_focus(focus)
                        .map(|s| if style.compact { s.px_2() } else { s.px_3() })
                        .py_1()
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .flex_shrink_0()
                        .when(is_selected, |s| {
                            s.bg(colors::bg_hover())
                                .border_b_2()
                                .border_color(colors::accent())
                        })
                        .hover(|s| s.bg(colors::bg_hover()))
                        .on_picker_activate(cx.listener(move |this: &mut S, _, _window, cx| {
                            select_tab(this, tab, cx);
                        }))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(if is_selected {
                                    colors::accent_text()
                                } else {
                                    colors::text_muted()
                                })
                                .child(tab.icon()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .whitespace_nowrap()
                                .text_color(if is_selected {
                                    colors::text_primary()
                                } else {
                                    colors::text_secondary()
                                })
                                .when(is_selected, |s| s.font_weight(FontWeight::SEMIBOLD))
                                .child(tab.label()),
                        )
                })),
        )
}

/// Search-row element ids of one picker surface.
pub(super) struct SearchStyle {
    pub(super) shell_id: &'static str,
    pub(super) fallback_id: &'static str,
    pub(super) clear_id: &'static str,
    /// Focus-order key of the clear button.
    pub(super) clear_focus_key: &'static str,
}

/// The search field, or a static placeholder before the field exists.
pub(super) fn render_search_field<S: PickerShell>(
    this: &S,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let grid = this.grid();
    let style = &grid.host.search;
    let Some(search_input) = &grid.search_input else {
        return apply_standard_input_chrome(
            div()
                .id(style.fallback_id)
                .flex_1()
                .h(px(40.0))
                .flex()
                .items_center()
                .gap_2()
                .px_1(),
            false,
        )
        .child(div().text_sm().text_color(colors::text_muted()).child("⌕"))
        .child(
            div()
                .text_sm()
                .text_color(colors::text_muted())
                .child("Search emotes…"),
        )
        .into_any_element();
    };
    let is_focused = search_input.read(cx).focus_handle().is_focused(window);
    let focus_handle = search_input.read(cx).focus_handle().clone();
    apply_standard_input_chrome(
        div()
            .id(style.shell_id)
            .flex_1()
            .h(px(40.0))
            .flex()
            .items_center()
            .gap_2()
            .cursor_text()
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                window.focus(&focus_handle, cx);
            }),
        is_focused,
    )
    .child(div().text_sm().text_color(colors::text_muted()).child("⌕"))
    .child(
        div()
            .flex_1()
            .flex()
            .items_center()
            .text_sm()
            .text_color(colors::text_primary())
            .child(
                Input::new(search_input)
                    .accessibility_label("Search emotes")
                    .accessibility_search(true)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false),
            ),
    )
    .into_any_element()
}

/// The clear-search button, while there is search text to clear.
pub(super) fn render_clear_search_button<S: PickerShell>(
    this: &mut S,
    cx: &mut Context<S>,
) -> Option<Stateful<Div>> {
    if !this.grid().has_search(cx) {
        return None;
    }
    let grid = this.grid_mut();
    let style = &grid.host.search;
    let focus = grid.focus_order.register(style.clear_focus_key, cx);
    Some(
        div()
            .id(style.clear_id)
            .track_focus(&focus)
            .role(Role::Button)
            .aria_label("Clear search")
            .on_hover(crate::components::overlays::text_tooltip_on_hover(
                style.clear_id.to_string(),
                "Clear search".to_string(),
            ))
            .w(px(28.0))
            .h(px(28.0))
            .rounded(Radii::sm())
            .cursor_pointer()
            .hover(|s| s.bg(colors::bg_hover()))
            .flex()
            .items_center()
            .justify_center()
            .on_picker_activate(cx.listener(|this: &mut S, _, window, cx| {
                this.clear_search(window, cx);
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child(crate::icons::CLOSE_GLYPH),
            ),
    )
}
