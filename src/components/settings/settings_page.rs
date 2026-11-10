//! Settings content-page primitives.
//!
//! A settings page is a single scrolling column of *direct anchor blocks*: one
//! immediate child per subsection, each opening with a level-two heading that
//! carries a stable element id. Rendering the blocks as immediate children of
//! the scrolling element is what makes them addressable — GPUI's
//! [`ScrollHandle`] indexes the scrolling element's direct children, so
//! `scroll_to_top_of_item(index)` and the scroll-spy read below both work off
//! the same ordering. Wrapping the page in one nested `div` (the previous
//! shape) collapses that list to a single child and makes anchors unreachable.
//!
//! This module owns the shared page furniture — breadcrumb header, subsection
//! headings, block assembly, and the scroll-spy contract — so the modal and the
//! popout host identical content hierarchy and only differ in window chrome.

use crate::components::settings::settings_navigation::{
    SettingsLocation, SettingsSectionId, SettingsSubsectionId, location_breadcrumb,
    presentation_location, subsection_descriptor,
};
use crate::components::settings::widgets::SettingsMetrics;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Element id of a subsection anchor block.
///
/// Derived from the descriptor slugs rather than the enum discriminants, so the
/// id stays stable across reorderings and matches the slug written into
/// persisted navigation preferences.
pub fn anchor_element_id(
    section: SettingsSectionId,
    subsection: SettingsSubsectionId,
) -> SharedString {
    let section_slug = crate::components::settings::settings_navigation::section_slug(section);
    let subsection_slug = subsection_descriptor(section, subsection)
        .map(|descriptor| descriptor.slug)
        .unwrap_or("subsection");
    SharedString::from(format!("settings-anchor-{section_slug}-{subsection_slug}"))
}

/// Element id of the single block a leaf page renders.
pub fn leaf_element_id(section: SettingsSectionId) -> SharedString {
    let section_slug = crate::components::settings::settings_navigation::section_slug(section);
    SharedString::from(format!("settings-page-{section_slug}"))
}

/// Live `Page › Anchor` breadcrumb for the fixed content header.
///
/// The category is deliberately omitted from the visual line (it is already the
/// expanded tree branch) but kept in the accessible label, so a screen-reader
/// user hears their full location without the sighted user reading three levels
/// of chrome on every page.
pub fn breadcrumb(location: SettingsLocation) -> impl IntoElement {
    let parts = location_breadcrumb(location);
    let accessible = parts.join(" \u{203a} ");
    let mut visible = parts.into_iter().skip(1);
    let page = visible.next().unwrap_or("Settings").to_string();
    let anchor = visible.next().map(str::to_string);

    div()
        .id("settings-breadcrumb")
        .role(Role::Heading)
        .aria_level(1)
        .aria_label(SharedString::from(accessible))
        .flex()
        .items_center()
        .gap(px(Spacing::XS_PLUS))
        .min_w_0()
        .child(
            div()
                .text_size(px(SettingsMetrics::TITLE_TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child(page),
        )
        .when_some(anchor, |header, anchor| {
            header
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(SettingsMetrics::HELPER_TEXT))
                        .text_color(colors::text_secondary())
                        .child("\u{203a}"),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_ellipsis()
                        .text_size(px(SettingsMetrics::LABEL_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_secondary())
                        .child(anchor),
                )
        })
}

/// Fixed content header bar. Hosts supply the breadcrumb (or an override such
/// as the no-results title) as its child.
pub fn content_header<C: IntoElement>(child: C) -> impl IntoElement + use<C> {
    div()
        .w_full()
        .flex_shrink_0()
        .h(px(SettingsMetrics::CONTENT_HEADER_HEIGHT))
        .px(px(SettingsMetrics::CONTENT_PADDING))
        .border_b_1()
        .border_color(colors::settings_rule(colors::bg_surface()))
        .flex()
        .items_center()
        .min_w_0()
        .child(child)
}

/// Level-two heading that opens an anchor block.
///
/// 14px semibold with a solid 2px accent rail and a 1px full-width rule, at a
/// minimum of 32px. The rail and rule are contrast-qualified, so a low-contrast
/// custom accent falls back to a readable foreground instead of leaving the
/// heading indistinguishable from the 13px group headings beneath it.
pub fn subsection_header(
    label: impl Into<SharedString>,
    element_id: impl Into<ElementId>,
    position: usize,
    count: usize,
) -> impl IntoElement {
    let label: SharedString = label.into();
    let surface = colors::bg_surface();

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id(element_id)
                .role(Role::Heading)
                .aria_level(2)
                .aria_label(label.clone())
                .aria_position_in_set(position)
                .aria_size_of_set(count)
                .w_full()
                .min_w_0()
                .min_h(px(SettingsMetrics::SUBSECTION_HEADER_MIN_HEIGHT))
                .flex()
                .items_center()
                .gap(px(Spacing::SM))
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(SettingsMetrics::SUBSECTION_RAIL_WIDTH))
                        .h(px(SettingsMetrics::SECTION_TITLE_TEXT))
                        .rounded(Radii::full())
                        .bg(colors::settings_selection_rail(surface)),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(SettingsMetrics::SECTION_TITLE_TEXT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::text_primary())
                        .child(label),
                ),
        )
        .child(div().w_full().h(px(1.0)).bg(colors::settings_rule(surface)))
}

