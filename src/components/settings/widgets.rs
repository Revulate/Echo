//! Reusable settings UI widgets
//!
//! Provides consistent, accessible UI components for settings:
//! - Toggle switches
//! - Sliders with labels
//! - Select/dropdown menus
//! - Buttons (primary, secondary, destructive)
//! - Text inputs
//! - Radio button groups
//! - Color pickers
#![allow(clippy::too_many_arguments)]
//! - Setting rows with label + description

pub use crate::components::ui::{focus_ring, is_button_activation_key as is_activation_key};

use super::settings_navigation::SettingDescriptor;
use super::settings_results::target_scroll_offset;
use crate::backend::lock_utils::MutexExt;
use crate::components::ui::ButtonExt;
use crate::components::ui::PressToClick;
use crate::components::ui::input::{Input, InputEvent, InputState};
use crate::components::ui::menu::{MenuAction, MenuKeyboard, MenuNav, menu_item, menu_panel};
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Sizes, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};

/// Spacing, control, and type scale for settings surfaces only.
///
/// Settings deliberately run denser than chat and the Zed-derived overlays, so
/// the scale lives here instead of shrinking the global GPUI spacing tokens
/// (which back the message list, tooltips, and every other surface).
///
/// Two rules hold everywhere in this scale:
/// - an interactive row is at least [`SettingsMetrics::CONTROL_MIN_HEIGHT`]
///   tall even when its painted control (a toggle track, an icon) is smaller;
/// - no ordinary settings text is smaller than [`SettingsMetrics::HELPER_TEXT`].
///
/// Spacing and text values are expressed in the shared `theme::tokens`
/// scales (`Spacing`, `FontSize`); the remaining literals are component sizes.
pub struct SettingsMetrics;

impl SettingsMetrics {
    /// Shell top bar (title + global search + window actions).
    ///
    /// The content header that names the selected destination is part of the
    /// same scale and lives in [`crate::config::SETTINGS_CONTENT_HEADER_HEIGHT_PX`],
    /// where the rest of the settings-window dimensions already are.
    pub const TOP_BAR_HEIGHT: f32 = 40.0;
    /// Inset around the scrolling content pane.
    pub const CONTENT_PADDING: f32 = Spacing::LG;
    /// Keep descriptions and trailing controls within one comfortable reading column.
    pub const CONTENT_MAX_WIDTH: f32 = 800.0;
    /// Shared search field height.
    pub const SEARCH_FIELD_HEIGHT: f32 = 28.0;
    /// Gap between a group title and its card, and between stacked groups.
    pub const GROUP_GAP: f32 = Spacing::SM_PLUS;
    /// Inset inside a settings card.
    pub const CARD_PADDING: f32 = Spacing::XS_PLUS;
    /// Vertical padding per side of a plain setting row.
    pub const ROW_PADDING_Y: f32 = Spacing::XS;
    /// Gap between a label and its description/helper line.
    pub const LABEL_GAP: f32 = Spacing::XXS;
    /// Painted toggle track width.
    pub const TOGGLE_TRACK_WIDTH: f32 = 32.0;
    /// Painted toggle track height.
    pub const TOGGLE_TRACK_HEIGHT: f32 = 18.0;
    /// Toggle pointer target height (larger than the painted track).
    pub const TOGGLE_TARGET_HEIGHT: f32 = 28.0;
    /// Text-input horizontal padding.
    pub const INPUT_PADDING_X: f32 = Spacing::SM;
    /// Text-input minimum height.
    pub const INPUT_MIN_HEIGHT: f32 = 30.0;
    /// Compact (list-row) button horizontal padding.
    pub const COMPACT_BUTTON_PADDING_X: f32 = Spacing::SM;
    /// Compact (list-row) button vertical padding.
    pub const COMPACT_BUTTON_PADDING_Y: f32 = Spacing::XS;
    /// Standalone action button horizontal padding.
    pub const ACTION_BUTTON_PADDING_X: f32 = Spacing::SM_PLUS;
    /// Standalone action button vertical padding.
    pub const ACTION_BUTTON_PADDING_Y: f32 = Spacing::XS;
    /// Segmented (button-group) option horizontal padding.
    pub const SEGMENTED_PADDING_X: f32 = Spacing::SM;
    /// Segmented (button-group) option vertical padding.
    pub const SEGMENTED_PADDING_Y: f32 = Spacing::XS;
    /// Minimum height of any interactive settings row or control.
    pub const CONTROL_MIN_HEIGHT: f32 = Sizes::BUTTON_SM;

    /// Fixed content header carrying the live `Page › Anchor` breadcrumb.
    pub const CONTENT_HEADER_HEIGHT: f32 = crate::config::SETTINGS_CONTENT_HEADER_HEIGHT_PX;
    /// Minimum height of a subsection (level-two) heading block.
    pub const SUBSECTION_HEADER_MIN_HEIGHT: f32 = 32.0;
    /// Width of the solid accent rail beside a subsection heading.
    pub const SUBSECTION_RAIL_WIDTH: f32 = 2.0;
    /// Vertical breathing room above a subsection heading, so an anchor jump
    /// does not land the heading flush against the previous block.
    pub const SUBSECTION_HEADER_GAP: f32 = Spacing::SM_PLUS;

    /// Flex basis of a split row's text column.
    pub const ROW_TEXT_BASIS: f32 = 280.0;
    /// Gap between the text and control columns of a split row.
    pub const ROW_COLUMN_GAP: f32 = Spacing::LG;

    /// Dimensions for the additive calm row layout.
    pub const CALM_ROW_MIN_HEIGHT: f32 = 48.0;
    pub const CALM_CONTROL_WIDTH: f32 = 200.0;
    pub const CALM_SLIDER_CONTROL_WIDTH: f32 = 360.0;
    pub const CALM_COLOR_CONTROL_WIDTH: f32 = 260.0;
    pub const COLOR_EDITOR_MIN_WIDTH: f32 = 180.0;
    pub const CALM_LABEL_TEXT: f32 = FontSize::MD;
    pub const CALM_HELPER_TEXT: f32 = FontSize::SM;

    /// Row height in the settings navigation tree.
    pub const TREE_ROW_HEIGHT: f32 = 28.0;
    /// Indentation applied per settings navigation tree level.
    pub const TREE_INDENT: f32 = Spacing::MD;

    /// Settings/page title.
    pub const TITLE_TEXT: f32 = FontSize::LG;
    /// Content section title.
    pub const SECTION_TITLE_TEXT: f32 = FontSize::MD;
    /// Group title, setting label, and control value.
    pub const LABEL_TEXT: f32 = FontSize::BASE;
    /// Description and helper text — the smallest permitted settings text.
    pub const HELPER_TEXT: f32 = FontSize::XS;
}

/// Make a settings control a keyboard tab stop with the shared focus ring.
///
/// `focus_visible` is GPUI's `:focus-visible` equivalent, so the ring appears
/// for keyboard traversal and stays out of the way of pointer interaction.
fn keyboard_control<T: 'static>(
    element: Stateful<Div>,
    surface: Hsla,
    cx: &mut Context<T>,
    focus: Option<FocusHandle>,
) -> Stateful<Div> {
    let focus = focus.unwrap_or_else(|| {
        let id = Element::id(&element)
            .map(|id| format!("control-{id:?}"))
            .unwrap_or_else(|| "control".to_string());
        control_focus(&id, cx)
    });
    let ring = focus_ring(surface);
    crate::components::ui::accessibility::accessible_focus(element, focus)
        .tab_index(0)
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .focus_visible(move |style| style.shadow(ring))
}

pub(crate) fn settings_keyboard_control<T: 'static>(
    element: Stateful<Div>,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    keyboard_control(element, colors::settings_group_surface(), cx, None)
}

pub(crate) fn settings_keyboard_control_if_enabled<T: 'static>(
    element: Stateful<Div>,
    enabled: bool,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let id = Element::id(&element)
        .map(|id| format!("control-{id:?}"))
        .unwrap_or_else(|| "control".to_string());
    let focus = control_focus(&id, cx).tab_stop(enabled);
    if enabled {
        keyboard_control(element, colors::settings_group_surface(), cx, Some(focus))
    } else {
        // Retain the same focused handle at a bound, but skip it on the next
        // traversal. Removing it would restart Tab at the window's beginning.
        element.track_focus(&focus)
    }
}

pub(crate) fn settings_keyboard_control_with_focus<T: 'static>(
    element: Stateful<Div>,
    cx: &mut Context<T>,
    focus: FocusHandle,
) -> Stateful<Div> {
    keyboard_control(element, colors::settings_group_surface(), cx, Some(focus))
}

/// Stable, human-readable element-id fragment derived from a label.
///
/// Widget helpers that take a label but no id derive their id (and therefore
/// their focus slot) from it, matching [`action_button`].
pub fn control_slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut pending_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
    }
    slug
}

thread_local! {
    /// Labels of the settings the active search matched on the page being
    /// built. Rows with one of these labels mark it like a find-in-page hit.
    static SEARCH_MATCH_LABELS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// Set the labels the settings page currently being built should mark.
pub(crate) fn set_search_match_labels(labels: Vec<&'static str>) {
    SEARCH_MATCH_LABELS.with(|current| *current.borrow_mut() = labels);
}

fn mark_search_match(label_text: Div, label: &str) -> Div {
    let matched = SEARCH_MATCH_LABELS.with(|labels| labels.borrow().contains(&label));
    label_text.when(matched, |text| {
        text.self_start()
            .px(px(Spacing::XS))
            .mx(px(-Spacing::XS))
            .rounded(Radii::sm())
            .bg(colors::bg_hover())
    })
}

struct SettingTargetContext {
    scroll: ScrollHandle,
    matched: Vec<&'static str>,
    selected: Option<&'static str>,
    anchors: HashMap<&'static str, SettingRowAnchor>,
}

#[derive(Clone)]
pub(crate) struct SettingRowAnchor {
    scroll: ScrollHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl SettingRowAnchor {
    pub(crate) fn scroll_to(&self, window: &mut Window) -> bool {
        let Some(bounds) = self.bounds.get() else {
            return false;
        };
        let offset = self.scroll.offset();
        let viewport = self.scroll.bounds();
        let Some(y) = target_scroll_offset(
            viewport.top().into(),
            viewport.size.height.into(),
            bounds.top().into(),
            bounds.size.height.into(),
            offset.y.into(),
            self.scroll.max_offset().y.into(),
            Spacing::XS,
        ) else {
            return false;
        };
        self.scroll.set_offset(point(offset.x, px(y)));
        window.refresh();
        true
    }
}

thread_local! {
    static SETTING_TARGET_CONTEXT: RefCell<Option<SettingTargetContext>> = const { RefCell::new(None) };
}

/// Render-local capture restored even if a section renderer unwinds.
pub(crate) struct SettingTargetCapture(Option<SettingTargetContext>);

impl SettingTargetCapture {
    pub(crate) fn new(
        scroll: ScrollHandle,
        matched: Vec<&'static str>,
        selected: Option<&'static str>,
    ) -> Self {
        Self(SETTING_TARGET_CONTEXT.with(|current| {
            current.replace(Some(SettingTargetContext {
                scroll,
                matched,
                selected,
                anchors: HashMap::new(),
            }))
        }))
    }

    pub(crate) fn anchor(&self, id: &str) -> Option<SettingRowAnchor> {
        SETTING_TARGET_CONTEXT.with(|current| {
            current
                .borrow()
                .as_ref()
                .and_then(|context| context.anchors.get(id).cloned())
        })
    }
}

impl Drop for SettingTargetCapture {
    fn drop(&mut self) {
        SETTING_TARGET_CONTEXT.with(|current| *current.borrow_mut() = self.0.take());
    }
}

/// Exact row identity without an extra layout container around the control.
pub fn setting_target<C: IntoElement>(setting: SettingDescriptor, body: C) -> SettingTargetElement {
    let (anchor, matched, selected) = SETTING_TARGET_CONTEXT.with(|current| {
        let mut current = current.borrow_mut();
        let Some(context) = current.as_mut() else {
            return (None, false, false);
        };
        let anchor = SettingRowAnchor {
            scroll: context.scroll.clone(),
            bounds: Rc::new(Cell::new(None)),
        };
        debug_assert!(
            !context.anchors.contains_key(setting.id),
            "duplicate setting row {}",
            setting.id
        );
        context
            .anchors
            .entry(setting.id)
            .or_insert_with(|| anchor.clone());
        (
            Some(anchor),
            context.matched.contains(&setting.id),
            context.selected == Some(setting.id),
        )
    });
    SettingTargetElement {
        setting,
        body: body.into_any_element(),
        anchor,
        matched,
        selected,
    }
}

pub struct SettingTargetElement {
    setting: SettingDescriptor,
    body: AnyElement,
    anchor: Option<SettingRowAnchor>,
    matched: bool,
    selected: bool,
}

impl IntoElement for SettingTargetElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SettingTargetElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.setting.id.into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.body.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(anchor) = &self.anchor {
            anchor.bounds.set(Some(Bounds::new(
                bounds.origin - window.element_offset(),
                bounds.size,
            )));
        }
        self.body.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.body.paint(window, cx);
        if self.matched {
            let width = if self.selected { px(3.0) } else { px(1.0) };
            let height = bounds
                .size
                .height
                .min(px(SettingsMetrics::CALM_ROW_MIN_HEIGHT));
            let color = if self.selected {
                colors::text_secondary()
            } else {
                colors::settings_group_rule(colors::bg_surface())
            };
            window.paint_quad(fill(Bounds::new(bounds.origin, size(width, height)), color));
        }
    }
}

