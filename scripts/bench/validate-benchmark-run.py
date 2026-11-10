#!/usr/bin/env python3
"""Validate benchmark artifact directories.

Cross-platform counterpart of scripts/bench/validate-benchmark-run.ps1. It also
validates the Echo chat replay run reports written by run-echo-profile.py.
"""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, load_json, utc_stamp, write_json


def add_check(checks: list[dict[str, Any]], name: str, passed: bool, detail: str = "", path: str = "") -> None:
    checks.append(
        {
            "name": name,
            "passed": bool(passed),
            "detail": detail,
            "path": path,
        }
    )


def text_present(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def nonnegative_int(value: Any) -> bool:
    try:
        return int(value) >= 0
    except (TypeError, ValueError):
        return False


def int_at_least(value: Any, minimum: int) -> bool:
    try:
        return int(value) >= minimum
    except (TypeError, ValueError):
        return False


def numeric(value: Any) -> bool:
    try:
        int(value)
        return True
    except (TypeError, ValueError):
        return False


def resolve_artifact_path(run_dir: Path, value: Any) -> Path | None:
    if not text_present(value):
        return None
    path = Path(str(value)).expanduser()
    if not path.is_absolute():
        path = run_dir / path
    return path


def test_json_artifact(checks: list[dict[str, Any]], run_dir: Path, name: str, path_value: Any) -> dict[str, Any] | None:
    path = resolve_artifact_path(run_dir, path_value)
    if path is None:
        add_check(checks, f"{name} path present", False, "Missing JSON artifact path.")
        return None
    add_check(checks, f"{name} file exists", path.exists(), path=str(path))
    if not path.exists():
        return None
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except Exception as exc:  # noqa: BLE001 - validator should capture parse failures.
        add_check(checks, f"{name} parses as JSON", False, str(exc), str(path))
        return None
    add_check(checks, f"{name} parses as JSON", True, path=str(path))
    return data


def test_ndjson_artifact(checks: list[dict[str, Any]], run_dir: Path, name: str, path_value: Any, minimum_records: int) -> None:
    path = resolve_artifact_path(run_dir, path_value)
    if path is None:
        add_check(checks, f"{name} path present", False, "Missing NDJSON artifact path.")
        return
    add_check(checks, f"{name} file exists", path.exists(), path=str(path))
    if not path.exists():
        return

    first_line = None
    last_line = None
    line_count = 0
    try:
        with path.open("r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                if first_line is None:
                    first_line = line
                last_line = line
                line_count += 1
    except Exception as exc:  # noqa: BLE001 - validator should capture read failures.
        add_check(checks, f"{name} can be read", False, str(exc), str(path))
        return

    add_check(
        checks,
        f"{name} has minimum records",
        line_count >= minimum_records,
        f"records={line_count} minimum={minimum_records}",
        str(path),
    )
    for label, line in (("first", first_line), ("last", last_line)):
        if line is None:
            add_check(checks, f"{name} {label} record present", False, path=str(path))
            continue
        try:
            json.loads(line)
        except Exception as exc:  # noqa: BLE001 - validator should capture parse failures.
            add_check(checks, f"{name} {label} record parses as JSON", False, str(exc), str(path))
        else:
            add_check(checks, f"{name} {label} record parses as JSON", True, path=str(path))


def test_raw_sample_artifact(
    checks: list[dict[str, Any]],
    run_dir: Path,
    name: str,
    path_value: Any,
    minimum_samples: int,
) -> None:
    path = resolve_artifact_path(run_dir, path_value)
    if path is None:
        add_check(checks, f"{name} path present", False, "Missing raw artifact path.")
        return
    add_check(checks, f"{name} raw file exists", path.exists(), path=str(path))
    if not path.exists():
        return
    try:
        with path.open("r", encoding="utf-8", newline="") as handle:
            rows = list(csv.DictReader(handle))
    except Exception as exc:  # noqa: BLE001 - validator should capture parse failures.
        add_check(checks, f"{name} raw file can be parsed", False, str(exc), str(path))
        return
    add_check(
        checks,
        f"{name} raw file has minimum samples",
        len(rows) >= minimum_samples,
        f"samples={len(rows)} minimum={minimum_samples}",
        str(path),
    )


def test_cache_snapshots(checks: list[dict[str, Any]], run_dir: Path, cache_snapshots: Any) -> None:
    if not isinstance(cache_snapshots, dict):
        add_check(checks, "cache snapshots present", False, "Missing raw_artifacts.cache_snapshots.")
        return
    add_check(checks, "cache snapshots registered", bool(cache_snapshots), f"count={len(cache_snapshots)}")
    for snapshot_name, path_value in cache_snapshots.items():
        snapshot = test_json_artifact(checks, run_dir, f"cache snapshot {snapshot_name}", path_value)
        if snapshot is None:
            continue
        schema_version = snapshot.get("schema_version")
        add_check(
            checks,
            f"cache snapshot {snapshot_name} has schema",
            int_at_least(schema_version, 1),
            f"schema_version={schema_version}",
        )
        for field in ("app", "cache_name", "phase", "cache_path"):
            value = snapshot.get(field)
            add_check(
                checks,
                f"cache snapshot {snapshot_name} has {field}",
                text_present(value),
                f"{field}={value}",
            )
        add_check(
            checks,
            f"cache snapshot {snapshot_name} has cache_exists",
            "cache_exists" in snapshot,
            f"cache_exists={snapshot.get('cache_exists')}",
        )
        for field in ("file_count", "directory_count", "total_bytes"):
            value = snapshot.get(field)
            add_check(
                checks,
                f"cache snapshot {snapshot_name} has nonnegative {field}",
                nonnegative_int(value),
                f"{field}={value}",
            )
        decision = snapshot.get("decision")
        add_check(
            checks,
            f"cache snapshot {snapshot_name} records non-comparison decision",
            text_present(decision) and "not a benchmark comparison" in str(decision),
            f"decision={decision}",
        )


def test_cache_comparisons(checks: list[dict[str, Any]], run_dir: Path, cache_comparisons: Any) -> None:
    if not isinstance(cache_comparisons, dict):
        add_check(checks, "cache comparisons present", False, "Missing raw_artifacts.cache_comparisons.")
        return
    add_check(checks, "cache comparisons registered", bool(cache_comparisons), f"count={len(cache_comparisons)}")
    for comparison_name, path_value in cache_comparisons.items():
        comparison = test_json_artifact(checks, run_dir, f"cache comparison {comparison_name}", path_value)
        if comparison is None:
            continue
        schema_version = comparison.get("schema_version")
        add_check(
            checks,
            f"cache comparison {comparison_name} has schema",
            int_at_least(schema_version, 1),
            f"schema_version={schema_version}",
        )
        for field in ("app", "cache_name", "before_snapshot", "after_snapshot", "before_phase", "after_phase"):
            value = comparison.get(field)
            add_check(
                checks,
                f"cache comparison {comparison_name} has {field}",
                text_present(value),
                f"{field}={value}",
            )
        for field in ("before_snapshot", "after_snapshot"):
            snapshot_path = resolve_artifact_path(run_dir, comparison.get(field))
            add_check(
                checks,
                f"cache comparison {comparison_name} {field} exists",
                snapshot_path is not None and snapshot_path.exists(),
                path=str(snapshot_path) if snapshot_path is not None else "",
            )
        delta = comparison.get("delta")
        add_check(checks, f"cache comparison {comparison_name} has delta", isinstance(delta, dict))
        if isinstance(delta, dict):
            for field in ("file_count", "directory_count", "total_bytes"):
                value = delta.get(field)
                add_check(
                    checks,
                    f"cache comparison {comparison_name} has numeric delta {field}",
                    numeric(value),
                    f"{field}={value}",
                )
        decision = comparison.get("decision")
        add_check(
            checks,
            f"cache comparison {comparison_name} records non-performance decision",
            text_present(decision) and "not an app performance comparison" in str(decision),
            f"decision={decision}",
        )


def test_render_fixture(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], minimum_records: int) -> None:
    test_ndjson_artifact(checks, run_dir, "render fixture", raw_artifacts.get("render_fixture"), minimum_records)
    manifest = test_json_artifact(checks, run_dir, "render fixture manifest", raw_artifacts.get("render_fixture_manifest"))
    if manifest is None:
        return

    fixture_kind = manifest.get("fixture_kind")
    add_check(
        checks,
        "render fixture manifest has kind",
        text_present(fixture_kind),
        f"fixture_kind={fixture_kind}",
    )

    messages_path = resolve_artifact_path(run_dir, manifest.get("messages_path"))
    add_check(
        checks,
        "render fixture manifest messages path exists",
        messages_path is not None and messages_path.exists(),
        path=str(messages_path) if messages_path is not None else "",
    )

    layout = manifest.get("layout")
    add_check(checks, "render fixture manifest has layout", isinstance(layout, dict))
    if isinstance(layout, dict):
        split_count = layout.get("split_count")
        add_check(
            checks,
            "render fixture layout has split count",
            int_at_least(split_count, 1),
            f"split_count={split_count}",
        )

    decision = manifest.get("decision")
    add_check(
        checks,
        "render fixture records non-measured decision",
        text_present(decision) and "not measured" in str(decision),
        f"decision={decision}",
    )


def test_fixture_suite(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any]) -> None:
    manifest = test_json_artifact(checks, run_dir, "fixture suite manifest", raw_artifacts.get("fixture_suite_manifest"))
    if manifest is None:
        return

    chat_fixtures = manifest.get("chat_fixtures")
    render_fixtures = manifest.get("render_fixtures")
    add_check(
        checks,
        "fixture suite has chat fixtures",
        isinstance(chat_fixtures, list) and len(chat_fixtures) > 0,
        f"count={len(chat_fixtures) if isinstance(chat_fixtures, list) else 0}",
    )
    add_check(
        checks,
        "fixture suite has render fixtures",
        isinstance(render_fixtures, list) and len(render_fixtures) > 0,
        f"count={len(render_fixtures) if isinstance(render_fixtures, list) else 0}",
    )

    fixtures: list[dict[str, Any]] = []
    if isinstance(chat_fixtures, list):
        fixtures.extend(entry for entry in chat_fixtures if isinstance(entry, dict))
    if isinstance(render_fixtures, list):
        fixtures.extend(entry for entry in render_fixtures if isinstance(entry, dict))

    for fixture in fixtures:
        scenario = fixture.get("scenario")
        kind = fixture.get("kind")
        label = f"chat {scenario}" if text_present(scenario) else f"render {kind}"
        for path_field in ("run_dir", "manifest_path", "validation_path"):
            path = resolve_artifact_path(run_dir, fixture.get(path_field))
            add_check(
                checks,
                f"fixture suite {label} {path_field} exists",
                path is not None and path.exists(),
                path=str(path) if path is not None else "",
            )

        validation = fixture.get("validation_passed")
        add_check(
            checks,
            f"fixture suite {label} validation passed",
            bool(validation),
            f"validation_passed={validation}",
        )

    validation_passed = manifest.get("validation_passed")
    add_check(
        checks,
        "fixture suite records passing child validations",
        bool(validation_passed),
        f"validation_passed={validation_passed}",
    )
    decision = manifest.get("decision")
    add_check(
        checks,
        "fixture suite records non-measured decision",
        text_present(decision) and "not measured" in str(decision),
        f"decision={decision}",
    )


