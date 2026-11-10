//! Tab Bar Renderer Entity - Extracted from app.rs monolith
//!
//! This entity handles all tab bar rendering logic with support for multiple overflow modes:
//! - Scroll: Single row with scroll arrows
//! - Dropdown: Single row with overflow menu
//! - MultiLine: Tabs wrap to multiple rows
//!
//! Extracted from app.rs to reduce its size from 5,752 lines.

use crate::backend;
use crate::components::chat::chat_ui::format_viewer_count;
use crate::components::overlays::{
    clear_global_tooltip, clear_global_tooltip_if_source_in_window, show_text_tooltip,
    text_tooltip_on_hover,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Input, InputEvent, InputState, SelectAll};
use crate::components::ui::menu::{
    MenuAction, MenuHost, MenuKeyboard, menu_divider, menu_header, menu_item, menu_panel,
};
use crate::config;
use crate::icons::{CLOSE_GLYPH, MenuMetrics, modal_backdrop_style};
use crate::managers::tab_manager::{
    ScrollDirection, TAB_CONTEXT_MENU_ITEMS, TabDropTarget, TabDropZone, TabManager,
};
use crate::state::{AppState, SplitNode, TabState};
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

/// Minimal tabs are 28 px tall and clamp their content width to 150 px. These
/// dimensions apply only to the minimal appearance; the browser-style themes
/// keep their own sizing.
const MINIMAL_TAB_HEIGHT_PX: f32 = 28.0;
const MINIMAL_TAB_MIN_WIDTH_PX: f32 = 28.0;
const MINIMAL_TAB_MAX_WIDTH_PX: f32 = 150.0;
const MINIMAL_TAB_CHROME_WIDTH_PX: f32 = 32.0;
const MINIMAL_TAB_SPACER_PX: f32 = 1.0;
const TAB_TITLE_FONT_SIZE_PX: f32 = FontSize::BASE;
const TAB_LOCK_GLYPH_WIDTH_PX: f32 = 8.0;
const TAB_CONTEXT_MENU_MAX_HEIGHT_PX: f32 = 400.0;
/// Browser-style tabs always render at their measured title/chrome width and
/// never shrink; tabs that do not fit move to the overflow menu instead. This
/// floor only sizes an empty title. Active state never changes width.
const BROWSER_TAB_MIN_WIDTH_PX: f32 = 40.0;
const BROWSER_ADD_TAB_CONTROL_WIDTH_PX: f32 = 36.0;
/// Compact all-tabs affordance shared by visibility math and rendering. Keeping
/// this narrower than one compact tab prevents the first overflow transition
/// from displacing two tabs at once.
const DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX: f32 = 28.0;
const SCROLL_ARROW_WIDTH_PX: f32 = 24.0;

fn browser_tab_chrome_width(theme: &backend::TabTheme, live: bool, unread: bool) -> f32 {
    let shell = match theme {
        backend::TabTheme::Chrome => 20.0,
        backend::TabTheme::Firefox | backend::TabTheme::Edge | backend::TabTheme::Square => 26.0,
        backend::TabTheme::Arc => 30.0,
        backend::TabTheme::Minimal => unreachable!("minimal tabs use their own chrome"),
    };
    shell + 22.0 + if live { 12.0 } else { 0.0 } + if unread { 12.0 } else { 0.0 }
}

/// The inline-titlebar reservation needed to keep the complete current tab set
/// and its always-rendered controls visible. Every layout publishes its
/// measured full-width footprint, independent from the most recently laid out
/// tab-slot width, which has already been reduced by title branding.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineTabDemand {
    known: bool,
    full_tab_set_reservation_px: f32,
    controls_and_spacers_reservation_px: f32,
    context: u64,
}

#[derive(Clone, Debug, Default)]
struct DropdownTabLayout {
    /// Positions (into the laid-out tab slice) rendered inline, in render
    /// order. Every other position belongs to the overflow menu.
    visible_positions: Vec<usize>,
    /// Explicit full natural widths for browser-style shells, aligned with
    /// `visible_positions`. Minimal tabs keep their measured intrinsic widths.
    browser_tab_widths_px: Option<Vec<f32>>,
}

/// Theme- and overflow-mode-specific dimensions shared by tab layout paths.
/// Keeping these named avoids coupling callers to the helper's return order.
#[derive(Clone, Copy)]
struct TabLayoutMetrics {
    character_width: f32,
    chrome_width: f32,
    minimum_width: f32,
    add_button_width: f32,
    tab_spacer: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct ScrollTabGeometry {
    active_start: f32,
    active_end: f32,
    bar_width: f32,
    max_scroll: f32,
}

impl ScrollTabGeometry {
    fn viewport_width(self, offset: f32) -> f32 {
        (self.bar_width
            - if offset > 0.5 {
                SCROLL_ARROW_WIDTH_PX
            } else {
                0.0
            }
            - if offset < self.max_scroll - 0.5 {
                SCROLL_ARROW_WIDTH_PX
            } else {
                0.0
            })
        .max(0.0)
    }

    fn active_is_visible(self, offset: f32) -> bool {
        let viewport_width = self.viewport_width(offset);
        self.active_start >= offset - 0.5
            && self.active_start + (self.active_end - self.active_start).min(viewport_width)
                <= offset + viewport_width + 0.5
    }
}

/// Choose the browser-style tabs rendered inline in Dropdown mode. Tabs keep
/// their full natural widths and never shrink: a source-order prefix stays
/// visible while it fits beside the add and overflow controls, and the rest
/// move to the overflow menu. The active tab is always visible, in source
/// order. A single tab wider than the whole lane is clipped at the lane edge
/// rather than ellipsized.
fn solve_natural_dropdown_tabs(
    natural_widths: &[f32],
    active_position: Option<usize>,
    bar_width: f32,
    tab_spacer: f32,
    add_button_width: f32,
    overflow_button_width: f32,
) -> Vec<usize> {
    let tab_count = natural_widths.len();
    let all_positions = || (0..tab_count).collect::<Vec<_>>();
    if bar_width <= 1.0 {
        return all_positions();
    }

    let all_tabs_width =
        natural_widths.iter().sum::<f32>() + tab_spacer * tab_count.saturating_sub(1) as f32;
    if all_tabs_width + add_button_width <= bar_width + 0.5 {
        return all_positions();
    }

    let available_width = (bar_width - add_button_width - overflow_button_width).max(0.0);
    let active_position = active_position.filter(|&position| position < tab_count);
    let mut used_width = active_position.map_or(0.0, |position| natural_widths[position]);
    let mut visible_count = usize::from(active_position.is_some());
    let mut visible = Vec::with_capacity(tab_count);
    for (position, &width) in natural_widths.iter().enumerate() {
        if Some(position) == active_position {
            continue;
        }
        let spacer = if visible_count == 0 { 0.0 } else { tab_spacer };
        if used_width + spacer + width > available_width + 0.5 {
            break;
        }
        used_width += spacer + width;
        visible_count += 1;
        visible.push(position);
    }
    match active_position {
        Some(active) => {
            visible.push(active);
            visible.sort_unstable();
        }
        None if visible.is_empty() && tab_count > 0 => visible.push(0),
        None => {}
    }
    visible
}

impl InlineTabDemand {
    pub fn is_known(self) -> bool {
        self.known
    }

    /// Minimum tab-slot width that keeps every current tab and its
    /// always-rendered controls visible without overflow.
    pub fn required_width_px(self) -> f32 {
        self.full_tab_set_reservation_px + self.controls_and_spacers_reservation_px
    }

    /// Identifies the visible tab/mode inputs that produced this reservation.
    /// The titlebar resets hysteresis on a context change instead of carrying a
    /// previous tab set's suppression into the new layout.
    pub fn context(self) -> u64 {
        self.context
    }
}

/// Calculate the context-menu height from the same fixed row metrics used by
/// its static and dynamic sections. Dynamic split rows must participate before
/// placement is clamped, otherwise a short native window can hide the last
/// target below an incorrectly placed panel.
fn tab_context_menu_preferred_height(split_target_count: usize) -> f32 {
    let static_height: f32 = TAB_CONTEXT_MENU_ITEMS
        .iter()
        .map(|item| {
            if item.is_divider {
                MenuMetrics::DIVIDER_HEIGHT
            } else {
                MenuMetrics::ITEM_HEIGHT
            }
        })
        .sum();
    let split_section_height = if split_target_count > 1 {
        MenuMetrics::DIVIDER_HEIGHT
            + MenuMetrics::HEADER_HEIGHT
            + split_target_count as f32 * MenuMetrics::ITEM_HEIGHT
    } else {
        0.0
    };

    MenuMetrics::PANEL_PADDING + static_height + split_section_height
}

/// Live status aggregated across every channel a tab hosts.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
struct TabLiveState {
    live_channel_count: usize,
    viewer_count: u32,
    hide_viewer_stats: bool,
}

/// Retained per-tab data derived from the tab's layout, title and live status.
///
/// The first four fields form the rebuild key. The layout `Arc` is held rather
/// than reduced to a raw address so pointer identity can never alias a tree that
/// was freed and reallocated at the same location.
struct TabRenderModel {
    layout: Arc<SplitNode>,
    custom_title: Option<String>,
    watching_channel: Option<String>,
    measure_font: Font,
    title: SharedString,
    shaped_title_width: f32,
    position_in_set: usize,
    size_of_set: usize,
    /// Minimal intrinsic width: shaped title plus chrome, already clamped.
    minimal_width: f32,
    channels: Vec<String>,
    channel_tooltip: Option<SharedString>,
    channel_tooltip_source: SharedString,
    live_tooltip_source: SharedString,
    live: TabLiveState,
    live_tooltip: SharedString,
}

/// Emitted after a tab-bar interaction may have changed the active split, so
/// the app can hand keyboard focus to that split's input.
pub struct TabBarActiveSplitChanged;

impl EventEmitter<TabBarActiveSplitChanged> for TabBarRenderer {}
impl EventEmitter<crate::app::TabCloseRequest> for TabBarRenderer {}

/// Entity for rendering the tab bar with all overflow modes
pub struct TabBarRenderer {
    /// Tab manager for tab bar state (drag-drop, context menu, scroll, etc.)
    pub tab_manager: TabManager,
    app_state: Entity<AppState>,
    rename_input: Option<Entity<InputState>>,
    rename_input_subscriptions: Vec<Subscription>,
    rename_input_tab_id: Option<String>,
    last_overflow_mode: Option<backend::TabOverflowMode>,
    scroll_active_tab_id: Option<String>,
    scroll_geometry: Option<ScrollTabGeometry>,
    multiline_row_count: usize,
    is_visible: bool,
    inline_tab_demand: InlineTabDemand,
    /// Retained render models, keyed by tab id.
    tab_models: FxHashMap<String, TabRenderModel>,
    /// Hash of every input the next render would read, taken by `prepare`.
    frame_fingerprint: u64,
    /// Fingerprint the retained (cached) subtree was last rendered from.
    rendered_fingerprint: Option<u64>,
}

impl TabBarRenderer {
    pub fn new(app_state: Entity<AppState>, _cx: &mut Context<Self>) -> Self {
        Self {
            tab_manager: TabManager::new(),
            app_state,
            rename_input: None,
            rename_input_subscriptions: Vec::new(),
            rename_input_tab_id: None,
            last_overflow_mode: None,
            scroll_active_tab_id: None,
            scroll_geometry: None,
            multiline_row_count: 1,
            is_visible: true,
            inline_tab_demand: InlineTabDemand::default(),
            tab_models: FxHashMap::default(),
            frame_fingerprint: 0,
            rendered_fingerprint: None,
        }
    }

    fn effective_tab_theme() -> backend::TabTheme {
        crate::benchmark_ui_fixture::tab_theme_override().unwrap_or_else(|| {
            use crate::backend::lock_utils::MutexExt;
            crate::backend::settings::SETTINGS
                .lock_or_recover()
                .tab_theme
                .clone()
                .into()
        })
    }

    fn effective_overflow_mode() -> backend::TabOverflowMode {
        crate::benchmark_ui_fixture::tab_overflow_override()
            .unwrap_or_else(backend::get_tab_overflow_mode)
    }

    fn save_tabs_if_allowed(state: &AppState, action: &str) {
        if crate::benchmark_ui_fixture::should_suppress_persistence() {
            return;
        }
        crate::log_ui!(trace, "Queueing tab save after {}", action);
        crate::config::queue_save_tabs(state.tabs_for_persistence());
    }