/// One subsection of a settings page, ready to render as a direct anchor block.
pub struct SettingsSubsectionView {
    id: SettingsSubsectionId,
    label: SharedString,
    anchor_id: SharedString,
    body: AnyElement,
}

impl SettingsSubsectionView {
    /// Build a view from the descriptor tables, so the anchor's label, slug, and
    /// element id can never drift from the tree and search entries that point at
    /// it.
    pub fn new(
        section: SettingsSectionId,
        subsection: SettingsSubsectionId,
        body: impl IntoElement,
    ) -> Self {
        let label = subsection_descriptor(section, subsection)
            .map(|descriptor| descriptor.label)
            .unwrap_or("Settings");
        Self {
            id: subsection,
            label: SharedString::new_static(label),
            anchor_id: anchor_element_id(section, subsection),
            body: body.into_any_element(),
        }
    }
}

/// A settings page: an ordered list of subsection anchor blocks.
///
/// A page with fewer than two meaningful anchors is a leaf and renders one
/// unlabelled block, matching the tree, where such pages are plain selectable
/// rows with no chevron.
pub struct SettingsPage {
    section: SettingsSectionId,
    subsections: Vec<SettingsSubsectionView>,
    leaf: Option<AnyElement>,
    quiet: bool,
}

impl SettingsPage {
    pub fn new(section: SettingsSectionId) -> Self {
        Self {
            section,
            subsections: Vec::new(),
            leaf: None,
            quiet: false,
        }
    }

    /// A page leaf: one block, no subsection heading.
    pub fn leaf(section: SettingsSectionId, body: impl IntoElement) -> Self {
        Self {
            section,
            subsections: Vec::new(),
            leaf: Some(body.into_any_element()),
            quiet: false,
        }
    }

    /// The migrated page supplies its own group hierarchy.
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Append an anchor block.
    pub fn subsection(mut self, subsection: SettingsSubsectionId, body: impl IntoElement) -> Self {
        self.subsections
            .push(SettingsSubsectionView::new(self.section, subsection, body));
        self
    }

    /// Remove an anchor body so an existing renderer can be composed into a
    /// different destination without rebuilding its interactive controls.
    pub(crate) fn take_subsection(
        &mut self,
        subsection: SettingsSubsectionId,
    ) -> Option<AnyElement> {
        self.subsections
            .iter()
            .position(|view| view.id == subsection)
            .map(|index| self.subsections.remove(index).body)
    }

    pub(crate) fn replace_subsection_body(
        &mut self,
        subsection: SettingsSubsectionId,
        body: impl IntoElement,
    ) {
        if let Some(view) = self
            .subsections
            .iter_mut()
            .find(|view| view.id == subsection)
        {
            view.body = body.into_any_element();
        }
    }

    /// Remove the sole body from a leaf renderer for composition elsewhere.
    pub(crate) fn take_leaf(&mut self) -> Option<AnyElement> {
        self.leaf.take()
    }

