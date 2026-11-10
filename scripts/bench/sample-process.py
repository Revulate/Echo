#!/usr/bin/env python3
"""Sample process RAM/CPU into benchmark raw artifacts.

This records process samples for one process on platforms where the Python
runtime is available.
"""

from __future__ import annotations

import argparse
import csv
import os
import signal
import subprocess
import time
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, finalize_notes_status, initialize_run_dir, load_json, resolve_executable, series_stats, utc_stamp, write_json


FIELDNAMES = [
    "sample_index",
    "timestamp_utc",
    "elapsed_ms",
    "process_id",
    "process_name",
    "has_exited",
    "working_set_bytes",
    "private_memory_bytes",
    "paged_memory_bytes",
    "virtual_memory_bytes",
    "handle_count",
    "thread_count",
    "total_cpu_ms",
    "cpu_percent_all_processors",
    "cpu_percent_single_core",
]


def parse_status(pid: int) -> dict[str, str]:
    status: dict[str, str] = {}
    path = Path("/proc") / str(pid) / "status"
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            if ":" not in line:
                continue
            key, value = line.split(":", 1)
            status[key.strip()] = value.strip()
    return status


def kb_value(status: dict[str, str], key: str) -> int | None:
    value = status.get(key)
    if not value:
        return None
    parts = value.split()
    try:
        return int(parts[0]) * 1024
    except (IndexError, ValueError):
        return None


def read_private_memory(pid: int) -> int | None:
    path = Path("/proc") / str(pid) / "smaps_rollup"
    if not path.exists():
        return None
    private_bytes = 0
    found = False
    try:
        with path.open("r", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith(("Private_Clean:", "Private_Dirty:")):
                    parts = line.split()
                    if len(parts) >= 2:
                        private_bytes += int(parts[1]) * 1024
                        found = True
    except (OSError, ValueError):
        return None
    return private_bytes if found else None


def read_cpu_ms(pid: int) -> float | None:
    try:
        stat = (Path("/proc") / str(pid) / "stat").read_text(encoding="utf-8")
        right = stat.rsplit(") ", 1)[1]
        fields = right.split()
        # /proc stat fields after comm start at field 3; utime/stime are fields 14/15.
        utime = int(fields[11])
        stime = int(fields[12])
        ticks_per_second = os.sysconf(os.sysconf_names["SC_CLK_TCK"])
        return ((utime + stime) / ticks_per_second) * 1000.0
    except (IndexError, KeyError, OSError, ValueError):
        return None


def process_exists(pid: int) -> bool:
    return (Path("/proc") / str(pid)).exists()


def process_name(pid: int) -> str:
    try:
        return (Path("/proc") / str(pid) / "comm").read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def find_pid_by_name(name: str) -> int:
    normalized = Path(name).stem
    matches = []
    for proc_dir in Path("/proc").iterdir():
        if not proc_dir.name.isdigit():
            continue
        pid = int(proc_dir.name)
        if process_name(pid) == normalized:
            matches.append(pid)
    if len(matches) != 1:
        raise RuntimeError(f"ProcessName '{name}' matched {len(matches)} processes. Use --target-process-id.")
    return matches[0]


def collect_sample(pid: int, sample_index: int, started_at: float, previous_elapsed_ms: int | None, previous_cpu_ms: float | None) -> tuple[dict[str, Any], float | None]:
    elapsed_ms = round((time.monotonic() - started_at) * 1000)
    has_exited = not process_exists(pid)
    status = parse_status(pid) if not has_exited else {}
    cpu_ms = read_cpu_ms(pid) if not has_exited else None
    processor_count = max(os.cpu_count() or 1, 1)

    cpu_single = None
    cpu_all = None
    if previous_elapsed_ms is not None and previous_cpu_ms is not None and cpu_ms is not None:
        elapsed_delta = elapsed_ms - previous_elapsed_ms
        cpu_delta = cpu_ms - previous_cpu_ms
        if elapsed_delta > 0:
            cpu_single = round((cpu_delta / elapsed_delta) * 100.0, 3)
            cpu_all = round(cpu_single / processor_count, 3)

    handle_count = None
    fd_dir = Path("/proc") / str(pid) / "fd"
    if fd_dir.exists():
        try:
            handle_count = len(list(fd_dir.iterdir()))
        except OSError:
            handle_count = None

    row = {
        "sample_index": sample_index,
        "timestamp_utc": utc_stamp(),
        "elapsed_ms": elapsed_ms,
        "process_id": pid,
        "process_name": process_name(pid) if not has_exited else "",
        "has_exited": has_exited,
        "working_set_bytes": kb_value(status, "VmRSS"),
        "private_memory_bytes": read_private_memory(pid) if not has_exited else None,
        "paged_memory_bytes": kb_value(status, "VmSwap"),
        "virtual_memory_bytes": kb_value(status, "VmSize"),
        "handle_count": handle_count,
        "thread_count": int(status["Threads"]) if status.get("Threads", "").isdigit() else None,
        "total_cpu_ms": round(cpu_ms, 3) if cpu_ms is not None else None,
        "cpu_percent_all_processors": cpu_all,
        "cpu_percent_single_core": cpu_single,
    }
    return row, cpu_ms


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDNAMES, lineterminator="\n")
        writer.writeheader()
        for row in rows:
            writer.writerow({key: "" if row.get(key) is None else row.get(key) for key in FIELDNAMES})