def numeric_float(value: Any) -> bool:
    try:
        float(value)
        return True
    except (TypeError, ValueError):
        return False


def test_frame_trace(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], minimum_samples: int) -> None:
    frame_summary = test_json_artifact(checks, run_dir, "frame trace summary", raw_artifacts.get("frame_trace"))
    if frame_summary is None:
        return

    frame_count = frame_summary.get("frame_count")
    add_check(
        checks,
        "frame trace summary has frames",
        int_at_least(frame_count, minimum_samples),
        f"frame_count={frame_count} minimum={minimum_samples}",
    )
    for metric in ("average_ms", "p50_ms", "p95_ms", "p99_ms", "max_ms"):
        value = frame_summary.get(metric)
        add_check(
            checks,
            f"frame trace summary has numeric {metric}",
            numeric_float(value),
            f"{metric}={value}",
        )

    trace_path = resolve_artifact_path(run_dir, frame_summary.get("trace_path"))
    add_check(
        checks,
        "frame trace source exists",
        trace_path is not None and trace_path.exists(),
        path=str(trace_path) if trace_path is not None else "",
    )

    decision = frame_summary.get("decision")
    add_check(
        checks,
        "frame trace summary records non-comparison decision",
        text_present(decision) and "not an app comparison" in str(decision),
        f"decision={decision}",
    )


