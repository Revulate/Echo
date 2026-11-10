#!/usr/bin/env python3
"""Summarize a saved GPU metric CSV trace."""

from __future__ import annotations

import argparse
import csv
import re
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, load_json, percentile, slug, utc_stamp, write_json


GPU_TIME_COLUMNS = (
    "gpu_time_ms",
    "GpuTimeMs",
    "GPUTimeMs",
    "gpu_duration_ms",
    "GpuDurationMs",
    "GPUDurationMs",
    "gpu_busy_ms",
    "GpuBusyMs",
    "GPUBusyMs",
    "gpu_elapsed_ms",
    "GpuElapsedMs",
    "ms_on_gpu",
    "MsOnGpu",
    "GPU Time (ms)",
    "GPU Duration (ms)",
    "GPU Busy (ms)",
)
GPU_UTILIZATION_COLUMNS = (
    "gpu_utilization_percent",
    "GpuUtilizationPercent",
    "GPUUtilizationPercent",
    "gpu_usage_percent",
    "GpuUsagePercent",
    "GPUUsagePercent",
    "gpu_busy_percent",
    "GpuBusyPercent",
    "GPUBusyPercent",
    "gpu_percent",
    "GPU Percent",
    "GPU Busy (%)",
    "GPU Utilization (%)",
)
GPU_MEMORY_COLUMNS = (
    "gpu_memory_bytes",
    "GpuMemoryBytes",
    "GPUMemoryBytes",
    "dedicated_gpu_memory_bytes",
    "DedicatedGpuMemoryBytes",
    "vram_bytes",
    "VRAMBytes",
    "gpu_memory_mb",
    "GpuMemoryMB",
    "GPUMemoryMB",
    "vram_mb",
    "VRAMMB",
    "GPU Memory (MB)",
    "Dedicated GPU Memory (MB)",
)


def normalize_column_name(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "", value.lower())


def resolve_trace_column(columns: list[str], explicit_column: str, aliases: tuple[str, ...]) -> str | None:
    if explicit_column.strip():
        if explicit_column in columns:
            return explicit_column
        raise SystemExit(f"Column '{explicit_column}' was not found. Available columns: {', '.join(columns)}")

    normalized_aliases = {normalize_column_name(alias) for alias in aliases}
    for column in columns:
        if normalize_column_name(column) in normalized_aliases:
            return column
    return None


def metric_multiplier(column: str, metric_name: str) -> float:
    normalized = normalize_column_name(column)
    if metric_name == "gpu_time_ms":
        if re.search(r"micro|usec|us$", normalized):
            return 0.001
        if re.search(r"nanosecond|nsec|ns$", normalized):
            return 0.000001
        if re.search(r"second|sec$|seconds$", normalized) and not re.search(r"ms|millisecond", normalized):
            return 1000.0

    if metric_name == "gpu_memory_bytes":
        if "gib" in normalized:
            return 1073741824.0
        if "gb" in normalized:
            return 1000000000.0
        if "mib" in normalized:
            return 1048576.0
        if "mb" in normalized:
            return 1000000.0
        if "kib" in normalized:
            return 1024.0
        if "kb" in normalized:
            return 1000.0

    return 1.0


def parse_float(value: Any) -> float | None:
    if value is None or not str(value).strip():
        return None
    try:
        return float(str(value))
    except ValueError:
        return None