def parse_launch_environment(entries: list[str]) -> dict[str, str]:
    environment: dict[str, str] = {}
    for entry in entries:
        if "=" not in entry:
            raise argparse.ArgumentTypeError(
                f"launch environment entry must use KEY=VALUE syntax: {entry!r}"
            )
        key, value = entry.split("=", 1)
        key = key.strip()
        if not key:
            raise argparse.ArgumentTypeError("launch environment key cannot be empty.")
        if "\0" in key or "\0" in value:
            raise argparse.ArgumentTypeError("launch environment entries cannot contain NUL bytes.")
        environment[key] = value
    return environment


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--scenario", default="")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--target-process-id", type=int, default=0)
    parser.add_argument("--process-name", default="")
    parser.add_argument("--launch-path", default="")
    parser.add_argument("--launch-argument", action="append", default=[])
    parser.add_argument("--launch-working-directory", default="")
    parser.add_argument(
        "--launch-environment",
        action="append",
        default=[],
        help="Environment override for launched targets, as KEY=VALUE. Requires --launch-path.",
    )
    parser.add_argument("--duration-seconds", type=float, default=30.0)
    parser.add_argument("--interval-milliseconds", type=int, default=1000)
    parser.add_argument("--startup-wait-seconds", type=float, default=0.0)
    parser.add_argument("--stop-launched-process", action="store_true")
    parser.add_argument("--echo-path", default="")
    parser.add_argument("--echo-build-type", default="")
    parser.add_argument("--cache-state", default="")
    parser.add_argument("--network-state", default="")
    parser.add_argument("--fixture-name", default="")
    parser.add_argument("--notes", default="")
    args = parser.parse_args()

    selector_count = sum(bool(value) for value in (args.target_process_id > 0, args.process_name, args.launch_path))
    if selector_count != 1:
        raise SystemExit("Provide exactly one target selector: --target-process-id, --process-name, or --launch-path.")
    if not args.run_dir and not args.scenario:
        raise SystemExit("--scenario is required when --run-dir is not provided.")
    try:
        launch_environment = parse_launch_environment(args.launch_environment)
    except argparse.ArgumentTypeError as exc:
        raise SystemExit(str(exc)) from exc
    if launch_environment and not args.launch_path:
        raise SystemExit("--launch-environment can only be used with --launch-path.")

    run_dir = initialize_run_dir(
        run_dir=args.run_dir,
        output_root=args.output_root,
        scenario=args.scenario or "process sample",
        echo_path=args.echo_path,
        echo_build_type=args.echo_build_type,
        cache_state=args.cache_state,
        network_state=args.network_state,
        fixture_name=args.fixture_name,
        notes=args.notes,
    )

    process: subprocess.Popen | None = None
    launched = False
    launch_path = ""
    launch_argument: list[str] = []
    if args.target_process_id > 0:
        pid = args.target_process_id
    elif args.process_name:
        pid = find_pid_by_name(args.process_name)
    else:
        executable = resolve_executable(args.launch_path)
        launch_path = str(executable)
        launch_argument = list(args.launch_argument)
        cwd = Path(args.launch_working_directory).expanduser() if args.launch_working_directory else None
        child_env = os.environ.copy()
        child_env.update(launch_environment)
        process = subprocess.Popen(
            [str(executable), *launch_argument],
            cwd=str(cwd) if cwd else None,
            env=child_env if launch_environment else None,
        )
        pid = process.pid
        launched = True

    if args.startup_wait_seconds > 0:
        time.sleep(args.startup_wait_seconds)

    rows: list[dict[str, Any]] = []
    started_at = time.monotonic()
    previous_elapsed_ms: int | None = None
    previous_cpu_ms: float | None = None
    sample_index = 0
    while time.monotonic() - started_at < args.duration_seconds:
        if launched and process is not None:
            # Polling reaps a launched child, so a clean exit cannot linger as
            # a zombie with an existing /proc directory and empty metrics.
            process.poll()
        row, cpu_ms = collect_sample(pid, sample_index, started_at, previous_elapsed_ms, previous_cpu_ms)
        rows.append(row)
        if row["has_exited"]:
            break
        previous_elapsed_ms = int(row["elapsed_ms"])
        previous_cpu_ms = cpu_ms
        sample_index += 1
        time.sleep(max(args.interval_milliseconds, 1) / 1000.0)

    observed_duration = round(time.monotonic() - started_at, 3)
    if launched and args.stop_launched_process:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        elif process_exists(pid):
            os.kill(pid, signal.SIGTERM)

    raw_path = run_dir / f"{args.app}.raw.txt"
    write_csv(raw_path, rows)

    summary_path = run_dir / "summary.json"
    summary = load_json(summary_path) or {
        "schema_version": 1,
        "run_id": run_dir.parent.name,
        "scenario": args.scenario or run_dir.name,
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(summary, "raw_artifacts")
    metrics = ensure_object(summary, "metrics")
    metric = {
        "app": args.app,
        "raw_artifact": str(raw_path),
        "sample_count": len(rows),
        "requested_duration_seconds": args.duration_seconds,
        "observed_duration_seconds": observed_duration,
        "interval_milliseconds": args.interval_milliseconds,
        "processor_count": max(os.cpu_count() or 1, 1),
        "launched_by_sampler": launched,
        "launch_path": launch_path,
        "launch_argument": launch_argument,
        "launch_environment_keys": sorted(launch_environment),
        "process_id": pid,
        "process_name": rows[0]["process_name"] if rows else "",
        "process_exited": bool(rows[-1]["has_exited"]) if rows else True,
        "working_set_bytes": series_stats([row["working_set_bytes"] for row in rows]),
        "private_memory_bytes": series_stats([row["private_memory_bytes"] for row in rows]),
        "paged_memory_bytes": series_stats([row["paged_memory_bytes"] for row in rows]),
        "virtual_memory_bytes": series_stats([row["virtual_memory_bytes"] for row in rows]),
        "cpu_percent_all_processors": series_stats([row["cpu_percent_all_processors"] for row in rows]),
        "cpu_percent_single_core": series_stats([row["cpu_percent_single_core"] for row in rows]),
    }
    raw_artifacts[args.app] = str(raw_path)
    metrics[args.app] = metric
    summary["measurements_recorded"] = True
    summary["status"] = "sampled"
    summary["decision"] = "not compared"
    write_json(summary_path, summary)
    finalize_notes_status(run_dir, summary["status"], True)

    append_notes(
        run_dir,
        f"Process sample: {args.app}",
        [
            f"- Raw artifact: {raw_path}",
            f"- Samples: {len(rows)}",
            "- Decision: not compared",
        ],
    )

    print(raw_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