def test_gpu_trace(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], minimum_samples: int) -> None:
    gpu_summary = test_json_artifact(checks, run_dir, "GPU trace summary", raw_artifacts.get("gpu_trace"))
    if gpu_summary is None:
        return

    sample_count = gpu_summary.get("sample_count")
    add_check(
        checks,
        "GPU trace summary has samples",
        int_at_least(sample_count, minimum_samples),
        f"sample_count={sample_count} minimum={minimum_samples}",
    )
    metric_count = gpu_summary.get("metric_count")
    add_check(
        checks,
        "GPU trace summary has metrics",
        int_at_least(metric_count, 1),
        f"metric_count={metric_count}",
    )
    metrics = gpu_summary.get("metrics")
    add_check(checks, "GPU trace summary metrics object present", isinstance(metrics, dict))
    if isinstance(metrics, dict):
        add_check(
            checks,
            "GPU trace summary has metric entries",
            len(metrics) > 0,
            f"metric_entries={len(metrics)}",
        )
        for metric_name, metric in metrics.items():
            if not isinstance(metric, dict):
                add_check(checks, f"GPU trace {metric_name} metric object present", False)
                continue
            metric_sample_count = metric.get("sample_count")
            add_check(
                checks,
                f"GPU trace {metric_name} has minimum samples",
                int_at_least(metric_sample_count, minimum_samples),
                f"sample_count={metric_sample_count} minimum={minimum_samples}",
            )
            for field in ("avg", "p95", "max"):
                value = metric.get(field)
                add_check(
                    checks,
                    f"GPU trace {metric_name} has numeric {field}",
                    numeric_float(value),
                    f"{field}={value}",
                )

    trace_path = resolve_artifact_path(run_dir, gpu_summary.get("trace_path"))
    add_check(
        checks,
        "GPU trace source exists",
        trace_path is not None and trace_path.exists(),
        path=str(trace_path) if trace_path is not None else "",
    )

    decision = gpu_summary.get("decision")
    add_check(
        checks,
        "GPU trace summary records non-comparison decision",
        text_present(decision) and "not an app comparison" in str(decision),
        f"decision={decision}",
    )


