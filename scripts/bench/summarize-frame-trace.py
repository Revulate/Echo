#!/usr/bin/env python3
"""Summarize a saved frame-time CSV trace."""

from __future__ import annotations

import argparse
import csv
from pathlib import Path

from benchlib import append_notes, ensure_object, load_json, percentile, slug, utc_stamp, write_json


FRAME_TIME_COLUMNS = (
    "MsUntilDisplayed",
    "msUntilDisplayed",
    "MsBetweenPresents",
    "msBetweenPresents",
    "MsInPresentAPI",
    "frame_time_ms",
    "FrameTimeMs",
    "frameTimeMs",
    "frametime",
    "FrameTime",
    "duration_ms",
    "DurationMs",
)


def find_frame_time_column(columns: list[str]) -> str | None:
    for column in FRAME_TIME_COLUMNS:
        if column in columns:
            return column
    return None


def read_frame_times(trace_path: Path) -> tuple[str, list[float]]:
    with trace_path.open("r", encoding="utf-8", newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        raise SystemExit(f"Frame trace has no rows: {trace_path}")

    frame_time_column = find_frame_time_column(list(rows[0].keys()))
    if not frame_time_column:
        raise SystemExit(f"Could not identify a frame-time column in trace. Columns: {', '.join(rows[0].keys())}")

    frame_times: list[float] = []
    for row in rows:
        raw_value = row.get(frame_time_column)
        if raw_value is None or not str(raw_value).strip():
            continue
        try:
            parsed = float(str(raw_value))
        except ValueError:
            continue
        if parsed >= 0:
            frame_times.append(parsed)

    if not frame_times:
        raise SystemExit(f"Frame trace did not contain parseable nonnegative frame times in column '{frame_time_column}'.")
    return frame_time_column, frame_times


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trace-path", required=True)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-file", default="")
    parser.add_argument("--target-frame-time-ms", type=float, default=16.667)
    parser.add_argument("--dropped-frame-threshold-ms", type=float, default=33.333)
    args = parser.parse_args()

    if args.target_frame_time_ms < 1 or args.target_frame_time_ms > 1000:
        raise SystemExit("--target-frame-time-ms must be in range 1..1000.")
    if args.dropped_frame_threshold_ms < 1 or args.dropped_frame_threshold_ms > 1000:
        raise SystemExit("--dropped-frame-threshold-ms must be in range 1..1000.")

    trace_path = Path(args.trace_path).expanduser().resolve()
    if not trace_path.exists():
        raise SystemExit(f"Trace file does not exist: {trace_path}")

    frame_time_column, frame_times = read_frame_times(trace_path)
    sorted_frame_times = sorted(frame_times)
    frames_over_target = sum(1 for value in frame_times if value > args.target_frame_time_ms)
    dropped_frame_count = sum(1 for value in frame_times if value > args.dropped_frame_threshold_ms)
    frame_count = len(frame_times)
    summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "app": args.app,
        "trace_path": str(trace_path),
        "frame_time_column": frame_time_column,
        "frame_count": frame_count,
        "target_frame_time_ms": args.target_frame_time_ms,
        "dropped_frame_threshold_ms": args.dropped_frame_threshold_ms,
        "min_ms": percentile(sorted_frame_times, 0),
        "p50_ms": percentile(sorted_frame_times, 50),
        "p90_ms": percentile(sorted_frame_times, 90),
        "p95_ms": percentile(sorted_frame_times, 95),
        "p99_ms": percentile(sorted_frame_times, 99),
        "max_ms": percentile(sorted_frame_times, 100),
        "average_ms": round(sum(frame_times) / frame_count, 6),
        "frames_over_target": frames_over_target,
        "dropped_frame_count": dropped_frame_count,
        "frames_over_target_percent": round((frames_over_target / float(frame_count)) * 100.0, 6),
        "dropped_frame_percent": round((dropped_frame_count / float(frame_count)) * 100.0, 6),
        "decision": "frame trace summarized; not an app comparison",
    }

    run_dir = Path(args.run_dir).expanduser() if args.run_dir else trace_path.parent
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir = run_dir.resolve()
    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / f"{slug(args.app)}.frame-summary.json"
    if not output_file.is_absolute():
        output_file = run_dir / output_file
    output_file = output_file.resolve()
    write_json(output_file, summary)

    summary_path = run_dir / "summary.json"
    run_summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": "frame trace summary",
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(run_summary, "raw_artifacts")
    frame_traces = ensure_object(raw_artifacts, "frame_traces")
    frame_summaries = ensure_object(raw_artifacts, "frame_summaries")
    app_key = slug(args.app)
    frame_traces[app_key] = str(trace_path)
    frame_summaries[app_key] = str(output_file)
    raw_artifacts["frame_trace"] = str(output_file)
    frame_summary = ensure_object(run_summary, "frame_summary")
    frame_summary[app_key] = summary
    run_summary["decision"] = "frame trace summarized; not compared"
    write_json(summary_path, run_summary)

    append_notes(
        run_dir,
        "Frame trace summary",
        [
            f"- App: {args.app}",
            f"- Trace artifact: {trace_path}",
            f"- Summary artifact: {output_file}",
            f"- Frame count: {summary['frame_count']}",
            f"- P95 ms: {summary['p95_ms']}",
            "- Decision: frame trace summarized; not compared",
        ],
    )

    print(output_file)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