fn row_text_column(label: &str, description: Option<&str>) -> Div {
    div()
        .flex_grow(1.0)
        .flex_shrink(1.0)
        .flex_basis(px(SettingsMetrics::ROW_TEXT_BASIS))
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::LABEL_GAP))
        .child(mark_search_match(
            div()
                .text_size(px(SettingsMetrics::LABEL_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child(label.to_string()),
            label,
        ))
        .when_some(description, |column, description| {
            column.child(
                div()
                    .text_size(px(SettingsMetrics::HELPER_TEXT))
                    .text_color(colors::text_secondary())
                    .child(description.to_string()),
            )
        })
}

static COLOR_EDITOR_STATE: Mutex<Option<HashMap<String, ColorEditorState>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorEditorField {
    Value,
}

#[derive(Clone)]
struct ColorEditorState {
    persisted_value: Option<String>,
    draft: ColorValue,
    is_clear: bool,
    expanded: bool,
    /// Mirror of the color-code editor's real focus, kept so a persisted change
    /// made elsewhere cannot overwrite text the user is still editing.
    active_field: Option<ColorEditorField>,
    value_text: String,
    dirty: bool,
    invalid: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ColorValue {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

impl ColorValue {
    const OPAQUE_FALLBACK: Self = Self {
        red: 124,
        green: 58,
        blue: 237,
        alpha: 255,
    };

    fn from_persisted(value: &str, mode: ColorAlphaMode) -> Option<Self> {
        let (red, green, blue, alpha) = crate::backend::settings::hex_to_rgba(value)?;
        Some(Self {
            red,
            green,
            blue,
            alpha: alpha.unwrap_or_else(|| mode.default_alpha()),
        })
    }

    fn from_text(value: &str, mode: ColorAlphaMode) -> Option<Self> {
        let value = value.trim();
        if let Some(parsed) = parse_css_rgba(value) {
            return Some(Self {
                alpha: if mode.allows_alpha() {
                    parsed.alpha
                } else {
                    255
                },
                ..parsed
            });
        }

        Self::from_persisted(value, mode).map(|mut parsed| {
            if !mode.allows_alpha() {
                parsed.alpha = 255;
            }
            parsed
        })
    }

    fn to_hsla(self) -> Hsla {
        let (hue, saturation, value) = self.to_hsv();
        hsv_to_hsla(hue, saturation, value, self.alpha)
    }

    fn to_hsv(self) -> (f32, f32, f32) {
        let red = self.red as f32 / 255.0;
        let green = self.green as f32 / 255.0;
        let blue = self.blue as f32 / 255.0;
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        let delta = max - min;

        let hue = if delta <= f32::EPSILON {
            0.0
        } else if (max - red).abs() <= f32::EPSILON {
            ((green - blue) / delta).rem_euclid(6.0) / 6.0
        } else if (max - green).abs() <= f32::EPSILON {
            ((blue - red) / delta + 2.0) / 6.0
        } else {
            ((red - green) / delta + 4.0) / 6.0
        };

        let saturation = if max <= f32::EPSILON {
            0.0
        } else {
            delta / max
        };
        (hue, saturation, max)
    }

    fn from_hsv(hue: f32, saturation: f32, value: f32, alpha: u8) -> Self {
        let hue = hue.rem_euclid(1.0) * 6.0;
        let saturation = saturation.clamp(0.0, 1.0);
        let value = value.clamp(0.0, 1.0);
        let chroma = value * saturation;
        let secondary = chroma * (1.0 - ((hue.rem_euclid(2.0)) - 1.0).abs());
        let match_value = value - chroma;
        let (red, green, blue) = match hue.floor() as i32 {
            0 => (chroma, secondary, 0.0),
            1 => (secondary, chroma, 0.0),
            2 => (0.0, chroma, secondary),
            3 => (0.0, secondary, chroma),
            4 => (secondary, 0.0, chroma),
            _ => (chroma, 0.0, secondary),
        };

        Self {
            red: ((red + match_value) * 255.0).round() as u8,
            green: ((green + match_value) * 255.0).round() as u8,
            blue: ((blue + match_value) * 255.0).round() as u8,
            alpha,
        }
    }

    fn to_hex(self, mode: ColorAlphaMode) -> String {
        if mode.allows_alpha() {
            format!(
                "#{:02X}{:02X}{:02X}{:02X}",
                self.alpha, self.red, self.green, self.blue
            )
        } else {
            format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
        }
    }

    fn to_rgb_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }
}

/// Whether a color target consumes an alpha channel.
///
/// Theme palettes and accent resolution are intentionally opaque today. In
/// contrast, message highlights honor `#AARRGGBB` values, so their picker
/// exposes a real alpha control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorAlphaMode {
    Opaque,
    Highlight { default_alpha: u8 },
}

pub fn setting_colors_match(left: &str, right: &str, mode: ColorAlphaMode) -> bool {
    match (
        ColorValue::from_persisted(left.trim(), mode),
        ColorValue::from_persisted(right.trim(), mode),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => left.trim().eq_ignore_ascii_case(right.trim()),
    }
}

impl ColorAlphaMode {
    fn allows_alpha(self) -> bool {
        matches!(self, Self::Highlight { .. })
    }

    fn default_alpha(self) -> u8 {
        match self {
            Self::Opaque => 255,
            Self::Highlight { default_alpha } => default_alpha,
        }
    }
}

/// Options for the shared Settings color editor.
pub struct ColorEditorOptions<'a> {
    pub alpha_mode: ColorAlphaMode,
    pub presets: &'a [(&'a str, &'a str)],
    pub allow_clear: bool,
    pub default_value: Option<&'a str>,
}

/// Marker struct for intensity slider drag state
#[derive(Clone)]
pub struct IntensitySliderDrag {
    pub slider_id: String,
}

impl Render for IntensitySliderDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

/// Marker struct for generic slider drag state
#[derive(Clone)]
pub struct SliderDrag {
    pub slider_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorControlKind {
    Spectrum,
    Hue,
    Opacity,
}

#[derive(Clone)]
struct ColorControlDrag {
    editor_key: String,
    control: ColorControlKind,
}

impl Render for ColorControlDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

impl Render for SliderDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

/// A setting row that spans full width (for larger controls).
///
/// Full-width editors always stack, so this row never splits into columns
/// regardless of the content width.
pub fn setting_row_full<C: IntoElement>(
    label: &str,
    description: Option<&str>,
    control: C,
) -> impl IntoElement + use<C> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::LABEL_GAP))
        .py(px(SettingsMetrics::ROW_PADDING_Y))
        .child(row_text_column(label, description).flex_none())
        .child(control)
}

/// Label, optional short help, trailing control and an independent reset slot.
/// Use `stacked` at compact widths; the row itself never activates its children.
pub fn setting_row<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    control: C,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
) -> Stateful<Div> {
    setting_row_with_control_width(
        id,
        label,
        help,
        control,
        modified,
        reset,
        stacked,
        SettingsMetrics::CALM_CONTROL_WIDTH,
    )
}

pub fn setting_slider_row<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    control: C,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
) -> Stateful<Div> {
    setting_row_with_control_width(
        id,
        label,
        help,
        control,
        modified,
        reset,
        stacked,
        SettingsMetrics::CALM_SLIDER_CONTROL_WIDTH,
    )
}

pub fn setting_float_modified(value: f32, default: f32) -> bool {
    (value - default).abs() >= 0.0001
}

/// A compact path row: actions align with the label, and the path occupies one
/// full-width line. The prefix truncates while the final component remains
/// visible; accessibility and Copy retain the complete path.
pub fn settings_path_row<C: IntoElement>(
    id: &str,
    label: &str,
    path: &str,
    actions: C,
    stacked: bool,
) -> impl IntoElement + use<C> {
    let chars: Vec<char> = path.chars().collect();
    let split = chars.len().saturating_sub(24);
    let (prefix, suffix) = if chars.len() > 36 {
        (
            chars[..split].iter().collect::<String>(),
            chars[split..].iter().collect::<String>(),
        )
    } else {
        (path.to_string(), String::new())
    };
    div()
        .id(SharedString::from(format!("{id}-path-row")))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(setting_row_layout(
            id.to_string(),
            label,
            None,
            crate::components::ui::accessibility::described_control(actions, label, Some(path)),
            false,
            None,
            stacked,
            SettingsMetrics::CALM_CONTROL_WIDTH,
            !stacked,
            true,
        ))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                .text_color(colors::text_secondary())
                .child(div().min_w_0().flex_shrink_1().truncate().child(prefix))
                .child(div().flex_shrink_0().child(suffix)),
        )
}

pub fn setting_row_with_control_width<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    control: C,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
    control_width: f32,
) -> Stateful<Div> {
    setting_row_layout(
        id,
        label,
        help,
        control,
        modified,
        reset,
        stacked,
        control_width,
        false,
        true,
    )
}

/// Small controls stay beside the label at narrow widths; help uses full width.
pub fn setting_compact_row<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    control: C,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
) -> Stateful<Div> {
    setting_row_layout(
        id,
        label,
        help,
        control,
        modified,
        reset,
        stacked,
        SettingsMetrics::CALM_CONTROL_WIDTH,
        true,
        true,
    )
}

/// Disabled rows dim text without fading contrast-qualified controls.
pub fn setting_row_with_enabled<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    control: C,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
    control_width: f32,
    compact: bool,
    enabled: bool,
) -> Stateful<Div> {
    setting_row_layout(
        id,
        label,
        help,
        control,
        modified,
        reset,
        stacked,
        control_width,
        compact,
        enabled,
    )
}

fn calm_row_text(label: &str, help: Option<&str>, modified: bool, enabled: bool) -> Div {
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::LABEL_GAP))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(Spacing::SM))
                .child(mark_search_match(
                    div()
                        .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if enabled {
                            colors::text_primary()
                        } else {
                            colors::text_muted()
                        })
                        .child(label.to_string()),
                    label,
                ))
                .when(modified, |row| {
                    row.child(
                        div()
                            .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                            .text_color(colors::text_secondary())
                            .child("Modified"),
                    )
                }),
        )
        .when_some(help, |text, help| {
            text.child(
                div()
                    .min_w_0()
                    .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                    .text_color(if enabled {
                        colors::text_secondary()
                    } else {
                        colors::text_muted()
                    })
                    .whitespace_normal()
                    .child(help.to_string()),
            )
        })
}

fn setting_row_layout<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    control: C,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
    control_width: f32,
    compact: bool,
    enabled: bool,
) -> Stateful<Div> {
    let reset = reset.filter(|_| modified);
    let control = div()
        .min_w_0()
        .flex_shrink_0()
        .map(|column| {
            if compact {
                column
            } else if stacked {
                column.w_full()
            } else {
                column.w(px(control_width))
            }
        })
        .flex()
        .items_center()
        .justify_end()
        .gap(px(Spacing::SM))
        .child(
            div()
                .min_w_0()
                .map(|column| if compact { column } else { column.flex_1() })
                .flex()
                .items_center()
                .justify_end()
                .child(crate::components::ui::accessibility::described_control(
                    control, label, help,
                )),
        )
        .when_some(reset, |column, reset| column.child(reset));
    let root = div()
        .id(id.into())
        .w_full()
        .min_w_0()
        .min_h(px(SettingsMetrics::CALM_ROW_MIN_HEIGHT))
        .flex();
    if compact {
        root.py(px(Spacing::XS))
            .flex_col()
            .gap(px(SettingsMetrics::LABEL_GAP))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::SM))
                    .child(calm_row_text(label, None, modified, enabled))
                    .child(control),
            )
            .when_some(help, |row, help| {
                row.child(
                    div()
                        .min_w_0()
                        .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                        .text_color(if enabled {
                            colors::text_secondary()
                        } else {
                            colors::text_muted()
                        })
                        .whitespace_normal()
                        .child(help.to_string()),
                )
            })
    } else {
        root.py(px(Spacing::SM))
            .gap(px(SettingsMetrics::ROW_COLUMN_GAP))
            .map(|row| {
                if stacked {
                    row.flex_col().items_start()
                } else {
                    row.items_center()
                }
            })
            .child(calm_row_text(label, help, modified, enabled))
            .child(control)
    }
}

fn settings_action(
    id: impl Into<SharedString>,
    label: &str,
    action: MenuAction,
    focus: FocusHandle,
) -> Stateful<Div> {
    settings_action_with_focus_return(id, label, action, None, focus)
}

fn settings_action_with_focus_return(
    id: impl Into<SharedString>,
    label: &str,
    action: MenuAction,
    return_focus: Option<FocusHandle>,
    focus: FocusHandle,
) -> Stateful<Div> {
    let click = action.clone();
    div()
        .id(id.into())
        .role(Role::Button)
        .aria_label(SharedString::from(label.to_string()))
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .map(|element| crate::components::ui::accessibility::accessible_focus(element, focus))
        .rounded(Radii::md())
        .text_color(colors::text_secondary())
        .cursor_pointer()
        .hover(|style| style.bg(colors::bg_hover()))
        .on_activate(move |_, window, cx| click(window, cx))
        .focus_visible(|style| {
            style
                .bg(colors::settings_group_surface())
                .shadow(focus_ring(colors::settings_group_surface()))
        })
        .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
            if return_focus
                .as_ref()
                .is_some_and(|handle| handle.is_focused(window))
            {
                window.focus_prev(cx);
            }
            action(window, cx)
        })
}

/// Stable secondary list action without a filled danger surface.
pub fn quiet_action_button<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    danger: bool,
    cx: &mut Context<T>,
    on_click: impl Fn(&mut Context<T>) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let focus = control_focus(&id, cx);
    let entity = cx.entity();
    settings_action(
        id,
        label,
        Rc::new(move |_, cx| entity.update(cx, |_, cx| on_click(cx))),
        focus.clone(),
    )
    .track_focus(&focus)
    .px(px(Spacing::SM))
    .py(px(Spacing::XS))
    .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
    .flex_shrink_0()
    .border_1()
    .border_color(colors::border())
    .bg(colors::bg_surface())
    .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
    .text_color(if danger {
        colors::signal_red()
    } else {
        colors::text_secondary()
    })
    .child(label.to_string())
}

/// Backend defaults shared by rows and reset handlers without repeated allocations.
pub fn settings_defaults() -> &'static crate::backend::settings::AppSettings {
    static DEFAULTS: LazyLock<crate::backend::settings::AppSettings> =
        LazyLock::new(crate::backend::settings::AppSettings::default);
    &DEFAULTS
}

/// Retained controls for one page in the current settings session.
#[derive(Default)]
pub struct SettingsPageUi {
    pub stacked: bool,
    pub searching: bool,
    pub selected_setting: Option<&'static str>,
    pub matched_settings: Vec<&'static str>,
    pub(crate) last_control_focus: Rc<RefCell<Option<WeakFocusHandle>>>,
    pub(crate) disclosures: HashMap<SharedString, SettingsDisclosureState>,
    selects: HashMap<SharedString, SettingsSelectState>,
}

/// Reveal a newly focused control using the exact-target minimal-scroll rule.
pub(crate) fn settings_focus_scroll(
    body: impl IntoElement,
    scroll: ScrollHandle,
    last_focus: Rc<RefCell<Option<WeakFocusHandle>>>,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    if last_focus
        .borrow()
        .as_ref()
        .and_then(WeakFocusHandle::upgrade)
        != window.focused(cx)
    {
        *last_focus.borrow_mut() = None;
    }
    crate::components::ui::accessibility::focus_reveal_scope(
        body,
        Rc::new(move |focus, bounds, window, _cx| {
            if last_focus
                .borrow()
                .as_ref()
                .and_then(WeakFocusHandle::upgrade)
                .as_ref()
                == Some(focus)
            {
                // Already revealed: consume a repeated AT request too.
                crate::components::ui::accessibility::take_accessible_focus(focus);
                return;
            }
            *last_focus.borrow_mut() = Some(focus.downgrade());
            if !crate::components::ui::accessibility::should_reveal_focus(focus, window) {
                return;
            }
            crate::components::ui::accessibility::take_accessible_focus(focus);
            let offset = scroll.offset();
            let viewport = scroll.bounds();
            if let Some(y) = target_scroll_offset(
                viewport.top().into(),
                viewport.size.height.into(),
                (bounds.top() - offset.y).into(),
                bounds.size.height.into(),
                offset.y.into(),
                scroll.max_offset().y.into(),
                Spacing::XS,
            ) {
                scroll.set_offset(point(offset.x, px(y)));
                window.on_next_frame(|window, _| window.refresh());
            }
        }),
    )
}

impl SettingsPageUi {
    pub(crate) fn close_selects(&self, window: &mut Window, cx: &mut App) {
        for state in self.selects.values() {
            if state.is_open() {
                state.close(window, cx);
            }
        }
    }
    pub fn disclosure(
        &mut self,
        id: impl Into<SharedString>,
        cx: &mut App,
    ) -> SettingsDisclosureState {
        self.disclosures
            .entry(id.into())
            .or_insert_with(|| SettingsDisclosureState::new(cx))
            .clone()
    }

    pub fn select(&mut self, id: impl Into<SharedString>, cx: &mut App) -> SettingsSelectState {
        self.selects
            .entry(id.into())
            .or_insert_with(|| SettingsSelectState::new(cx))
            .clone()
    }
}

/// Independent reset action; activation preserves each setting's backend setter.
pub fn setting_reset<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    cx: &mut Context<T>,
    on_reset: impl Fn(&mut Context<T>) + 'static,
) -> AnyElement {
    setting_reset_with_window(id, label, cx, move |_, cx| on_reset(cx))
}