def test_connection_trace(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], minimum_samples: int) -> None:
    connection = test_json_artifact(checks, run_dir, "connection trace summary", raw_artifacts.get("connection_trace"))
    if connection is None:
        return

    event_count = connection.get("event_count")
    add_check(
        checks,
        "connection trace summary has events",
        int_at_least(event_count, minimum_samples),
        f"event_count={event_count} minimum={minimum_samples}",
    )
    for metric in (
        "disconnect_count",
        "reconnect_attempt_count",
        "reconnect_complete_count",
        "duplicate_message_count",
        "dropped_message_count",
    ):
        value = connection.get(metric)
        add_check(
            checks,
            f"connection trace summary has numeric {metric}",
            numeric(value),
            f"{metric}={value}",
        )

    trace_path = resolve_artifact_path(run_dir, connection.get("trace_path"))
    add_check(
        checks,
        "connection trace source exists",
        trace_path is not None and trace_path.exists(),
        path=str(trace_path) if trace_path is not None else "",
    )
    decision = connection.get("decision")
    add_check(
        checks,
        "connection trace summary records non-comparison decision",
        text_present(decision) and "not an app comparison" in str(decision),
        f"decision={decision}",
    )


def test_long_session_samples(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], minimum_samples: int) -> None:
    summary = test_json_artifact(checks, run_dir, "long-session samples summary", raw_artifacts.get("long_session_samples"))
    if summary is None:
        return

    sample_count = summary.get("sample_count")
    add_check(
        checks,
        "long-session summary has minimum samples",
        int_at_least(sample_count, minimum_samples),
        f"sample_count={sample_count} minimum={minimum_samples}",
    )
    duration_ms = summary.get("observed_duration_ms")
    duration_valid = False
    try:
        duration_valid = float(duration_ms) > 0
    except (TypeError, ValueError):
        duration_valid = False
    add_check(
        checks,
        "long-session summary has positive duration",
        duration_valid,
        f"observed_duration_ms={duration_ms}",
    )
    sample_path = resolve_artifact_path(run_dir, summary.get("sample_path"))
    add_check(
        checks,
        "long-session source samples exist",
        sample_path is not None and sample_path.exists(),
        path=str(sample_path) if sample_path is not None else "",
    )
    metrics = summary.get("metrics")
    add_check(checks, "long-session summary has metrics", isinstance(metrics, dict))
    if isinstance(metrics, dict):
        for metric_name in ("working_set_bytes", "private_memory_bytes"):
            metric = metrics.get(metric_name)
            add_check(checks, f"long-session summary has {metric_name}", isinstance(metric, dict))
            if not isinstance(metric, dict):
                continue
            for field in ("first", "last", "max", "delta", "growth_per_minute"):
                value = metric.get(field)
                add_check(
                    checks,
                    f"long-session {metric_name} has numeric {field}",
                    numeric_float(value),
                    f"{field}={value}",
                )
    decision = summary.get("decision")
    add_check(
        checks,
        "long-session summary records non-comparison decision",
        text_present(decision) and "not an app comparison" in str(decision),
        f"decision={decision}",
    )