    /// Resolve the minimal appearance's inactive activity hierarchy. Kept as
    /// one helper so the native UI fixture can assert the exact model used for
    /// painting.
    pub(crate) fn minimal_visual_state(
        tab: &TabState,
        is_active: bool,
    ) -> crate::state::TabHighlightState {
        if is_active {
            return crate::state::TabHighlightState::None;
        }

        match tab.highlight_state {
            crate::state::TabHighlightState::Highlighted => {
                crate::state::TabHighlightState::Highlighted
            }
            crate::state::TabHighlightState::NewMessage => {
                crate::state::TabHighlightState::NewMessage
            }
            crate::state::TabHighlightState::None if tab.has_unread => {
                crate::state::TabHighlightState::NewMessage
            }
            crate::state::TabHighlightState::None => crate::state::TabHighlightState::None,
        }
    }

    fn measure_title_width(title: &SharedString, font: &Font, window: &Window) -> f32 {
        if title.is_empty() {
            0.0
        } else {
            let runs = [TextRun {
                len: title.len(),
                font: font.clone(),
                color: colors::text_primary(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            window
                .text_system()
                .shape_line(title.clone(), px(TAB_TITLE_FONT_SIZE_PX), &runs, None)
                .width
                .as_f32()
        }
    }

    /// Title width in UI-tab font metrics plus 32 pixels for the close
    /// affordance, clamped between one tab height and 150 pixels. The close
    /// slot stays reserved, so the same formula applies whether or not the
    /// pointer is currently over the tab.
    fn minimal_width(title_width: f32) -> f32 {
        (title_width + MINIMAL_TAB_CHROME_WIDTH_PX)
            .clamp(MINIMAL_TAB_MIN_WIDTH_PX, MINIMAL_TAB_MAX_WIDTH_PX)
    }

    /// Width produced by `render_tab_item` for overflow layout.
    ///
    /// Browser appearances use the same shaped title width as painting, then
    /// add theme/mode chrome and a small safety minimum.
    fn measured_tab_width(
        &self,
        tab: &TabState,
        tab_theme: &backend::TabTheme,
        _legacy_character_width: f32,
        _legacy_chrome_width: f32,
        legacy_min_width: f32,
    ) -> f32 {
        let minimal = *tab_theme == backend::TabTheme::Minimal;
        let lock_width = if tab.is_locked {
            TAB_LOCK_GLYPH_WIDTH_PX + if minimal { 8.0 } else { 6.0 }
        } else {
            0.0
        };
        match self.tab_models.get(&tab.id) {
            Some(model) if minimal && tab.is_locked => {
                Self::minimal_width(model.shaped_title_width + lock_width)
            }
            Some(model) if minimal => model.minimal_width,
            Some(model) => (model.shaped_title_width
                + lock_width
                + browser_tab_chrome_width(
                    tab_theme,
                    model.live.live_channel_count > 0,
                    tab.has_unread || tab.highlight_state != crate::state::TabHighlightState::None,
                ))
            .ceil()
            .max(legacy_min_width),
            None if minimal => MINIMAL_TAB_MIN_WIDTH_PX,
            None => legacy_min_width,
        }
    }

    fn tab_layout_metrics(
        tab_theme: &backend::TabTheme,
        overflow_mode: &backend::TabOverflowMode,
    ) -> TabLayoutMetrics {
        let (character_width, chrome_width, minimum_width) =
            if *overflow_mode == backend::TabOverflowMode::Scroll {
                (6.5, 24.0, 50.0)
            } else {
                (7.6, 28.0, BROWSER_TAB_MIN_WIDTH_PX)
            };
        let add_button_width = if *tab_theme == backend::TabTheme::Minimal {
            28.0
        } else {
            BROWSER_ADD_TAB_CONTROL_WIDTH_PX
        };
        let tab_spacer = if *tab_theme == backend::TabTheme::Minimal {
            MINIMAL_TAB_SPACER_PX
        } else {
            0.0
        };

        TabLayoutMetrics {
            character_width,
            chrome_width,
            minimum_width,
            add_button_width,
            tab_spacer,
        }
    }

    fn uses_browser_tab_sizing(tab_theme: &backend::TabTheme) -> bool {
        *tab_theme != backend::TabTheme::Minimal
    }

    fn publish_inline_tab_demand(
        &mut self,
        tabs: &[(usize, &TabState)],
        _active_idx: usize,
        tab_theme: &backend::TabTheme,
        overflow_mode: &backend::TabOverflowMode,
    ) {
        if tabs.is_empty() || *overflow_mode == backend::TabOverflowMode::MultiLine {
            self.inline_tab_demand = InlineTabDemand::default();
            return;
        }

        let metrics = Self::tab_layout_metrics(tab_theme, overflow_mode);
        let tab_width = |tab: &TabState| {
            self.measured_tab_width(
                tab,
                tab_theme,
                metrics.character_width,
                metrics.chrome_width,
                metrics.minimum_width,
            )
        };
        let full_tab_set_width: f32 = tabs.iter().map(|(_, tab)| tab_width(tab)).sum();
        let controls_and_spacers_width = metrics.add_button_width
            + metrics.tab_spacer
                * match overflow_mode {
                    backend::TabOverflowMode::Scroll => tabs.len(),
                    backend::TabOverflowMode::Dropdown => tabs.len().saturating_sub(1),
                    backend::TabOverflowMode::MultiLine => 0,
                } as f32;

        let mut hasher = rustc_hash::FxHasher::default();
        match overflow_mode {
            backend::TabOverflowMode::Scroll => 0_u8.hash(&mut hasher),
            backend::TabOverflowMode::Dropdown => 1_u8.hash(&mut hasher),
            backend::TabOverflowMode::MultiLine => 2_u8.hash(&mut hasher),
        }
        match tab_theme {
            backend::TabTheme::Chrome => 0_u8.hash(&mut hasher),
            backend::TabTheme::Firefox => 1_u8.hash(&mut hasher),
            backend::TabTheme::Edge => 2_u8.hash(&mut hasher),
            backend::TabTheme::Square => 3_u8.hash(&mut hasher),
            backend::TabTheme::Arc => 4_u8.hash(&mut hasher),
            backend::TabTheme::Minimal => 5_u8.hash(&mut hasher),
        }
        for (idx, tab) in tabs {
            idx.hash(&mut hasher);
            tab.id.hash(&mut hasher);
            tab.display_title().hash(&mut hasher);
        }

        self.inline_tab_demand = InlineTabDemand {
            known: true,
            full_tab_set_reservation_px: full_tab_set_width,
            controls_and_spacers_reservation_px: controls_and_spacers_width,
            context: hasher.finish(),
        };
    }

    fn calculate_dropdown_layout(
        &self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        tab_theme: &backend::TabTheme,
        bar_width: f32,
        metrics: TabLayoutMetrics,
        overflow_button_width: f32,
    ) -> DropdownTabLayout {
        if tabs.is_empty() {
            return DropdownTabLayout::default();
        }

        let tab_width = |tab: &TabState| {
            self.measured_tab_width(
                tab,
                tab_theme,
                metrics.character_width,
                metrics.chrome_width,
                metrics.minimum_width,
            )
        };
        let active_position = tabs.iter().position(|(idx, _)| *idx == active_idx);

        let natural_widths: Vec<f32> = tabs.iter().map(|(_, tab)| tab_width(tab)).collect();
        let visible_positions = solve_natural_dropdown_tabs(
            &natural_widths,
            active_position,
            bar_width,
            metrics.tab_spacer,
            metrics.add_button_width,
            overflow_button_width,
        );
        let browser_tab_widths_px = Self::uses_browser_tab_sizing(tab_theme).then(|| {
            visible_positions
                .iter()
                .map(|&position| natural_widths[position])
                .collect()
        });
        DropdownTabLayout {
            visible_positions,
            browser_tab_widths_px,
        }
    }

    /// Rebuild the retained render models whose inputs changed.
    ///
    /// The layout tree, custom title, resolved Watching channel and measurement
    /// font form the rebuild key. Live status is folded in on every sync because
    /// stream metadata changes independently of that key.
    fn sync_tab_models(
        &mut self,
        tabs: &[(usize, &TabState)],
        stream_info: &HashMap<String, Option<backend::StreamInfo>>,
        window: &Window,
    ) {
        let watching_channel = backend::browser_watching::current_channel();
        let hide_viewer_stats = backend::streamer_mode::should_hide_viewer_count();
        let font = window.text_style().font();

        for (position, &(_, tab)) in tabs.iter().enumerate() {
            let needs_rebuild = match self.tab_models.get(&tab.id) {
                Some(model) => {
                    !Arc::ptr_eq(&model.layout, &tab.layout)
                        || model.custom_title != tab.custom_title
                        || model.watching_channel != watching_channel
                        || model.measure_font != font
                }
                None => true,
            };
            if needs_rebuild {
                let model = Self::build_tab_model(tab, &watching_channel, &font, window);
                self.tab_models.insert(tab.id.clone(), model);
            }

            let Some(model) = self.tab_models.get_mut(&tab.id) else {
                continue;
            };
            model.position_in_set = position + 1;
            model.size_of_set = tabs.len();
            let mut live = TabLiveState {
                hide_viewer_stats,
                ..TabLiveState::default()
            };
            for channel in &model.channels {
                let live_info = stream_info
                    .get(channel)
                    .and_then(|info| info.as_ref())
                    .filter(|info| info.is_live);
                if let Some(info) = live_info {
                    live.live_channel_count += 1;
                    live.viewer_count = live.viewer_count.saturating_add(info.viewer_count);
                }
            }
            if model.live != live {
                model.live = live;
                model.live_tooltip = Self::live_indicator_tooltip(live);
            }
        }

        if self.tab_models.len() > tabs.len() {
            let mounted: HashSet<&str> = tabs.iter().map(|(_, tab)| tab.id.as_str()).collect();
            self.tab_models
                .retain(|id, _| mounted.contains(id.as_str()));
        }
    }

    fn build_tab_model(
        tab: &TabState,
        watching_channel: &Option<String>,
        font: &Font,
        window: &Window,
    ) -> TabRenderModel {
        let title = SharedString::from(tab.display_title());
        let shaped_title_width = Self::measure_title_width(&title, font, window);
        let minimal_width = Self::minimal_width(shaped_title_width);
        let channel_targets = tab.channel_targets();
        let channel_tooltip = (channel_targets.len() > 1).then(|| {
            SharedString::from(format!(
                "Channels\n{}",
                channel_targets
                    .iter()
                    .map(|(_, label)| label.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        });

        TabRenderModel {
            layout: tab.layout.clone(),
            custom_title: tab.custom_title.clone(),
            watching_channel: watching_channel.clone(),
            measure_font: font.clone(),
            title,
            shaped_title_width,
            position_in_set: 0,
            size_of_set: 0,
            minimal_width,
            channels: tab.channels(),
            channel_tooltip,
            channel_tooltip_source: SharedString::from(format!("tab-tooltip-{}", tab.id)),
            live_tooltip_source: SharedString::from(format!("tab-live-indicator-{}", tab.id)),
            live: TabLiveState::default(),
            live_tooltip: SharedString::from("Offline"),
        }
    }

    fn live_indicator_tooltip(live: TabLiveState) -> SharedString {
        if live.live_channel_count == 0 {
            return SharedString::from("Offline");
        }
        if live.hide_viewer_stats {
            return if live.live_channel_count > 1 {
                SharedString::from(format!("{} live channels", live.live_channel_count))
            } else {
                SharedString::from("Live")
            };
        }
        let noun = if live.viewer_count == 1 {
            "viewer"
        } else {
            "viewers"
        };
        if live.live_channel_count > 1 {
            SharedString::from(format!(
                "{} live • {} total {noun}",
                live.live_channel_count,
                format_viewer_count(live.viewer_count)
            ))
        } else {
            SharedString::from(format!(
                "Live • {} {noun}",
                format_viewer_count(live.viewer_count)
            ))
        }
    }

    fn tab_tooltip(
        &self,
        idx: usize,
        title: &SharedString,
        channels: Option<&SharedString>,
    ) -> Option<String> {
        if self.tab_manager.clipped_titles.contains(&idx) {
            Some(match channels {
                Some(channels) => format!("{title}\n\n{channels}"),
                None => title.to_string(),
            })
        } else {
            channels.map(ToString::to_string)
        }
    }

    /// Pack multiline tabs greedily, left to right.
    ///
    /// Tabs retain their measured content width and a one-pixel spacer. Only the final tab
    /// reserves room for the add button; when the pair does not fit, that final tab moves to
    /// the next row with the button.
    /// Rows are deliberately not stretched or rebalanced.
    fn calculate_multiline_rows(
        &self,
        tabs: &[(usize, &TabState)],
        tab_theme: &backend::TabTheme,
        bar_width: f32,
    ) -> Vec<Vec<usize>> {
        if bar_width <= 1.0 {
            return vec![(0..tabs.len()).collect()];
        }

        let add_button_width = if *tab_theme == backend::TabTheme::Minimal {
            28.0
        } else {
            36.0
        };
        let tab_spacer = if *tab_theme == backend::TabTheme::Minimal {
            MINIMAL_TAB_SPACER_PX
        } else {
            0.0
        };
        let mut rows = vec![Vec::new()];
        let mut row_width = 0.0_f32;

        for (position, (_, tab)) in tabs.iter().enumerate() {
            let row_idx = rows.len() - 1;
            let width = self
                .measured_tab_width(tab, tab_theme, 7.6, 28.0, 55.0)
                .min(bar_width);
            let is_last = position + 1 == tabs.len();
            let reserved_add_width = if is_last { add_button_width } else { 0.0 };
            if !rows[row_idx].is_empty() && row_width + width + reserved_add_width > bar_width + 0.5
            {
                rows.push(vec![position]);
                row_width = width + tab_spacer;
            } else {
                rows[row_idx].push(position);
                row_width += width + tab_spacer;
            }
        }

        if row_width + add_button_width > bar_width + 0.5 {
            rows.push(Vec::new());
        }

        rows
    }

    /// Per-frame sync run by the app before layout, whether or not the cached
    /// tab bar view re-renders: it normalizes transient state, refreshes the
    /// retained tab models and the geometry the app lays out around (height,
    /// dedicated row, inline demand), then fingerprints every render input.
    ///
    /// Returns `true` when the retained subtree is stale and the view must be
    /// rendered uncached this frame. Tab state, stream info, settings and theme
    /// change without notifying this entity, so the fingerprint, not a notify,
    /// is what invalidates the cached view for them.
    pub fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let app_state = self.app_state.clone();
        let (drag_snapshot_matches, tab_bar_hidden, rename_target_mounted) = {
            let state = app_state.read(cx);
            let rename_state = &self.tab_manager.rename_state;
            (
                self.tab_manager
                    .drag_snapshot_matches(state.tabs.iter().map(|tab| tab.id.as_str())),
                state.should_hide_tab_bar(),
                rename_state.is_active
                    && state.visible_tab_indices().into_iter().any(|idx| {
                        state
                            .tabs
                            .get(idx)
                            .is_some_and(|tab| tab.id == rename_state.tab_id)
                    }),
            )
        };
        if !drag_snapshot_matches {
            self.cancel_tab_drag();
        }

        if tab_bar_hidden {
            self.is_visible = false;
            self.scroll_active_tab_id = None;
            self.scroll_geometry = None;
            self.tab_manager.scroll_viewport_bounds = None;
            self.inline_tab_demand = InlineTabDemand::default();
            if self.tab_manager.rename_state.is_active {
                self.commit_rename(cx);
            }
            self.tab_models.clear();
            let mut hasher = rustc_hash::FxHasher::default();
            u8::MAX.hash(&mut hasher);
            self.frame_fingerprint = hasher.finish();
            return self.rendered_fingerprint != Some(self.frame_fingerprint);
        }

        self.is_visible = true;

        if rename_target_mounted {
            self.ensure_rename_input(window, cx);
        } else if self.tab_manager.rename_state.is_active {
            self.commit_rename(cx);
        }

        let overflow_mode = Self::effective_overflow_mode();
        let tab_theme = Self::effective_tab_theme();
        if self.last_overflow_mode.as_ref() != Some(&overflow_mode) {
            self.tab_manager.hovered_idx = None;
            self.tab_manager.hit_areas.clear();
            self.tab_manager.overflow_button_bounds = None;
            self.tab_manager.scroll_viewport_bounds = None;
            self.scroll_active_tab_id = None;
            self.scroll_geometry = None;
            self.last_overflow_mode = Some(overflow_mode.clone());
        }
        if overflow_mode != backend::TabOverflowMode::Scroll {
            self.tab_manager.scroll_offset = 0.0;
            self.tab_manager.max_scroll_offset = 0.0;
            self.tab_manager.scroll_direction = None;
            self.tab_manager.scroll_viewport_bounds = None;
        }

        let state = app_state.read(cx);
        let active_idx = state.active_tab_idx;
        let visible_tabs = state
            .visible_tab_indices()
            .into_iter()
            .filter_map(|idx| state.tabs.get(idx).map(|tab| (idx, tab)))
            .collect::<Vec<_>>();
        if self
            .tab_manager
            .hovered_idx
            .is_some_and(|hovered| !visible_tabs.iter().any(|(idx, _)| *idx == hovered))
        {
            self.tab_manager.hovered_idx = None;
        }
        self.sync_tab_models(&visible_tabs, &state.stream_info, window);
        if overflow_mode == backend::TabOverflowMode::Scroll {
            self.prepare_scroll_tabs(&visible_tabs, active_idx, &tab_theme);
        }
        self.publish_inline_tab_demand(&visible_tabs, active_idx, &tab_theme, &overflow_mode);
        self.multiline_row_count = if overflow_mode == backend::TabOverflowMode::MultiLine {
            let bar_width =
                crate::benchmark_ui_fixture::cap_tab_layout_width(self.tab_manager.bar_width);
            self.calculate_multiline_rows(&visible_tabs, &tab_theme, bar_width)
                .len()
                .max(1)
        } else {
            1
        };
        self.frame_fingerprint = self.render_fingerprint(
            &visible_tabs,
            active_idx,
            &tab_theme,
            &overflow_mode,
            window,
        );
        self.rendered_fingerprint != Some(self.frame_fingerprint)
    }

    fn render_fingerprint(
        &self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        tab_theme: &backend::TabTheme,
        overflow_mode: &backend::TabOverflowMode,
        window: &Window,
    ) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        colors::render_key().hash(&mut hasher);
        std::mem::discriminant(tab_theme).hash(&mut hasher);
        std::mem::discriminant(overflow_mode).hash(&mut hasher);
        f32::from(window.rem_size()).to_bits().hash(&mut hasher);
        window.scale_factor().to_bits().hash(&mut hasher);
        active_idx.hash(&mut hasher);

        let manager = &self.tab_manager;
        manager.bar_width.to_bits().hash(&mut hasher);
        manager.scroll_offset.to_bits().hash(&mut hasher);
        manager.hovered_idx.hash(&mut hasher);
        manager.context_menu.is_open.hash(&mut hasher);
        manager.overflow_dropdown_open.hash(&mut hasher);
        manager.rename_state.is_active.hash(&mut hasher);
        manager.rename_state.tab_id.hash(&mut hasher);
        self.rename_input
            .as_ref()
            .map(|input| input.entity_id())
            .hash(&mut hasher);
        let drag = &manager.drag_state;
        drag.is_dragging.hash(&mut hasher);
        drag.source_idx.hash(&mut hasher);
        drag.drop_target
            .as_ref()
            .map(|target| (target.target_idx, std::mem::discriminant(&target.zone)))
            .hash(&mut hasher);

        tabs.len().hash(&mut hasher);
        for &(idx, tab) in tabs {
            idx.hash(&mut hasher);
            tab.id.hash(&mut hasher);
            tab.is_locked.hash(&mut hasher);
            tab.has_unread.hash(&mut hasher);
            (tab.highlight_state as u8).hash(&mut hasher);
            if let Some(model) = self.tab_models.get(&tab.id) {
                model.title.hash(&mut hasher);
                model.shaped_title_width.to_bits().hash(&mut hasher);
                model.channel_tooltip.hash(&mut hasher);
                model.live.hash(&mut hasher);
                model.live_tooltip.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// The tab bar as a child view: cached (reused until notified) unless
    /// `prepare` reported its inputs changed. The cached layout box mirrors the
    /// root the view renders, so both paths lay out identically.
    pub fn view_element(entity: &Entity<Self>, height: f32, stale: bool) -> AnyElement {
        if stale || crate::benchmark_ui_fixture::is_enabled() {
            entity.clone().into_any_element()
        } else {
            entity
                .clone()
                .cached(
                    StyleRefinement::default()
                        .w_full()
                        .h(px(height))
                        .flex_shrink_0(),
                )
                .into_any_element()
        }
    }

    pub fn is_tab_drag_pending(&self) -> bool {
        self.tab_manager.drag_state.is_pending
    }

    pub fn is_tab_dragging(&self) -> bool {
        self.tab_manager.is_dragging()
    }

    pub fn cancel_tab_drag(&mut self) {
        self.tab_manager.cancel_drag();
    }

    pub fn complete_tab_drag<'a>(
        &mut self,
        tab_order: impl Iterator<Item = &'a str>,
    ) -> Option<(usize, TabDropTarget)> {
        if !self.tab_manager.drag_snapshot_matches(tab_order) {
            self.cancel_tab_drag();
            return None;
        }
        self.tab_manager.end_drag()
    }

    pub fn is_context_menu_open(&self) -> bool {
        self.tab_manager.is_context_menu_open()
    }

    pub fn context_menu_info(&self) -> (usize, &str, f32, f32) {
        let ctx = &self.tab_manager.context_menu;
        (ctx.tab_idx, &ctx.tab_id, ctx.x, ctx.y)
    }

    pub fn close_context_menu(&mut self) {
        self.tab_manager.close_context_menu();
    }

    pub fn is_overflow_open(&self) -> bool {
        self.tab_manager.is_overflow_dropdown_open()
    }

    pub fn close_overflow(&mut self) {
        self.tab_manager.overflow_dropdown_open = false;
    }

    /// Clear the transient tab-bar state created by the disposable UI
    /// fixture. This deliberately avoids changing saved tabs or the normal
    /// interaction paths; the benchmark driver calls it only while tearing
    /// down its in-memory fixture.
    #[doc(hidden)]
    pub fn benchmark_close_transients(&mut self) -> bool {
        self.close_overflow();
        self.close_context_menu();
        self.tab_manager.hovered_idx = None;
        self.tab_manager.scroll_direction = None;
        !self.is_overflow_open()
            && !self.is_context_menu_open()
            && self.hovered_tab_idx().is_none()
            && self.tab_manager.scroll_direction.is_none()
    }

    pub fn bar_width(&self) -> f32 {
        self.tab_manager.bar_width
    }

    pub fn inline_tab_demand(&self) -> InlineTabDemand {
        self.inline_tab_demand
    }

    pub fn overflow_button_bounds(&self) -> Option<&Bounds<Pixels>> {
        self.tab_manager.overflow_button_bounds.as_ref()
    }

    pub fn tab_bounds(&self, idx: usize) -> Option<&Bounds<Pixels>> {
        self.tab_manager.hit_areas.get(&idx)
    }

    /// Return a small safe body rectangle for runners that derive their input
    /// point from a marker's center. The rectangle excludes the right-side
    /// close slot by construction.
    pub fn tab_hover_target_bounds(&self, idx: usize) -> Option<(f32, f32, f32, f32)> {
        let bounds = self.tab_bounds(idx)?;
        let x: f32 = bounds.origin.x.into();
        let y: f32 = bounds.origin.y.into();
        let width: f32 = bounds.size.width.into();
        let height: f32 = bounds.size.height.into();
        Some((x + 4.0, y, (width - 32.0).max(2.0), height))
    }

    /// Return the overflow button's main-client rectangle for the harness's
    /// one explicitly safe native click.
    pub fn overflow_button_target_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let bounds = self.overflow_button_bounds()?;
        Some((
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        ))
    }

    pub fn hovered_tab_idx(&self) -> Option<usize> {
        self.tab_manager.hovered_idx
    }

    pub(crate) fn update_hovered_tab_from_position(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let in_viewport = self
            .tab_manager
            .scroll_viewport_bounds
            .as_ref()
            .is_none_or(|bounds| bounds.contains(&position));
        let hovered_idx = in_viewport
            .then(|| {
                self.tab_manager
                    .hit_areas
                    .iter()
                    .find_map(|(idx, bounds)| bounds.contains(&position).then_some(*idx))
            })
            .flatten();
        if self.tab_manager.hovered_idx != hovered_idx {
            self.tab_manager.hovered_idx = hovered_idx;
            cx.notify();
        }
    }

    pub fn multiline_row_count(&self) -> usize {
        self.multiline_row_count.max(1)
    }

    pub fn effective_tab_bar_height(&self) -> f32 {
        if !self.is_visible {
            return 0.0;
        }

        if Self::effective_overflow_mode() == backend::TabOverflowMode::MultiLine {
            self.multiline_row_count() as f32 * config::TAB_BAR_HEIGHT_PX
        } else {
            config::TAB_BAR_HEIGHT_PX
        }
    }

    /// Multi-line tabs require the full-width dedicated row. This intentionally
    /// follows the fixture override as well as the persisted overflow setting.
    pub fn requires_dedicated_tab_row(&self) -> bool {
        Self::effective_overflow_mode() == backend::TabOverflowMode::MultiLine
    }

    pub fn is_renaming(&self) -> bool {
        self.tab_manager.rename_state.is_active
    }

    pub fn start_rename(&mut self, tab_id: String) {
        self.cancel_rename();
        self.cancel_tab_drag();
        self.tab_manager.start_rename(tab_id);
    }

    pub fn cancel_rename(&mut self) {
        self.tab_manager.cancel_rename();
        self.rename_input = None;
        self.rename_input_subscriptions.clear();
        self.rename_input_tab_id = None;
    }

    pub fn rename_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.app_state.read(cx);
        if state.should_hide_tab_bar() {
            return;
        }
        let tab_id = state.active_tab().id.clone();
        self.commit_rename(cx);
        self.close_context_menu();
        self.close_overflow();
        self.start_rename(tab_id);
        self.scroll_active_tab_id = None;
        self.ensure_rename_input(window, cx);
        cx.notify();
    }

    pub fn open_active_tab_menu(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rename(cx);
        self.cancel_tab_drag();
        self.close_overflow();
        let state = self.app_state.read(cx);
        let idx = state.active_tab_idx;
        let tab_id = state.active_tab().id.clone();
        let (x, y) = self
            .tab_bounds(idx)
            .map_or((8.0, backend::settings::titlebar_height_px()), |bounds| {
                (f32::from(bounds.origin.x), f32::from(bounds.bottom()))
            });
        self.tab_manager.context_menu = crate::managers::tab_manager::TabContextMenu {
            is_open: true,
            tab_idx: idx,
            tab_id,
            x,
            y,
        };
        cx.notify();
    }

    pub fn is_rename_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.tab_manager.rename_state.is_active
            && self
                .rename_input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle().is_focused(window))
    }

    fn ensure_rename_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = self.tab_manager.rename_state.tab_id.clone();
        if self.rename_input_tab_id.as_deref() == Some(tab_id.as_str()) {
            return;
        }

        if self.rename_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx));
            let subscription = cx.subscribe(&input, |this, input, event, cx| {
                if this.rename_input.as_ref().map(Entity::entity_id) != Some(input.entity_id()) {
                    return;
                }
                match event {
                    InputEvent::PressEnter => {
                        this.commit_rename(cx);
                        // Dropping the focused rename input leaves the window without
                        // focus; hand it back to the composer.
                        cx.emit(TabBarActiveSplitChanged);
                    }
                    InputEvent::Blur => this.commit_rename(cx),
                    InputEvent::Change | InputEvent::Focus => {}
                }
            });
            // The input renders as a `RenderOnce` inside this cached view, so
            // its own notifies (typing, caret, selection) must repaint the bar.
            let observer = cx.observe(&input, |_, _, cx| cx.notify());
            self.rename_input = Some(input);
            self.rename_input_subscriptions = vec![subscription, observer];
        }

