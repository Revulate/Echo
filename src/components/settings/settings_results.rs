//! The concrete rows shared by settings rendering and keyboard navigation.

pub(crate) const SETTINGS_STACKED_CONTENT_MAX_WIDTH_PX: f32 = 600.0;

pub(crate) fn available_settings_width(surface: f32, sidebar: f32, inset: f32) -> f32 {
    (surface - sidebar - 1.0 - inset * 2.0).max(0.0)
}

use super::settings_navigation::{
    SettingsLocation, SettingsSearchModel, SettingsSectionId, SettingsSubsectionId,
    canonical_section, section_slug, subsection_descriptor,
};

/// Browsing tree nodes and exact setting results while searching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsTreeNode {
    Page(SettingsSectionId),
    Anchor(SettingsSectionId, SettingsSubsectionId),
    Setting(
        SettingsSectionId,
        Option<SettingsSubsectionId>,
        &'static str,
    ),
}

impl SettingsTreeNode {
    /// Content destination for this node.
    pub fn location(self) -> Option<SettingsLocation> {
        match self {
            Self::Page(section) => Some(SettingsLocation::page(section)),
            Self::Anchor(section, subsection) => {
                Some(SettingsLocation::anchor(section, subsection))
            }
            Self::Setting(section, subsection, _) => Some(SettingsLocation {
                section,
                subsection,
            }),
        }
    }

    /// Zero-based depth, which is also the indentation level.
    pub fn level(self) -> usize {
        match self {
            Self::Page(_) => 0,
            Self::Anchor(..) => 1,
            Self::Setting(_, Some(_), _) => 2,
            Self::Setting(_, None, _) => 1,
        }
    }

    pub(crate) fn element_fragment(self) -> String {
        match self {
            Self::Page(id) => format!("page-{}", section_slug(id)),
            Self::Anchor(section, subsection) => format!(
                "anchor-{}-{}",
                section_slug(section),
                subsection_descriptor(section, subsection)
                    .map(|descriptor| descriptor.slug)
                    .unwrap_or("subsection")
            ),
            Self::Setting(_, _, id) => format!("setting-{id}"),
        }
    }
}

/// The node that represents a content location in the tree.
pub(crate) fn node_for_location(location: SettingsLocation) -> SettingsTreeNode {
    SettingsTreeNode::Page(canonical_section(location.section))
}

/// One visible tree row, resolved for a single frame.
///
/// Built from the search model plus the expansion set, and used by both the
/// renderer and the keyboard model so what the user sees and what the arrow keys
/// traverse are the same list.
#[derive(Clone, Debug)]
pub struct SettingsTreeRow {
    pub node: SettingsTreeNode,
    pub label: &'static str,
    pub icon: Option<&'static str>,
    pub level: usize,
    pub can_expand: bool,
    pub expanded: bool,
    /// This row is the current content location.
    pub selected: bool,
    /// This row is an ancestor of the current content location.
    pub on_path: bool,
    /// Search-only match count badge.
    pub badge: Option<usize>,
    /// Search-only secondary line ("Hide timestamps when live · +2").
    pub secondary: Option<String>,
    pub position: usize,
    pub set_size: usize,
}

