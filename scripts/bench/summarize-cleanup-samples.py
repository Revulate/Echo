#!/usr/bin/env python3
"""Summarize before/after cleanup samples around a leave-channel boundary."""

from __future__ import annotations

import argparse
import csv
import re
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, load_json, slug, utc_stamp, write_json


ELAPSED_FIELDS = ("elapsed_ms", "time_ms", "timestamp_ms", "t_ms")
TIMESTAMP_FIELDS = ("timestamp_utc", "timestamp", "time_utc")
EXIT_FIELDS = ("has_exited", "process_exited", "exited")
BOUNDARY_FIELDS = ("cleanup_start_elapsed_ms", "leave_elapsed_ms")
MARKER_FIELDS = ("phase", "state", "event", "marker")
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


def percentile(sorted_values: list[float], percent: float) -> float | None:
    if not sorted_values:
        return None
    if len(sorted_values) == 1:
        return round(sorted_values[0], 6)
    rank = (percent / 100.0) * (len(sorted_values) - 1)
    lower = int(rank)
    upper = min(lower + 1, len(sorted_values) - 1)
    if lower == upper:
        return round(sorted_values[lower], 6)
    weight = rank - lower
    value = (sorted_values[lower] * (1.0 - weight)) + (sorted_values[upper] * weight)
    return round(value, 6)


def series_stats(rows: list[dict[str, Any]], aliases: tuple[str, ...]) -> dict[str, Any]:
    values: list[float] = []
    for row in rows:
        parsed = parse_float(first_value(row, aliases))
        if parsed is not None:
            values.append(parsed)
    if not values:
        return {
            "sample_count": 0,
            "first": None,
            "last": None,
            "min": None,
            "avg": None,
            "p50": None,
            "p95": None,
            "max": None,
        }
    sorted_values = sorted(values)
    return {
        "sample_count": len(values),
        "first": round(values[0], 6),
        "last": round(values[-1], 6),
        "min": percentile(sorted_values, 0),
        "avg": round(sum(values) / len(values), 6),
        "p50": percentile(sorted_values, 50),
        "p95": percentile(sorted_values, 95),
        "max": percentile(sorted_values, 100),
    }


def window_comparison(before_rows: list[dict[str, Any]], after_rows: list[dict[str, Any]], name: str, aliases: tuple[str, ...]) -> dict[str, Any]:
    before = series_stats(before_rows, aliases)
    after = series_stats(after_rows, aliases)
    delta_last = None
    delta_last_percent = None
    after_lower_than_before_last = None
    if before["last"] is not None and after["last"] is not None:
        delta_last = round(float(after["last"]) - float(before["last"]), 6)
        after_lower_than_before_last = float(after["last"]) < float(before["last"])
        delta_last_percent = round((delta_last / float(before["last"])) * 100.0, 6) if float(before["last"]) != 0.0 else None
    delta_avg = None
    if before["avg"] is not None and after["avg"] is not None:
        delta_avg = round(float(after["avg"]) - float(before["avg"]), 6)

    return {
        "name": name,
        "before": before,
        "after": after,
        "delta_last": delta_last,
        "delta_last_percent": delta_last_percent,
        "delta_avg": delta_avg,
        "after_lower_than_before_last": after_lower_than_before_last,
    }