        let input = self
            .rename_input
            .as_ref()
            .expect("rename input was initialized");
        let title = self
            .app_state
            .read(cx)
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.display_title())
            .unwrap_or_default();
        input.update(cx, |state, cx| {
            state.set_value(title, window, cx);
            state.handle_select_all(&SelectAll, window, cx);
        });
        let focus = input.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.rename_input_tab_id = Some(tab_id);
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.rename_input.as_ref() else {
            self.cancel_rename();
            return;
        };
        let tab_id = self.tab_manager.rename_state.tab_id.clone();
        let title = input.read(cx).value().to_string();
        let committed = self.app_state.update(cx, |state, _cx| {
            if let Some(tab) = state.tabs_mut().iter_mut().find(|tab| tab.id == tab_id) {
                tab.set_title(title);
                true
            } else {
                false
            }
        });
        self.cancel_rename();
        if committed {
            Self::save_tabs_if_allowed(self.app_state.read(cx), "rename tab");
        }
        cx.notify();
    }

    /// Dispatch to the appropriate tab bar renderer based on overflow mode
    fn render_tab_bar_fast(
        &mut self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        app_state: Entity<AppState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match Self::effective_overflow_mode() {
            backend::TabOverflowMode::Scroll => {
                self.multiline_row_count = 1;
                self.render_tab_bar_scroll(tabs, active_idx, app_state, cx)
                    .into_any_element()
            }
            backend::TabOverflowMode::Dropdown => {
                self.multiline_row_count = 1;
                self.render_tab_bar_dropdown(tabs, active_idx, app_state, cx)
                    .into_any_element()
            }
            backend::TabOverflowMode::MultiLine => self
                .render_tab_bar_multiline(tabs, active_idx, app_state, cx)
                .into_any_element(),
        }
    }

    fn prepare_scroll_tabs(
        &mut self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        tab_theme: &backend::TabTheme,
    ) {
        let bar_width =
            crate::benchmark_ui_fixture::cap_tab_layout_width(self.tab_manager.bar_width);
        let metrics = Self::tab_layout_metrics(tab_theme, &backend::TabOverflowMode::Scroll);
        let mut content_width = 0.0;
        let mut active_bounds = None;
        let mut active_id = None;
        for &(idx, tab) in tabs {
            let width = self.measured_tab_width(
                tab,
                tab_theme,
                metrics.character_width,
                metrics.chrome_width,
                metrics.minimum_width,
            );
            if idx == active_idx {
                active_bounds = Some((content_width, content_width + width));
                active_id = Some(tab.id.as_str());
            }
            content_width += width + metrics.tab_spacer;
        }
        content_width += metrics.add_button_width;
        let has_overflow = bar_width > 0.0 && content_width > bar_width + 1.0;
        let max_scroll = if has_overflow {
            // At the end only the left arrow occupies space beside the strip.
            (content_width - (bar_width - SCROLL_ARROW_WIDTH_PX).max(0.0)).max(0.0)
        } else {
            0.0
        };
        let manager = &mut self.tab_manager;
        let was_visible = self
            .scroll_geometry
            .is_some_and(|geometry| geometry.active_is_visible(manager.scroll_offset));
        manager.total_tabs_width = content_width;
        manager.max_scroll_offset = max_scroll;
        manager.scroll_offset = manager.scroll_offset.clamp(0.0, max_scroll);
        if bar_width > 1.0 {
            let geometry = active_bounds.map(|(start, end)| ScrollTabGeometry {
                active_start: start,
                active_end: end,
                bar_width,
                max_scroll,
            });
            let active_changed = self.scroll_active_tab_id.as_deref() != active_id;
            if (active_changed || (geometry != self.scroll_geometry && was_visible))
                && let Some(geometry) = geometry
            {
                let start = geometry.active_start;
                let end = geometry.active_end;
                let viewport_width = geometry.viewport_width(manager.scroll_offset);
                if start < manager.scroll_offset || end - start > viewport_width {
                    manager.scroll_offset = start.clamp(0.0, max_scroll);
                } else if end > manager.scroll_offset + viewport_width {
                    let viewport_width = (bar_width - SCROLL_ARROW_WIDTH_PX * 2.0).max(0.0);
                    manager.scroll_offset = (end - viewport_width).clamp(0.0, max_scroll);
                }
            }
            if active_changed {
                self.scroll_active_tab_id = active_id.map(str::to_owned);
            }
            self.scroll_geometry = geometry;
        }
    }

    /// Render tab bar with scroll arrows (single row, scrollable)
    fn render_tab_bar_scroll(
        &mut self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        app_state: Entity<AppState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab_theme = Self::effective_tab_theme();
        let metrics = Self::tab_layout_metrics(&tab_theme, &backend::TabOverflowMode::Scroll);
        let effective_scroll = self.tab_manager.scroll_offset;
        let show_left_arrow = effective_scroll > 0.5;
        let show_right_arrow = effective_scroll < self.tab_manager.max_scroll_offset - 0.5;
        let entity = cx.entity();

        let mut tab_elements: Vec<AnyElement> = Vec::with_capacity(tabs.len() + 1);
        for &(idx, tab) in tabs {
            let Some(model) = self.tab_models.get(&tab.id) else {
                continue;
            };
            let is_active = idx == active_idx;
            tab_elements.push(
                self.render_tab_item(
                    tab,
                    model,
                    &tab_theme,
                    None,
                    is_active,
                    idx,
                    app_state.clone(),
                    cx,
                )
                .into_any_element(),
            );
        }
        tab_elements.push(
            self.render_add_tab_button(app_state.clone(), cx)
                .into_any_element(),
        );

        div()
            .id("tab-bar")
            .h(px(config::TAB_BAR_HEIGHT_PX))
            .flex_shrink_0()
            .w_full()
            .relative()
            .flex()
            .items_center()
            .bg(colors::bg_plate())
            .map(|bar| Self::with_bar_width_probe(bar, cx))
            .when(show_left_arrow, |el| {
                el.child(
                    div()
                        .id("tab-scroll-left")
                        .role(Role::Button)
                        .aria_label("Scroll tabs left")
                        .tab_index(0)
                        .focus_visible(|style| {
                            style.shadow(crate::components::ui::focus_ring(colors::bg_plate()))
                        })
                        .on_key_down(cx.listener(|this, event, _window, cx| {
                            if crate::components::ui::is_button_activation_key(event) {
                                cx.stop_propagation();
                                if this.tab_manager.scroll_by(-50.0) {
                                    cx.notify();
                                }
                            }
                        }))
                        .on_a11y_action(accesskit::Action::Click, {
                            let click = cx.listener(|this, _: &(), _window, cx| {
                                if this.tab_manager.scroll_by(-50.0) {
                                    cx.notify();
                                }
                            });
                            move |_, window, cx| click(&(), window, cx)
                        })
                        .w(px(SCROLL_ARROW_WIDTH_PX))
                        .h_full()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(colors::bg_hover()))
                        .text_color(colors::text_muted())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event: &MouseDownEvent, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                if this.tab_manager.scroll_direction.is_some() {
                                    return;
                                }
                                this.tab_manager.scroll_direction = Some(ScrollDirection::Left);
                                this.tab_manager.scroll_by(-50.0);
                                let entity = cx.entity().clone();
                                cx.spawn(async move |_this, cx| {
                                    loop {
                                        cx.background_executor()
                                            .timer(std::time::Duration::from_millis(50))
                                            .await;
                                        let should_continue = entity.update(cx, |renderer, cx| {
                                            if renderer.tab_manager.scroll_direction
                                                != Some(ScrollDirection::Left)
                                            {
                                                return false;
                                            }
                                            if !renderer.tab_manager.scroll_by(-30.0) {
                                                renderer.tab_manager.scroll_direction = None;
                                                return false;
                                            }
                                            cx.notify();
                                            true
                                        });
                                        if !should_continue {
                                            break;
                                        }
                                    }
                                })
                                .detach();
                                cx.notify();
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                this.tab_manager.scroll_direction = None;
                                cx.notify();
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                this.tab_manager.scroll_direction = None;
                                cx.notify();
                            }),
                        )
                        .child(div().text_xs().child("<")),
                )
            })
            .child(
                div()
                    .id("tab-scroll-container")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .on_scroll(cx.listener(move |this, event: &ScrollEvent, _window, cx| {
                        let delta: f32 = match event.delta {
                            ScrollDelta::Lines(lines) => {
                                let axis = if lines.x != 0.0 { lines.x } else { lines.y };
                                axis * 50.0
                            }
                            ScrollDelta::Pixels(pixels) => {
                                let x: f32 = pixels.x.into();
                                if x != 0.0 { x } else { pixels.y.into() }
                            }
                        };
                        if this.tab_manager.scroll_by(-delta) {
                            cx.notify();
                        }
                    }))
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let bounds = bounds.intersect(&window.content_mask().bounds);
                                entity.update(cx, |renderer, _| {
                                    renderer.tab_manager.scroll_viewport_bounds = Some(bounds);
                                });
                            },
                            |_, _: (), _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(
                        div()
                            .id("tab-scroll-inner")
                            .flex()
                            .h_full()
                            .items_center()
                            .gap(px(metrics.tab_spacer))
                            .ml(px(-effective_scroll))
                            .children(tab_elements),
                    ),
            )
            .when(show_right_arrow, |el| {
                el.child(
                    div()
                        .id("tab-scroll-right")
                        .role(Role::Button)
                        .aria_label("Scroll tabs right")
                        .tab_index(0)
                        .focus_visible(|style| {
                            style.shadow(crate::components::ui::focus_ring(colors::bg_plate()))
                        })
                        .on_key_down(cx.listener(|this, event, _window, cx| {
                            if crate::components::ui::is_button_activation_key(event) {
                                cx.stop_propagation();
                                if this.tab_manager.scroll_by(50.0) {
                                    cx.notify();
                                }
                            }
                        }))
                        .on_a11y_action(accesskit::Action::Click, {
                            let click = cx.listener(|this, _: &(), _window, cx| {
                                if this.tab_manager.scroll_by(50.0) {
                                    cx.notify();
                                }
                            });
                            move |_, window, cx| click(&(), window, cx)
                        })
                        .w(px(SCROLL_ARROW_WIDTH_PX))
                        .h_full()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(colors::bg_hover()))
                        .text_color(colors::text_muted())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                if this.tab_manager.scroll_direction.is_some() {
                                    return;
                                }
                                this.tab_manager.scroll_direction = Some(ScrollDirection::Right);
                                this.tab_manager.scroll_by(50.0);
                                let entity = cx.entity().clone();
                                cx.spawn(async move |_this, cx| {
                                    loop {
                                        cx.background_executor()
                                            .timer(std::time::Duration::from_millis(50))
                                            .await;
                                        let should_continue = entity.update(cx, |renderer, cx| {
                                            if renderer.tab_manager.scroll_direction
                                                != Some(ScrollDirection::Right)
                                            {
                                                return false;
                                            }
                                            if !renderer.tab_manager.scroll_by(30.0) {
                                                renderer.tab_manager.scroll_direction = None;
                                                return false;
                                            }
                                            cx.notify();
                                            true
                                        });
                                        if !should_continue {
                                            break;
                                        }
                                    }
                                })
                                .detach();
                                cx.notify();
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                this.tab_manager.scroll_direction = None;
                                cx.notify();
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                this.tab_manager.scroll_direction = None;
                                cx.notify();
                            }),
                        )
                        .child(div().text_xs().child(">")),
                )
            })
    }

    /// Render tab bar with dropdown overflow (single row, overflow menu)
    fn render_tab_bar_dropdown(
        &mut self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        app_state: Entity<AppState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab_theme = Self::effective_tab_theme();
        let tab_bar_width = self.tab_manager.bar_width;

        let metrics = Self::tab_layout_metrics(&tab_theme, &backend::TabOverflowMode::Dropdown);
        let overflow_button_width = DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX;
        let dropdown_layout = self.calculate_dropdown_layout(
            tabs,
            active_idx,
            &tab_theme,
            tab_bar_width,
            metrics,
            overflow_button_width,
        );
        let visible_tabs: Vec<(usize, &TabState)> = dropdown_layout
            .visible_positions
            .iter()
            .map(|&position| tabs[position])
            .collect();
        let overflow_count = tabs.len().saturating_sub(visible_tabs.len());
        let has_overflow = overflow_count > 0;
        let mut unread_count = 0;
        let mut mention_count = 0;
        for (position, (_, tab)) in tabs.iter().enumerate() {
            if dropdown_layout
                .visible_positions
                .binary_search(&position)
                .is_ok()
            {
                continue;
            }
            if tab.has_unread || tab.highlight_state != crate::state::TabHighlightState::None {
                unread_count += 1;
            }
            if tab.highlight_state == crate::state::TabHighlightState::Highlighted {
                mention_count += 1;
            }
        }
        let assigned_widths = dropdown_layout.browser_tab_widths_px;

        let mut tab_elements: Vec<AnyElement> = Vec::with_capacity(visible_tabs.len());
        for (position, (idx, tab)) in visible_tabs.into_iter().enumerate() {
            let Some(model) = self.tab_models.get(&tab.id) else {
                continue;
            };
            let is_active = idx == active_idx;
            tab_elements.push(
                self.render_tab_item(
                    tab,
                    model,
                    &tab_theme,
                    assigned_widths
                        .as_ref()
                        .and_then(|widths| widths.get(position).copied()),
                    is_active,
                    idx,
                    app_state.clone(),
                    cx,
                )
                .into_any_element(),
            );
        }

        div()
            .id("tab-bar")
            .h(px(config::TAB_BAR_HEIGHT_PX))
            .flex_shrink_0()
            .w_full()
            .flex()
            .items_center()
            .bg(if tab_theme == backend::TabTheme::Minimal {
                colors::bg_plate()
            } else {
                colors::bg_surface()
            })
            .when(tab_theme != backend::TabTheme::Minimal, |bar| {
                bar.border_b_1().border_color(colors::border())
            })
            .map(|bar| Self::with_bar_width_probe(bar, cx))
            .child(
                div()
                    .id("tab-dropdown-visible-tabs")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(metrics.tab_spacer))
                    .overflow_hidden()
                    .children(tab_elements),
            )
            .when(has_overflow, |el| {
                el.child(self.render_overflow_button(
                    overflow_count,
                    unread_count,
                    mention_count,
                    cx,
                ))
            })
            .child(self.render_add_tab_button(app_state, cx))
    }

    /// Render tab bar with multiline wrapping
    fn render_tab_bar_multiline(
        &mut self,
        tabs: &[(usize, &TabState)],
        active_idx: usize,
        app_state: Entity<AppState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab_theme = Self::effective_tab_theme();
        let row_height = config::TAB_BAR_HEIGHT_PX;
        let bar_width =
            crate::benchmark_ui_fixture::cap_tab_layout_width(self.tab_manager.bar_width);
        let rows = self.calculate_multiline_rows(tabs, &tab_theme, bar_width);
        self.multiline_row_count = rows.len().max(1);
        let bar_height = self.multiline_row_count as f32 * row_height;
        let tab_spacer = if tab_theme == backend::TabTheme::Minimal {
            MINIMAL_TAB_SPACER_PX
        } else {
            0.0
        };

        let mut row_elements = Vec::with_capacity(rows.len());
        let final_row_idx = rows.len().saturating_sub(1);
        for (row_idx, row_positions) in rows.into_iter().enumerate() {
            let is_final_row = row_idx == final_row_idx;
            let mut tab_elements = Vec::with_capacity(row_positions.len());

            for (idx, tab) in row_positions
                .into_iter()
                .filter_map(|position| tabs.get(position).map(|(idx, tab)| (*idx, *tab)))
            {
                let Some(model) = self.tab_models.get(&tab.id) else {
                    continue;
                };
                let is_active = idx == active_idx;
                let width = self.measured_tab_width(tab, &tab_theme, 7.6, 28.0, 55.0);
                tab_elements.push(
                    self.render_tab_item(
                        tab,
                        model,
                        &tab_theme,
                        (bar_width > 1.0 && width > bar_width).then_some(bar_width),
                        is_active,
                        idx,
                        app_state.clone(),
                        cx,
                    )
                    .into_any_element(),
                );
            }

            row_elements.push(
                div()
                    .id(("tab-multiline-row", row_idx))
                    .w_full()
                    .h(px(row_height))
                    .flex()
                    .items_center()
                    .gap(px(tab_spacer))
                    .overflow_hidden()
                    .children(tab_elements)
                    .when(is_final_row, |row| {
                        row.child(self.render_add_tab_button(app_state.clone(), cx))
                    })
                    .into_any_element(),
            );
        }

        div()
            .id("tab-bar")
            .relative()
            .h(px(bar_height))
            .flex_shrink_0()
            .w_full()
            .when(crate::benchmark_ui_fixture::is_enabled(), |bar| {
                bar.w(px(crate::benchmark_ui_fixture::FIXTURE_TAB_LAYOUT_WIDTH_PX))
            })
            .flex()
            .flex_col()
            .items_start()
            .bg(colors::bg_plate())
            .map(|bar| Self::with_bar_width_probe(bar, cx))
            .children(row_elements)
    }

    /// Attach the absolutely positioned canvas that tracks the bar's laid-out
    /// width, plus the hover tracker shared by every tab bar mode.
    fn with_bar_width_probe(bar: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let entity = cx.entity();
        bar.role(Role::TabList)
            .aria_label("Chat tabs")
            .aria_orientation(accesskit::Orientation::Horizontal)
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        entity.update(cx, |renderer, cx| {
                            let width: f32 = bounds.size.width.into();
                            if (renderer.tab_manager.bar_width - width).abs() > 0.5 {
                                renderer.tab_manager.bar_width = width;
                                cx.notify();
                            }
                        });
                    },
                    |_, _: (), _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                this.update_hovered_tab_from_position(event.position, cx);
            }))
            .hover_listener_mode(HoverListenerMode::InputModalityIndependent)
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !*hovered {
                    let hover_changed = this.tab_manager.hovered_idx.take().is_some();
                    let target_changed = this.tab_manager.drag_state.drop_target.take().is_some();
                    if hover_changed || target_changed {
                        cx.notify();
                    }
                }
            }))
    }

    /// Render overflow button for dropdown mode
    fn render_overflow_button(
        &self,
        overflow_count: usize,
        unread_count: usize,
        mention_count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let entity = cx.entity().clone();
        let minimal_style = Self::effective_tab_theme() == backend::TabTheme::Minimal;
        let noun = if overflow_count == 1 { "tab" } else { "tabs" };
        let mut label = format!("{overflow_count} more {noun}");
        if mention_count > 0 {
            label.push_str(&format!(", {mention_count} with mentions"));
        } else if unread_count > 0 {
            label.push_str(&format!(", {unread_count} unread"));
        }

        div()
            .id("tab-overflow-btn")
            .relative()
            .role(Role::Button)
            .aria_label(label)
            .aria_expanded(self.is_overflow_open())
            .w(px(DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX))
            .h(px(if minimal_style {
                MINIMAL_TAB_HEIGHT_PX
            } else {
                config::TAB_BAR_HEIGHT_PX
            }))
            .px_1()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap_1()
            .cursor_pointer()
            .hover(|s| s.bg(colors::bg_hover()))
            .on_accessible_activate(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.tab_manager.overflow_dropdown_open = !this.tab_manager.overflow_dropdown_open;
                cx.notify();
            }))
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        entity.update(cx, |renderer, _| {
                            renderer.tab_manager.overflow_button_bounds = Some(bounds);
                        });
                    },
                    |_, _: (), _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child("…"),
            )
            .when(unread_count > 0 || mention_count > 0, |button| {
                button.child(
                    div()
                        .absolute()
                        .top(px(3.0))
                        .right(px(3.0))
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded(Radii::full())
                        .bg(if mention_count > 0 {
                            colors::mention_border()
                        } else {
                            colors::accent()
                        }),
                )
            })
    }

    /// Render a single tab item; each tab has its own layout.
    #[allow(clippy::too_many_arguments)]
    fn render_tab_item(
        &self,
        tab: &TabState,
        model: &TabRenderModel,
        tab_theme: &backend::TabTheme,
        assigned_width_px: Option<f32>,
        is_active: bool,
        idx: usize,
        app_state: Entity<AppState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (
            active_bg,
            inactive_bg,
            hover_bg,
            tab_padding_x,
            radius,
            top_only_radius,
            add_border,
            active_border_top,
            active_border_bottom,
            minimal_style,
        ) = match tab_theme {
            backend::TabTheme::Chrome => (
                colors::bg_plate(),
                colors::bg_plate(),
                colors::bg_hover(),
                10.0_f32,
                0.0_f32,
                false,
                false,
                false,
                true,
                false,
            ),
            backend::TabTheme::Firefox => {
                let mut active = colors::bg_elevated();
                active.a = 0.95;
                (
                    active,
                    TabManager::tab_bg(false),
                    colors::bg_hover(),
                    12.0_f32,
                    9.0_f32,
                    true,
                    true,
                    false,
                    false,
                    false,
                )
            }
            backend::TabTheme::Edge => {
                let mut inactive = colors::bg_surface();
                inactive.a = 0.9;
                (
                    TabManager::tab_bg(true),
                    inactive,
                    colors::bg_hover(),
                    12.0_f32,
                    5.0_f32,
                    false,
                    true,
                    true,
                    false,
                    false,
                )
            }
            backend::TabTheme::Square => (
                TabManager::tab_bg(true),
                TabManager::tab_bg(false),
                colors::bg_elevated(),
                12.0_f32,
                0.0_f32,
                false,
                true,
                false,
                true,
                false,
            ),
            backend::TabTheme::Arc => {
                let active = colors::bg_elevated();
                let mut hover = colors::bg_hover();
                hover.a = 0.75;
                (
                    active,
                    TabManager::tab_bg(false),
                    hover,
                    14.0_f32,
                    999.0_f32,
                    false,
                    true,
                    false,
                    false,
                    false,
                )
            }
            backend::TabTheme::Minimal => (
                colors::bg_elevated(),
                colors::bg_plate(),
                colors::bg_hover(),
                4.0_f32,
                0.0_f32,
                false,
                false,
                false,
                false,
                true,
            ),
        };

        let tab_highlight = if is_active {
            crate::state::TabHighlightState::None
        } else {
            tab.highlight_state
        };
        let minimal_visual_state = Self::minimal_visual_state(tab, is_active);
        let bg = if is_active { active_bg } else { inactive_bg };
        let text_color =
            if is_active || minimal_visual_state != crate::state::TabHighlightState::None {
                colors::text_primary()
            } else {
                colors::text_tertiary()
            };
        let unread_dot_color = match tab_highlight {
            crate::state::TabHighlightState::Highlighted => colors::mention_border(),
            _ => colors::accent(),
        };

        let title = model.title.clone();
        let tab_id = tab.id.clone();
        let is_locked = tab.is_locked;
        let has_unread = (tab.has_unread || tab_highlight != crate::state::TabHighlightState::None)
            && !is_active;

        let is_live = model.live.live_channel_count > 0;
        let live_indicator_tooltip = model.live_tooltip.clone();
        let live_indicator_tooltip_source = model.live_tooltip_source.clone();
        let channel_tooltip = model.channel_tooltip.clone();
        let channel_tooltip_source = model.channel_tooltip_source.clone();

        let is_renaming = self.tab_manager.rename_state.is_active
            && self.tab_manager.rename_state.tab_id == tab_id;
        let rename_input = is_renaming.then(|| self.rename_input.clone()).flatten();
        let rename_min_width = is_renaming.then(|| {
            assigned_width_px
                .unwrap_or_else(|| self.measured_tab_width(tab, tab_theme, 7.6, 28.0, 55.0))
        });

        let is_dragging = self.tab_manager.drag_state.is_dragging;
        let is_source = is_dragging && self.tab_manager.drag_state.source_idx == idx;
        let is_drop_target = self
            .tab_manager
            .drag_state
            .drop_target
            .as_ref()
            .map(|t| t.target_idx == idx)
            .unwrap_or(false);
        let drop_zone = self
            .tab_manager
            .drag_state
            .drop_target
            .as_ref()
            .filter(|t| t.target_idx == idx)
            .map(|t| t.zone);

        let is_hovered = self.tab_manager.hovered_idx == Some(idx);
        let show_close_button =
            (is_hovered || (minimal_style && is_active)) && !is_renaming && !is_dragging;
        let minimal_line_color = if is_active {
            colors::text_secondary()
        } else {
            match minimal_visual_state {
                crate::state::TabHighlightState::Highlighted => colors::mention_border(),
                crate::state::TabHighlightState::NewMessage => colors::text_muted(),
                crate::state::TabHighlightState::None if is_hovered => colors::border(),
                crate::state::TabHighlightState::None => colors::border_subtle(),
            }
        };
        let tab_height = if minimal_style {
            MINIMAL_TAB_HEIGHT_PX
        } else {
            config::TAB_BAR_HEIGHT_PX
        };
        let tab_gap = if minimal_style { 8.0 } else { 6.0 };
        let constrain_title = assigned_width_px.is_some_and(|width| {
            width + 0.5 < self.measured_tab_width(tab, tab_theme, 0.0, 0.0, 0.0)
        });

        let tab_id_for_close = tab_id.clone();
        let tab_id_for_middle = tab_id.clone();
        let tab_id_for_middle_up = tab_id.clone();
        let tab_id_for_middle_out = tab_id.clone();
        let tab_id_for_close_up = tab_id.clone();
        let tab_id_for_close_out = tab_id.clone();
        let tab_id_for_ctx = tab_id.clone();
        let tab_id_for_dblclick = tab_id.clone();

        let entity = cx.entity().clone();
        let title_entity = entity.clone();
        let title_width = model.shaped_title_width;
        let tab_id_for_accessibility = tab_id.clone();
        let tab_id_for_accessible_close = tab_id.clone();
        // Screen readers announce the name first and often skip the description,
        // so live and unread state ride on the name ("#xqc, live, unread").
        let mut accessible_name = title.to_string();
        match model.live.live_channel_count {
            0 => {}
            1 => accessible_name.push_str(", live"),
            count => accessible_name.push_str(&format!(", {count} live channels")),
        }
        if has_unread {
            accessible_name.push_str(
                if tab_highlight == crate::state::TabHighlightState::Highlighted {
                    ", unread mentions"
                } else {
                    ", unread"
                },
            );
        }
        let accessibility_state = [
            is_live.then(|| live_indicator_tooltip.to_string()),
            is_locked.then(|| "Layout locked".to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(". ");

        let mut tab_el = div()
            .id(("tab", idx))
            .role(Role::Tab)
            .aria_label(accessible_name)
            .aria_description(accessibility_state)
            .aria_selected(is_active)
            .aria_position_in_set(model.position_in_set)
            .aria_size_of_set(model.size_of_set)
            .on_a11y_action(accesskit::Action::Click, {
                let click = cx.listener(move |this, _: &(), _window, cx| {
                    this.commit_rename(cx);
                    this.close_context_menu();
                    this.close_overflow();
                    let changed = this.app_state.update(cx, |state, cx| {
                        let Some(idx) = state
                            .tabs
                            .iter()
                            .position(|tab| tab.id == tab_id_for_accessibility)
                        else {
                            return false;
                        };
                        state.switch_tab(idx, cx);
                        true
                    });
                    if changed {
                        Self::save_tabs_if_allowed(
                            this.app_state.read(cx),
                            "accessible tab switch",
                        );
                        cx.emit(TabBarActiveSplitChanged);
                        cx.notify();
                    }
                });
                move |_, window, cx| click(&(), window, cx)
            })
            .group(SharedString::from(format!("tab-{}", idx)))
            .h(px(tab_height))
            .flex_shrink_0()
            .px(px(tab_padding_x))
            .pt(px(Spacing::HAIRLINE))
            .flex()
            .items_center()
            .gap(px(tab_gap))
            .bg(bg)
            .relative()
            .when_some(assigned_width_px, |tab, width| tab.w(px(width)))
            .when(
                !is_renaming && !is_dragging && (!minimal_style || !is_active),
                |s| s.hover(|s| s.bg(hover_bg)),
            )
            .cursor_default()
            .opacity(if is_source { 0.5 } else { 1.0 })
            .when(
                is_drop_target && drop_zone == Some(TabDropZone::SwapWith),
                |s| s.border_2().border_color(colors::accent()),
            )
            .when(minimal_style, |tab| {
                tab.min_w(px(MINIMAL_TAB_MIN_WIDTH_PX))
                    .max_w(px(MINIMAL_TAB_MAX_WIDTH_PX))
                    .overflow_hidden()
            })
            // The rename field has no intrinsic width, so an intrinsically sized
            // tab collapses around it; hold the tab at its pre-rename width.
            .when_some(rename_min_width, |tab, width| tab.min_w(px(width)));

        tab_el = if top_only_radius {
            tab_el.rounded_t(px(radius))
        } else {
            tab_el.rounded(px(radius))
        };

        if add_border {
            tab_el = tab_el.border_1().border_color(colors::border());
        }
        if is_active && active_border_top {
            tab_el = tab_el
                .border_t(px(3.0))
                .border_color(colors::text_secondary());
        }
        if is_active && active_border_bottom {
            tab_el = tab_el
                .border_b(px(3.0))
                .border_color(colors::text_secondary());
        }
        if minimal_style {
            tab_el = tab_el.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(if is_active { 3.0 } else { 2.0 }))
                    .bg(minimal_line_color),
            );
        }

        let show_live_indicator = is_live;

        tab_el
            .on_hover({
                let tooltip = channel_tooltip.clone();
                let tooltip_source = channel_tooltip_source.clone();
                let title = title.clone();
                let entity = entity.clone();
                move |hovered, window, app| {
                    if *hovered {
                        if let Some(tooltip) =
                            entity.read(app).tab_tooltip(idx, &title, tooltip.as_ref())
                        {
                            show_text_tooltip(
                                window.window_handle(),
                                tooltip_source.to_string(),
                                tooltip,
                                window.mouse_position(),
                            );
                        }
                    } else {
                        clear_global_tooltip_if_source_in_window(
                            window.window_handle(),
                            &tooltip_source,
                        );
                    }
                }
            })
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    if this.tab_manager.hovered_idx != Some(idx) {
                        this.tab_manager.hovered_idx = Some(idx);
                        cx.notify();
                    }

                    if this.tab_manager.update_drag_position(
                        event.position,
                        event.pressed_button == Some(MouseButton::Left),
                    ) {
                        cx.notify();
                    }
                    if !this.tab_manager.drag_state.is_dragging {
                        return;
                    }
                    let source_idx = this.tab_manager.drag_state.source_idx;

                    let bounds = this.tab_manager.hit_areas.get(&idx).cloned();
                    if let Some(bounds) = bounds {
                        let zone = TabManager::calculate_tab_drop_zone(event.position, bounds);

                        let skip = match zone {
                            TabDropZone::InsertBefore => idx == source_idx || idx == source_idx + 1,
                            TabDropZone::InsertAfter => idx == source_idx || idx + 1 == source_idx,
                            TabDropZone::SwapWith => idx == source_idx,
                        };

                        if skip {
                            if this.tab_manager.drag_state.drop_target.is_some() {
                                this.tab_manager.drag_state.drop_target = None;
                                cx.notify();
                            }
                            return;
                        }

                        let new_target = TabDropTarget {
                            target_idx: idx,
                            zone,
                        };
                        let should_update = match &this.tab_manager.drag_state.drop_target {
                            Some(t) => t.target_idx != idx || t.zone != zone,
                            None => true,
                        };

                        if should_update {
                            this.tab_manager.drag_state.drop_target = Some(new_target);
                            cx.notify();
                        }
                    }
                }),
            )
            .when(has_unread && !minimal_style, |s| {
                s.child(
                    div()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded(Radii::full())
                        .bg(unread_dot_color),
                )
            })
            .when(show_live_indicator, |s| {
                let indicator_color = if minimal_style {
                    colors::signal_red()
                } else {
                    colors::signal_green()
                };
                let tooltip_source = live_indicator_tooltip_source.clone();
                let tooltip_text = live_indicator_tooltip.clone();
                let channel_tooltip_source = channel_tooltip_source.clone();
                let channel_tooltip = channel_tooltip.clone();
                let tooltip_title = title.clone();
                let tooltip_entity = entity.clone();
                s.child(
                    div()
                        .id(("live-indicator", idx))
                        .w(px(if minimal_style { 4.0 } else { 6.0 }))
                        .h(px(if minimal_style { 4.0 } else { 6.0 }))
                        .rounded(Radii::full())
                        .bg(indicator_color)
                        .when(minimal_style, |indicator| {
                            indicator
                                .absolute()
                                .top(px(if is_active { 4.0 } else { 5.0 }))
                                .right(px(3.0))
                        })
                        .on_hover(move |hovered, window, app| {
                            if *hovered {
                                clear_global_tooltip_if_source_in_window(
                                    window.window_handle(),
                                    &channel_tooltip_source,
                                );
                                show_text_tooltip(
                                    window.window_handle(),
                                    tooltip_source.to_string(),
                                    tooltip_text.to_string(),
                                    window.mouse_position(),
                                );
                            } else {
                                clear_global_tooltip_if_source_in_window(
                                    window.window_handle(),
                                    &tooltip_source,
                                );
                                let renderer = tooltip_entity.read(app);
                                let position = window.mouse_position();
                                let over_tab = renderer
                                    .tab_bounds(idx)
                                    .is_some_and(|bounds| bounds.contains(&position))
                                    && renderer
                                        .tab_manager
                                        .scroll_viewport_bounds
                                        .as_ref()
                                        .is_none_or(|bounds| bounds.contains(&position));
                                if let Some(channel_tooltip) = over_tab
                                    .then(|| {
                                        renderer.tab_tooltip(
                                            idx,
                                            &tooltip_title,
                                            channel_tooltip.as_ref(),
                                        )
                                    })
                                    .flatten()
                                {
                                    show_text_tooltip(
                                        window.window_handle(),
                                        channel_tooltip_source.to_string(),
                                        channel_tooltip.to_string(),
                                        window.mouse_position(),
                                    );
                                }
                            }
                        }),
                )
            })
            .when(!is_renaming, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if this.is_renaming() {
                            this.commit_rename(cx);
                        }
                        let close_button_press = this
                            .tab_manager
                            .is_close_button_press(&tab_id_for_dblclick, event.position);
                        if !close_button_press {
                            this.tab_manager.close_press = None;
                        }
                        if !close_button_press && this.tab_manager.check_tab_double_click(idx) {
                            this.start_rename(tab_id_for_dblclick.clone());
                            this.ensure_rename_input(window, cx);
                            this.tab_manager.drag_state =
                                crate::managers::tab_manager::TabDragState::default();
                            cx.notify();
                            return;
                        }

                        if !close_button_press {
                            app_state.update(cx, |state, cx| {
                                state.switch_tab(idx, cx);
                            });
                            let state = app_state.read(cx);
                            Self::save_tabs_if_allowed(state, "tab switch");
                            cx.emit(TabBarActiveSplitChanged);
                        }

                        let tab_order = app_state
                            .read(cx)
                            .tabs
                            .iter()
                            .map(|tab| tab.id.clone())
                            .collect();
                        this.tab_manager.begin_drag(
                            idx,
                            tab_id_for_dblclick.clone(),
                            tab_order,
                            event.position,
                        );
                        cx.notify();
                    }),
                )
            })
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.tab_manager.arm_close_press(
                        tab_id_for_middle.clone(),
                        MouseButton::Middle,
                        event.position,
                    );
                }),
            )
            .on_mouse_up_out(
                MouseButton::Middle,
                cx.listener(move |this, _event, _window, _cx| {
                    this.tab_manager
                        .disarm_close_press(&tab_id_for_middle_out, MouseButton::Middle);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, event: &MouseUpEvent, _window, cx| {
                    cx.stop_propagation();
                    if !this.tab_manager.take_completed_close(
                        &tab_id_for_middle_up,
                        MouseButton::Middle,
                        event.position,
                    ) {
                        return;
                    }
                    this.cancel_tab_drag();
                    cx.emit(crate::app::TabCloseRequest {
                        tab_id: tab_id_for_middle_up.clone(),
                    });
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.tab_manager.context_menu = crate::managers::tab_manager::TabContextMenu {
                        is_open: true,
                        tab_idx: idx,
                        tab_id: tab_id_for_ctx.clone(),
                        x: event.position.x.into(),
                        y: event.position.y.into(),
                    };
                    cx.notify();
                }),
            )
            .when(is_locked && !is_renaming, |el| {
                el.child(render_tab_lock_glyph(text_color))
            })
            .child(if is_renaming {
                let rename_input = rename_input.expect("active rename has an input state");
                let rename_input = div()
                    .id(("tab-rename-input", idx))
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        if event.keystroke.key == "escape" {
                            this.cancel_rename();
                            cx.stop_propagation();
                            cx.emit(TabBarActiveSplitChanged);
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Input::new(&rename_input)
                            .appearance(true)
                            .bordered(true)
                            .focus_bordered(true)
                            .accessibility_label("Rename tab"),
                    );
                rename_input.into_any_element()
            } else {
                div()
                    .relative()
                    .text_size(px(TAB_TITLE_FONT_SIZE_PX))
                    .text_color(text_color)
                    .when(minimal_style, |title| {
                        title
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_center()
                    })
                    .when(!minimal_style && assigned_width_px.is_some(), |title| {
                        title.flex_1().whitespace_nowrap().text_center()
                    })
                    .when(constrain_title, |title| {
                        title.min_w_0().overflow_hidden().text_ellipsis()
                    })
                    .child(title)
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let visible = bounds.intersect(&window.content_mask().bounds);
                                title_entity.update(cx, |renderer, _| {
                                    if title_width > f32::from(visible.size.width) + 0.5 {
                                        renderer.tab_manager.clipped_titles.insert(idx);
                                    } else {
                                        renderer.tab_manager.clipped_titles.remove(&idx);
                                    }
                                });
                            },
                            |_, _: (), _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .into_any_element()
            })
            .when(
                show_close_button
                    || minimal_style
                    || assigned_width_px.is_none()
                    || constrain_title,
                |el| {
                    // Layout only ever sees this 16 px slot, hovered or not.
                    // Taffy undersizes an intrinsically sized flex row when a
                    // non-shrinking item has a negative margin, which made a
                    // hovered tab collapse out from under the pointer, so the
                    // larger close hit area is positioned absolutely instead.
                    let slot = div().w(px(16.0)).h(px(16.0)).flex_shrink_0().relative();
                    if !show_close_button {
                        return el.child(slot);
                    }
                    let hover_bg = if minimal_style {
                        colors::text_primary().opacity(0.16)
                    } else {
                        colors::error().opacity(0.15)
                    };
                    let hover_text = if minimal_style {
                        colors::text_primary()
                    } else {
                        colors::signal_red()
                    };
                    el.child(
                        slot.child(
                            div()
                                .id(("tab-close", idx))
                                .role(Role::Button)
                                .aria_label("Close tab")
                                .on_a11y_action(accesskit::Action::Click, {
                                    let click = cx.listener(move |this, _: &(), _window, cx| {
                                        this.cancel_tab_drag();
                                        cx.emit(crate::app::TabCloseRequest {
                                            tab_id: tab_id_for_accessible_close.clone(),
                                        });
                                        cx.notify();
                                    });
                                    move |_, window, cx| click(&(), window, cx)
                                })
                                .on_hover(text_tooltip_on_hover(
                                    format!("tab-close-{idx}"),
                                    "Close tab".to_string(),
                                ))
                                // Extends into the existing gap and padding without changing layout.
                                .absolute()
                                .left(px(-4.0))
                                .top(px(-4.0))
                                .w(px(24.0))
                                .h(px(24.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(if minimal_style { px(0.0) } else { Radii::sm() })
                                .text_size(px(FontSize::XS))
                                .text_color(colors::text_muted())
                                .hover(move |s| s.bg(hover_bg).text_color(hover_text))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(
                                        move |this, event: &MouseDownEvent, _window, _cx| {
                                            if this.tab_manager.drag_state.is_dragging {
                                                return;
                                            }
                                            this.tab_manager.arm_close_press(
                                                tab_id_for_close.clone(),
                                                MouseButton::Left,
                                                event.position,
                                            );
                                        },
                                    ),
                                )
                                .on_mouse_up_out(
                                    MouseButton::Left,
                                    cx.listener(move |this, _event, _window, _cx| {
                                        this.tab_manager.disarm_close_press(
                                            &tab_id_for_close_out,
                                            MouseButton::Left,
                                        );
                                    }),
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseUpEvent, _window, cx| {
                                        if !this.tab_manager.take_completed_close(
                                            &tab_id_for_close_up,
                                            MouseButton::Left,
                                            event.position,
                                        ) {
                                            return;
                                        }
                                        this.cancel_tab_drag();
                                        cx.emit(crate::app::TabCloseRequest {
                                            tab_id: tab_id_for_close_up.clone(),
                                        });
                                        cx.notify();
                                    }),
                                )
                                .child(CLOSE_GLYPH),
                        ),
                    )
                },
            )
            .when(
                is_drop_target && drop_zone == Some(TabDropZone::InsertBefore),
                |el| {
                    el.child(
                        div()
                            .absolute()
                            .left(px(-2.0))
                            .top(px(6.0))
                            .bottom(px(6.0))
                            .w(px(3.0))
                            .rounded(Radii::xs())
                            .bg(colors::accent()),
                    )
                },
            )
            .when(
                is_drop_target && drop_zone == Some(TabDropZone::InsertAfter),
                |el| {
                    el.child(
                        div()
                            .absolute()
                            .right(px(-2.0))
                            .top(px(6.0))
                            .bottom(px(6.0))
                            .w(px(3.0))
                            .rounded(Radii::xs())
                            .bg(colors::accent()),
                    )
                },
            )
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        entity.update(cx, |renderer, _| {
                            renderer.tab_manager.hit_areas.insert(idx, bounds);
                        });
                    },
                    |_bounds, _: (), _window, _cx| {},
                )
                .absolute()
                .inset_0(),
            )
    }

    /// Add new tab button
    fn render_add_tab_button(
        &self,
        _app_state: Entity<AppState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let minimal_style = Self::effective_tab_theme() == backend::TabTheme::Minimal;

        div()
            .id("add-tab")
            .role(Role::Button)
            .aria_label("New tab")
            .w(px(if minimal_style {
                MINIMAL_TAB_HEIGHT_PX
            } else {
                BROWSER_ADD_TAB_CONTROL_WIDTH_PX
            }))
            .h(px(if minimal_style {
                MINIMAL_TAB_HEIGHT_PX
            } else {
                config::TAB_BAR_HEIGHT_PX
            }))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(colors::bg_hover()))
            .on_accessible_activate(cx.listener(move |_this, _event, _window, cx| {
                cx.stop_propagation();
                crate::backend::push_pending_open_new_tab();
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(FontSize::BASE))
                    .text_color(colors::text_muted())
                    .child("+"),
            )
    }

    /// Render the tab's right-click context menu overlay.
    ///
    /// Extracted from app.rs to reduce monolith size.
    pub fn render_context_menu_overlay(
        &mut self,
        app_state: Entity<AppState>,
        menu: &MenuHost,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.is_context_menu_open() {
            return div().into_any_element();
        }

        clear_global_tooltip();

        let (_, tab_id_ref, x, y) = self.context_menu_info();
        let tab_id = tab_id_ref.to_string();

        let target = {
            let state = app_state.read(cx);
            state.tabs.iter().enumerate().find_map(|(idx, tab)| {
                (tab.id == tab_id).then(|| {
                    (
                        idx,
                        tab.is_locked,
                        tab.mark_unread_on_new,
                        tab.valid_active_split_path(),
                        tab.channel_targets(),
                    )
                })
            })
        };
        let Some((idx, is_locked, mark_unread, active_split_path, channel_targets)) = target else {
            self.close_context_menu();
            cx.notify();
            return div().into_any_element();
        };
        self.tab_manager.context_menu.tab_idx = idx;

        let viewport = window.viewport_size();
        let menu_fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            220.0,
            tab_context_menu_preferred_height(channel_targets.len())
                .min(TAB_CONTEXT_MENU_MAX_HEIGHT_PX),
            8.0,
        );
        let x = crate::utils::clamp_overlay_axis(
            x,
            menu_fit.width,
            f32::from(viewport.width),
            menu_fit.margin_x,
        );
        let y = crate::utils::clamp_overlay_axis(
            y,
            menu_fit.height,
            f32::from(viewport.height),
            menu_fit.margin_y,
        );

        let tab_bar = cx.entity();
        let nav = &menu.nav;
        let mut actions: Vec<MenuAction> = Vec::new();
        let mut menu_items: Vec<AnyElement> = Vec::new();
        for item in TAB_CONTEXT_MENU_ITEMS {
            if item.is_divider {
                menu_items.push(menu_divider().into_any_element());
                continue;
            }
            let action = item.action;
            let tab_id = tab_id.clone();
            let app_state = app_state.clone();
            let tab_bar = tab_bar.clone();
            let toggle_state = TabManager::get_toggle_state(action, is_locked, mark_unread);
            let run: MenuAction = Rc::new(move |window: &mut Window, cx: &mut App| {
                tab_bar.update(cx, |this, cx| {
                    this.handle_tab_action(action, &tab_id, app_state.clone(), window, cx);
                    this.close_context_menu();
                    cx.notify();
                });
            });
            let index = actions.len();
            actions.push(run.clone());
            menu_items.push(
                menu_item(item.action, index, nav, run)
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(MenuMetrics::ITEM_TEXT))
                            .text_color(if item.is_destructive {
                                colors::signal_red()
                            } else {
                                colors::text_primary()
                            })
                            .child(item.label),
                    )
                    .when(item.is_toggle && toggle_state, |row| {
                        row.child(
                            div()
                                .text_size(px(MenuMetrics::ITEM_TEXT))
                                .text_color(colors::signal_green())
                                .child("✓"),
                        )
                    })
                    .into_any_element(),
            );
        }

        if channel_targets.len() > 1 {
            menu_items.push(menu_divider().into_any_element());
            menu_items.push(
                menu_header()
                    .id(("tab-channel-heading", idx))
                    .child("Switch to split")
                    .into_any_element(),
            );

            for (target_idx, (path, label)) in channel_targets.into_iter().enumerate() {
                let is_active_target = path == active_split_path;
                let app_state = app_state.clone();
                let tab_bar = tab_bar.clone();
                let tab_id = tab_id.clone();
                let run: MenuAction = Rc::new(move |_window: &mut Window, cx: &mut App| {
                    tab_bar.update(cx, |this, cx| {
                        let mut changed = false;
                        app_state.update(cx, |state, cx| {
                            let Some(idx) = state.tabs.iter().position(|tab| tab.id == tab_id)
                            else {
                                return;
                            };
                            changed = state.active_tab_idx != idx;
                            state.switch_tab(idx, cx);
                            changed |= state.tabs_mut()[idx].set_active_split_path(path.clone());
                        });
                        if changed {
                            let state = app_state.read(cx);
                            Self::save_tabs_if_allowed(state, "switch split from tab menu");
                            cx.emit(TabBarActiveSplitChanged);
                        }
                        this.close_context_menu();
                        cx.notify();
                    });
                });
                let index = actions.len();
                actions.push(run.clone());
                menu_items.push(
                    menu_item(
                        SharedString::from(format!("tab-channel-target-{idx}-{target_idx}")),
                        index,
                        nav,
                        run,
                    )
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(MenuMetrics::ITEM_TEXT))
                            .text_color(colors::text_primary())
                            .child(label),
                    )
                    .when(is_active_target, |item| {
                        item.child(
                            div()
                                .text_size(px(MenuMetrics::ITEM_TEXT))
                                .text_color(colors::signal_green())
                                .child("✓"),
                        )
                    })
                    .into_any_element(),
                );
            }
        }

        modal_backdrop_style()
            .id("tab-context-menu-overlay")
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    this.close_context_menu();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .child(
                menu_panel()
                    .id("tab-context-menu")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(menu_fit.width))
                    .max_h(px(menu_fit.height))
                    .overflow_y_scroll()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .menu_keyboard(&menu.focus, &menu.nav, actions, menu.close_handler())
                    .children(menu_items),
            )
            .into_any_element()
    }

    /// Render tab overflow dropdown overlay for dropdown mode
    ///
    /// Extracted from app.rs to reduce monolith size.
    pub fn render_overflow_dropdown_overlay(
        &mut self,
        app_state: Entity<AppState>,
        menu: &MenuHost,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.is_overflow_open() {
            return div().into_any_element();
        }

        clear_global_tooltip();

        if Self::effective_overflow_mode() != backend::TabOverflowMode::Dropdown {
            self.close_overflow();
            return div().into_any_element();
        }

        if app_state.read(cx).should_hide_tab_bar() {
            self.close_overflow();
            return div().into_any_element();
        }

        let tab_bar_width = self.bar_width();

        let tab_theme = Self::effective_tab_theme();
        let metrics = Self::tab_layout_metrics(&tab_theme, &backend::TabOverflowMode::Dropdown);
        let overflow_button_width = DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX;

        let overflow_rows: Vec<(
            usize,
            SharedString,
            bool,
            Hsla,
            crate::state::TabHighlightState,
        )> = {
            let state = app_state.read(cx);
            let active_idx = state.active_tab_idx;
            let tabs = state
                .visible_tab_indices()
                .into_iter()
                .filter_map(|idx| state.tabs.get(idx).map(|tab| (idx, tab)))
                .collect::<Vec<_>>();
            self.sync_tab_models(&tabs, &state.stream_info, window);
            let visible_positions = self
                .calculate_dropdown_layout(
                    &tabs,
                    active_idx,
                    &tab_theme,
                    tab_bar_width,
                    metrics,
                    overflow_button_width,
                )
                .visible_positions;

            let overflow_tabs: Vec<(usize, &TabState)> = tabs
                .iter()
                .enumerate()
                .filter(|(position, _)| !visible_positions.contains(position))
                .map(|(_, tab)| *tab)
                .collect();

            overflow_tabs
                .into_iter()
                .map(|(idx, tab)| {
                    let title = self
                        .tab_models
                        .get(&tab.id)
                        .map(|model| model.title.clone())
                        .unwrap_or_else(|| SharedString::from(tab.display_title()));
                    let is_active = idx == active_idx;
                    let tab_highlight = if is_active {
                        crate::state::TabHighlightState::None
                    } else {
                        tab.highlight_state
                    };
                    let visual_state = match tab_highlight {
                        crate::state::TabHighlightState::None if !is_active && tab.has_unread => {
                            crate::state::TabHighlightState::NewMessage
                        }
                        state => state,
                    };
                    let text_color =
                        if is_active || visual_state != crate::state::TabHighlightState::None {
                            colors::text_primary()
                        } else {
                            colors::text_tertiary()
                        };
                    (idx, title, is_active, text_color, visual_state)
                })
                .collect()
        };

        if overflow_rows.is_empty() {
            self.close_overflow();
            return div().into_any_element();
        }

        let viewport = window.viewport_size();
        let dropdown_fit = crate::utils::fit_overlay_to_viewport(viewport, 180.0, 400.0, 8.0);
        let dropdown_width = dropdown_fit.width;
        let (x, y) = if let Some(bounds) = self.overflow_button_bounds() {
            let bx: f32 = bounds.origin.x.into();
            let by: f32 = bounds.origin.y.into();
            let height: f32 = bounds.size.height.into();
            let constrained_x = crate::utils::clamp_overlay_axis(
                bx,
                dropdown_width,
                f32::from(viewport.width),
                dropdown_fit.margin_x,
            );
            (constrained_x, by + height)
        } else {
            (
                0.0,
                backend::settings::titlebar_height_px() + config::TAB_BAR_HEIGHT_PX,
            )
        };

        let tab_bar = cx.entity();
        let mut actions: Vec<MenuAction> = Vec::with_capacity(overflow_rows.len());
        let overflow_items: Vec<_> = overflow_rows
            .into_iter()
            .enumerate()
            .map(
                |(position, (idx, title, is_active, text_color, visual_state))| {
                    let app_state = app_state.clone();
                    let tab_bar = tab_bar.clone();
                    let run: MenuAction = Rc::new(move |_window: &mut Window, cx: &mut App| {
                        tab_bar.update(cx, |this, cx| {
                            app_state.update(cx, |state, cx| {
                                state.switch_tab(idx, cx);
                            });
                            this.close_overflow();
                            let state = app_state.read(cx);
                            Self::save_tabs_if_allowed(state, "overflow switch");
                            cx.emit(TabBarActiveSplitChanged);
                            cx.notify();
                        });
                    });
                    actions.push(run.clone());
                    let state_color = if is_active {
                        colors::text_secondary()
                    } else {
                        match visual_state {
                            crate::state::TabHighlightState::Highlighted => {
                                colors::mention_border()
                            }
                            crate::state::TabHighlightState::NewMessage => colors::signal_yellow(),
                            crate::state::TabHighlightState::None => colors::border_subtle(),
                        }
                    };

                    menu_item(("overflow-tab", idx), position, &menu.nav, run)
                        .border_l(px(if is_active { 3.0 } else { 2.0 }))
                        .border_color(state_color)
                        .when(is_active && !menu.nav.is_highlighted(position), |s| {
                            s.bg(colors::bg_plate())
                        })
                        .child(
                            div()
                                .text_size(px(MenuMetrics::ITEM_TEXT))
                                .text_color(text_color)
                                .child(title),
                        )
                        .into_any_element()
                },
            )
            .collect();

        let y = crate::utils::clamp_overlay_axis(
            y,
            dropdown_fit.height,
            f32::from(viewport.height),
            dropdown_fit.margin_y,
        );

        modal_backdrop_style()
            .id("tab-overflow-dropdown-overlay")
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    this.close_overflow();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .child(
                menu_panel()
                    .id("tab-overflow-dropdown")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(dropdown_width))
                    .max_h(px(dropdown_fit.height))
                    .overflow_y_scroll()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .menu_keyboard(&menu.focus, &menu.nav, actions, menu.close_handler())
                    .children(overflow_items),
            )
            .into_any_element()
    }

    /// Handle a tab context menu action.
    ///
    /// Extracted from app.rs to reduce monolith size.
    pub fn handle_tab_action(
        &mut self,
        action: &str,
        tab_id: &str,
        app_state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = app_state
            .read(cx)
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
        else {
            self.cancel_tab_drag();
            return;
        };
        self.cancel_tab_drag();

        match action {
            "rename" => {
                self.start_rename(tab_id.to_string());
                self.ensure_rename_input(window, cx);
                cx.notify();
            }
            "duplicate" => {
                app_state.update(cx, |state, _cx| {
                    state.duplicate_tab(idx);
                });
            }
            "close" => {
                cx.emit(crate::app::TabCloseRequest {
                    tab_id: tab_id.to_string(),
                });
                return;
            }
            "close_others" | "close_right" => {
                let to_right = action == "close_right";
                let tab_count = app_state.read(cx).tabs.len();
                let count = if to_right {
                    tab_count.saturating_sub(idx + 1)
                } else {
                    tab_count.saturating_sub(1)
                };
                if count > 0 {
                    crate::backend::push_ui_action(crate::backend::UiAction::ConfirmCloseTabs {
                        tab_id: tab_id.to_string(),
                        to_right,
                        count,
                    });
                }
            }
            "reopen_closed" => {
                let reopened = app_state.update(cx, |state, cx| state.reopen_closed_tabs(cx));
                if reopened == 0 {
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                        message: "No closed tab".to_string(),
                        detail: Some("Close a tab before using Reopen Closed Tab.".to_string()),
                    });
                }
            }
            "toggle_unread" => {
                app_state.update(cx, |state, _cx| {
                    if idx < state.tabs.len() {
                        let current = state.tabs[idx].mark_unread_on_new;
                        state.tabs_mut()[idx].mark_unread_on_new = !current;
                    }
                });
            }
            "toggle_lock" => {
                app_state.update(cx, |state, _cx| {
                    if idx < state.tabs.len() {
                        let current = state.tabs[idx].is_locked;
                        state.tabs_mut()[idx].is_locked = !current;
                    }
                });
            }
            "sort_alpha" => {
                app_state.update(cx, |state, _cx| {
                    state.sort_tabs_alphabetically();
                });
            }
            _ => {
                crate::log_ui!(warn, "Unknown tab action: {}", action);
            }
        }
        if action == "reopen_closed" {
            cx.emit(TabBarActiveSplitChanged);
        }
        let state = app_state.read(cx);
        Self::save_tabs_if_allowed(state, "context action");
    }
}