    pub(crate) fn append_search_notice(
        &mut self,
        location: SettingsLocation,
        notice: impl IntoElement,
    ) {
        let index = location
            .subsection
            .and_then(|id| self.index_of(id))
            .or_else(|| (!self.subsections.is_empty() && self.leaf.is_none()).then_some(0));
        let body = if let Some(index) = index {
            Some(&mut self.subsections[index].body)
        } else {
            self.leaf.as_mut()
        };
        if let Some(body) = body {
            let old = std::mem::replace(body, div().into_any_element());
            *body = div()
                .flex()
                .flex_col()
                .gap(px(SettingsMetrics::GROUP_GAP))
                .child(old)
                .child(notice)
                .into_any_element();
        } else {
            self.leaf = Some(notice.into_any_element());
        }
    }

    /// Number of scrollable blocks, which is also the child count the page's
    /// [`ScrollHandle`] will index.
    pub fn block_count(&self) -> usize {
        if self.subsections.is_empty() {
            usize::from(self.leaf.is_some())
        } else {
            self.subsections.len()
        }
    }

    /// Anchor ids in render order.
    pub fn anchors(&self) -> impl Iterator<Item = SettingsSubsectionId> + '_ {
        self.subsections.iter().map(|view| view.id)
    }

    /// Block index of `subsection`, for [`ScrollHandle::scroll_to_top_of_item`].
    pub fn index_of(&self, subsection: SettingsSubsectionId) -> Option<usize> {
        let subsection = presentation_location(SettingsLocation::anchor(self.section, subsection))
            .subsection
            .expect("subsection presentation keeps an anchor");
        self.subsections
            .iter()
            .position(|view| view.id == subsection)
    }

    /// Anchor rendered at `index`, for reporting scroll-spy results back to the
    /// tree and breadcrumb.
    pub fn subsection_at(&self, index: usize) -> Option<SettingsSubsectionId> {
        self.subsections.get(index).map(|view| view.id)
    }

    /// Assert that the rendered blocks are exactly the anchors this page's
    /// descriptor declares, in declaration order.
    ///
    /// Tree rows, breadcrumb, anchor jumps, scroll-spy, and search counts all
    /// index the *descriptor*, while only the renderer decides what is actually
    /// laid out. If the two disagree, an anchor jump silently does nothing
    /// (`index_of` -> `None`) and scroll-spy reports the wrong location — both
    /// of which look like flaky UI rather than a broken table. This turns that
    /// class of drift into a loud debug-build failure at the moment the page is
    /// built.
    pub fn debug_assert_matches_descriptor(&self) {
        #[cfg(debug_assertions)]
        {
            use crate::components::settings::settings_navigation::section_descriptor;

            let Some(descriptor) = section_descriptor(self.section) else {
                return;
            };
            let built: Vec<Option<SettingsSubsectionId>> = self.anchors().map(Some).collect();
            let declared: Vec<Option<SettingsSubsectionId>> = descriptor
                .subsections
                .iter()
                .filter(|subsection| {
                    !crate::components::settings::settings_navigation::subsection_is_developer_only(
                        subsection.id,
                    ) || built.contains(&Some(subsection.id))
                })
                .map(|subsection| {
                    presentation_location(SettingsLocation::anchor(self.section, subsection.id))
                        .subsection
                })
                .fold(Vec::new(), |mut anchors, subsection| {
                    if !anchors.contains(&subsection) {
                        anchors.push(subsection);
                    }
                    anchors
                });
            assert_eq!(
                declared, built,
                "settings page '{}' renders anchors {built:?} but its descriptor declares {declared:?}",
                descriptor.slug
            );
        }
    }

    /// Render the page as direct anchor blocks.
    ///
    /// The caller must spread these as immediate children of the scrolling
    /// element (`.children(page.into_blocks())`); nesting them inside another
    /// container breaks anchor addressing and scroll-spy.
    pub fn into_blocks(self) -> Vec<AnyElement> {
        let section = self.section;
        let count = self.subsections.len();
        let quiet = self.quiet;
        if count == 0 {
            return self
                .leaf
                .map(|body| {
                    vec![
                        div()
                            .id(leaf_element_id(section))
                            .w_full()
                            .max_w(px(SettingsMetrics::CONTENT_MAX_WIDTH))
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(SettingsMetrics::GROUP_GAP))
                            .child(body)
                            .into_any_element(),
                    ]
                })
                .unwrap_or_default();
        }

        self.subsections
            .into_iter()
            .enumerate()
            .map(|(index, view)| {
                div()
                    .id(view.anchor_id.clone())
                    .w_full()
                    .max_w(px(SettingsMetrics::CONTENT_MAX_WIDTH))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(SettingsMetrics::GROUP_GAP))
                    .when(index > 0 && !quiet, |block| {
                        block.pt(px(SettingsMetrics::SUBSECTION_HEADER_GAP))
                    })
                    .when(!quiet, |block| {
                        block.child(subsection_header(
                            view.label.clone(),
                            SharedString::from(format!("{}-heading", view.anchor_id)),
                            index + 1,
                            count,
                        ))
                    })
                    .child(view.body)
                    .into_any_element()
            })
            .collect()
    }
}