def test_cleanup_samples(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], minimum_samples: int) -> None:
    summary = test_json_artifact(checks, run_dir, "cleanup samples summary", raw_artifacts.get("cleanup_samples"))
    if summary is None:
        return

    sample_count = summary.get("sample_count")
    add_check(
        checks,
        "cleanup summary has minimum samples",
        int_at_least(sample_count, minimum_samples),
        f"sample_count={sample_count} minimum={minimum_samples}",
    )
    for count_field in ("before_sample_count", "after_sample_count"):
        count_value = summary.get(count_field)
        has_samples = False
        try:
            has_samples = int(count_value) > 0
        except (TypeError, ValueError):
            has_samples = False
        add_check(
            checks,
            f"cleanup summary has {count_field}",
            has_samples,
            f"{count_field}={count_value}",
        )
    cleanup_start_ms = summary.get("cleanup_start_elapsed_ms")
    cleanup_start_valid = False
    try:
        cleanup_start_valid = float(cleanup_start_ms) >= 0
    except (TypeError, ValueError):
        cleanup_start_valid = False
    add_check(
        checks,
        "cleanup summary has cleanup boundary",
        cleanup_start_valid,
        f"cleanup_start_elapsed_ms={cleanup_start_ms}",
    )
    sample_path = resolve_artifact_path(run_dir, summary.get("sample_path"))
    add_check(
        checks,
        "cleanup source samples exist",
        sample_path is not None and sample_path.exists(),
        path=str(sample_path) if sample_path is not None else "",
    )
    metrics = summary.get("metrics")
    add_check(checks, "cleanup summary has metrics", isinstance(metrics, dict))
    if isinstance(metrics, dict):
        for metric_name in ("working_set_bytes", "private_memory_bytes"):
            metric = metrics.get(metric_name)
            add_check(checks, f"cleanup summary has {metric_name}", isinstance(metric, dict))
            if not isinstance(metric, dict):
                continue
            for field in ("delta_last", "delta_avg"):
                value = metric.get(field)
                add_check(
                    checks,
                    f"cleanup {metric_name} has numeric {field}",
                    numeric_float(value),
                    f"{field}={value}",
                )
    decision = summary.get("decision")
    add_check(
        checks,
        "cleanup summary records non-comparison decision",
        text_present(decision) and "not an app comparison" in str(decision),
        f"decision={decision}",
    )


