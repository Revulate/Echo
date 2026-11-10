//! Deterministic, non-persistent updater states for the live UI acceptance
//! harness.

use super::*;

/// Present deterministic, non-persistent updater states to the existing live
/// UI acceptance harness. These snapshots never enter the download/install
/// state machine, so fixture metadata cannot be installed accidentally.
pub(crate) fn show_up_to_date_for_benchmark() {
    set_benchmark_release(UpdateStateKind::UpToDate, compiled_version());
}

pub(crate) fn show_available_for_benchmark() -> String {
    let mut version = compiled_version();
    version.patch = version.patch.saturating_add(1);
    version.pre = semver::Prerelease::EMPTY;
    version.build = semver::BuildMetadata::EMPTY;
    let rendered = version.to_string();
    set_benchmark_release(UpdateStateKind::Available, version);
    rendered
}

pub(crate) fn clear_benchmark_snapshot() {
    *BENCHMARK_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    bump_state_generation();
}

fn set_benchmark_release(kind: UpdateStateKind, version: semver::Version) {
    let detail = match kind {
        UpdateStateKind::UpToDate => "You are running the latest stable Echo release",
        UpdateStateKind::Available => "A verified Echo update is ready to download",
        _ => unreachable!("benchmark release fixtures only cover stable UI states"),
    };
    let version_text = version.to_string();
    *BENCHMARK_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(UpdateState {
        kind,
        release: Some(VerifiedRelease {
            version,
            published_at: "2026-07-27T00:00:00Z".to_string(),
            changelog: format!(
                "Echo {version_text} update status and single-executable updater verification."
            ),
            package: VerifiedPackage {
                url: format!(
                    "{UPDATE_ORIGIN}/stable/{}/releases/{version_text}/echo.exe",
                    platform_id()
                ),
                bytes: 1,
                sha256: "0".repeat(64),
            },
            envelope: Arc::from([]),
        }),
        downloaded_path: None,
        detail: Some(detail.to_string()),
    });
    bump_state_generation();
}