pub fn setting_reset_with_window<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    cx: &mut Context<T>,
    on_reset: impl Fn(&mut Window, &mut Context<T>) + 'static,
) -> AnyElement {
    let entity = cx.entity();
    let id = id.into();
    let focus = control_focus(&id, cx);
    let keyboard_focus = focus.clone();
    let label = format!("Reset {label} to default");
    settings_action_with_focus_return(
        id,
        &label,
        Rc::new(move |window, cx| entity.update(cx, |_, cx| on_reset(window, cx))),
        Some(focus.clone()),
        focus.clone(),
    )
    .aria_description("Restore this setting's default value")
    .track_focus(&focus)
    .capture_key_down(move |event, window, cx| {
        if is_activation_key(event) && !event.is_held && keyboard_focus.is_focused(window) {
            window.focus_prev(cx);
        }
    })
    .flex_shrink_0()
    .px(px(Spacing::SM))
    .py(px(Spacing::XS))
    .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
    .child("Reset")
    .into_any_element()
}

pub(crate) fn control_focus<T: 'static>(id: &str, cx: &mut Context<T>) -> FocusHandle {
    let key = format!("{:?}:{id}", cx.entity_id());
    CONTROL_FOCUS_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        // Most renders stay below this bound; avoid an O(n) scan per control.
        if handles.len() >= CONTROL_FOCUS_PRUNE_AT.with(Cell::get) {
            handles.retain(|_, handle| handle.upgrade().is_some());
            CONTROL_FOCUS_PRUNE_AT.with(|threshold| threshold.set(512.max(handles.len() * 2)));
        }
        if let Some(handle) = handles.get(&key).and_then(WeakFocusHandle::upgrade) {
            return handle;
        }
        let handle = cx.focus_handle().tab_index(0).tab_stop(true);
        handles.insert(key, handle.downgrade());
        handle
    })
}

thread_local! {
    static CONTROL_FOCUS_HANDLES: RefCell<HashMap<String, WeakFocusHandle>> = RefCell::new(HashMap::new());
    static CONTROL_FOCUS_PRUNE_AT: Cell<usize> = const { Cell::new(512) };
}

/// Session-only disclosure state retained by the shared settings surface.
#[derive(Clone)]
pub struct SettingsDisclosureState {
    expanded: Rc<Cell<bool>>,
    search_revealed: Rc<Cell<bool>>,
    search_dismissed: Rc<Cell<bool>>,
    trigger_focus: FocusHandle,
    content_focus: FocusHandle,
}

impl SettingsDisclosureState {
    pub(crate) fn focus_trigger(&self, window: &mut Window, cx: &mut App) {
        self.trigger_focus.focus(window, cx);
    }
    pub fn new(cx: &mut App) -> Self {
        Self {
            expanded: Rc::new(Cell::new(false)),
            search_revealed: Rc::new(Cell::new(false)),
            search_dismissed: Rc::new(Cell::new(false)),
            trigger_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            content_focus: cx.focus_handle().tab_stop(false),
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded.get() || self.search_revealed.get()
    }

    /// Identity of this retained state. A settings surface that closes and
    /// reopens gets a new one, so callers can tell a fresh expansion apart.
    pub(crate) fn instance_key(&self) -> usize {
        Rc::as_ptr(&self.expanded) as usize
    }

    pub fn is_manually_expanded(&self) -> bool {
        self.expanded.get()
    }

    pub fn is_search_revealed(&self) -> bool {
        self.search_revealed.get()
    }

    pub(crate) fn reset_search_dismissal(&self) {
        self.search_dismissed.set(false);
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.trigger_focus.is_focused(window) || self.content_focus.contains_focused(window, cx)
    }

    pub(crate) fn reveal_for_search(&self, revealed: bool, window: &mut Window, cx: &mut App) {
        if !revealed {
            self.search_dismissed.set(false);
        }
        let revealed = revealed && !self.search_dismissed.get();
        let collapsing = self.is_expanded() && !self.expanded.get() && !revealed;
        if collapsing && self.content_focus.contains_focused(window, cx) {
            window.focus(&self.trigger_focus, cx);
        }
        if self.search_revealed.replace(revealed) != revealed {
            window.refresh();
        }
    }

    /// Return descendant focus before removing the disclosure body.
    pub fn set_expanded(&self, expanded: bool, window: &mut Window, cx: &mut App) -> bool {
        if self.is_expanded() == expanded {
            return false;
        }
        if !expanded && self.content_focus.contains_focused(window, cx) {
            window.focus(&self.trigger_focus, cx);
        }
        if !expanded && self.search_revealed.get() {
            self.search_revealed.set(false);
            self.search_dismissed.set(true);
        }
        self.expanded.set(expanded);
        window.refresh();
        true
    }
}

/// One heading and a lazily built body; collapsed children are not tab stops.
pub fn settings_disclosure<T: 'static, C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    summary: Option<&str>,
    modified_count: usize,
    state: &SettingsDisclosureState,
    cx: &mut Context<T>,
    body: impl FnOnce(&mut Context<T>) -> C,
) -> Stateful<Div> {
    settings_disclosure_with_layout(id, label, summary, modified_count, state, false, cx, body)
}

pub fn settings_disclosure_with_layout<T: 'static, C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    summary: Option<&str>,
    modified_count: usize,
    state: &SettingsDisclosureState,
    stacked: bool,
    cx: &mut Context<T>,
    body: impl FnOnce(&mut Context<T>) -> C,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let expanded = state.is_expanded();
    let toggle = state.clone();
    let expand = state.clone();
    let collapse = state.clone();
    let detail = match (summary, modified_count) {
        (Some(summary), 0) => summary.to_string(),
        (None, 0) => String::new(),
        (Some(summary), count) => format!("{summary} · {count} modified"),
        (None, count) => format!("{count} modified"),
    };

    div()
        .id(id.clone())
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(
            settings_action(
                SharedString::from(format!("{id}-trigger")),
                label,
                Rc::new(move |window, cx| {
                    toggle.set_expanded(!toggle.is_expanded(), window, cx);
                }),
                state.trigger_focus.clone(),
            )
            .track_focus(&state.trigger_focus)
            .focus_visible(|style| {
                style
                    .bg(colors::settings_group_surface())
                    .shadow(focus_ring(colors::settings_group_surface()))
            })
            .aria_expanded(expanded)
            .when(!detail.is_empty(), |trigger| {
                trigger.aria_description(SharedString::from(detail.clone()))
            })
            .on_a11y_action(AccessibleAction::Expand, move |_, window, cx| {
                expand.set_expanded(true, window, cx);
            })
            .on_a11y_action(AccessibleAction::Collapse, move |_, window, cx| {
                collapse.set_expanded(false, window, cx);
            })
            .h(px(SettingsMetrics::CALM_ROW_MIN_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(Spacing::SM))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(Spacing::XXS))
                    .child(
                        div()
                            .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .child(label.to_string()),
                    )
                    .when(stacked && !detail.is_empty(), |column| {
                        column.child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(Spacing::SM))
                                .when_some(summary, |row, summary| {
                                    row.child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                                            .text_color(colors::text_secondary())
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .child(summary.to_string()),
                                    )
                                })
                                .when(modified_count > 0, |row| {
                                    row.child(disclosure_modified_count(modified_count))
                                }),
                        )
                    }),
            )
            .when(!stacked, |row| {
                row.when_some(summary, |row, summary| {
                    row.child(
                        div()
                            .min_w_0()
                            .max_w(relative(0.30))
                            .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                            .text_color(colors::text_secondary())
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(summary.to_string()),
                    )
                })
                .when(modified_count > 0, |row| {
                    row.child(disclosure_modified_count(modified_count))
                })
            })
            .child(
                div()
                    .flex_shrink_0()
                    .when(stacked, |glyph| glyph.self_start().mt(px(Spacing::XS)))
                    .child(if expanded { "▾" } else { "▸" }),
            ),
        )
        .when(expanded, |root| {
            root.child(
                div()
                    .id(SharedString::from(format!("{id}-body")))
                    .role(Role::Group)
                    .aria_label(SharedString::from(label.to_string()))
                    .track_focus(&state.content_focus)
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .child(body(cx)),
            )
        })
}

fn disclosure_modified_count(count: usize) -> Div {
    div()
        .flex_shrink_0()
        .whitespace_nowrap()
        .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
        .text_color(colors::text_secondary())
        .child(format!("{count} modified"))
}

fn settings_summary_row_with_chevron<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    summary: &str,
    chevron: &'static str,
    focus: Option<FocusHandle>,
    cx: &mut Context<T>,
    on_open: impl Fn(&mut Window, &mut Context<T>) + 'static,
) -> Stateful<Div> {
    let entity = cx.entity();
    let id = id.into();
    let focus = focus.unwrap_or_else(|| control_focus(&id, cx));
    settings_action(
        id,
        label,
        Rc::new(move |window, cx| entity.update(cx, |_, cx| on_open(window, cx))),
        focus,
    )
    .aria_description(SharedString::from(summary.to_string()))
    .w_full()
    .min_w_0()
    .min_h(px(SettingsMetrics::CALM_ROW_MIN_HEIGHT))
    .flex()
    .items_center()
    .gap(px(Spacing::SM))
    .child(
        div()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(SettingsMetrics::LABEL_GAP))
            .child(
                div()
                    .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child(label.to_string()),
            )
            .child(
                div()
                    .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                    .text_color(colors::text_secondary())
                    .child(summary.to_string()),
            ),
    )
    .child(chevron)
}

/// Summary entry with a focus-safe, lazily mounted collection editor.
pub fn settings_summary_disclosure<T: 'static, C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    summary: &str,
    state: &SettingsDisclosureState,
    cx: &mut Context<T>,
    body: impl FnOnce(&mut Context<T>) -> C,
) -> Stateful<Div> {
    let id = id.into();
    let toggle = state.clone();
    let expand = state.clone();
    let collapse = state.clone();
    let expanded = state.is_expanded();
    div()
        .id(id.clone())
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(
            settings_summary_row_with_chevron(
                SharedString::from(format!("{id}-trigger")),
                label,
                summary,
                if expanded { "▾" } else { "▸" },
                Some(state.trigger_focus.clone()),
                cx,
                move |window, cx| {
                    toggle.set_expanded(!toggle.is_expanded(), window, cx);
                },
            )
            .track_focus(&state.trigger_focus)
            .focus_visible(|style| {
                style
                    .bg(colors::settings_group_surface())
                    .shadow(focus_ring(colors::settings_group_surface()))
            })
            .aria_expanded(expanded)
            .on_a11y_action(AccessibleAction::Expand, move |_, window, cx| {
                expand.set_expanded(true, window, cx);
            })
            .on_a11y_action(AccessibleAction::Collapse, move |_, window, cx| {
                collapse.set_expanded(false, window, cx);
            }),
        )
        .when(expanded, |root| {
            root.child(
                div()
                    .id(SharedString::from(format!("{id}-body")))
                    .role(Role::Group)
                    .aria_label(SharedString::from(label.to_string()))
                    .track_focus(&state.content_focus)
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .child(body(cx)),
            )
        })
}

/// Scalar summary and action with a full-width, focus-safe editor underneath.
pub fn settings_editor_row<T: 'static, C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    summary: &str,
    action_label: &'static str,
    modified: bool,
    reset: Option<AnyElement>,
    state: &SettingsDisclosureState,
    stacked: bool,
    cx: &mut Context<T>,
    body: impl FnOnce(&mut Context<T>) -> C,
) -> Stateful<Div> {
    let id = id.into();
    let expanded = state.is_expanded();
    let toggle = state.clone();
    let expand = state.clone();
    let collapse = state.clone();
    let action = settings_action(
        SharedString::from(format!("{id}-trigger")),
        &format!("{action_label} {label}"),
        Rc::new(move |window, cx| {
            toggle.set_expanded(!toggle.is_expanded(), window, cx);
        }),
        state.trigger_focus.clone(),
    )
    .track_focus(&state.trigger_focus)
    .focus_visible(|style| {
        style
            .bg(colors::settings_group_surface())
            .shadow(focus_ring(colors::settings_group_surface()))
    })
    .aria_expanded(expanded)
    .on_a11y_action(AccessibleAction::Expand, move |_, window, cx| {
        expand.set_expanded(true, window, cx);
    })
    .on_a11y_action(AccessibleAction::Collapse, move |_, window, cx| {
        collapse.set_expanded(false, window, cx);
    })
    .px(px(Spacing::SM))
    .py(px(Spacing::XS))
    .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
    .border_1()
    .border_color(colors::border())
    .child(if expanded { "Close" } else { action_label });
    div()
        .id(id.clone())
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_row(
            SharedString::from(format!("{id}-row")),
            label,
            Some(summary),
            action,
            modified,
            reset,
            stacked,
        ))
        .when(expanded, |root| {
            root.child(
                div()
                    .id(SharedString::from(format!("{id}-body")))
                    .role(Role::Group)
                    .aria_label(SharedString::from(label.to_string()))
                    .track_focus(&state.content_focus)
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .child(body(cx)),
            )
        })
}

/// A single quiet group surface with an optional, non-duplicated heading.
pub fn settings_group<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    show_heading: bool,
    content: C,
) -> Stateful<Div> {
    settings_group_surface(id, label, show_heading, content, true)
}

/// Quiet subtopics reuse the enclosing surface and inset.
pub fn settings_subtopic<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    content: C,
) -> Stateful<Div> {
    settings_group_contents(
        id,
        label,
        false,
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(Spacing::SM))
            .child(settings_subheading(label))
            .child(content),
    )
}

pub fn settings_subheading(label: &str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!(
            "subheading-{}",
            control_slug(label)
        )))
        .role(Role::Heading)
        .aria_label(SharedString::from(label.to_string()))
        .aria_level(3)
        .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors::text_secondary())
        .child(label.to_string())
}

/// Nested contents keep the enclosing group's inset and surface.
pub fn settings_group_contents<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    show_heading: bool,
    content: C,
) -> Stateful<Div> {
    settings_group_surface(id, label, show_heading, content, false)
}

fn settings_group_surface<C: IntoElement>(
    id: impl Into<SharedString>,
    label: &str,
    show_heading: bool,
    content: C,
    surface: bool,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .when(surface, |group| {
            group.role(Role::Group).when(!show_heading, |group| {
                group.aria_label(SharedString::from(label.to_string()))
            })
        })
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::MD))
        .when(surface, |group| {
            group
                .px(px(Spacing::MD))
                .rounded(Radii::md())
                .bg(colors::settings_group_surface())
                .when(
                    colors::settings_group_border(colors::settings_group_surface())
                        != colors::settings_group_surface(),
                    |group| {
                        group.border_1().border_color(colors::settings_group_border(
                            colors::settings_group_surface(),
                        ))
                    },
                )
        })
        .when(show_heading, |group| {
            group.child(
                div()
                    .id("heading")
                    .role(Role::Heading)
                    .aria_label(SharedString::from(label.to_string()))
                    .aria_level(2)
                    .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child(label.to_string()),
            )
        })
        .child(content)
}

/// A group of settings under a level-three heading.
///
/// Flat groups with rules: a 13px medium heading over a body fenced by subtle
/// top/bottom rules, with no generic side border, rounded rectangle, or
/// indentation. The former card treatment stacked several equally weighted
/// boxes beneath a subsection heading that differed from them only by a faint
/// color change, so nothing carried the hierarchy. Complex editors, lists,
/// color pickers, and uploader mappings still supply their own internal
/// container where one is structurally necessary.
pub fn setting_group<C: IntoElement>(title: &str, children: C) -> impl IntoElement + use<C> {
    setting_group_titled(title, children)
}