pub(crate) fn visible_settings_rows(
    model: &SettingsSearchModel,
    location: SettingsLocation,
    selected_setting: Option<&str>,
) -> Vec<SettingsTreeRow> {
    let searching = model.is_searching();
    let sections: Vec<_> = model.tree().iter().collect();
    let mut rows = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let descriptor = section.descriptor;
        let current = canonical_section(location.section) == descriptor.id;
        rows.push(SettingsTreeRow {
            node: SettingsTreeNode::Page(descriptor.id),
            label: descriptor.label,
            icon: Some(descriptor.icon),
            level: 0,
            can_expand: false,
            expanded: false,
            selected: current
                && (!searching || (selected_setting.is_none() && location.subsection.is_none())),
            on_path: searching && current,
            badge: searching
                .then_some(section.counts.settings)
                .filter(|count| *count > 0),
            secondary: search_secondary(
                section.first_match_label(),
                section.additional_match_count(),
            ),
            position: index + 1,
            set_size: sections.len(),
        });
        if !searching {
            continue;
        }
        append_setting_rows(
            &mut rows,
            model,
            SettingsLocation::page(descriptor.id),
            selected_setting,
            1,
        );
        for (anchor_index, anchor) in section.subsections.iter().enumerate() {
            rows.push(SettingsTreeRow {
                node: SettingsTreeNode::Anchor(descriptor.id, anchor.descriptor.id),
                label: anchor.descriptor.label,
                icon: None,
                level: 1,
                can_expand: false,
                expanded: false,
                selected: current
                    && selected_setting.is_none()
                    && location.subsection == Some(anchor.descriptor.id),
                on_path: false,
                badge: Some(anchor.counts.settings).filter(|count| *count > 0),
                secondary: search_secondary(
                    anchor.first_match_label(),
                    anchor.additional_match_count(),
                ),
                position: anchor_index + 1,
                set_size: section.subsections.len(),
            });
            append_setting_rows(
                &mut rows,
                model,
                SettingsLocation::anchor(descriptor.id, anchor.descriptor.id),
                selected_setting,
                2,
            );
        }
    }
    rows
}

fn search_secondary(first_match: Option<&str>, additional_matches: usize) -> Option<String> {
    first_match.map(|label| match additional_matches {
        0 => label.to_string(),
        count => format!("{label} · +{count}"),
    })
}

fn append_setting_rows(
    rows: &mut Vec<SettingsTreeRow>,
    model: &SettingsSearchModel,
    location: SettingsLocation,
    selected: Option<&str>,
    level: usize,
) {
    let targets: Vec<_> = model
        .targets()
        .iter()
        .filter(|target| target.location == location)
        .collect();
    for (index, target) in targets.iter().enumerate() {
        rows.push(SettingsTreeRow {
            node: SettingsTreeNode::Setting(location.section, location.subsection, target.row_id()),
            label: target.setting.label,
            icon: None,
            level,
            can_expand: false,
            expanded: false,
            selected: selected == Some(target.row_id()),
            on_path: false,
            badge: None,
            secondary: target
                .is_developer_only()
                .then(|| "Developer only".to_string()),
            position: index + 1,
            set_size: targets.len(),
        });
    }
}

