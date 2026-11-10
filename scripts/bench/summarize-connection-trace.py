#!/usr/bin/env python3
"""Summarize a saved reconnect/connection event trace."""

from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, load_json, percentile, slug, utc_stamp, write_json


TIMESTAMP_FIELDS = ("timestamp_ms", "time_ms", "elapsed_ms", "at_ms", "t_ms")
KIND_FIELDS = ("kind", "event", "state", "connection_state", "message")
CHANNEL_FIELDS = ("channel", "room", "target")


def first_property_value(event: dict[str, Any], names: tuple[str, ...]) -> Any | None:
    for name in names:
        value = event.get(name)
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


def event_class(value: str) -> str:
    normalized = value.lower()
    if "duplicate" in normalized:
        return "duplicate-message"
    if re.search(r"dropped|drop", normalized):
        return "dropped-message"
    if re.search(r"reconnect[-_ ]?complete|reconnected|connected", normalized):
        return "reconnect-complete"
    if re.search(r"reconnect[-_ ]?attempt|reconnecting|retry", normalized):
        return "reconnect-attempt"
    if re.search(r"disconnect|connection[-_ ]?lost|closed", normalized):
        return "disconnect"
    if "join" in normalized:
        return "join"
    if re.search(r"error|fail|timeout", normalized):
        return "error"
    return "other"


def read_raw_events(trace_path: Path) -> list[dict[str, Any]]:
    suffix = trace_path.suffix.lower()
    if suffix in (".ndjson", ".jsonl"):
        events: list[dict[str, Any]] = []
        with trace_path.open("r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line:
                    parsed = json.loads(line)
                    if isinstance(parsed, dict):
                        events.append(parsed)
        return events
    if suffix == ".json":
        with trace_path.open("r", encoding="utf-8") as handle:
            parsed = json.load(handle)
        if isinstance(parsed, list):
            return [event for event in parsed if isinstance(event, dict)]
        if isinstance(parsed, dict):
            events = parsed.get("events")
            if isinstance(events, list):
                return [event for event in events if isinstance(event, dict)]
            return [parsed]
        return []

    with trace_path.open("r", encoding="utf-8", newline="") as handle:
        return [dict(row) for row in csv.DictReader(handle)]


def normalized_events(raw_events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for index, event in enumerate(raw_events):
        timestamp_ms = parse_float(first_property_value(event, TIMESTAMP_FIELDS))
        kind_value = first_property_value(event, KIND_FIELDS)
        kind_text = str(kind_value) if kind_value is not None else "event"
        channel = first_property_value(event, CHANNEL_FIELDS)
        events.append(
            {
                "sequence": index,
                "timestamp_ms": timestamp_ms,
                "kind": kind_text,
                "class": event_class(kind_text),
                "channel": str(channel) if channel is not None else "",
            }
        )
    return sorted(events, key=lambda event: (float("inf") if event["timestamp_ms"] is None else event["timestamp_ms"], event["sequence"]))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trace-path", required=True)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-file", default="")
    args = parser.parse_args()

    trace_path = Path(args.trace_path).expanduser().resolve()
    if not trace_path.exists():
        raise SystemExit(f"Trace file does not exist: {trace_path}")

    raw_events = read_raw_events(trace_path)
    if not raw_events:
        raise SystemExit(f"Connection trace has no events: {trace_path}")

    events = normalized_events(raw_events)
    classes = [event["class"] for event in events]
    channels = sorted({event["channel"] for event in events if event["channel"]})
    disconnects = [event for event in events if event["class"] == "disconnect" and event["timestamp_ms"] is not None]
    reconnect_completes = [event for event in events if event["class"] == "reconnect-complete" and event["timestamp_ms"] is not None]
    durations: list[float] = []
    for disconnect in disconnects:
        complete = next(
            (
                event
                for event in reconnect_completes
                if float(event["timestamp_ms"]) >= float(disconnect["timestamp_ms"])
            ),
            None,
        )
        if complete is not None:
            durations.append(float(complete["timestamp_ms"]) - float(disconnect["timestamp_ms"]))

    sorted_durations = sorted(durations)
    timestamps = [event["timestamp_ms"] for event in events if event["timestamp_ms"] is not None]
    summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "app": args.app,
        "trace_path": str(trace_path),
        "event_count": len(events),
        "channel_count": len(channels),
        "first_timestamp_ms": timestamps[0] if timestamps else None,
        "last_timestamp_ms": timestamps[-1] if timestamps else None,
        "disconnect_count": classes.count("disconnect"),
        "reconnect_attempt_count": classes.count("reconnect-attempt"),
        "reconnect_complete_count": classes.count("reconnect-complete"),
        "join_count": classes.count("join"),
        "error_count": classes.count("error"),
        "duplicate_message_count": classes.count("duplicate-message"),
        "dropped_message_count": classes.count("dropped-message"),
        "reconnect_duration_count": len(durations),
        "reconnect_duration_average_ms": round(sum(durations) / len(durations), 6) if durations else None,
        "reconnect_duration_p50_ms": percentile(sorted_durations, 50),
        "reconnect_duration_p95_ms": percentile(sorted_durations, 95),
        "reconnect_duration_max_ms": percentile(sorted_durations, 100),
        "events_by_class": [{"class": value, "count": classes.count(value)} for value in sorted(set(classes))],
        "decision": "connection trace summarized; not an app comparison",
    }

    run_dir = Path(args.run_dir).expanduser() if args.run_dir else trace_path.parent
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir = run_dir.resolve()
    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / f"{slug(args.app)}.connection-summary.json"
    if not output_file.is_absolute():
        output_file = run_dir / output_file
    output_file = output_file.resolve()
    write_json(output_file, summary)

    summary_path = run_dir / "summary.json"
    run_summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": "connection trace summary",
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(run_summary, "raw_artifacts")
    connection_traces = ensure_object(raw_artifacts, "connection_traces")
    connection_summaries = ensure_object(raw_artifacts, "connection_summaries")
    app_key = slug(args.app)
    connection_traces[app_key] = str(trace_path)
    connection_summaries[app_key] = str(output_file)
    raw_artifacts["connection_trace"] = str(output_file)
    connection_summary = ensure_object(run_summary, "connection_summary")
    connection_summary[app_key] = summary
    run_summary["decision"] = "connection trace summarized; not compared"
    write_json(summary_path, run_summary)

    append_notes(
        run_dir,
        "Connection trace summary",
        [
            f"- App: {args.app}",
            f"- Trace artifact: {trace_path}",
            f"- Summary artifact: {output_file}",
            f"- Event count: {summary['event_count']}",
            f"- Reconnect duration count: {summary['reconnect_duration_count']}",
            "- Decision: connection trace summarized; not compared",
        ],
    )

    print(output_file)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