/// Group heading plus content. See [`setting_group`] — both spellings now share
/// the flat treatment; the pair is kept because ~90 call sites import one or
/// the other (frequently aliased as `render_setting_group`).
pub fn setting_group_titled<C: IntoElement>(title: &str, content: C) -> impl IntoElement + use<C> {
    let surface = colors::settings_group_surface();
    let rule = colors::settings_group_rule(surface);
    let slug = control_slug(title);

    div()
        .id(SharedString::from(format!("settings-group-{slug}")))
        .role(Role::Group)
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(
            div()
                .id("heading")
                .role(Role::Heading)
                .aria_label(SharedString::from(title.to_string()))
                .aria_level(3)
                .text_size(px(SettingsMetrics::LABEL_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child(title.to_string()),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .py(px(SettingsMetrics::GROUP_GAP))
                .border_t_1()
                .border_b_1()
                .border_color(rule)
                .child(content),
        )
}

/// Shared empty-state treatment for list sections: boxed + centered.
pub fn empty_state(message: impl Into<SharedString>) -> impl IntoElement {
    div()
        .w_full()
        .p_2()
        .bg(colors::bg_surface())
        .rounded(Radii::sm())
        .border_1()
        .border_color(colors::border())
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_size(px(SettingsMetrics::LABEL_TEXT))
                .text_color(colors::text_muted())
                .child(message.into()),
        )
}

/// Style A secondary/primary button: compact pill used inside list-row and
/// editor-form actions (Edit/Delete/Save/Cancel-style controls). Byte-identical
/// helper that was hand-copied across commands.rs, nicknames.rs, filters.rs,
/// moderation.rs, and highlights.rs (blacklist rows).
pub fn button<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    label: &'static str,
    primary: bool,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let id = SharedString::from(format!("settings-button-{}", control_slug(label)));
    button_keyed(id, label, primary, cx, on_click)
}

/// [`button`] with an explicit element id.
///
/// A button's id owns its focus slot, so repeated list-row actions ("Edit",
/// "Delete") must supply one per row; the label-derived id in [`button`] is
/// only unique when the surrounding row already carries an id of its own.
pub fn button_keyed<T: 'static, F: Fn(&mut Context<T>) + 'static, I: Into<SharedString>>(
    id: I,
    label: &str,
    primary: bool,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F, I> {
    let surface = colors::settings_group_surface();
    let on_click = Rc::new(on_click);
    let click = Rc::clone(&on_click);
    let keyboard = Rc::clone(&on_click);
    let entity = cx.entity();

    let mouse_listener = cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
        (*click)(cx);
    });
    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
        if is_activation_key(event) {
            cx.stop_propagation();
            (*keyboard)(cx);
        }
    });

    keyboard_control(
        div()
            .id(id.into())
            .role(Role::Button)
            .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
            .aria_label(SharedString::from(label.to_string())),
        surface,
        cx,
        None,
    )
    .px(px(SettingsMetrics::COMPACT_BUTTON_PADDING_X))
    .py(px(SettingsMetrics::COMPACT_BUTTON_PADDING_Y))
    .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
    .flex()
    .items_center()
    .rounded(Radii::sm())
    .cursor_pointer()
    .bg(if primary {
        colors::accent().alpha(1.0)
    } else {
        colors::bg_surface()
    })
    .border_1()
    .border_color(if primary {
        colors::accent()
    } else {
        colors::border()
    })
    .hover(move |style| {
        if primary {
            style
                .bg(colors::accent_hover())
                .text_color(colors::text_on_aa(colors::accent_hover()))
        } else {
            style.bg(colors::bg_hover())
        }
    })
    .text_xs()
    .font_weight(FontWeight::SEMIBOLD)
    .text_color(if primary {
        colors::text_on_accent_contrast()
    } else {
        colors::interactive_foreground()
    })
    .child(label.to_string())
    .on_completed_click(mouse_listener)
    .on_key_down(key_listener)
    .on_a11y_action(AccessibleAction::Click, move |_data, _window, app| {
        entity.update(app, |_this, cx| (*on_click)(cx));
    })
}

/// Style B action button: standalone action control with a destructive (red)
/// treatment option. Identical (minus a per-file id-prefix string) across
/// general.rs, privacy.rs, and proxy.rs. Derives its element id from the
/// label, so it is only for SINGLETON buttons (unique label per surface);
/// repeated list-row actions must use [`action_button_keyed`] so each row
/// gets a distinct element id.
pub fn action_button<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    label: &str,
    is_destructive: bool,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F> {
    let id = SharedString::from(format!("btn-{}", label.replace(' ', "-")));
    action_button_keyed(id, label, is_destructive, cx, on_click)
}

/// [`action_button`] with an explicit element id, for repeated rows (e.g. a
/// Delete per list entry) where a label-derived id would collide across rows
/// and share one GPUI element-state slot.
pub fn action_button_keyed<T: 'static, F: Fn(&mut Context<T>) + 'static, I: Into<SharedString>>(
    id: I,
    label: &str,
    is_destructive: bool,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F, I> {
    let bg_color = if is_destructive {
        let mut c = colors::error();
        c.a = 0.1;
        c
    } else {
        colors::bg_elevated()
    };
    let text_color = if is_destructive {
        colors::error()
    } else {
        colors::text_primary()
    };
    let border_color = if is_destructive {
        let mut c = colors::error();
        c.a = 0.3;
        c
    } else {
        colors::border()
    };

    let surface = colors::settings_group_surface();
    let on_click = Rc::new(on_click);
    let click = Rc::clone(&on_click);
    let keyboard = Rc::clone(&on_click);
    let entity = cx.entity();

    let mouse_listener = cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
        (*click)(cx);
    });
    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
        if is_activation_key(event) {
            cx.stop_propagation();
            (*keyboard)(cx);
        }
    });

    keyboard_control(
        div()
            .id(id.into())
            .role(Role::Button)
            .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
            .aria_label(SharedString::from(label.to_string())),
        surface,
        cx,
        None,
    )
    .px(px(SettingsMetrics::ACTION_BUTTON_PADDING_X))
    .py(px(SettingsMetrics::ACTION_BUTTON_PADDING_Y))
    .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
    .flex()
    .items_center()
    .rounded(Radii::sm())
    .bg(bg_color)
    .border_1()
    .border_color(border_color)
    .cursor_pointer()
    .hover(|s| {
        s.bg(if is_destructive {
            colors::error().opacity(0.18)
        } else {
            colors::bg_hover()
        })
    })
    .on_completed_click(mouse_listener)
    .on_key_down(key_listener)
    .on_a11y_action(AccessibleAction::Click, move |_data, _window, app| {
        entity.update(app, |_this, cx| (*on_click)(cx));
    })
    .child(
        div()
            .text_size(px(SettingsMetrics::LABEL_TEXT))
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .child(label.to_string()),
    )
}

/// Painted toggle track (32×18). Non-interactive: callers wrap it in a taller
/// pointer target so the compact visual never shrinks the click area.
fn toggle_track(is_on: bool, enabled: bool) -> impl IntoElement {
    let bg = colors::settings_switch_track(is_on, enabled);

    div()
        .w(px(SettingsMetrics::TOGGLE_TRACK_WIDTH))
        .h(px(SettingsMetrics::TOGGLE_TRACK_HEIGHT))
        .rounded(Radii::full())
        .bg(bg)
        .border_1()
        .border_color(if is_on && enabled {
            bg
        } else {
            colors::border()
        })
        .flex()
        .items_center()
        .px(px(Spacing::XXS))
        .child(
            div()
                .w(px(14.0))
                .h(px(14.0))
                .rounded_full()
                .bg(colors::settings_switch_knob(is_on, enabled))
                .shadow_sm()
                .when(!(is_on && enabled), |s| {
                    s.border_1().border_color(colors::border())
                })
                .when(is_on, |s| s.ml(px(Spacing::MD))),
        )
}

/// Attach the shared switch contract — `Role::Switch`, toggled state, pointer
/// and Space/Enter activation, an AccessKit Default (Click) action, and the
/// keyboard focus ring — to an already-styled element.
///
/// The a11y action reaches the owning entity rather than the render closure's
/// `Context`, because AccessKit handlers run with a bare `App`.
fn switch_behavior<T: 'static, F: Fn(bool, &mut Context<T>) + 'static>(
    element: Stateful<Div>,
    label: Option<&str>,
    description: Option<&str>,
    is_on: bool,
    enabled: bool,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    let surface = colors::settings_group_surface();
    let focus_id = Element::id(&element)
        .map(|id| format!("switch-{id:?}"))
        .unwrap_or_else(|| "switch".to_string());
    let focus = control_focus(&focus_id, cx);
    let on_change = Rc::new(on_change);
    let pointer = Rc::clone(&on_change);
    let keyboard = Rc::clone(&on_change);
    let entity = cx.entity();

    let mouse_listener = cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
        if enabled {
            (*pointer)(!is_on, cx);
        }
    });
    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
        if enabled && is_activation_key(event) && !event.is_held {
            cx.stop_propagation();
            (*keyboard)(!is_on, cx);
        }
    });

    let element = element
        .role(Role::Switch)
        .a11y_synthetic_children(move |builder| {
            crate::components::ui::accessibility::apply_description(builder);
            if !enabled {
                builder.parent_node().set_disabled();
            }
        })
        .aria_toggled(if is_on { Toggled::True } else { Toggled::False })
        .when_some(label, |el, label| {
            el.aria_label(SharedString::from(label.to_string()))
        })
        .when_some(description, |el, description| {
            el.aria_description(SharedString::from(description.to_string()))
        });

    let element = if enabled {
        keyboard_control(element, surface, cx, Some(focus.clone())).cursor_pointer()
    } else {
        element.cursor_default()
    };

    element
        .on_completed_click(mouse_listener)
        .on_key_down(key_listener)
        .on_a11y_action(AccessibleAction::Click, move |_data, _window, app| {
            if enabled {
                entity.update(app, |_this, cx| (*on_change)(!is_on, cx));
            }
        })
}

/// Toggle switch widget
pub fn toggle<T: 'static, F: Fn(bool, &mut Context<T>) + 'static, I: Into<SharedString>>(
    id: I,
    is_on: bool,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F, I> {
    toggle_with_enabled(id, is_on, true, cx, on_change)
}

/// Toggle with enabled/disabled state
/// When disabled, the toggle is visually muted and non-interactive
pub fn toggle_with_enabled<
    T: 'static,
    F: Fn(bool, &mut Context<T>) + 'static,
    I: Into<SharedString>,
>(
    id: I,
    is_on: bool,
    enabled: bool,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    switch_behavior(
        div()
            .id(id.into())
            .w(px(SettingsMetrics::TOGGLE_TRACK_WIDTH))
            .h(px(SettingsMetrics::TOGGLE_TARGET_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(Radii::full()),
        None,
        None,
        is_on,
        enabled,
        cx,
        on_change,
    )
    .when(
        enabled && colors::settings_switch_uses_accent(),
        |element| {
            element.focus_visible(|style| {
                let surface = colors::settings_group_surface();
                // Compact wrappers only. The same 4px outer edge and 2px gap
                // remain in place when Space changes the on/off state.
                style.shadow(vec![
                    BoxShadow::new(px(0.0), px(0.0), colors::settings_focus_ring(surface))
                        .spread_radius(px(Spacing::XS)),
                    BoxShadow::new(px(0.0), px(0.0), surface.alpha(1.0))
                        .spread_radius(px(Spacing::XXS)),
                ])
            })
        },
    )
    .child(toggle_track(is_on, enabled))
}

/// Compact switch with an explicit accessible name for a calm setting row.
pub fn labelled_toggle<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    is_on: bool,
    cx: &mut Context<T>,
    on_change: impl Fn(bool, &mut Context<T>) + 'static,
) -> Stateful<Div> {
    toggle_with_enabled(id, is_on, true, cx, on_change)
        .aria_label(SharedString::from(label.to_string()))
}

/// Small toggle for compact displays (display only).
///
/// Shares the painted track with the interactive toggle. These callers embed it
/// in an already-clickable pill or list row, which owns the pointer target, so
/// the track is not padded out to the interactive minimum here — doing that
/// would make those rows taller rather than denser.
pub fn toggle_small_static(is_on: bool) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .cursor_pointer()
        .child(toggle_track(is_on, true))
}

/// Option for select widget
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

impl SelectOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// Retained menu focus and highlight for a compact settings select.
#[derive(Clone)]
pub struct SettingsSelectState {
    open: Rc<Cell<bool>>,
    focus: FocusHandle,
    menu_focus: FocusHandle,
    nav: MenuNav,
}

impl SettingsSelectState {
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }
    pub fn new(cx: &mut App) -> Self {
        Self {
            open: Rc::new(Cell::new(false)),
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            menu_focus: cx.focus_handle().tab_stop(false),
            nav: MenuNav::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn open(&self, selected_index: Option<usize>, window: &mut Window, cx: &mut App) {
        if self.open.replace(true) {
            return;
        }
        self.nav.highlight(selected_index.unwrap_or(0));
        window.focus(&self.menu_focus, cx);
        window.refresh();
    }

    pub fn close(&self, window: &mut Window, cx: &mut App) {
        if !self.open.replace(false) {
            return;
        }
        self.nav.reset();
        if self.menu_focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        window.refresh();
        // Blur can run during prepaint, when refresh is intentionally ignored.
        window.on_next_frame(|window, _| window.refresh());
    }
}

/// Finite choices in one compact trigger, using the shared menu key contract.
pub fn settings_select<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    options: &[SelectOption],
    selected: &str,
    state: &SettingsSelectState,
    cx: &mut Context<T>,
    on_change: impl Fn(String, &mut Context<T>) + 'static,
) -> Stateful<Div> {
    render_settings_choice_menu(id, label, options, Some(selected), state, cx, on_change)
}

/// A compact action menu, retaining the shared select menu's focus and keys.
pub fn settings_menu_button<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    options: &[SelectOption],
    state: &SettingsSelectState,
    cx: &mut Context<T>,
    on_activate: impl Fn(String, &mut Context<T>) + 'static,
) -> Stateful<Div> {
    render_settings_choice_menu(id, label, options, None, state, cx, on_activate)
}

