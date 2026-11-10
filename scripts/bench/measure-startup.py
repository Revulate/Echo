#!/usr/bin/env python3
"""Measure one app launch and record startup artifacts.

This is the Python companion to measure-startup.ps1. It records process
creation and readiness timing, then captures a short process sample for the
launched process so startup runs have both timing and resource context.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, initialize_run_dir, load_json, resolve_executable, utc_stamp, write_json


def run_script(script_name: str, args: list[str]) -> str:
    script_path = Path(__file__).resolve().parent / script_name
    result = subprocess.run(
        [sys.executable, str(script_path), *args],
        cwd=str(Path(__file__).resolve().parents[2]),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"{script_name} failed:\n{result.stderr.strip()}")
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not lines:
        raise RuntimeError(f"{script_name} did not print an artifact path.")
    return lines[-1]


def process_has_exited(process: subprocess.Popen[Any]) -> bool:
    return process.poll() is not None


def find_visible_window_linux(pid: int) -> bool:
    xdotool = shutil.which("xdotool")
    if not xdotool:
        return False
    result = subprocess.run(
        [xdotool, "search", "--pid", str(pid), "--onlyvisible"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def wait_for_startup_ready(process: subprocess.Popen[Any], ready_mode: str, timeout_seconds: int, started_at: float) -> dict[str, Any]:
    elapsed_ms = lambda: round((time.monotonic() - started_at) * 1000)
    if ready_mode == "process-start":
        return {
            "ready_observed": True,
            "ready_elapsed_ms": elapsed_ms(),
            "window_observed": False,
            "window_elapsed_ms": None,
            "input_idle_observed": False,
            "input_idle_elapsed_ms": None,
            "timed_out": False,
            "process_exited": False,
        }

    window_observed = False
    window_elapsed_ms = None
    input_idle_observed = False
    input_idle_elapsed_ms = None
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        if process_has_exited(process):
            return {
                "ready_observed": False,
                "ready_elapsed_ms": None,
                "window_observed": window_observed,
                "window_elapsed_ms": window_elapsed_ms,
                "input_idle_observed": input_idle_observed,
                "input_idle_elapsed_ms": input_idle_elapsed_ms,
                "timed_out": False,
                "process_exited": True,
            }

        if not window_observed and find_visible_window_linux(process.pid):
            window_observed = True
            window_elapsed_ms = elapsed_ms()
            if ready_mode == "main-window":
                return {
                    "ready_observed": True,
                    "ready_elapsed_ms": window_elapsed_ms,
                    "window_observed": True,
                    "window_elapsed_ms": window_elapsed_ms,
                    "input_idle_observed": False,
                    "input_idle_elapsed_ms": None,
                    "timed_out": False,
                    "process_exited": False,
                }

        # There is no portable stdlib input-idle probe. On Linux, use visible
        # window observation as the closest dependency-free readiness boundary.
        if ready_mode == "input-idle" and window_observed:
            input_idle_observed = False
            input_idle_elapsed_ms = None
            return {
                "ready_observed": True,
                "ready_elapsed_ms": window_elapsed_ms,
                "window_observed": True,
                "window_elapsed_ms": window_elapsed_ms,
                "input_idle_observed": input_idle_observed,
                "input_idle_elapsed_ms": input_idle_elapsed_ms,
                "timed_out": False,
                "process_exited": False,
            }

        time.sleep(0.05)

    return {
        "ready_observed": False,
        "ready_elapsed_ms": None,
        "window_observed": window_observed,
        "window_elapsed_ms": window_elapsed_ms,
        "input_idle_observed": input_idle_observed,
        "input_idle_elapsed_ms": input_idle_elapsed_ms,
        "timed_out": not process_has_exited(process),
        "process_exited": process_has_exited(process),
    }


def stop_process(process: subprocess.Popen[Any]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--scenario", default="")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--launch-path", required=True)
    parser.add_argument("--launch-argument", action="append", default=[])
    parser.add_argument("--launch-working-directory", default="")
    parser.add_argument("--ready-mode", choices=["process-start", "main-window", "input-idle"], default="main-window")
    parser.add_argument("--ready-timeout-seconds", type=int, default=30)
    parser.add_argument("--sample-duration-seconds", type=float, default=10.0)
    parser.add_argument("--interval-milliseconds", type=int, default=1000)
    parser.add_argument("--stop-launched-process", action="store_true")
    parser.add_argument("--echo-path", default="")
    parser.add_argument("--echo-build-type", default="")
    parser.add_argument("--cache-state", default="")
    parser.add_argument("--network-state", default="")
    parser.add_argument("--fixture-name", default="")
    parser.add_argument("--notes", default="")
    args = parser.parse_args()

    if not args.run_dir and not args.scenario:
        raise SystemExit("--scenario is required when --run-dir is not provided.")
    if args.ready_timeout_seconds < 1 or args.ready_timeout_seconds > 300:
        raise SystemExit("--ready-timeout-seconds must be in range 1..300.")
    if args.sample_duration_seconds <= 0:
        raise SystemExit("--sample-duration-seconds must be greater than zero.")
    if args.interval_milliseconds < 100 or args.interval_milliseconds > 60000:
        raise SystemExit("--interval-milliseconds must be in range 100..60000.")

    run_dir = initialize_run_dir(
        run_dir=args.run_dir,
        output_root=args.output_root,
        scenario=args.scenario or "startup sample",
        echo_path=args.echo_path,
        echo_build_type=args.echo_build_type,
        cache_state=args.cache_state,
        network_state=args.network_state,
        fixture_name=args.fixture_name,
        notes=args.notes,
    )
    executable = resolve_executable(args.launch_path)
    cwd_path = Path(args.launch_working_directory).expanduser() if args.launch_working_directory else None
    if cwd_path is not None:
        cwd_path.mkdir(parents=True, exist_ok=True)

    launch_timestamp = utc_stamp()
    started_at = time.monotonic()
    process = subprocess.Popen(
        [str(executable), *args.launch_argument],
        cwd=str(cwd_path) if cwd_path else None,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    process_created_elapsed_ms = round((time.monotonic() - started_at) * 1000)
    ready = wait_for_startup_ready(process, args.ready_mode, args.ready_timeout_seconds, started_at)

    startup_raw_path = run_dir / f"{args.app}.startup.raw.txt"
    startup_record = {
        "schema_version": 1,
        "app": args.app,
        "scenario": args.scenario or run_dir.name,
        "launch_timestamp_utc": launch_timestamp,
        "launch_path": str(executable),
        "launch_argument": list(args.launch_argument),
        "process_id": process.pid,
        "process_name": Path(str(executable)).stem,
        "ready_mode": args.ready_mode,
        "ready_timeout_seconds": args.ready_timeout_seconds,
        "process_created_elapsed_ms": process_created_elapsed_ms,
        "ready_observed": ready["ready_observed"],
        "ready_elapsed_ms": ready["ready_elapsed_ms"],
        "window_observed": ready["window_observed"],
        "window_elapsed_ms": ready["window_elapsed_ms"],
        "input_idle_observed": ready["input_idle_observed"],
        "input_idle_elapsed_ms": ready["input_idle_elapsed_ms"],
        "timed_out": ready["timed_out"],
        "process_exited_before_ready": ready["process_exited"],
    }
    write_json(startup_raw_path, startup_record)

    sample_args = [
        "--app",
        args.app,
        "--run-dir",
        str(run_dir),
        "--target-process-id",
        str(process.pid),
        "--duration-seconds",
        str(args.sample_duration_seconds),
        "--interval-milliseconds",
        str(args.interval_milliseconds),
    ]
    sample_raw_path = run_script("sample-process.py", sample_args)

    if args.stop_launched_process:
        stop_process(process)

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
    startup_key = f"{args.app}_startup"
    startup_metric = {
        "app": args.app,
        "raw_artifact": str(startup_raw_path),
        "sample_raw_artifact": sample_raw_path,
        "launch_path": str(executable),
        "launch_argument": list(args.launch_argument),
        "ready_mode": args.ready_mode,
        "ready_timeout_seconds": args.ready_timeout_seconds,
        "process_created_elapsed_ms": process_created_elapsed_ms,
        "ready_observed": ready["ready_observed"],
        "ready_elapsed_ms": ready["ready_elapsed_ms"],
        "window_observed": ready["window_observed"],
        "window_elapsed_ms": ready["window_elapsed_ms"],
        "input_idle_observed": ready["input_idle_observed"],
        "input_idle_elapsed_ms": ready["input_idle_elapsed_ms"],
        "timed_out": ready["timed_out"],
        "process_exited_before_ready": ready["process_exited"],
    }
    raw_artifacts[startup_key] = str(startup_raw_path)
    metrics[startup_key] = startup_metric
    summary["measurements_recorded"] = True
    summary["status"] = "startup-sampled"
    summary["decision"] = "not compared"
    write_json(summary_path, summary)

    append_notes(
        run_dir,
        f"Startup sample: {args.app}",
        [
            f"- Startup raw artifact: {startup_raw_path}",
            f"- Process raw artifact: {sample_raw_path}",
            f"- Ready mode: {args.ready_mode}",
            f"- Ready observed: {ready['ready_observed']}",
            "- Decision: not compared",
        ],
    )

    print(startup_raw_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