def test_echo_profile(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any]) -> None:
    profile = test_json_artifact(checks, run_dir, "echo profile run", raw_artifacts.get("echo_profile_run"))
    if profile is None:
        return

    add_check(checks, "echo profile run has app echo", profile.get("app") == "echo", f"app={profile.get('app')}")
    add_check(
        checks,
        "echo profile run has profile name",
        text_present(profile.get("profile_name")),
        f"profile_name={profile.get('profile_name')}",
    )
    add_check(
        checks,
        "echo profile run has launch path",
        text_present(profile.get("launch_path")),
        f"launch_path={profile.get('launch_path')}",
    )
    for artifact_name in ("stdout", "stderr", "log"):
        artifacts = profile.get("artifacts")
        artifact_path = resolve_artifact_path(run_dir, artifacts.get(artifact_name) if isinstance(artifacts, dict) else None)
        add_check(
            checks,
            f"echo profile {artifact_name} artifact exists",
            artifact_path is not None and artifact_path.exists(),
            path=str(artifact_path) if artifact_path is not None else "",
        )

    for raw_key in ("echo_profile_stdout", "echo_profile_stderr", "echo_profile_log"):
        artifact_path = resolve_artifact_path(run_dir, raw_artifacts.get(raw_key))
        add_check(
            checks,
            f"echo profile raw_artifacts has {raw_key}",
            artifact_path is not None and artifact_path.exists(),
            path=str(artifact_path) if artifact_path is not None else "",
        )

    environment = profile.get("environment")
    expected_env = {
        "ECHO_FPS_PROFILER",
        "ECHO_PERF_MONITOR",
        "ECHO_LOG_FILE",
        "ECHO_LOG_FILE_PATH",
        "ECHO_LOG_PERF",
        "ECHO_LOG_PERF_RENDER",
        "ECHO_LOG_PERF_EMOTES",
        "ECHO_LOG_IRC",
        "ECHO_LOG_EMOTES",
        "ECHO_LOG_NET",
        "ECHO_LOG_UI",
    }
    add_check(checks, "echo profile run has environment", isinstance(environment, dict))
    if isinstance(environment, dict):
        for key in sorted(expected_env):
            value = environment.get(key)
            add_check(checks, f"echo profile env has {key}", text_present(value), f"{key}={value}")

    stop = profile.get("stop")
    add_check(checks, "echo profile run has stop object", isinstance(stop, dict))
    if isinstance(stop, dict):
        method = stop.get("method")
        add_check(
            checks,
            "echo profile stop method is known",
            method
            in {
                "close-main-window",
                "kill",
                "leave-running",
                "already-exited",
                "app-owned-quit",
            },
            f"method={method}",
        )

    decision = profile.get("decision")
    add_check(
        checks,
        "echo profile run records non-comparison decision",
        text_present(decision) and "not compared" in str(decision),
        f"decision={decision}",
    )