fn render_settings_choice_menu<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    options: &[SelectOption],
    selected: Option<&str>,
    state: &SettingsSelectState,
    cx: &mut Context<T>,
    on_change: impl Fn(String, &mut Context<T>) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let is_open = state.is_open() && !options.is_empty();
    let selected_index =
        selected.and_then(|value| options.iter().position(|option| option.value == value));
    let display = selected_index.map_or(selected.unwrap_or(label), |index| {
        options[index].label.as_str()
    });
    let on_change = Rc::new(on_change);
    let entity = cx.entity();
    let actions: Vec<MenuAction> = options
        .iter()
        .map(|option| {
            let value = option.value.clone();
            let on_change = on_change.clone();
            let state = state.clone();
            let entity = entity.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                state.close(window, cx);
                entity.update(cx, |_, cx| on_change(value.clone(), cx));
            }) as MenuAction
        })
        .collect();
    let toggle = state.clone();
    let has_options = !options.is_empty();
    let open_action: MenuAction = Rc::new(move |window, cx| {
        if !has_options {
            return;
        }
        if toggle.is_open() {
            toggle.close(window, cx);
        } else {
            toggle.open(selected_index, window, cx);
        }
    });
    let expand = state.clone();
    let collapse = state.clone();
    let trigger = settings_action(
        SharedString::from(format!("{id}-trigger")),
        label,
        open_action,
        state.focus.clone().tab_stop(has_options),
    )
    .role(if selected.is_some() {
        Role::ComboBox
    } else {
        Role::Button
    })
    .track_focus(&state.focus)
    .aria_expanded(is_open)
    .when(selected.is_some(), |trigger| {
        trigger.aria_value(SharedString::from(display.to_string()))
    })
    .w_full()
    .min_w_0()
    .min_h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
    .px(px(SettingsMetrics::INPUT_PADDING_X))
    .border_1()
    .border_color(colors::border())
    .bg(colors::bg_surface())
    .text_color(colors::text_secondary())
    .flex()
    .items_center()
    .gap(px(Spacing::SM))
    .child(
        div()
            .min_w_0()
            .flex_1()
            .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
            .text_color(colors::text_primary())
            .text_ellipsis()
            .whitespace_nowrap()
            .child(display.to_string()),
    )
    .child(if is_open { "▴" } else { "▾" })
    .on_a11y_action(AccessibleAction::Expand, move |_, window, cx| {
        if has_options {
            expand.open(selected_index, window, cx);
        }
    })
    .on_a11y_action(AccessibleAction::Collapse, move |_, window, cx| {
        collapse.close(window, cx);
    });
    let outside = state.clone();
    let blur = state.clone();
    div()
        .id(id.clone())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::XS))
        .on_mouse_down_out(move |_, window, cx| {
            if outside.is_open() {
                outside.close(window, cx);
            }
        })
        .child(trigger.when(!has_options, |trigger| trigger.disabled_button()))
        .when(is_open, |root| {
            root.child(
                canvas(
                    move |_, window, cx| {
                        if blur.is_open()
                            && !blur.menu_focus.contains_focused(window, cx)
                            && !blur.focus.is_focused(window)
                        {
                            blur.close(window, cx);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
        })
        .when(is_open, |root| {
            let close = state.clone();
            root.child(
                menu_panel()
                    .id(SharedString::from(format!("{id}-menu")))
                    .role(Role::Menu)
                    .aria_label(SharedString::from(label.to_string()))
                    .menu_keyboard(
                        &state.menu_focus,
                        &state.nav,
                        actions.clone(),
                        move |window, cx| close.close(window, cx),
                    )
                    .max_h(px(240.0))
                    .overflow_y_scroll()
                    .children(options.iter().zip(actions).enumerate().map(
                        |(index, (option, action))| {
                            let accessible = action.clone();
                            menu_item(
                                SharedString::from(format!("{id}-option-{index}")),
                                index,
                                &state.nav,
                                action,
                            )
                            .role(Role::MenuItem)
                            .aria_label(SharedString::from(option.label.clone()))
                            .when(selected.is_some(), |item| {
                                item.aria_selected(selected == Some(option.value.as_str()))
                            })
                            .when(state.nav.is_highlighted(index), |row| {
                                row.aria_active_descendant()
                            })
                            .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                                accessible(window, cx);
                            })
                            .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                            .text_color(colors::text_primary())
                            .child(option.label.clone())
                            .when(selected == Some(option.value.as_str()), |row| {
                                row.child("✓")
                            })
                        },
                    )),
            )
        })
}

/// Owned key-down listener, as handed to
/// [`InteractiveElement::on_key_down`](gpui::InteractiveElement::on_key_down).
pub type KeyDownListener = Box<dyn Fn(&KeyDownEvent, &mut Window, &mut App) + 'static>;

/// Roving-selection key handler shared by the segmented and radio groups.
///
/// The group itself is the tab stop and keeps keyboard focus; arrows move the
/// selection and `aria_active_descendant` reports the selected option as the
/// focused node. That is the composite-widget pattern GPUI supports without
/// per-option focus handles.
fn option_group_keys<T: 'static, F: Fn(String, &mut Context<T>) + Clone + 'static>(
    values: Vec<String>,
    selected_index: Option<usize>,
    cx: &mut Context<T>,
    on_change: F,
) -> KeyDownListener {
    Box::new(
        cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
            let modifiers = &event.keystroke.modifiers;
            if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
                return;
            }
            let Some(last) = values.len().checked_sub(1) else {
                return;
            };
            let current = selected_index.unwrap_or(0);
            let next = match event.keystroke.key.as_str() {
                "left" | "up" => current.saturating_sub(1),
                "right" | "down" => (current + 1).min(last),
                "home" => 0,
                "end" => last,
                "enter" | "space" => current,
                _ => return,
            };
            cx.stop_propagation();
            if selected_index != Some(next) {
                on_change(values[next].clone(), cx);
            }
        }),
    )
}

/// Segmented button group (horizontal select)
pub fn select_buttons<T: 'static, F: Fn(String, &mut Context<T>) + Clone + 'static>(
    id_prefix: &str,
    options: &[SelectOption],
    selected: &str,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let prefix = id_prefix.to_string();
    let surface = colors::settings_group_surface();
    let count = options.len();
    let values: Vec<String> = options.iter().map(|opt| opt.value.clone()).collect();
    let selected_index = values.iter().position(|value| value == selected);
    let key_listener = option_group_keys(values, selected_index, cx, on_change.clone());

    keyboard_control(
        div()
            .id(SharedString::from(format!("{prefix}-segmented")))
            .role(Role::RadioGroup)
            .aria_orientation(Orientation::Horizontal),
        surface,
        cx,
        None,
    )
    .flex()
    .flex_wrap()
    .gap_1()
    .rounded(Radii::sm())
    .on_key_down(key_listener)
    .children(options.iter().enumerate().map(|(index, opt)| {
        let is_selected = opt.value == selected;
        let value = opt.value.clone();
        let label = opt.label.clone();
        let on_change = on_change.clone();
        let id = format!("{}-{}", prefix, opt.value);

        div()
            .id(SharedString::from(id))
            .role(Role::RadioButton)
            .aria_label(SharedString::from(label.clone()))
            // Windows SelectionItem reads aria-checked for radio roles.
            .aria_toggled(if is_selected {
                Toggled::True
            } else {
                Toggled::False
            })
            .aria_selected(is_selected)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(count)
            .when(is_selected, |el| el.aria_active_descendant())
            .px(px(SettingsMetrics::SEGMENTED_PADDING_X))
            .py(px(SettingsMetrics::SEGMENTED_PADDING_Y))
            .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
            .flex()
            .items_center()
            .rounded(Radii::sm())
            .bg(if is_selected {
                colors::accent_transparent(0.12)
            } else {
                colors::bg_surface()
            })
            .border_1()
            .border_color(if is_selected {
                colors::accent()
            } else {
                colors::border()
            })
            .cursor_pointer()
            .hover(|s| {
                s.bg(if is_selected {
                    colors::accent_transparent(0.18)
                } else {
                    colors::bg_hover()
                })
            })
            .on_completed_click(cx.listener(move |_this, _event, _window, cx| {
                on_change(value.clone(), cx);
            }))
            .child(
                div()
                    .text_size(px(SettingsMetrics::LABEL_TEXT))
                    .font_weight(if is_selected {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if is_selected {
                        colors::text_primary()
                    } else {
                        colors::interactive_foreground()
                    })
                    .child(label),
            )
    }))
}

/// Clamp `value` into `min..=max` and snap it onto the nearest `step`.
///
/// Shared by pointer, wheel, and keyboard input so all three land on the same
/// values instead of drifting off the step grid.
fn snap_slider_value(value: f32, min: f32, max: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return value.clamp(min, max);
    }
    let snapped = ((value - min) / step).round() * step + min;
    snapped.clamp(min, max)
}

/// Slider semantics for a custom-drawn slider: `Role::Slider` with its label
/// and value attributes, a tab stop with the focus ring, arrow/Page/Home/End
/// keys, and AccessKit increment/decrement. Values snap to `step`.
#[allow(clippy::too_many_arguments)]
pub fn slider_keyboard<T: 'static, F: Fn(f32, &mut Context<T>) + Clone + 'static>(
    element: Stateful<Div>,
    label: &'static str,
    value: f32,
    min: f32,
    max: f32,
    step: f32,
    display_value: String,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    let page = ((max - min) / 10.0).max(step);
    let on_change_keys = on_change.clone();
    let on_change_increment = on_change.clone();
    let on_change_decrement = on_change;
    let entity_increment = cx.entity();
    let entity_decrement = cx.entity();
    let next_value = snap_slider_value(value + step, min, max, step);
    let prev_value = snap_slider_value(value - step, min, max, step);
    // Key auto-repeat can arrive before the next render rebuilds this
    // listener, so step from the last value it set rather than `value`.
    let current = Cell::new(value);
    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let value = current.get();
        let candidate = match event.keystroke.key.as_str() {
            "left" | "down" => value - step,
            "right" | "up" => value + step,
            "pageup" => value + page,
            "pagedown" => value - page,
            "home" => min,
            "end" => max,
            _ => return,
        };
        cx.stop_propagation();
        let snapped = snap_slider_value(candidate, min, max, step);
        if (snapped - value).abs() > f32::EPSILON {
            current.set(snapped);
            on_change_keys(snapped, cx);
        }
    });

    keyboard_control(
        element
            .role(Role::Slider)
            .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
            .aria_label(label)
            .aria_orientation(Orientation::Horizontal)
            .aria_value(SharedString::from(display_value))
            .aria_numeric_value(value as f64)
            .aria_min_numeric_value(min as f64)
            .aria_max_numeric_value(max as f64)
            .aria_numeric_value_step(step as f64),
        colors::settings_group_surface(),
        cx,
        None,
    )
    .on_key_down(key_listener)
    .on_a11y_action(AccessibleAction::Increment, move |_data, _window, app| {
        entity_increment.update(app, |_this, cx| on_change_increment(next_value, cx));
    })
    .on_a11y_action(AccessibleAction::Decrement, move |_data, _window, app| {
        entity_decrement.update(app, |_this, cx| on_change_decrement(prev_value, cx));
    })
}

/// Draggable slider with value display, +/- buttons, and scroll-wheel support
pub fn slider<
    T: 'static,
    I: Into<SharedString>,
    V: Fn(f32) -> String + 'static,
    F: Fn(f32, &mut Context<T>) + Clone + 'static,
>(
    id: I,
    value: f32,
    min: f32,
    max: f32,
    step: f32,
    format_value: V,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    let normalized = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let display_value = format_value(value);

    let prev_value = (value - step).max(min);
    let next_value = (value + step).min(max);

    let id_str: SharedString = id.into();
    let id_root = SharedString::from(format!("{}-slider", id_str));
    let id_minus = SharedString::from(format!("{}-minus", id_str));
    let id_plus = SharedString::from(format!("{}-plus", id_str));
    let id_drag = SharedString::from(format!("{}-drag", id_str));
    let slider_id_string = id_str.to_string();
    let slider_id_for_check = slider_id_string.clone();

    let surface = colors::settings_group_surface();
    let entity_increment = cx.entity();
    let entity_decrement = cx.entity();
    let on_change_minus = on_change.clone();
    let on_change_plus = on_change.clone();
    let on_change_drag = on_change.clone();
    let on_change_keys = on_change.clone();
    let on_change_increment = on_change.clone();
    let on_change_decrement = on_change;

    let page = ((max - min) / 10.0).max(step);
    let current = Cell::new(value);
    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let value = current.get();
        let candidate = match event.keystroke.key.as_str() {
            "left" | "down" => value - step,
            "right" | "up" => value + step,
            "pageup" => value + page,
            "pagedown" => value - page,
            "home" => min,
            "end" => max,
            _ => return,
        };
        cx.stop_propagation();
        let snapped = snap_slider_value(candidate, min, max, step);
        if (snapped - value).abs() > f32::EPSILON {
            current.set(snapped);
            on_change_keys(snapped, cx);
        }
    });

    let track_height = 8.0_f32;
    let thumb_size = 18.0_f32;

    keyboard_control(
        div()
            .id(id_root)
            .role(Role::Slider)
            .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
            .aria_orientation(Orientation::Horizontal)
            .aria_value(SharedString::from(display_value.clone()))
            .aria_numeric_value(value as f64)
            .aria_min_numeric_value(min as f64)
            .aria_max_numeric_value(max as f64)
            .aria_numeric_value_step(step as f64),
        surface,
        cx,
        None,
    )
    .rounded(Radii::sm())
    .on_key_down(key_listener)
    .on_a11y_action(AccessibleAction::Increment, move |_data, _window, app| {
        entity_increment.update(app, |_this, cx| on_change_increment(next_value, cx));
    })
    .on_a11y_action(AccessibleAction::Decrement, move |_data, _window, app| {
        entity_decrement.update(app, |_this, cx| on_change_decrement(prev_value, cx));
    })
    .flex()
    .items_center()
    .gap_3()
    .w_full()
    .min_w_0()
    .child(
        div()
            .id(id_minus)
            .w(px(28.0))
            .h(px(28.0))
            .rounded(Radii::md())
            .bg(colors::bg_surface())
            .border_1()
            .border_color(colors::border())
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(colors::bg_hover()))
            .active(|s| s.bg(colors::bg_hover()))
            .on_completed_click(cx.listener(move |_this, _event, _window, cx| {
                on_change_minus(prev_value, cx);
            }))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(colors::interactive_foreground())
                    .child("\u{2212}"),
            ),
    )
    .child(
        div()
            .id(id_str.clone())
            .flex_1()
            .min_w(px(128.0))
            .h(px(thumb_size))
            .relative()
            .cursor(CursorStyle::PointingHand)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px((thumb_size - track_height) / 2.0))
                    .h(px(track_height))
                    .rounded(px(track_height / 2.0))
                    .bg(colors::bg_elevated()),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px((thumb_size - track_height) / 2.0))
                    .h(px(track_height))
                    .w(relative(normalized))
                    .rounded(px(track_height / 2.0))
                    .bg(colors::accent()),
            )
            .child(
                div()
                    .id(id_drag)
                    .absolute()
                    .inset_0()
                    .on_drag(
                        SliderDrag {
                            slider_id: slider_id_string,
                        },
                        |drag, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| drag.clone())
                        },
                    )
                    .on_drag_move(cx.listener(
                        move |_this, event: &DragMoveEvent<SliderDrag>, _window, cx| {
                            let drag_state = event.drag(cx);
                            if drag_state.slider_id != slider_id_for_check {
                                return;
                            }
                            let bounds = event.bounds;
                            let mouse_x = event.event.position.x;
                            let relative_x = mouse_x - bounds.origin.x;
                            let width = bounds.size.width;
                            let norm = (relative_x / width).clamp(0.0, 1.0);
                            let raw_value = min + norm * (max - min);
                            let snapped = ((raw_value - min) / step).round() * step + min;
                            on_change_drag(snapped.clamp(min, max), cx);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _event, _window, cx| {
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(relative(normalized))
                    .ml(px(-thumb_size / 2.0))
                    .w(px(thumb_size))
                    .h(px(thumb_size))
                    .rounded_full()
                    .bg(colors::white_alpha(1.0))
                    .border_2()
                    .border_color(colors::accent())
                    .shadow_sm(),
            ),
    )
    .child(
        div()
            .id(id_plus)
            .w(px(28.0))
            .h(px(28.0))
            .rounded(Radii::md())
            .bg(colors::bg_surface())
            .border_1()
            .border_color(colors::border())
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(colors::bg_hover()))
            .active(|s| s.bg(colors::bg_hover()))
            .on_completed_click(cx.listener(move |_this, _event, _window, cx| {
                on_change_plus(next_value, cx);
            }))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(colors::interactive_foreground())
                    .child("+"),
            ),
    )
    .child(
        div()
            .min_w(px(52.0))
            .px_2()
            .py_1()
            .rounded(Radii::sm())
            .bg(colors::bg_surface())
            .border_1()
            .border_color(colors::border())
            .text_center()
            .text_size(px(SettingsMetrics::LABEL_TEXT))
            .font_weight(FontWeight::MEDIUM)
            .text_color(colors::text_primary())
            .child(display_value),
    )
}

const COLOR_HUE_SEGMENTS: usize = 6;

/// Shared full-spectrum color editor used by Theme and Highlights settings.
///
/// The spectrum, hue, and opacity surfaces update continuously while dragging.
/// Their backend setters use the shared debounced settings save path so live
/// feedback does not translate into one disk write per pointer event.
pub fn color_editor<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    id_prefix: &str,
    persisted_value: Option<&str>,
    options: ColorEditorOptions<'_>,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    render_color_editor(id_prefix, persisted_value, options, None, cx, on_change)
}

struct ColorSettingRow {
    id: SharedString,
    label: String,
    help: Option<String>,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
}

