#!/usr/bin/env python3
"""Summarize saved long-session process samples."""

from __future__ import annotations

import argparse
import csv
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, load_json, percentile, slug, utc_stamp, write_json


ELAPSED_FIELDS = ("elapsed_ms", "time_ms", "timestamp_ms", "t_ms")
TIMESTAMP_FIELDS = ("timestamp_utc", "timestamp", "time_utc")
EXIT_FIELDS = ("has_exited", "process_exited", "exited")
SERIES_SPECS = (
    ("working_set_bytes", ("working_set_bytes", "rss_bytes", "rss", "working_set")),
    ("private_memory_bytes", ("private_memory_bytes", "private_bytes", "private_memory")),
    ("paged_memory_bytes", ("paged_memory_bytes", "paged_bytes")),
    ("virtual_memory_bytes", ("virtual_memory_bytes", "virtual_bytes", "vms_bytes")),
    ("handle_count", ("handle_count", "handles")),
    ("thread_count", ("thread_count", "threads")),
    ("cpu_percent_all_processors", ("cpu_percent_all_processors", "cpu_percent", "cpu_all_processors_percent")),
    ("cpu_percent_single_core", ("cpu_percent_single_core", "cpu_single_core_percent")),
)


def first_value(row: dict[str, Any], names: tuple[str, ...]) -> Any | None:
    for name in names:
        value = row.get(name)
        if value is not None and str(value).strip():
            return value
    return None


def parse_float(value: Any) -> float | None:
    if value is None or not str(value).strip():
        return None
    try:
        return float(str(value))
    except ValueError:
        return None


def truthy_marker(value: Any) -> bool:
    return value is not None and str(value).strip().lower() in {"true", "1", "yes"}


def series_summary(rows: list[dict[str, Any]], name: str, aliases: tuple[str, ...], observed_duration_minutes: float) -> dict[str, Any]:
    values: list[float] = []
    for row in rows:
        parsed = parse_float(first_value(row, aliases))
        if parsed is not None:
            values.append(parsed)

    if not values:
        return {
            "name": name,
            "sample_count": 0,
            "first": None,
            "last": None,
            "min": None,
            "avg": None,
            "p50": None,
            "p95": None,
            "max": None,
            "delta": None,
            "delta_percent": None,
            "max_minus_min": None,
            "growth_per_minute": None,
        }

    sorted_values = sorted(values)
    first = values[0]
    last = values[-1]
    delta = last - first
    return {
        "name": name,
        "sample_count": len(values),
        "first": round(first, 6),
        "last": round(last, 6),
        "min": percentile(sorted_values, 0),
        "avg": round(sum(values) / len(values), 6),
        "p50": percentile(sorted_values, 50),
        "p95": percentile(sorted_values, 95),
        "max": percentile(sorted_values, 100),
        "delta": round(delta, 6),
        "delta_percent": round((delta / first) * 100.0, 6) if first != 0.0 else None,
        "max_minus_min": round(sorted_values[-1] - sorted_values[0], 6),
        "growth_per_minute": round(delta / observed_duration_minutes, 6) if observed_duration_minutes > 0 else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sample-path", required=True)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-file", default="")
    parser.add_argument("--minimum-duration-milliseconds", type=int, default=1)
    args = parser.parse_args()

    if args.minimum_duration_milliseconds < 1 or args.minimum_duration_milliseconds > 86400000:
        raise SystemExit("--minimum-duration-milliseconds must be in range 1..86400000.")

    sample_path = Path(args.sample_path).expanduser().resolve()
    if not sample_path.exists():
        raise SystemExit(f"Sample file does not exist: {sample_path}")

    with sample_path.open("r", encoding="utf-8", newline="") as handle:
        rows = [dict(row) for row in csv.DictReader(handle)]
    if not rows:
        raise SystemExit(f"Long-session sample file has no rows: {sample_path}")

    ordered_rows = sorted(
        enumerate(rows),
        key=lambda item: (
            float("inf") if parse_float(first_value(item[1], ELAPSED_FIELDS)) is None else parse_float(first_value(item[1], ELAPSED_FIELDS)),
            item[0],
        ),
    )
    ordered = [row for _, row in ordered_rows]
    first_elapsed = parse_float(first_value(ordered[0], ELAPSED_FIELDS))
    last_elapsed = parse_float(first_value(ordered[-1], ELAPSED_FIELDS))
    if first_elapsed is None or last_elapsed is None:
        raise SystemExit("Long-session sample file needs an elapsed_ms, time_ms, timestamp_ms, or t_ms column.")

    observed_duration_ms = max(0.0, last_elapsed - first_elapsed)
    if observed_duration_ms < args.minimum_duration_milliseconds:
        raise SystemExit(
            "Long-session sample duration is shorter than required: "
            f"observed={observed_duration_ms}ms minimum={args.minimum_duration_milliseconds}ms."
        )
    observed_duration_minutes = observed_duration_ms / 60000.0
    exited_count = sum(1 for row in ordered if truthy_marker(first_value(row, EXIT_FIELDS)))
    metrics = {
        name: series_summary(ordered, name, aliases, observed_duration_minutes)
        for name, aliases in SERIES_SPECS
    }

    summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "app": args.app,
        "sample_path": str(sample_path),
        "sample_count": len(ordered),
        "first_timestamp_utc": first_value(ordered[0], TIMESTAMP_FIELDS),
        "last_timestamp_utc": first_value(ordered[-1], TIMESTAMP_FIELDS),
        "first_elapsed_ms": round(first_elapsed, 6),
        "last_elapsed_ms": round(last_elapsed, 6),
        "observed_duration_ms": round(observed_duration_ms, 6),
        "observed_duration_seconds": round(observed_duration_ms / 1000.0, 6),
        "minimum_duration_ms": args.minimum_duration_milliseconds,
        "process_exited_sample_count": exited_count,
        "metrics": metrics,
        "decision": "long-session samples summarized; not an app comparison",
    }

    run_dir = Path(args.run_dir).expanduser() if args.run_dir else sample_path.parent
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir = run_dir.resolve()
    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / f"{slug(args.app)}.long-session-summary.json"
    if not output_file.is_absolute():
        output_file = run_dir / output_file
    output_file = output_file.resolve()
    write_json(output_file, summary)

    summary_path = run_dir / "summary.json"
    run_summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": "long session summary",
        "status": "initialized",
        "measurements_recorded": True,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(run_summary, "raw_artifacts")
    raw_samples = ensure_object(raw_artifacts, "long_session_raw_samples")
    summaries = ensure_object(raw_artifacts, "long_session_summaries")
    app_key = slug(args.app)
    raw_samples[app_key] = str(sample_path)
    summaries[app_key] = str(output_file)
    raw_artifacts["long_session_samples"] = str(output_file)
    long_session_summary = ensure_object(run_summary, "long_session_summary")
    long_session_summary[app_key] = summary
    run_summary["measurements_recorded"] = True
    run_summary["decision"] = "long-session samples summarized; not compared"
    write_json(summary_path, run_summary)

    append_notes(
        run_dir,
        "Long-session sample summary",
        [
            f"- App: {args.app}",
            f"- Raw sample artifact: {sample_path}",
            f"- Summary artifact: {output_file}",
            f"- Samples: {summary['sample_count']}",
            f"- Observed duration seconds: {summary['observed_duration_seconds']}",
            "- Decision: long-session samples summarized; not compared",
        ],
    )

    print(output_file)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