def test_echo_chat_replay_run(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any]) -> None:
    replay = test_json_artifact(
        checks,
        run_dir,
        "echo chat replay run",
        raw_artifacts.get("echo_chat_replay_run"),
    )
    if replay is None:
        return

    replay_path = resolve_artifact_path(run_dir, replay.get("replay_path"))
    add_check(
        checks,
        "echo chat replay source fixture exists",
        replay_path is not None and replay_path.exists(),
        path=str(replay_path) if replay_path is not None else "",
    )

    inserted = replay.get("inserted_messages")
    add_check(
        checks,
        "echo chat replay inserted messages",
        int_at_least(inserted, 1),
        f"inserted_messages={inserted}",
    )
    add_check(
        checks,
        "echo chat replay records injected flag",
        replay.get("replay_injected") is True,
        f"replay_injected={replay.get('replay_injected')}",
    )
    parse_errors = replay.get("parse_errors")
    add_check(
        checks,
        "echo chat replay has no parse errors",
        parse_errors == 0,
        f"parse_errors={parse_errors}",
    )
    decision = replay.get("decision")
    add_check(
        checks,
        "echo chat replay records non-comparison decision",
        text_present(decision) and "not compared" in str(decision),
        f"decision={decision}",
    )


def test_baseline(checks: list[dict[str, Any]], run_dir: Path, raw_artifacts: dict[str, Any], artifact_key: str, label: str) -> None:
    baseline = test_json_artifact(checks, run_dir, label, raw_artifacts.get(artifact_key))
    if baseline is None:
        return
    file_object = baseline.get("file")
    sha = file_object.get("sha256") if isinstance(file_object, dict) else None
    add_check(
        checks,
        f"{label} has SHA256",
        text_present(sha) and len(str(sha)) == 64,
        f"sha256={sha}",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--output-file", default="")
    parser.add_argument("--require-echo", action="store_true")
    parser.add_argument("--require-echo-startup", action="store_true")
    parser.add_argument("--require-echo-baseline", action="store_true")
    parser.add_argument("--require-chat-replay", action="store_true")
    parser.add_argument("--require-render-fixture", action="store_true")
    parser.add_argument("--require-fixture-suite", action="store_true")
    parser.add_argument("--require-frame-trace", action="store_true")
    parser.add_argument("--require-gpu-trace", action="store_true")
    parser.add_argument("--require-connection-trace", action="store_true")
    parser.add_argument("--require-long-session-samples", action="store_true")
    parser.add_argument("--require-cleanup-samples", action="store_true")
    parser.add_argument("--require-echo-profile", action="store_true")
    parser.add_argument("--require-echo-chat-replay-run", action="store_true")
    parser.add_argument("--require-cache-snapshot", action="store_true")
    parser.add_argument("--require-cache-comparison", action="store_true")
    parser.add_argument("--minimum-samples", type=int, default=1)
    args = parser.parse_args()

    run_dir = Path(args.run_dir).expanduser().resolve()
    checks: list[dict[str, Any]] = []
    add_check(checks, "run directory exists", run_dir.exists(), path=str(run_dir))

    environment_path = run_dir / "environment.json"
    summary_path = run_dir / "summary.json"
    notes_path = run_dir / "notes.md"
    add_check(checks, "environment exists", environment_path.exists(), path=str(environment_path))
    add_check(checks, "summary exists", summary_path.exists(), path=str(summary_path))
    add_check(checks, "notes exists", notes_path.exists(), path=str(notes_path))

    process_gate_requested = any(
        (
            args.require_echo,
            args.require_echo_startup,
            args.require_gpu_trace,
            args.require_long_session_samples,
            args.require_cleanup_samples,
            args.require_echo_profile,
            args.require_echo_chat_replay_run,
        )
    )
    summary = load_json(summary_path)
    if summary is None:
        add_check(checks, "summary parses as JSON", False, path=str(summary_path))
    else:
        add_check(checks, "summary parses as JSON", True, path=str(summary_path))
        if process_gate_requested:
            add_check(
                checks,
                "summary measurements recorded",
                summary.get("measurements_recorded") is True,
                f"measurements_recorded={summary.get('measurements_recorded')}",
                str(summary_path),
            )
        raw_artifacts = summary.get("raw_artifacts")
        if not isinstance(raw_artifacts, dict):
            add_check(checks, "summary raw_artifacts present", False, path=str(summary_path))
        else:
            add_check(checks, "summary raw_artifacts present", True, path=str(summary_path))
            if args.require_echo:
                test_raw_sample_artifact(checks, run_dir, "echo", raw_artifacts.get("echo"), args.minimum_samples)
            if args.require_echo_startup:
                test_json_artifact(checks, run_dir, "echo startup", raw_artifacts.get("echo_startup"))
            if args.require_echo_baseline:
                test_baseline(checks, run_dir, raw_artifacts, "echo_baseline", "echo baseline")
            if args.require_chat_replay:
                test_ndjson_artifact(checks, run_dir, "chat replay", raw_artifacts.get("chat_replay"), max(args.minimum_samples, 1))
                manifest = test_json_artifact(checks, run_dir, "chat replay manifest", raw_artifacts.get("chat_replay_manifest"))
                if manifest is not None:
                    replay_scenario = manifest.get("replay_scenario")
                    add_check(
                        checks,
                        "chat replay manifest has scenario",
                        text_present(replay_scenario),
                        f"replay_scenario={replay_scenario}",
                    )
            if args.require_render_fixture:
                test_render_fixture(checks, run_dir, raw_artifacts, max(args.minimum_samples, 1))
            if args.require_fixture_suite:
                test_fixture_suite(checks, run_dir, raw_artifacts)
            if args.require_frame_trace:
                test_frame_trace(checks, run_dir, raw_artifacts, max(args.minimum_samples, 1))
            if args.require_gpu_trace:
                test_gpu_trace(checks, run_dir, raw_artifacts, max(args.minimum_samples, 1))
            if args.require_connection_trace:
                test_connection_trace(checks, run_dir, raw_artifacts, max(args.minimum_samples, 1))
            if args.require_long_session_samples:
                test_long_session_samples(checks, run_dir, raw_artifacts, max(args.minimum_samples, 1))
            if args.require_cleanup_samples:
                test_cleanup_samples(checks, run_dir, raw_artifacts, max(args.minimum_samples, 1))
            if args.require_echo_profile:
                test_echo_profile(checks, run_dir, raw_artifacts)
            if args.require_echo_chat_replay_run:
                test_echo_chat_replay_run(checks, run_dir, raw_artifacts)
            if args.require_cache_snapshot:
                test_cache_snapshots(checks, run_dir, raw_artifacts.get("cache_snapshots"))
            if args.require_cache_comparison:
                test_cache_comparisons(checks, run_dir, raw_artifacts.get("cache_comparisons"))

    failed = [check for check in checks if not check["passed"]]
    validation = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "run_dir": str(run_dir),
        "passed": not failed,
        "check_count": len(checks),
        "failed_check_count": len(failed),
        "minimum_samples": max(args.minimum_samples, 1),
        "checks": checks,
    }

    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / "validation.json"
    output_file = output_file.resolve()
    write_json(output_file, validation)

    if summary is not None:
        raw_artifacts = ensure_object(summary, "raw_artifacts")
        raw_artifacts["validation"] = str(output_file)
        summary["validation"] = validation
        write_json(summary_path, summary)

    append_notes(
        run_dir,
        "Benchmark validation",
        [
            f"- Validation artifact: {output_file}",
            f"- Passed: {validation['passed']}",
            f"- Failed checks: {validation['failed_check_count']}",
        ],
    )

    print(output_file)
    return 0 if validation["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