/// Compact trigger in the row; the spectrum and code editor span the group below.
pub fn color_setting_row<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    id: impl Into<SharedString>,
    label: &str,
    help: Option<&str>,
    id_prefix: &str,
    persisted_value: Option<&str>,
    options: ColorEditorOptions<'_>,
    modified: bool,
    reset: Option<AnyElement>,
    stacked: bool,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    let row = ColorSettingRow {
        id: id.into(),
        label: label.to_string(),
        help: help.map(str::to_string),
        modified,
        reset,
        stacked,
    };
    render_color_editor(
        id_prefix,
        persisted_value,
        options,
        Some(row),
        cx,
        on_change,
    )
}

fn render_color_editor<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    id_prefix: &str,
    persisted_value: Option<&str>,
    options: ColorEditorOptions<'_>,
    row: Option<ColorSettingRow>,
    cx: &mut Context<T>,
    on_change: F,
) -> Stateful<Div> {
    let compact = row.is_some();
    let alpha_mode = options.alpha_mode;
    let key = color_editor_key(id_prefix, cx);
    let fallback = default_editor_color(options.default_value, alpha_mode);
    let state = color_editor_state(&key, persisted_value, fallback, alpha_mode);
    let (hue, saturation, value) = state.draft.to_hsv();
    let is_alpha_target = alpha_mode.allows_alpha();
    let is_expanded = state.expanded;
    let is_clear = state.is_clear;
    if !is_expanded {
        release_color_editor_input(&key);
    }
    let display_value = if is_clear {
        "No override".to_string()
    } else if compact && is_alpha_target {
        format!(
            "{} · {}%",
            state.draft.to_rgb_hex(),
            (state.draft.alpha as f32 / 2.55).round()
        )
    } else if compact {
        state.draft.to_rgb_hex()
    } else if is_alpha_target {
        format!(
            "{} · {}% opacity",
            state.draft.to_rgb_hex(),
            (state.draft.alpha as f32 / 2.55).round()
        )
    } else {
        state.draft.to_rgb_hex()
    };
    let editor_key_for_toggle = key.clone();
    let editor_key_for_accessible = key.clone();
    let trigger_focus = control_focus(&format!("{key}-color-trigger"), cx);
    let content_focus = control_focus(&format!("{key}-color-body"), cx).tab_stop(false);
    let pointer_focus = trigger_focus.clone();
    let pointer_content = content_focus.clone();
    let accessible_focus = trigger_focus.clone();
    let accessible_content = content_focus.clone();
    let entity = cx.entity();
    let trigger_label = row.as_ref().map_or_else(
        || display_value.clone(),
        |row| {
            if is_alpha_target && !is_clear {
                format!(
                    "{}: {}, {}% opacity",
                    row.label,
                    state.draft.to_rgb_hex(),
                    (state.draft.alpha as f32 / 2.55).round()
                )
            } else {
                format!("Edit {}: {display_value}", row.label)
            }
        },
    );
    let surface = colors::settings_group_surface();

    div()
        .id(SharedString::from(format!("{key}-color-editor")))
        .w_full()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .map(|root| {
            let trigger = keyboard_control(
                div()
                    .id(SharedString::from(format!("{key}-color-trigger")))
                    .role(Role::Button)
                    .aria_label(SharedString::from(trigger_label))
                    .aria_expanded(is_expanded),
                surface,
             cx, Some(trigger_focus.clone()))
                .w_full()
                .track_focus(&trigger_focus)
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .child(crate::components::ui::accessibility::focus_marker(trigger_focus.clone()))
                .flex()
                .items_center()
                .gap_2()
                .p(px(if compact { Spacing::SM } else { SettingsMetrics::CARD_PADDING }))
                .rounded(Radii::lg())
                .bg(if is_expanded {
                    colors::bg_elevated()
                } else {
                    colors::bg_surface()
                })
                .border_1()
                .border_color(if is_expanded {
                    colors::accent().opacity(0.7)
                } else {
                    colors::border()
                })
                .cursor_pointer()
                .hover(|style| style.bg(colors::bg_hover()).border_color(colors::accent().opacity(0.55)))
                .on_activate(
                    cx.listener(move |_this, _event: &ClickEvent, window, cx| {
                        if is_expanded && pointer_content.contains_focused(window, cx) {
                            window.focus(&pointer_focus, cx);
                        }
                        toggle_color_editor(&editor_key_for_toggle);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .on_a11y_action(AccessibleAction::Click, move |_, window, app| {
                    entity.update(app, |_, cx| {
                        if is_expanded && accessible_content.contains_focused(window, cx) {
                            window.focus(&accessible_focus, cx);
                        }
                        toggle_color_editor(&editor_key_for_accessible);
                        cx.notify();
                    });
                })
                .child(color_swatch(state.draft, !is_clear, if compact { 20.0 } else { 30.0 }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(SettingsMetrics::LABEL_GAP))
                        .child(
                            div()
                                .text_size(px(SettingsMetrics::LABEL_TEXT))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if is_clear {
                                    colors::text_muted()
                                } else {
                                    colors::text_primary()
                                })
                                .whitespace_nowrap().child(display_value),
                        )
                        .when(!compact, |column| column.child(
                            div()
                                .text_size(px(SettingsMetrics::HELPER_TEXT))
                                .text_color(colors::text_muted())
                                .child(if is_alpha_target {
                                    "Click to choose color and opacity"
                                } else {
                                    "Click to choose a color"
                                }),
                        )),
                )
                .child(
                    div()
                        .px(px(SettingsMetrics::COMPACT_BUTTON_PADDING_X))
                        .py(px(SettingsMetrics::COMPACT_BUTTON_PADDING_Y))
                        .rounded(Radii::md())
                        .bg(colors::accent().opacity(0.12))
                        .text_size(px(SettingsMetrics::HELPER_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::accent_text())
                        .child(if is_expanded { "Close" } else { "Edit" }),
                );
            let trigger = if let Some(row) = row {
                setting_row_with_control_width(row.id, &row.label, row.help.as_deref(), trigger,
                    row.modified, row.reset, row.stacked, SettingsMetrics::CALM_COLOR_CONTROL_WIDTH).into_any_element()
            } else { trigger.into_any_element() };
            root.child(trigger)
        })
        .when(is_expanded, |root| {
            let editor_key_for_default = key.clone();
            let editor_key_for_clear = key.clone();
            let on_change_for_default = on_change.clone();
            let on_change_for_clear = on_change.clone();
            let default_value = options.default_value.map(str::to_string);

            root.child(
                div()
                    .id(SharedString::from(format!("{key}-color-body")))
                    .track_focus(&content_focus)
                    .w_full()
                    .min_w(px(SettingsMetrics::COLOR_EDITOR_MIN_WIDTH))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .rounded(Radii::xl())
                    .bg(colors::bg_elevated())
                    .border_1()
                    .border_color(colors::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(SettingsMetrics::LABEL_GAP))
                                    .child(
                                        div()
                                            .text_size(px(SettingsMetrics::LABEL_TEXT))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors::text_primary())
                                            .child("Color spectrum"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(SettingsMetrics::HELPER_TEXT))
                                            .text_color(colors::text_muted())
                                            .child("Drag anywhere in the color field, then adjust hue or opacity below."),
                                    ),
                            )
                            .child(color_swatch(state.draft, !state.is_clear, 38.0)),
                    )
                    .child(color_spectrum_plane(
                        &key,
                        hue,
                        saturation,
                        value,
                        state.draft.alpha,
                        alpha_mode,
                        cx,
                        on_change.clone(),
                    ))
                    .child(color_hue_rail(
                        &key,
                        hue,
                        saturation,
                        value,
                        state.draft.alpha,
                        alpha_mode,
                        cx,
                        on_change.clone(),
                    ))
                    .when(is_alpha_target, |content| {
                        content.child(color_alpha_rail(
                            &key,
                            hue,
                            saturation,
                            value,
                            state.draft.alpha,
                            alpha_mode,
                            cx,
                            on_change.clone(),
                        ))
                    })
                    .child(color_text_field(
                        &key,
                        "Color code",
                        &state.value_text,
                        state.invalid,
                        alpha_mode,
                        cx,
                        on_change.clone(),
                    ))
                    .child(color_message_preview(state.draft, !state.is_clear))
                    .when(!options.presets.is_empty(), |content| {
                        content.child(color_presets(
                            &key,
                            options.presets,
                            alpha_mode,
                            cx,
                            on_change.clone(),
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .when_some(default_value, |actions, default_value| {
                                let on_change = on_change_for_default.clone();
                                actions.child(color_editor_action(
                                    SharedString::from(format!(
                                        "{}-color-reset",
                                        editor_key_for_default
                                    )),
                                    "Reset",
                                    false,
                                    cx,
                                    move |window, cx| {
                                        let Some(color) =
                                            ColorValue::from_text(&default_value, alpha_mode)
                                        else {
                                            return;
                                        };
                                        set_color_editor_value(
                                            &editor_key_for_default,
                                            color,
                                            false,
                                            alpha_mode,
                                        );
                                        if let Some(value) = commit_color_editor(
                                            &editor_key_for_default,
                                            alpha_mode,
                                        ) {
                                            on_change(value, cx);
                                        }
                                        sync_color_editor_input_text(
                                            &editor_key_for_default,
                                            false,
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                            })
                            .when(options.allow_clear, |actions| {
                                actions.child(color_editor_action(
                                    SharedString::from(format!(
                                        "{}-color-clear",
                                        editor_key_for_clear
                                    )),
                                    "Clear override",
                                    true,
                                    cx,
                                    move |window, cx| {
                                        clear_color_editor(&editor_key_for_clear);
                                        if let Some(value) =
                                            commit_color_editor(&editor_key_for_clear, alpha_mode)
                                        {
                                            on_change_for_clear(value, cx);
                                        }
                                        sync_color_editor_input_text(
                                            &editor_key_for_clear,
                                            false,
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                            }),
                    ),
            )
        })
}

/// Secondary action inside the expanded color editor (Reset, Clear override).
///
/// Kept separate from [`action_button_keyed`] because these sit inside the
/// editor's own container and read at helper-text size, but they carry the same
/// button contract: role, label, focus ring, Space/Enter, and a Click action.
fn color_editor_action<T: 'static, F: Fn(&mut Window, &mut Context<T>) + 'static>(
    id: SharedString,
    label: &'static str,
    danger: bool,
    cx: &mut Context<T>,
    action: F,
) -> impl IntoElement + use<T, F> {
    let surface = colors::bg_elevated();
    let action = Rc::new(action);
    let pointer = Rc::clone(&action);
    let keyboard = Rc::clone(&action);

    let mouse_listener = cx.listener(move |_this, _event: &ClickEvent, window, cx| {
        (*pointer)(window, cx);
        cx.stop_propagation();
        cx.notify();
    });
    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, window, cx| {
        if is_activation_key(event) {
            cx.stop_propagation();
            (*keyboard)(window, cx);
            cx.notify();
        }
    });

    keyboard_control(
        div()
            .id(id)
            .role(Role::Button)
            .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
            .aria_label(SharedString::new_static(label)),
        surface,
        cx,
        None,
    )
    .px(px(SettingsMetrics::COMPACT_BUTTON_PADDING_X))
    .py(px(SettingsMetrics::COMPACT_BUTTON_PADDING_Y))
    .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
    .flex()
    .items_center()
    .rounded(Radii::md())
    .bg(colors::bg_surface())
    .border_1()
    .border_color(colors::border())
    .cursor_pointer()
    .hover(move |style| {
        style.bg(colors::bg_hover()).border_color(if danger {
            colors::signal_red()
        } else {
            colors::accent()
        })
    })
    .on_completed_click(mouse_listener)
    .on_key_down(key_listener)
    .child(
        div()
            .text_size(px(SettingsMetrics::HELPER_TEXT))
            .text_color(colors::text_secondary())
            .child(label),
    )
}

fn color_editor_key<T: 'static>(id_prefix: &str, cx: &Context<T>) -> String {
    format!("{:?}:{id_prefix}", cx.entity_id())
}

/// Reset only this editor's draft, retaining its expansion and input focus.
pub fn reset_color_editor_draft<T: 'static>(
    id_prefix: &str,
    persisted_value: Option<&str>,
    fallback: Option<&str>,
    mode: ColorAlphaMode,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let key = color_editor_key(id_prefix, cx);
    let color = persisted_value
        .and_then(|value| ColorValue::from_persisted(value, mode))
        .unwrap_or_else(|| default_editor_color(fallback, mode));
    with_color_editor_state(&key, |state| {
        state.persisted_value = persisted_value.map(str::to_string);
        state.draft = color;
        state.is_clear = persisted_value.is_none();
        state.dirty = false;
        state.invalid = false;
        refresh_color_editor_text(state, mode);
    });
    sync_color_editor_input_text(&key, true, window, cx);
}

fn default_editor_color(value: Option<&str>, mode: ColorAlphaMode) -> ColorValue {
    value
        .and_then(|value| ColorValue::from_text(value, mode))
        .unwrap_or_else(|| ColorValue {
            alpha: mode.default_alpha(),
            ..ColorValue::OPAQUE_FALLBACK
        })
}

fn color_editor_state(
    key: &str,
    persisted_value: Option<&str>,
    fallback: ColorValue,
    mode: ColorAlphaMode,
) -> ColorEditorState {
    let persisted_value = persisted_value.map(str::to_string);
    let mut guard = COLOR_EDITOR_STATE.lock_or_recover();
    let states = guard.get_or_insert_with(HashMap::new);
    let state = states.entry(key.to_string()).or_insert_with(|| {
        let color = persisted_value
            .as_deref()
            .and_then(|value| ColorValue::from_persisted(value, mode))
            .unwrap_or(fallback);
        ColorEditorState {
            persisted_value: persisted_value.clone(),
            draft: color,
            is_clear: persisted_value.is_none(),
            expanded: false,
            active_field: None,
            value_text: color.to_rgb_hex(),
            dirty: false,
            invalid: false,
        }
    });

    if state.persisted_value != persisted_value && !state.dirty && state.active_field.is_none() {
        let color = persisted_value
            .as_deref()
            .and_then(|value| ColorValue::from_persisted(value, mode))
            .unwrap_or(fallback);
        state.persisted_value = persisted_value.clone();
        state.draft = color;
        state.is_clear = persisted_value.is_none();
        state.value_text = color.to_rgb_hex();
        state.invalid = false;
    }

    state.clone()
}

fn with_color_editor_state(key: &str, update: impl FnOnce(&mut ColorEditorState)) {
    let mut guard = COLOR_EDITOR_STATE.lock_or_recover();
    let Some(state) = guard.as_mut().and_then(|states| states.get_mut(key)) else {
        return;
    };
    update(state);
}

fn refresh_color_editor_text(state: &mut ColorEditorState, _mode: ColorAlphaMode) {
    state.value_text = state.draft.to_rgb_hex();
}

fn toggle_color_editor(key: &str) {
    let mut collapsed_keys: Vec<String> = Vec::new();
    {
        let mut guard = COLOR_EDITOR_STATE.lock_or_recover();
        let Some(states) = guard.as_mut() else {
            return;
        };
        let should_expand = states
            .get(key)
            .map(|state| !state.expanded)
            .unwrap_or(false);
        if should_expand {
            let scope = key.rsplit_once(':').map(|(scope, _)| scope).unwrap_or(key);
            for (other_key, other) in states.iter_mut() {
                let other_scope = other_key
                    .rsplit_once(':')
                    .map(|(other_scope, _)| other_scope)
                    .unwrap_or(other_key);
                if other_key != key && other_scope == scope {
                    if other.expanded {
                        collapsed_keys.push(other_key.clone());
                    }
                    other.expanded = false;
                    other.active_field = None;
                    other.invalid = false;
                }
            }
        }
        if let Some(state) = states.get_mut(key) {
            if state.expanded && !should_expand {
                collapsed_keys.push(key.to_string());
            }
            state.expanded = should_expand;
            state.active_field = None;
            state.invalid = false;
        }
    }

    for collapsed in collapsed_keys {
        release_color_editor_input(&collapsed);
    }
}

fn set_color_editor_value(key: &str, value: ColorValue, is_clear: bool, mode: ColorAlphaMode) {
    with_color_editor_state(key, |state| {
        state.draft = value;
        state.is_clear = is_clear;
        state.dirty = true;
        state.invalid = false;
        refresh_color_editor_text(state, mode);
    });
}

fn clear_color_editor(key: &str) {
    with_color_editor_state(key, |state| {
        state.is_clear = true;
        state.dirty = true;
        state.invalid = false;
    });
}

/// Returns `Some` only for a local value the user explicitly committed.
fn commit_color_editor(key: &str, mode: ColorAlphaMode) -> Option<Option<String>> {
    let mut guard = COLOR_EDITOR_STATE.lock_or_recover();
    let state = guard.as_mut()?.get_mut(key)?;
    if !state.dirty {
        return None;
    }

    state.dirty = false;
    state.invalid = false;
    let value = (!state.is_clear).then(|| state.draft.to_hex(mode));
    if state.persisted_value == value {
        refresh_color_editor_text(state, mode);
        return None;
    }
    state.persisted_value = value.clone();
    refresh_color_editor_text(state, mode);
    Some(value)
}

thread_local! {
    static COLOR_EDITOR_INPUTS: RefCell<HashMap<String, ColorEditorInput>> =
        RefCell::new(HashMap::new());
}

struct ColorEditorInput {
    input: Entity<InputState>,
    /// Retained so the field keeps reporting focus/typing/commit back into the
    /// shared color-editor state; dropping it would silently stop the editor.
    _subscription: Subscription,
}

fn color_editor_input(key: &str) -> Option<Entity<InputState>> {
    COLOR_EDITOR_INPUTS.with(|inputs| inputs.borrow().get(key).map(|entry| entry.input.clone()))
}

fn release_color_editor_input(key: &str) {
    let removed = COLOR_EDITOR_INPUTS.with(|inputs| {
        let is_empty = inputs.borrow().is_empty();
        if is_empty {
            return None;
        }
        inputs.borrow_mut().remove(key)
    });
    drop(removed);
}

fn color_editor_draft_text(key: &str) -> String {
    COLOR_EDITOR_STATE
        .lock_or_recover()
        .as_ref()
        .and_then(|states| states.get(key))
        .map(|state| state.value_text.clone())
        .unwrap_or_default()
}

/// Adopt `text` as the draft color. Returns whether it parsed; invalid text is
/// kept verbatim so the user can keep editing it.
fn apply_color_editor_text(key: &str, text: &str, mode: ColorAlphaMode) -> bool {
    let mut applied = false;
    with_color_editor_state(key, |state| {
        state.value_text = text.to_string();
        match ColorValue::from_text(text, mode) {
            Some(mut color) => {
                if mode.allows_alpha() && !color_text_has_explicit_alpha(text) {
                    color.alpha = state.draft.alpha;
                }
                state.draft = color;
                state.is_clear = false;
                state.dirty = true;
                state.invalid = false;
                applied = true;
            }
            None => state.invalid = !text.trim().is_empty(),
        }
    });
    applied
}

/// Push the canonical draft text back into the editor.
///
/// Skipped while the field owns focus unless `force` is set, so live spectrum
/// or preset changes never fight with in-progress typing.
fn sync_color_editor_input_text<T: 'static>(
    key: &str,
    force: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let Some(input) = color_editor_input(key) else {
        return;
    };
    let text = color_editor_draft_text(key);
    let (is_current, is_focused) = {
        let state = input.read(cx);
        (
            state.value() == text,
            state.focus_handle().is_focused(window),
        )
    };
    if is_current || (!force && is_focused) {
        return;
    }
    input.update(cx, |input, cx| {
        input.set_value(text, window, cx);
    });
}

fn ensure_color_editor_input<T, F>(
    key: &str,
    mode: ColorAlphaMode,
    on_change: F,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<InputState>
where
    T: 'static,
    F: Fn(Option<String>, &mut Context<T>) + Clone + 'static,
{
    if let Some(existing) = color_editor_input(key) {
        return existing;
    }

    let seed = color_editor_draft_text(key);
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .with_placeholder("#RRGGBB or rgb(…)")
            .with_value(seed)
    });

    let event_key = key.to_string();
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |_this, input, event: &InputEvent, window, cx| {
            let key = event_key.as_str();
            match event {
                InputEvent::Focus => {
                    with_color_editor_state(key, |state| {
                        state.active_field = Some(ColorEditorField::Value);
                        state.invalid = false;
                    });
                    cx.notify();
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    with_color_editor_state(key, |state| {
                        state.dirty |= state.value_text != text;
                        state.value_text = text;
                        state.invalid = false;
                    });
                    cx.notify();
                }
                InputEvent::PressEnter | InputEvent::Blur => {
                    let is_blur = matches!(event, InputEvent::Blur);
                    let text = input.read(cx).value().to_string();
                    let should_commit = COLOR_EDITOR_STATE
                        .lock_or_recover()
                        .as_ref()
                        .and_then(|states| states.get(key))
                        .is_some_and(|state| {
                            super::editor_state::color_input_should_commit(
                                is_blur,
                                state.dirty,
                                &state.value_text,
                                &text,
                            )
                        });
                    if is_blur && !should_commit {
                        with_color_editor_state(key, |state| state.active_field = None);
                        cx.notify();
                        return;
                    }
                    if apply_color_editor_text(key, &text, mode) {
                        if let Some(value) = commit_color_editor(key, mode) {
                            on_change(value, cx);
                        }
                    } else if is_blur {
                        with_color_editor_state(key, |state| {
                            super::editor_state::discard_invalid_color_input(
                                &mut state.dirty,
                                &mut state.invalid,
                            );
                            refresh_color_editor_text(state, mode);
                        });
                    }
                    if is_blur {
                        with_color_editor_state(key, |state| state.active_field = None);
                    }
                    sync_color_editor_input_text(key, true, window, cx);
                    cx.notify();
                }
            }
        },
    );

    COLOR_EDITOR_INPUTS.with(|inputs| {
        inputs.borrow_mut().insert(
            key.to_string(),
            ColorEditorInput {
                input: input.clone(),
                _subscription: subscription,
            },
        );
    });

    input
}

fn color_swatch(color: ColorValue, visible: bool, size: f32) -> impl IntoElement {
    div()
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .relative()
        .overflow_hidden()
        .rounded(px((size / 5.0).max(5.0)))
        .border_1()
        .border_color(colors::border())
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_wrap()
                .children((0..16).map(|index| {
                    let light = (index / 4 + index % 4) % 2 == 0;
                    div().w(relative(0.25)).h(relative(0.25)).bg(if light {
                        hsla(0.0, 0.0, 0.78, 1.0)
                    } else {
                        hsla(0.0, 0.0, 0.48, 1.0)
                    })
                })),
        )
        .when(visible, |swatch| {
            swatch.child(div().absolute().inset_0().bg(color.to_hsla()))
        })
}

/// Diameter of the saturation/brightness cursor; it is centered on the picked point.
const COLOR_PLANE_CURSOR_SIZE: f32 = 12.0;
/// Diameter of the hue and alpha rail thumbs; they are centered on the picked value.
const COLOR_RAIL_THUMB_SIZE: f32 = 14.0;

fn color_spectrum_plane<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    key: &str,
    hue: f32,
    saturation: f32,
    value: f32,
    alpha: u8,
    mode: ColorAlphaMode,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let bounds_for_prepaint = Rc::clone(&bounds);
    let bounds_for_click = Rc::clone(&bounds);
    let key_for_click = key.to_string();
    let key_for_drag = key.to_string();
    let key_for_check = key.to_string();
    let key_for_keys = key.to_string();
    let on_change_for_click = on_change.clone();
    let on_change_for_keys = on_change.clone();
    let on_change_for_drag = on_change;
    let hue_color = ColorValue::from_hsv(hue, 1.0, 1.0, 255).to_hsla();
    let surface = colors::bg_elevated();

    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, window, cx| {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let delta = if modifiers.shift { 0.1 } else { 0.02 };
        let (mut next_saturation, mut next_value) = (saturation, value);
        match event.keystroke.key.as_str() {
            "left" => next_saturation -= delta,
            "right" => next_saturation += delta,
            "up" => next_value += delta,
            "down" => next_value -= delta,
            "home" => next_saturation = 0.0,
            "end" => next_saturation = 1.0,
            _ => return,
        }
        cx.stop_propagation();
        let color = ColorValue::from_hsv(
            hue,
            next_saturation.clamp(0.0, 1.0),
            next_value.clamp(0.0, 1.0),
            alpha,
        );
        set_color_editor_value(&key_for_keys, color, false, mode);
        if let Some(committed) = commit_color_editor(&key_for_keys, mode) {
            on_change_for_keys(committed, cx);
        }
        sync_color_editor_input_text(&key_for_keys, false, window, cx);
        cx.notify();
    });

    keyboard_control(
        div()
            .on_children_prepainted(move |child_bounds, _window, _cx| {
                bounds_for_prepaint.set(child_bounds.first().copied());
            })
            .id(SharedString::from(format!("{key}-color-plane")))
            .role(Role::Group)
            .aria_label("Color field")
            .aria_description("Arrow keys adjust saturation and brightness")
            .aria_value(SharedString::from(format!(
                "{}% saturation, {}% brightness",
                (saturation * 100.0).round(),
                (value * 100.0).round()
            ))),
        surface,
        cx,
        None,
    )
    .w_full()
    .h(px(164.0))
    .relative()
    .overflow_hidden()
    .rounded(Radii::lg())
    .border_1()
    .border_color(colors::border())
    .cursor_pointer()
    .on_key_down(key_listener)
    .on_drag(
        ColorControlDrag {
            editor_key: key_for_drag,
            control: ColorControlKind::Spectrum,
        },
        |drag, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| drag.clone())
        },
    )
    .on_drag_move(cx.listener(
        move |_this, event: &DragMoveEvent<ColorControlDrag>, window, cx| {
            let drag = event.drag(cx);
            if drag.editor_key != key_for_check || drag.control != ColorControlKind::Spectrum {
                return;
            }
            let relative_x: f32 = (event.event.position.x - event.bounds.origin.x).into();
            let relative_y: f32 = (event.event.position.y - event.bounds.origin.y).into();
            let width: f32 = event.bounds.size.width.into();
            let height: f32 = event.bounds.size.height.into();
            let next_saturation = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
            let next_value = 1.0 - (relative_y / height.max(1.0)).clamp(0.0, 1.0);
            let color = ColorValue::from_hsv(hue, next_saturation, next_value, alpha);
            set_color_editor_value(&key_for_check, color, false, mode);
            if let Some(value) = commit_color_editor(&key_for_check, mode) {
                on_change_for_drag(value, cx);
            }
            sync_color_editor_input_text(&key_for_check, false, window, cx);
            cx.notify();
        },
    ))
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(move |_this, event: &MouseDownEvent, window, cx| {
            let Some(bounds) = bounds_for_click.get() else {
                return;
            };
            let relative_x: f32 = (event.position.x - bounds.origin.x).into();
            let relative_y: f32 = (event.position.y - bounds.origin.y).into();
            let width: f32 = bounds.size.width.into();
            let height: f32 = bounds.size.height.into();
            let next_saturation = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
            let next_value = 1.0 - (relative_y / height.max(1.0)).clamp(0.0, 1.0);
            let color = ColorValue::from_hsv(hue, next_saturation, next_value, alpha);
            set_color_editor_value(&key_for_click, color, false, mode);
            if let Some(value) = commit_color_editor(&key_for_click, mode) {
                on_change_for_click(value, cx);
            }
            sync_color_editor_input_text(&key_for_click, false, window, cx);
            cx.stop_propagation();
            cx.notify();
        }),
    )
    .child(div().absolute().inset_0().bg(hue_color))
    .child(div().absolute().inset_0().bg(linear_gradient(
        90.0,
        linear_color_stop(hsla(0.0, 0.0, 1.0, 1.0), 0.0),
        linear_color_stop(hsla(0.0, 0.0, 1.0, 0.0), 1.0),
    )))
    .child(div().absolute().inset_0().bg(linear_gradient(
        180.0,
        linear_color_stop(hsla(0.0, 0.0, 0.0, 0.0), 0.0),
        linear_color_stop(hsla(0.0, 0.0, 0.0, 1.0), 1.0),
    )))
    .child(
        div()
            .absolute()
            .left(relative(saturation.clamp(0.0, 1.0)))
            .top(relative((1.0 - value).clamp(0.0, 1.0)))
            .ml(px(-COLOR_PLANE_CURSOR_SIZE / 2.0))
            .mt(px(-COLOR_PLANE_CURSOR_SIZE / 2.0))
            .size(px(COLOR_PLANE_CURSOR_SIZE))
            .rounded_full()
            .border_2()
            .border_color(hsla(0.0, 0.0, 1.0, 0.95))
            .bg(hsla(0.0, 0.0, 0.0, 0.18))
            .shadow_sm(),
    )
}