impl Render for TabBarRenderer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rendered_fingerprint = Some(self.frame_fingerprint);
        if !self.is_visible {
            return div().into_any_element();
        }

        let app_state = self.app_state.clone();
        let (tabs, active_idx, visible_indices) = {
            let state = app_state.read(cx);
            (
                state.tabs.clone(),
                state.active_tab_idx,
                state.visible_tab_indices(),
            )
        };
        let visible_tabs = visible_indices
            .into_iter()
            .filter_map(|idx| tabs.get(idx).map(|tab| (idx, tab)))
            .collect::<Vec<_>>();
        self.tab_manager.hit_areas.clear();
        self.tab_manager.clipped_titles.clear();
        self.render_tab_bar_fast(&visible_tabs, active_idx, app_state, cx)
    }
}

#[cfg(test)]
mod browser_dropdown_layout_tests {
    use super::*;
    use core::prelude::v1::test;

    const ADD_WIDTH: f32 = BROWSER_ADD_TAB_CONTROL_WIDTH_PX;
    const OVERFLOW_WIDTH: f32 = DROPDOWN_OVERFLOW_CONTROL_WIDTH_PX;

    fn solve(widths: &[f32], active: Option<usize>, bar_width: f32) -> Vec<usize> {
        solve_natural_dropdown_tabs(widths, active, bar_width, 0.0, ADD_WIDTH, OVERFLOW_WIDTH)
    }

