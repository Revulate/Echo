//! Signed application update status, changelog, download, and install controls.

use crate::backend::updates::{self, UpdateStateKind};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SettingsPageUi, button, labelled_toggle, setting_reset, setting_row, setting_target,
    settings_defaults, settings_group, settings_group_contents, settings_summary_disclosure,
};
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::*;

const AUTO_CHECK_UPDATES: SettingDescriptor = SettingDescriptor {
    id: "updates.auto-check",
    label: "Automatically check for stable updates",
    aliases: &[
        "auto update",
        "manifest",
        "signature",
        "update origin",
        "startup",
    ],
    control_id: "toggle-auto-update-check",
    disclosures: &[],
};

/// Updates › Update checks.
pub const UPDATES_CHECKS_SETTINGS: &[SettingDescriptor] = &[AUTO_CHECK_UPDATES];

const UPDATE_STATUS: SettingDescriptor = SettingDescriptor {
    id: "updates.current-release",
    label: "Current release",
    aliases: &[
        "version",
        "changelog",
        "download update",
        "install",
        "restart",
        "check now",
    ],
    control_id: "update-status",
    disclosures: &[],
};

/// Updates › Current release.
pub const UPDATES_RELEASE_SETTINGS: &[SettingDescriptor] = &[UPDATE_STATUS];

pub const UPDATES_GENERAL_SETTINGS: &[SettingDescriptor] = &[AUTO_CHECK_UPDATES, UPDATE_STATUS];

/// Render updates as compact primary rows with release metadata on demand.
pub fn render_updates_section<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> SettingsPage {
    let snapshot = updates::snapshot();
    let auto_check = crate::backend::settings::get_auto_check_updates();
    let auto_check_default = settings_defaults().auto_check_updates;
    let details_state = ui.disclosure("general-update-details", cx);
    let stacked = ui.stacked;

    SettingsPage::new(SettingsSectionId::Updates).subsection(
        SettingsSubsectionId::UpdateChecks,
        settings_group(
            "general-updates",
            "Updates",
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(setting_target(
                    UPDATE_STATUS,
                    setting_row(
                        UPDATE_STATUS.id,
                        UPDATE_STATUS.label,
                        Some(&release_status_summary(&snapshot)),
                        render_update_action(&snapshot, cx),
                        false,
                        None,
                        stacked,
                    ),
                ))
                .child(setting_target(
                    AUTO_CHECK_UPDATES,
                    crate::components::settings::widgets::setting_compact_row(
                        AUTO_CHECK_UPDATES.id,
                        AUTO_CHECK_UPDATES.label,
                        Some("Checks the signed stable manifest after startup."),
                        labelled_toggle(
                            AUTO_CHECK_UPDATES.control_id,
                            AUTO_CHECK_UPDATES.label,
                            auto_check,
                            cx,
                            |enabled, cx| {
                                crate::backend::settings::set_auto_check_updates(enabled);
                                if enabled {
                                    updates::start_automatic_check();
                                }
                                cx.notify();
                            },
                        ),
                        auto_check != auto_check_default,
                        (auto_check != auto_check_default).then(|| {
                            setting_reset(
                                "reset-auto-update-check",
                                AUTO_CHECK_UPDATES.label,
                                cx,
                                |cx| {
                                    let enabled = settings_defaults().auto_check_updates;
                                    crate::backend::settings::set_auto_check_updates(enabled);
                                    if enabled {
                                        updates::start_automatic_check();
                                    }
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(settings_summary_disclosure(
                    "general-update-details",
                    "Release details",
                    "Version and release notes",
                    &details_state,
                    cx,
                    move |_cx| {
                        settings_group_contents(
                            "general-update-details-content",
                            "Release details",
                            false,
                            render_update_details(&snapshot),
                        )
                    },
                )),
        ),
    )
}

fn render_update_action<T: 'static>(
    snapshot: &updates::UpdateSnapshot,
    cx: &mut Context<T>,
) -> AnyElement {
    match snapshot.kind {
        UpdateStateKind::Available => button(
            "Download",
            false,
            |cx| {
                updates::download_available_update();
                cx.notify();
            },
            cx,
        )
        .into_any_element(),
        UpdateStateKind::Ready => button(
            "Install & restart",
            false,
            |cx| match updates::launch_installer() {
                Ok(()) => cx.quit(),
                Err(error) => {
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                        message: "Could not start the updater".to_string(),
                        detail: Some(error),
                    });
                    cx.notify();
                }
            },
            cx,
        )
        .into_any_element(),
        UpdateStateKind::Checking | UpdateStateKind::Downloading => div().into_any_element(),
        _ => button(
            "Check now",
            false,
            |cx| {
                updates::check_for_updates(true);
                cx.notify();
            },
            cx,
        )
        .into_any_element(),
    }
}

fn render_update_details(snapshot: &updates::UpdateSnapshot) -> impl IntoElement {
    let status = update_status_label(snapshot.kind);
    let status_color = match snapshot.kind {
        UpdateStateKind::UpToDate | UpdateStateKind::Ready => colors::success(),
        UpdateStateKind::Available => colors::accent(),
        UpdateStateKind::Failed => colors::warning(),
        _ => colors::text_secondary(),
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(status_color)
                .child(status),
        )
        .when_some(snapshot.published_at.clone(), |el, published_at| {
            el.child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(format!("Published {published_at}")),
            )
        })
        .when_some(snapshot.detail.clone(), |el, detail| {
            el.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(colors::text_secondary())
                    .child(detail),
            )
        })
        .when_some(snapshot.changelog.clone(), |el, changelog| {
            el.child(
                div()
                    .w_full()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_secondary())
                            .child("WHAT'S NEW"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .whitespace_normal()
                            .text_color(colors::text_primary())
                            .child(changelog),
                    ),
            )
        })
}

fn update_status_label(kind: UpdateStateKind) -> &'static str {
    match kind {
        UpdateStateKind::Idle => "Not checked",
        UpdateStateKind::Checking => "Checking…",
        UpdateStateKind::UpToDate => "Up to date",
        UpdateStateKind::Available => "Update available",
        UpdateStateKind::Downloading => "Downloading and verifying…",
        UpdateStateKind::Ready => "Ready to install",
        UpdateStateKind::Failed => "Could not check for updates",
    }
}

fn release_summary(snapshot: &updates::UpdateSnapshot) -> String {
    let latest = snapshot.latest_version.as_ref().map(|version| {
        let label = if snapshot.kind == UpdateStateKind::UpToDate {
            "Latest stable"
        } else {
            "Available"
        };
        format!("  ·  {label}: Echo {version}")
    });
    format!(
        "Installed: Echo {}{}",
        snapshot.current_version,
        latest.unwrap_or_default()
    )
}

fn release_status_summary(snapshot: &updates::UpdateSnapshot) -> String {
    let summary = format!(
        "{} · {}",
        update_status_label(snapshot.kind),
        release_summary(snapshot)
    );
    if snapshot.kind == UpdateStateKind::Failed {
        match snapshot.detail.as_deref() {
            Some(detail) if !detail.is_empty() => format!("{summary} · {detail}"),
            _ => summary,
        }
    } else {
        summary
    }
}