fn color_hue_rail<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    key: &str,
    hue: f32,
    saturation: f32,
    value: f32,
    alpha: u8,
    mode: ColorAlphaMode,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let bounds_for_prepaint = Rc::clone(&bounds);
    let bounds_for_click = Rc::clone(&bounds);
    let key_for_click = key.to_string();
    let key_for_drag = key.to_string();
    let key_for_check = key.to_string();
    let key_for_keys = key.to_string();
    let on_change_for_click = on_change.clone();
    let on_change_for_keys = on_change.clone();
    let on_change_for_drag = on_change;
    let surface = colors::bg_elevated();

    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, window, cx| {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let delta = if modifiers.shift { 10.0 } else { 1.0 } / 360.0;
        let next_hue = match event.keystroke.key.as_str() {
            "left" | "down" => hue - delta,
            "right" | "up" => hue + delta,
            "home" => 0.0,
            "end" => 1.0 - 1.0 / 360.0,
            _ => return,
        };
        cx.stop_propagation();
        let color = ColorValue::from_hsv(next_hue.clamp(0.0, 1.0), saturation, value, alpha);
        set_color_editor_value(&key_for_keys, color, false, mode);
        if let Some(committed) = commit_color_editor(&key_for_keys, mode) {
            on_change_for_keys(committed, cx);
        }
        sync_color_editor_input_text(&key_for_keys, false, window, cx);
        cx.notify();
    });

    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .justify_between()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .text_color(colors::text_muted())
                .child("Hue")
                .child(format!("{}°", (hue * 360.0).round())),
        )
        .child(
            keyboard_control(
                div()
                    .on_children_prepainted(move |child_bounds, _window, _cx| {
                        bounds_for_prepaint.set(child_bounds.first().copied());
                    })
                    .id(SharedString::from(format!("{key}-color-hue")))
                    .role(Role::Slider)
                    .a11y_synthetic_children(
                        crate::components::ui::accessibility::apply_description,
                    )
                    .aria_label("Hue")
                    .aria_orientation(Orientation::Horizontal)
                    .aria_numeric_value((hue * 360.0).round() as f64)
                    .aria_min_numeric_value(0.0)
                    .aria_max_numeric_value(359.0)
                    .aria_numeric_value_step(1.0),
                surface,
                cx,
                None,
            )
            .w_full()
            .h(px(20.0))
            .relative()
            .overflow_hidden()
            .rounded(Radii::md())
            .border_1()
            .border_color(colors::border())
            .cursor_pointer()
            .on_key_down(key_listener)
            .on_drag(
                ColorControlDrag {
                    editor_key: key_for_drag,
                    control: ColorControlKind::Hue,
                },
                |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            )
            .on_drag_move(cx.listener(
                move |_this, event: &DragMoveEvent<ColorControlDrag>, window, cx| {
                    let drag = event.drag(cx);
                    if drag.editor_key != key_for_check || drag.control != ColorControlKind::Hue {
                        return;
                    }
                    let relative_x: f32 = (event.event.position.x - event.bounds.origin.x).into();
                    let width: f32 = event.bounds.size.width.into();
                    let next_hue = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
                    let color = ColorValue::from_hsv(next_hue, saturation, value, alpha);
                    set_color_editor_value(&key_for_check, color, false, mode);
                    if let Some(value) = commit_color_editor(&key_for_check, mode) {
                        on_change_for_drag(value, cx);
                    }
                    sync_color_editor_input_text(&key_for_check, false, window, cx);
                    cx.notify();
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, event: &MouseDownEvent, window, cx| {
                    let Some(bounds) = bounds_for_click.get() else {
                        return;
                    };
                    let relative_x: f32 = (event.position.x - bounds.origin.x).into();
                    let width: f32 = bounds.size.width.into();
                    let next_hue = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
                    let color = ColorValue::from_hsv(next_hue, saturation, value, alpha);
                    set_color_editor_value(&key_for_click, color, false, mode);
                    if let Some(value) = commit_color_editor(&key_for_click, mode) {
                        on_change_for_click(value, cx);
                    }
                    sync_color_editor_input_text(&key_for_click, false, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .children((0..COLOR_HUE_SEGMENTS).map(|index| {
                        let start = index as f32 / COLOR_HUE_SEGMENTS as f32;
                        let end = (index + 1) as f32 / COLOR_HUE_SEGMENTS as f32;
                        let start_color = ColorValue::from_hsv(start, 1.0, 1.0, 255).to_hsla();
                        let end_color = ColorValue::from_hsv(end, 1.0, 1.0, 255).to_hsla();
                        div().flex_1().h_full().bg(linear_gradient(
                            90.0,
                            linear_color_stop(start_color, 0.0),
                            linear_color_stop(end_color, 1.0),
                        ))
                    })),
            )
            .child(
                div()
                    .absolute()
                    .left(relative(hue.clamp(0.0, 1.0)))
                    .top(px(2.0))
                    .ml(px(-COLOR_RAIL_THUMB_SIZE / 2.0))
                    .size(px(COLOR_RAIL_THUMB_SIZE))
                    .rounded_full()
                    .bg(ColorValue::from_hsv(hue, 1.0, 1.0, 255).to_hsla())
                    .border_2()
                    .border_color(hsla(0.0, 0.0, 0.0, 0.7)),
            ),
        )
}

fn color_alpha_rail<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    key: &str,
    hue: f32,
    saturation: f32,
    value: f32,
    alpha: u8,
    mode: ColorAlphaMode,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let bounds_for_prepaint = Rc::clone(&bounds);
    let bounds_for_click = Rc::clone(&bounds);
    let key_for_click = key.to_string();
    let key_for_drag = key.to_string();
    let key_for_check = key.to_string();
    let key_for_keys = key.to_string();
    let on_change_for_click = on_change.clone();
    let on_change_for_keys = on_change.clone();
    let on_change_for_drag = on_change;
    let transparent = ColorValue::from_hsv(hue, saturation, value, 0).to_hsla();
    let opaque = ColorValue::from_hsv(hue, saturation, value, 255).to_hsla();
    let surface = colors::bg_elevated();

    let key_listener = cx.listener(move |_this, event: &KeyDownEvent, window, cx| {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let delta = if modifiers.shift { 26 } else { 3 };
        let next_alpha = match event.keystroke.key.as_str() {
            "left" | "down" => alpha.saturating_sub(delta),
            "right" | "up" => alpha.saturating_add(delta),
            "home" => 0,
            "end" => 255,
            _ => return,
        };
        cx.stop_propagation();
        let color = ColorValue::from_hsv(hue, saturation, value, next_alpha);
        set_color_editor_value(&key_for_keys, color, false, mode);
        if let Some(committed) = commit_color_editor(&key_for_keys, mode) {
            on_change_for_keys(committed, cx);
        }
        sync_color_editor_input_text(&key_for_keys, false, window, cx);
        cx.notify();
    });

    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .justify_between()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .text_color(colors::text_muted())
                .child("Opacity")
                .child(format!("{}%", (alpha as f32 / 2.55).round())),
        )
        .child(
            keyboard_control(
                div()
                    .on_children_prepainted(move |child_bounds, _window, _cx| {
                        bounds_for_prepaint.set(child_bounds.first().copied());
                    })
                    .id(SharedString::from(format!("{key}-color-alpha")))
                    .role(Role::Slider)
                    .a11y_synthetic_children(
                        crate::components::ui::accessibility::apply_description,
                    )
                    .aria_label("Opacity")
                    .aria_orientation(Orientation::Horizontal)
                    .aria_numeric_value((alpha as f32 / 2.55).round() as f64)
                    .aria_min_numeric_value(0.0)
                    .aria_max_numeric_value(100.0)
                    .aria_numeric_value_step(1.0),
                surface,
                cx,
                None,
            )
            .w_full()
            .h(px(22.0))
            .relative()
            .overflow_hidden()
            .rounded(Radii::md())
            .border_1()
            .border_color(colors::border())
            .cursor_pointer()
            .on_key_down(key_listener)
            .on_drag(
                ColorControlDrag {
                    editor_key: key_for_drag,
                    control: ColorControlKind::Opacity,
                },
                |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            )
            .on_drag_move(cx.listener(
                move |_this, event: &DragMoveEvent<ColorControlDrag>, window, cx| {
                    let drag = event.drag(cx);
                    if drag.editor_key != key_for_check || drag.control != ColorControlKind::Opacity
                    {
                        return;
                    }
                    let relative_x: f32 = (event.event.position.x - event.bounds.origin.x).into();
                    let width: f32 = event.bounds.size.width.into();
                    let next_alpha =
                        ((relative_x / width.max(1.0)).clamp(0.0, 1.0) * 255.0).round() as u8;
                    let color = ColorValue::from_hsv(hue, saturation, value, next_alpha);
                    set_color_editor_value(&key_for_check, color, false, mode);
                    if let Some(value) = commit_color_editor(&key_for_check, mode) {
                        on_change_for_drag(value, cx);
                    }
                    sync_color_editor_input_text(&key_for_check, false, window, cx);
                    cx.notify();
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, event: &MouseDownEvent, window, cx| {
                    let Some(bounds) = bounds_for_click.get() else {
                        return;
                    };
                    let relative_x: f32 = (event.position.x - bounds.origin.x).into();
                    let width: f32 = bounds.size.width.into();
                    let next_alpha =
                        ((relative_x / width.max(1.0)).clamp(0.0, 1.0) * 255.0).round() as u8;
                    let color = ColorValue::from_hsv(hue, saturation, value, next_alpha);
                    set_color_editor_value(&key_for_click, color, false, mode);
                    if let Some(value) = commit_color_editor(&key_for_click, mode) {
                        on_change_for_click(value, cx);
                    }
                    sync_color_editor_input_text(&key_for_click, false, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(checkerboard(colors::bg_surface(), 6.0)),
            )
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.0,
                linear_color_stop(transparent, 0.0),
                linear_color_stop(opaque, 1.0),
            )))
            .child(
                div()
                    .absolute()
                    .left(relative(alpha as f32 / 255.0))
                    .top(px(3.0))
                    .ml(px(-COLOR_RAIL_THUMB_SIZE / 2.0))
                    .size(px(COLOR_RAIL_THUMB_SIZE))
                    .rounded_full()
                    .bg(ColorValue::from_hsv(hue, saturation, value, alpha).to_hsla())
                    .border_2()
                    .border_color(hsla(0.0, 0.0, 0.0, 0.7)),
            ),
        )
}

fn color_text_field<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    key: &str,
    label: &'static str,
    value: &str,
    invalid: bool,
    mode: ColorAlphaMode,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let key_for_focus = key.to_string();
    let input = color_editor_input(key);
    let display_value = value.to_string();

    div()
        .id(SharedString::from(format!("{key}-color-code-field")))
        .flex_1()
        .min_w(px(180.0))
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::LABEL_GAP))
        .child(
            div()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if invalid {
                    colors::signal_red()
                } else {
                    colors::text_muted()
                })
                .child(label),
        )
        .map(|field| match input {
            Some(input) => field.child(
                div()
                    .w_full()
                    .h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
                    .flex()
                    .items_center()
                    .text_size(px(SettingsMetrics::LABEL_TEXT))
                    .font_family("monospace")
                    .child(Input::new(&input).accessibility_label(label)),
            ),
            None => field.child(
                div()
                    .w_full()
                    .h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
                    .px(px(SettingsMetrics::INPUT_PADDING_X))
                    .flex()
                    .items_center()
                    .rounded(Radii::sm())
                    .bg(colors::bg_surface())
                    .border_1()
                    .border_color(colors::border())
                    .cursor_text()
                    .text_size(px(SettingsMetrics::LABEL_TEXT))
                    .font_family("monospace")
                    .text_color(colors::text_primary())
                    .child(display_value),
            ),
        })
        .when(invalid, |field| {
            field.child(
                div()
                    .text_size(px(SettingsMetrics::HELPER_TEXT))
                    .text_color(colors::signal_red())
                    .child("Enter a hex value such as #7C3AED, or rgb(124, 58, 237)."),
            )
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_this, _event, window, cx| {
                let input =
                    ensure_color_editor_input(&key_for_focus, mode, on_change.clone(), window, cx);
                let handle = input.read(cx).focus_handle().clone();
                if !handle.is_focused(window) {
                    window.focus(&handle, cx);
                }
                cx.stop_propagation();
                cx.notify();
            }),
        )
}