/// Distance below the viewport top at which an anchor is considered active.
///
/// Slightly more than the subsection header height, so an anchor stays selected
/// while its own heading is still the top thing on screen.
const SCROLL_SPY_THRESHOLD_PX: f32 = 40.0;

/// Tolerance for "the page is scrolled all the way down". Sub-pixel layout and
/// the two-decimal rounding GPUI applies to the scroll maximum both land just
/// short of the exact maximum.
const SCROLL_MAX_EPSILON_PX: f32 = 1.0;

/// Per-page scroll state: the handle, the active anchor, and any programmatic
/// jump still waiting to be confirmed by a laid-out frame.
///
/// Held in memory for the surface's lifetime — offsets are session state and are
/// never persisted. Every mutator reports whether visible state changed so hosts
/// can keep `cx.notify()` change-gated.
pub struct SettingsPageScroll {
    handle: ScrollHandle,
    active: usize,
    pending: Option<usize>,
}

impl Default for SettingsPageScroll {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsPageScroll {
    pub fn new() -> Self {
        Self {
            handle: ScrollHandle::new(),
            active: 0,
            pending: None,
        }
    }

    /// Handle to hand to `track_scroll` on the scrolling element.
    pub fn handle(&self) -> &ScrollHandle {
        &self.handle
    }

    pub(crate) fn cancel_pending(&mut self) {
        self.pending = None;
    }

    /// Index of the anchor currently reported to the tree and breadcrumb.
    pub fn active(&self) -> usize {
        self.active
    }

    /// Adopt `index` without scrolling. Returns whether the selection changed.
    pub fn set_active(&mut self, index: usize) -> bool {
        if self.active == index {
            return false;
        }
        self.active = index;
        true
    }

    /// Jump to an anchor.
    ///
    /// Selection updates immediately so the tree and breadcrumb respond on the
    /// same frame as the click, and the target is marked pending so scroll-spy
    /// ignores the stale pre-jump layout until a laid-out frame confirms it.
    pub fn scroll_to(&mut self, index: usize) -> bool {
        self.handle.scroll_to_top_of_item(index);
        self.pending = Some(index);
        self.set_active(index)
    }

    /// Read the laid-out scroll position and adopt the anchor it implies.
    ///
    /// Returns whether the active anchor changed, so callers notify only on a
    /// real change rather than on every wheel event. Call this at most once per
    /// frame; the underlying bounds only update during prepaint, so extra reads
    /// cannot produce new information.
    pub fn observe(&mut self, count: usize) -> bool {
        if count == 0 {
            self.pending = None;
            return self.set_active(0);
        }

        let last = count - 1;
        let viewport = self.handle.bounds();
        let threshold = viewport.top() - self.handle.offset().y + px(SCROLL_SPY_THRESHOLD_PX);

        let mut candidate = 0;
        for index in 0..count {
            let Some(bounds) = self.handle.bounds_for_item(index) else {
                break;
            };
            if bounds.top() <= threshold {
                candidate = index;
            } else {
                break;
            }
        }

        let max_offset = self.handle.max_offset().y;
        if max_offset > px(0.0) && -self.handle.offset().y >= max_offset - px(SCROLL_MAX_EPSILON_PX)
        {
            candidate = last;
        }

        if let Some(pending) = self.pending {
            if candidate == pending {
                self.pending = None;
            } else {
                return false;
            }
        }

        self.set_active(candidate.min(last))
    }
}