def find_cleanup_start(timed_rows: list[dict[str, Any]]) -> float:
    for sample in timed_rows:
        explicit = parse_float(first_value(sample["row"], BOUNDARY_FIELDS))
        if explicit is not None:
            return explicit
        marker = first_value(sample["row"], MARKER_FIELDS)
        if marker is None:
            continue
        if re.search(r"leave|left|cleanup|after[-_ ]?leave|channels[-_ ]?left", str(marker).lower()):
            return float(sample["elapsed_ms"])
    raise SystemExit(
        "Cleanup boundary not found. Provide --cleanup-start-elapsed-ms or a sample row with "
        "phase/state/event/marker containing leave, left, cleanup, after-leave, or channels-left."
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sample-path", required=True)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-file", default="")
    parser.add_argument("--cleanup-start-elapsed-ms", type=float, default=-1.0)
    parser.add_argument("--before-window-milliseconds", type=int, default=300000)
    parser.add_argument("--after-window-milliseconds", type=int, default=300000)
    args = parser.parse_args()

    if args.before_window_milliseconds < 1 or args.before_window_milliseconds > 86400000:
        raise SystemExit("--before-window-milliseconds must be in range 1..86400000.")
    if args.after_window_milliseconds < 1 or args.after_window_milliseconds > 86400000:
        raise SystemExit("--after-window-milliseconds must be in range 1..86400000.")

    sample_path = Path(args.sample_path).expanduser().resolve()
    if not sample_path.exists():
        raise SystemExit(f"Sample file does not exist: {sample_path}")

    with sample_path.open("r", encoding="utf-8", newline="") as handle:
        rows = [dict(row) for row in csv.DictReader(handle)]
    if not rows:
        raise SystemExit(f"Cleanup sample file has no rows: {sample_path}")

    timed_rows = []
    for index, row in enumerate(rows):
        elapsed_ms = parse_float(first_value(row, ELAPSED_FIELDS))
        if elapsed_ms is not None:
            timed_rows.append({"sequence": index, "elapsed_ms": elapsed_ms, "row": row})
    timed_rows.sort(key=lambda sample: (sample["elapsed_ms"], sample["sequence"]))
    if not timed_rows:
        raise SystemExit("Cleanup sample file needs an elapsed_ms, time_ms, timestamp_ms, or t_ms column.")

    cleanup_start = args.cleanup_start_elapsed_ms if args.cleanup_start_elapsed_ms >= 0 else find_cleanup_start(timed_rows)
    before_start = max(0.0, cleanup_start - float(args.before_window_milliseconds))
    after_end = cleanup_start + float(args.after_window_milliseconds)
    before_rows = [sample for sample in timed_rows if sample["elapsed_ms"] < cleanup_start and sample["elapsed_ms"] >= before_start]
    after_rows = [sample for sample in timed_rows if sample["elapsed_ms"] >= cleanup_start and sample["elapsed_ms"] <= after_end]
    if not before_rows:
        raise SystemExit("No samples found before cleanup boundary in the configured before window.")
    if not after_rows:
        raise SystemExit("No samples found after cleanup boundary in the configured after window.")

    before_sample_rows = [sample["row"] for sample in before_rows]
    after_sample_rows = [sample["row"] for sample in after_rows]
    metrics = {
        name: window_comparison(before_sample_rows, after_sample_rows, name, aliases)
        for name, aliases in SERIES_SPECS
    }
    exited_count = sum(1 for sample in timed_rows if truthy_marker(first_value(sample["row"], EXIT_FIELDS)))
    first_row = timed_rows[0]["row"]
    last_row = timed_rows[-1]["row"]
    summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "app": args.app,
        "sample_path": str(sample_path),
        "sample_count": len(timed_rows),
        "first_timestamp_utc": first_value(first_row, TIMESTAMP_FIELDS),
        "last_timestamp_utc": first_value(last_row, TIMESTAMP_FIELDS),
        "first_elapsed_ms": round(float(timed_rows[0]["elapsed_ms"]), 6),
        "last_elapsed_ms": round(float(timed_rows[-1]["elapsed_ms"]), 6),
        "cleanup_start_elapsed_ms": round(cleanup_start, 6),
        "before_window_ms": args.before_window_milliseconds,
        "after_window_ms": args.after_window_milliseconds,
        "before_sample_count": len(before_rows),
        "after_sample_count": len(after_rows),
        "process_exited_sample_count": exited_count,
        "metrics": metrics,
        "decision": "cleanup samples summarized; not an app comparison",
    }

    run_dir = Path(args.run_dir).expanduser() if args.run_dir else sample_path.parent
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir = run_dir.resolve()
    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / f"{slug(args.app)}.cleanup-summary.json"
    if not output_file.is_absolute():
        output_file = run_dir / output_file
    output_file = output_file.resolve()
    write_json(output_file, summary)

    summary_path = run_dir / "summary.json"
    run_summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": "cleanup summary",
        "status": "initialized",
        "measurements_recorded": True,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(run_summary, "raw_artifacts")
    raw_samples = ensure_object(raw_artifacts, "cleanup_raw_samples")
    summaries = ensure_object(raw_artifacts, "cleanup_summaries")
    app_key = slug(args.app)
    raw_samples[app_key] = str(sample_path)
    summaries[app_key] = str(output_file)
    raw_artifacts["cleanup_samples"] = str(output_file)
    cleanup_summary = ensure_object(run_summary, "cleanup_summary")
    cleanup_summary[app_key] = summary
    run_summary["measurements_recorded"] = True
    run_summary["decision"] = "cleanup samples summarized; not compared"
    write_json(summary_path, run_summary)

    append_notes(
        run_dir,
        "Cleanup sample summary",
        [
            f"- App: {args.app}",
            f"- Raw sample artifact: {sample_path}",
            f"- Summary artifact: {output_file}",
            f"- Cleanup start elapsed ms: {summary['cleanup_start_elapsed_ms']}",
            f"- Before samples: {summary['before_sample_count']}",
            f"- After samples: {summary['after_sample_count']}",
            "- Decision: cleanup samples summarized; not compared",
        ],
    )

    print(output_file)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