    #[test]
    fn keeps_every_tab_at_full_width_while_the_set_fits() {
        assert_eq!(solve(&[52.0, 118.0, 84.0], Some(0), 290.0), vec![0, 1, 2]);
        assert_eq!(
            solve(&[52.0, 118.0, 84.0], None, f32::INFINITY),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn overflows_whole_tabs_instead_of_shrinking_titles() {
        assert_eq!(solve(&[52.0, 118.0, 84.0], Some(0), 289.0), vec![0, 1]);
        assert_eq!(solve(&[52.0, 118.0, 84.0], Some(0), 200.0), vec![0]);
    }

    #[test]
    fn active_tab_stays_visible_in_source_order_without_overfilling() {
        let widths = [60.0, 60.0, 60.0, 120.0, 60.0];
        assert_eq!(solve(&widths, Some(3), 250.0), vec![0, 3]);
        assert_eq!(solve(&widths, Some(1), 250.0), vec![0, 1, 2]);
    }

    #[test]
    fn long_titles_keep_full_width_when_they_fit() {
        let natural = 30.0 + browser_tab_chrome_width(&backend::TabTheme::Chrome, false, false);
        let long = 260.0 + browser_tab_chrome_width(&backend::TabTheme::Chrome, false, false);
        assert!(long > 180.0);
        assert_eq!(solve(&[natural, long], Some(1), 420.0), vec![0, 1]);
        assert_eq!(solve(&[natural, long], Some(1), 400.0), vec![1]);
    }

    #[test]
    fn browser_chrome_includes_padding_indicators_and_close_slot() {
        let live_chrome = browser_tab_chrome_width(&backend::TabTheme::Chrome, true, false);
        assert_eq!(live_chrome, 54.0);
        assert_eq!(
            browser_tab_chrome_width(&backend::TabTheme::Chrome, true, true),
            66.0
        );
        for theme in [
            backend::TabTheme::Firefox,
            backend::TabTheme::Edge,
            backend::TabTheme::Square,
        ] {
            assert_eq!(browser_tab_chrome_width(&theme, true, false), 60.0);
        }
        assert_eq!(
            browser_tab_chrome_width(&backend::TabTheme::Arc, true, false),
            64.0
        );
    }

    #[test]
    fn startup_and_extreme_widths_do_not_publish_false_or_empty_geometry() {
        assert_eq!(solve(&[80.0, 80.0, 80.0], Some(2), 0.0), vec![0, 1, 2]);
        assert_eq!(solve(&[120.0, 80.0], Some(1), 70.0), vec![1]);
        assert_eq!(solve(&[120.0, 80.0], None, 70.0), vec![0]);
        assert!(solve(&[], Some(0), 300.0).is_empty());
    }
}

/// A small padlock drawn from boxes, marking a tab whose split layout is locked.
fn render_tab_lock_glyph(color: Hsla) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(8.0))
        .h(px(10.0))
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w(px(6.0))
                .h(px(5.0))
                .border_1()
                .border_b_0()
                .border_color(color)
                .rounded_t(Radii::sm()),
        )
        .child(div().w(px(8.0)).h(px(5.0)).rounded(Radii::xs()).bg(color))
}