fn color_text_has_explicit_alpha(value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("rgba(") {
        return true;
    }
    let hex = value.strip_prefix('#').unwrap_or(value);
    hex.len() == 8 && hex.chars().all(|character| character.is_ascii_hexdigit())
}

fn color_message_preview(color: ColorValue, visible: bool) -> impl IntoElement {
    let mut preview_color = color.to_hsla();
    preview_color.a = if visible { preview_color.a } else { 0.0 };
    div()
        .flex()
        .items_center()
        .gap_2()
        .p_2()
        .rounded(Radii::md())
        .bg(preview_color)
        .border_1()
        .border_color(colors::border())
        .child(
            div()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child("Preview"),
        )
        .child(
            div()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .text_color(colors::text_secondary())
                .child(if visible {
                    "This is how the tint reads behind a chat message."
                } else {
                    "Choose a color to preview this override."
                }),
        )
}

fn color_presets<T: 'static, F: Fn(Option<String>, &mut Context<T>) + Clone + 'static>(
    key: &str,
    presets: &[(&str, &str)],
    mode: ColorAlphaMode,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(SettingsMetrics::HELPER_TEXT))
                .text_color(colors::text_muted())
                .child("Presets"),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(
                    presets
                        .iter()
                        .enumerate()
                        .filter_map(|(index, (name, value))| {
                            let color = ColorValue::from_text(value, mode)?;
                            let key = key.to_string();
                            let key_for_keys = key.clone();
                            let on_change = on_change.clone();
                            let on_change_for_keys = on_change.clone();
                            let name = (*name).to_string();
                            let element_id = SharedString::from(format!("{key}-preset-{index}"));
                            let mouse_listener =
                                cx.listener(move |_this, _event: &ClickEvent, window, cx| {
                                    set_color_editor_value(&key, color, false, mode);
                                    if let Some(value) = commit_color_editor(&key, mode) {
                                        on_change(value, cx);
                                    }
                                    sync_color_editor_input_text(&key, false, window, cx);
                                    cx.stop_propagation();
                                    cx.notify();
                                });
                            let key_listener =
                                cx.listener(move |_this, event: &KeyDownEvent, window, cx| {
                                    if !is_activation_key(event) {
                                        return;
                                    }
                                    cx.stop_propagation();
                                    set_color_editor_value(&key_for_keys, color, false, mode);
                                    if let Some(value) = commit_color_editor(&key_for_keys, mode) {
                                        on_change_for_keys(value, cx);
                                    }
                                    sync_color_editor_input_text(&key_for_keys, false, window, cx);
                                    cx.notify();
                                });
                            Some(
                                keyboard_control(
                                    div()
                                        .id(element_id)
                                        .role(Role::Button)
                                        .a11y_synthetic_children(
                                            crate::components::ui::accessibility::apply_description,
                                        )
                                        .aria_label(SharedString::from(name.clone())),
                                    colors::bg_elevated(),
                                    cx,
                                    None,
                                )
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_2()
                                .py_1()
                                .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
                                .rounded(Radii::md())
                                .bg(colors::bg_surface())
                                .border_1()
                                .border_color(colors::border())
                                .cursor_pointer()
                                .hover(|style| {
                                    style.bg(colors::bg_hover()).border_color(colors::accent())
                                })
                                .on_completed_click(mouse_listener)
                                .on_key_down(key_listener)
                                .child(color_swatch(color, true, 14.0))
                                .child(
                                    div()
                                        .text_size(px(SettingsMetrics::HELPER_TEXT))
                                        .text_color(colors::interactive_foreground())
                                        .child(name),
                                ),
                            )
                        }),
                ),
        )
}

fn parse_css_rgba(value: &str) -> Option<ColorValue> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let (name, inner) = if let Some(inner) = lower.strip_prefix("rgba(") {
        ("rgba", inner.strip_suffix(')')?)
    } else {
        let inner = lower.strip_prefix("rgb(")?;
        ("rgb", inner.strip_suffix(')')?)
    };
    let parts = inner.split(',').map(str::trim).collect::<Vec<_>>();
    if (name == "rgb" && parts.len() != 3) || (name == "rgba" && parts.len() != 4) {
        return None;
    }
    let red = parse_rgb_channel(parts[0])?;
    let green = parse_rgb_channel(parts[1])?;
    let blue = parse_rgb_channel(parts[2])?;
    let alpha = if name == "rgba" {
        parse_alpha_channel(parts[3])?
    } else {
        255
    };
    Some(ColorValue {
        red,
        green,
        blue,
        alpha,
    })
}

fn parse_rgb_channel(value: &str) -> Option<u8> {
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent.trim().parse::<f32>().ok()?;
        return (0.0..=100.0)
            .contains(&percent)
            .then(|| (percent * 2.55).round() as u8);
    }
    let channel = value.parse::<f32>().ok()?;
    (0.0..=255.0)
        .contains(&channel)
        .then(|| channel.round() as u8)
}

fn parse_alpha_channel(value: &str) -> Option<u8> {
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent.trim().parse::<f32>().ok()?;
        return (0.0..=100.0)
            .contains(&percent)
            .then(|| (percent * 2.55).round() as u8);
    }
    let alpha = value.parse::<f32>().ok()?;
    if (0.0..=1.0).contains(&alpha) {
        Some((alpha * 255.0).round() as u8)
    } else {
        (0.0..=255.0).contains(&alpha).then(|| alpha.round() as u8)
    }
}

fn hsv_to_hsla(hue: f32, saturation: f32, value: f32, alpha: u8) -> Hsla {
    let lightness = value * (1.0 - saturation / 2.0);
    let hsl_saturation = if lightness <= f32::EPSILON || lightness >= 1.0 - f32::EPSILON {
        0.0
    } else {
        (value - lightness) / lightness.min(1.0 - lightness)
    };
    hsla(
        hue.rem_euclid(1.0),
        hsl_saturation.clamp(0.0, 1.0),
        lightness.clamp(0.0, 1.0),
        alpha as f32 / 255.0,
    )
}