def series_summary(
    rows: list[dict[str, str]],
    column: str,
    metric_name: str,
    unit: str,
    lower_is_better: bool,
) -> dict[str, Any]:
    multiplier = metric_multiplier(column, metric_name)
    values: list[float] = []
    for row in rows:
        parsed = parse_float(row.get(column))
        if parsed is not None:
            values.append(parsed * multiplier)

    if not values:
        return {
            "name": metric_name,
            "column": column,
            "unit": unit,
            "sample_count": 0,
            "min": None,
            "avg": None,
            "p50": None,
            "p95": None,
            "p99": None,
            "max": None,
            "lower_is_better": lower_is_better,
        }

    sorted_values = sorted(values)
    return {
        "name": metric_name,
        "column": column,
        "unit": unit,
        "sample_count": len(values),
        "min": percentile(sorted_values, 0),
        "avg": round(sum(values) / len(values), 6),
        "p50": percentile(sorted_values, 50),
        "p95": percentile(sorted_values, 95),
        "p99": percentile(sorted_values, 99),
        "max": percentile(sorted_values, 100),
        "lower_is_better": lower_is_better,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trace-path", required=True)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-file", default="")
    parser.add_argument("--gpu-time-column", default="")
    parser.add_argument("--gpu-utilization-column", default="")
    parser.add_argument("--gpu-memory-column", default="")
    args = parser.parse_args()

    trace_path = Path(args.trace_path).expanduser().resolve()
    if not trace_path.exists():
        raise SystemExit(f"Trace file does not exist: {trace_path}")

    with trace_path.open("r", encoding="utf-8", newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        raise SystemExit(f"GPU trace has no rows: {trace_path}")

    columns = list(rows[0].keys())
    gpu_time_column = resolve_trace_column(columns, args.gpu_time_column, GPU_TIME_COLUMNS)
    gpu_utilization_column = resolve_trace_column(columns, args.gpu_utilization_column, GPU_UTILIZATION_COLUMNS)
    gpu_memory_column = resolve_trace_column(columns, args.gpu_memory_column, GPU_MEMORY_COLUMNS)

    if not gpu_time_column and not gpu_utilization_column and not gpu_memory_column:
        raise SystemExit(
            "Could not identify GPU time, utilization, or memory columns. Provide "
            "--gpu-time-column, --gpu-utilization-column, or --gpu-memory-column. "
            f"Available columns: {', '.join(columns)}"
        )

    metrics: dict[str, Any] = {}
    if gpu_time_column:
        metrics["gpu_time_ms"] = series_summary(rows, gpu_time_column, "gpu_time_ms", "ms", True)
    if gpu_utilization_column:
        metrics["gpu_utilization_percent"] = series_summary(
            rows,
            gpu_utilization_column,
            "gpu_utilization_percent",
            "percent",
            True,
        )
    if gpu_memory_column:
        metrics["gpu_memory_bytes"] = series_summary(rows, gpu_memory_column, "gpu_memory_bytes", "bytes", True)

    sample_count = max((int(metric.get("sample_count") or 0) for metric in metrics.values()), default=0)
    if sample_count == 0:
        raise SystemExit("GPU trace columns were found, but no numeric GPU metric samples could be parsed.")

    summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "app": args.app,
        "trace_path": str(trace_path),
        "row_count": len(rows),
        "sample_count": sample_count,
        "metric_count": len(metrics),
        "columns": {
            "gpu_time_ms": gpu_time_column,
            "gpu_utilization_percent": gpu_utilization_column,
            "gpu_memory_bytes": gpu_memory_column,
        },
        "metrics": metrics,
        "decision": "GPU trace summarized; not an app comparison",
    }

    run_dir = Path(args.run_dir).expanduser() if args.run_dir else trace_path.parent
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir = run_dir.resolve()
    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / f"{slug(args.app)}.gpu-summary.json"
    if not output_file.is_absolute():
        output_file = run_dir / output_file
    output_file = output_file.resolve()
    write_json(output_file, summary)

    summary_path = run_dir / "summary.json"
    run_summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": "GPU trace summary",
        "status": "initialized",
        "measurements_recorded": True,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(run_summary, "raw_artifacts")
    gpu_traces = ensure_object(raw_artifacts, "gpu_traces")
    gpu_summaries = ensure_object(raw_artifacts, "gpu_summaries")
    app_key = slug(args.app)
    gpu_traces[app_key] = str(trace_path)
    gpu_summaries[app_key] = str(output_file)
    raw_artifacts["gpu_trace"] = str(output_file)
    gpu_summary = ensure_object(run_summary, "gpu_summary")
    gpu_summary[app_key] = summary
    run_summary["measurements_recorded"] = True
    run_summary["decision"] = "GPU trace summarized; not compared"
    write_json(summary_path, run_summary)

    append_notes(
        run_dir,
        "GPU trace summary",
        [
            f"- App: {args.app}",
            f"- Trace artifact: {trace_path}",
            f"- Summary artifact: {output_file}",
            f"- Metric count: {summary['metric_count']}",
            f"- Sample count: {summary['sample_count']}",
            "- Decision: GPU trace summarized; not compared",
        ],
    )

    print(output_file)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