/// Return only the offset needed to reveal a target, retaining visible rows.
pub(crate) fn target_scroll_offset(
    viewport_top: f32,
    viewport_height: f32,
    row_top: f32,
    row_height: f32,
    offset: f32,
    max_offset: f32,
    inset: f32,
) -> Option<f32> {
    if viewport_height <= 0.0 {
        return None;
    }
    let height = row_height.min((viewport_height - inset * 2.0).max(0.0));
    let top = row_top + offset;
    let bottom = top + height;
    let viewport_bottom = viewport_top + viewport_height;
    if top >= viewport_top && bottom <= viewport_bottom {
        return None;
    }
    let next = if top < viewport_top {
        viewport_top + inset - row_top
    } else {
        viewport_bottom - inset - row_top - height
    }
    .clamp(-max_offset.max(0.0), 0.0);
    (next != offset).then_some(next)
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;
    #[test]
    fn row_stacking_uses_content_width_across_the_sidebar_transition() {
        for (surface, sidebar) in [(600.0, 180.0), (760.0, 180.0), (761.0, 200.0)] {
            assert!(
                super::available_settings_width(surface, sidebar, 16.0)
                    <= super::SETTINGS_STACKED_CONTENT_MAX_WIDTH_PX
            );
        }
        assert!(
            super::available_settings_width(976.0, 200.0, 16.0)
                > super::SETTINGS_STACKED_CONTENT_MAX_WIDTH_PX
        );
    }

    use super::*;

    #[test]
    fn appearance_rows_keep_page_anchor_counts_and_exact_hit() {
        let model = SettingsSearchModel::compute("appearance", false);
        let rows = visible_settings_rows(
            &model,
            SettingsLocation::page(SettingsSectionId::Theme),
            None,
        );
        let page = rows
            .iter()
            .find(|row| row.node == SettingsTreeNode::Page(SettingsSectionId::Theme))
            .unwrap();
        assert_eq!(page.label, "Appearance");
        assert!(page.badge.is_some_and(|count| count > 0));
        assert_eq!(page.secondary.as_deref(), Some("Tab appearance"));
        let anchor = rows
            .iter()
            .find(|row| {
                row.node
                    == SettingsTreeNode::Anchor(
                        SettingsSectionId::Theme,
                        SettingsSubsectionId::AppearanceOptions,
                    )
            })
            .unwrap();
        assert!(anchor.badge.is_some_and(|count| count > 0));
        assert_eq!(anchor.secondary.as_deref(), Some("Tab appearance"));
        let hit = rows
            .iter()
            .find(|row| row.label == "Tab appearance")
            .unwrap();
        assert!(matches!(hit.node, SettingsTreeNode::Setting(..)));
        assert!(hit.level > anchor.level);
    }

    #[test]
    fn only_the_exact_hit_is_selected() {
        let model = SettingsSearchModel::compute("Tab Appearance", false);
        let target = model
            .targets()
            .iter()
            .find(|target| target.row_id() == "appearance.tab-appearance")
            .unwrap();
        let rows = visible_settings_rows(&model, target.location, Some(target.row_id()));
        assert_eq!(rows.iter().filter(|row| row.selected).count(), 1);
        assert!(
            rows.iter()
                .any(|row| row.selected && matches!(row.node, SettingsTreeNode::Setting(..)))
        );
    }

    #[test]
    fn page_and_anchor_keep_the_multi_match_search_summary() {
        let model = SettingsSearchModel::compute("streamlink", false);
        let rows = visible_settings_rows(
            &model,
            SettingsLocation::page(SettingsSectionId::Connections),
            None,
        );
        for node in [
            SettingsTreeNode::Page(SettingsSectionId::Connections),
            SettingsTreeNode::Anchor(
                SettingsSectionId::Connections,
                SettingsSubsectionId::Players,
            ),
        ] {
            let row = rows.iter().find(|row| row.node == node).unwrap();
            assert_eq!(row.badge, Some(4));
            assert_eq!(row.secondary.as_deref(), Some("Streamlink path · +3"));
        }
    }

    #[test]
    fn normal_browsing_has_no_setting_rows() {
        let model = SettingsSearchModel::compute("", false);
        let rows = visible_settings_rows(&model, SettingsLocation::default(), None);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.node, SettingsTreeNode::Setting(..)))
        );
        assert_eq!(
            rows.iter().map(|row| row.label).collect::<Vec<_>>(),
            vec![
                "General",
                "Appearance",
                "Chat",
                "Highlights & notifications",
                "Moderation & filters",
                "Links & media",
                "Shortcuts & commands",
                "Advanced",
            ]
        );
        assert!(
            rows.iter()
                .all(|row| matches!(row.node, SettingsTreeNode::Page(_))
                    && row.level == 0
                    && !row.can_expand
                    && !row.expanded)
        );
        assert_eq!(
            rows.last().unwrap().node,
            SettingsTreeNode::Page(SettingsSectionId::Advanced)
        );
        let legacy = visible_settings_rows(
            &model,
            SettingsLocation::page(SettingsSectionId::Sounds),
            None,
        );
        assert_eq!(legacy.iter().filter(|row| row.selected).count(), 1);
        assert_eq!(
            legacy.iter().find(|row| row.selected).unwrap().label,
            "Highlights & notifications"
        );
    }

    #[test]
    fn fully_visible_rows_do_not_move() {
        assert_eq!(
            target_scroll_offset(100.0, 300.0, 200.0, 40.0, -80.0, 1000.0, 4.0),
            None
        );
        assert_eq!(
            target_scroll_offset(100.0, 300.0, 180.0, 40.0, -80.0, 1000.0, 4.0),
            None
        );
    }

    #[test]
    fn clipped_rows_move_only_as_far_as_needed_with_inset() {
        assert_eq!(
            target_scroll_offset(100.0, 300.0, 200.0, 40.0, -110.0, 1000.0, 4.0),
            Some(-96.0)
        );
        assert_eq!(
            target_scroll_offset(100.0, 300.0, 460.0, 60.0, -80.0, 1000.0, 4.0),
            Some(-124.0)
        );
        assert_eq!(
            target_scroll_offset(100.0, 300.0, 460.0, 60.0, -80.0, 100.0, 4.0),
            Some(-100.0)
        );
    }
}
