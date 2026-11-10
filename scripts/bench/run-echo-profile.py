#!/usr/bin/env python3
"""Launch Echo with profiler logging enabled and record raw artifacts.

This mirrors scripts/bench/run-echo-profile.ps1 for environments where
PowerShell is not installed. It records raw profile artifacts for one Echo run.
"""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from benchlib import (
    REPO_ROOT,
    append_notes,
    ensure_object,
    finalize_notes_status,
    initialize_run_dir,
    load_json,
    resolve_executable,
    slug,
    utc_stamp,
    write_json,
)


PROFILE_ENV = {
    "ECHO_FPS_PROFILER": "1",
    "ECHO_PERF_MONITOR": "1",
    "ECHO_LOG_FILE": "1",
    "ECHO_LOG_PERF": "1",
    "ECHO_LOG_PERF_RENDER": "1",
    "ECHO_LOG_PERF_EMOTES": "1",
    "ECHO_LOG_IRC": "1",
    "ECHO_LOG_EMOTES": "1",
    "ECHO_LOG_NET": "1",
    "ECHO_LOG_UI": "1",
}


def apply_network_state(profile_env: dict[str, str], network_state: str) -> None:
    if network_state == "offline-fixture":
        # This is an enforced application startup mode, not only artifact metadata.
        # Echo accepts it only alongside an isolated ECHO_BENCH_PROFILE_ROOT.
        profile_env["ECHO_BENCH_OFFLINE"] = "1"


def positive_int(value: str, name: str, minimum: int, maximum: int) -> int:
    try:
        parsed = int(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(f"{name} must be an integer.") from exc
    if parsed < minimum or parsed > maximum:
        raise argparse.ArgumentTypeError(f"{name} must be between {minimum} and {maximum}.")
    return parsed


def duration_seconds(value: str) -> int:
    return positive_int(value, "duration-seconds", 1, 86400)


def exit_wait_seconds(value: str) -> int:
    return positive_int(value, "exit-wait-seconds", 1, 300)


def resolve_working_directory(value: str) -> Path:
    if not value:
        return REPO_ROOT
    path = Path(value).expanduser()
    path.mkdir(parents=True, exist_ok=True)
    return path.resolve()


def resolve_log_path(run_dir: Path, profile_name: str) -> Path:
    profile_root = os.environ.get("ECHO_BENCH_PROFILE_ROOT", "").strip()
    if not profile_root:
        return run_dir / f"{profile_name}.log"

    path = Path(profile_root).expanduser()
    if not path.is_absolute():
        raise SystemExit("ECHO_BENCH_PROFILE_ROOT must be absolute.")
    return path.resolve() / "logs" / "echo.log"


def process_exited(process: subprocess.Popen[Any]) -> bool:
    return process.poll() is not None


def stop_profile_process(process: subprocess.Popen[Any], stop_mode: str, exit_wait: int) -> dict[str, Any]:
    if stop_mode == "leave-running":
        return {
            "requested": False,
            "method": "leave-running",
            "graceful_close_requested": False,
            "killed": False,
            "exited_after_stop": False,
        }

    if process_exited(process):
        return {
            "requested": False,
            "method": "already-exited",
            "graceful_close_requested": False,
            "killed": False,
            "exited_after_stop": True,
        }

    graceful_close_requested = False
    killed = False
    error = None
    try:
        if stop_mode == "close-main-window":
            # PowerShell can request a real window close on Windows when a main
            # window handle is visible. Python's stdlib has no equivalent, so
            # use SIGTERM/TerminateProcess as the closest bounded stop and
            # record that no window-close request was made.
            process.terminate()
        else:
            if os.name == "nt":
                process.kill()
            else:
                process.send_signal(signal.SIGKILL)
            killed = True

        try:
            process.wait(timeout=exit_wait)
        except subprocess.TimeoutExpired:
            if not process_exited(process):
                process.kill()
                killed = True
                try:
                    process.wait(timeout=exit_wait)
                except subprocess.TimeoutExpired:
                    pass
    except Exception as exc:  # noqa: BLE001 - profile manifest should capture stop failures.
        error = str(exc)

    result = {
        "requested": True,
        "method": stop_mode,
        "graceful_close_requested": graceful_close_requested,
        "killed": killed,
        "exited_after_stop": process_exited(process),
    }
    if error:
        result["error"] = error
    return result


def find_profile_log_checker(explicit_path: str) -> Path | None:
    if explicit_path:
        checker = Path(explicit_path).expanduser()
        return checker.resolve() if checker.exists() else None
    for candidate in (
        REPO_ROOT / "target" / "release" / "profile-log-check.exe",
        REPO_ROOT / "target" / "release" / "profile-log-check",
        REPO_ROOT / "target" / "debug" / "profile-log-check.exe",
        REPO_ROOT / "target" / "debug" / "profile-log-check",
        REPO_ROOT / "target" / "profile-log-check" / "release" / "profile-log-check.exe",
        REPO_ROOT / "target" / "profile-log-check" / "release" / "profile-log-check",
    ):
        if candidate.exists():
            return candidate.resolve()
    return None


def run_profile_log_check(log_path: Path, checker_path: Path, output_path: Path) -> int:
    with output_path.open("w", encoding="utf-8") as output:
        result = subprocess.run(
            [str(checker_path), str(log_path)],
            cwd=str(REPO_ROOT),
            stdout=output,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    return int(result.returncode)


def validate_chat_replay_report(report_path: Path, replay_path: Path) -> tuple[bool, str]:
    try:
        report = load_json(report_path)
    except (OSError, ValueError) as exc:
        return False, f"chat replay report is unreadable: {exc}"
    if report is None:
        return False, "chat replay report was not produced"

    reported_path = report.get("replay_path")
    try:
        path_matches = (
            isinstance(reported_path, str)
            and Path(reported_path).expanduser().resolve() == replay_path
        )
    except OSError:
        path_matches = False
    if not path_matches:
        return False, "chat replay report does not identify the requested fixture"
    if report.get("replay_injected") is not True:
        return False, "chat replay did not report successful injection"
    if (
        not isinstance(report.get("inserted_messages"), int)
        or report["inserted_messages"] < 1
    ):
        return False, "chat replay inserted no messages"
    if report.get("parse_errors") != 0:
        return False, "chat replay reported parse errors"
    return True, "chat replay injected at least one message without parse errors"


def validate_benchmark_log(log_path: Path) -> tuple[bool, str]:
    try:
        lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as exc:
        return False, f"runtime log is unreadable: {exc}"
    failure = next(
        (line.strip() for line in lines if "bench." in line and " failed" in line),
        None,
    )
    if failure is not None:
        return False, failure
    return True, "no benchmark failure markers"


def validate_app_owned_shutdown(log_path: Path) -> tuple[bool, str]:
    try:
        content = log_path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        return False, f"runtime log is unreadable: {exc}"
    required = (
        "bench.app_smoke requesting graceful application quit",
        "GPUI app quit shutdown path complete",
        "Backend shutdown complete",
    )
    missing = [marker for marker in required if marker not in content]
    if missing:
        return False, f"missing graceful shutdown markers: {', '.join(missing)}"
    return True, "app-owned quit completed the GPUI and backend shutdown paths"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--scenario", default="echo profile run")
    parser.add_argument("--launch-path", required=True)
    parser.add_argument("--launch-argument", action="append", default=[])
    parser.add_argument("--launch-working-directory", default="")
    parser.add_argument("--duration-seconds", type=duration_seconds, default=12)
    parser.add_argument(
        "--app-owned-quit-after-ms",
        type=lambda value: positive_int(value, "app-owned-quit-after-ms", 1, 86_400_000),
        default=0,
    )
    parser.add_argument("--exit-wait-seconds", type=exit_wait_seconds, default=10)
    parser.add_argument("--stop-mode", choices=["close-main-window", "kill", "leave-running"], default="close-main-window")
    parser.add_argument("--enable-scroll-smoke", action="store_true")
    parser.add_argument("--scroll-smoke-sequence", default="")
    parser.add_argument("--scroll-smoke-delay-ms", type=int, default=0)
    parser.add_argument("--scroll-smoke-step-delay-ms", type=int, default=0)
    parser.add_argument("--chat-replay", default="", help="Optional chat replay NDJSON fixture to inject into Echo.")
    parser.add_argument("--chat-replay-channel", default="", help="Optional channel override for replay events.")
    parser.add_argument("--chat-replay-delay-ms", type=int, default=0)
    parser.add_argument("--chat-replay-speedup", type=float, default=0.0)
    parser.add_argument("--chat-replay-max-events", type=int, default=0)
    parser.add_argument("--chat-replay-clear-channel", action="store_true")
    parser.add_argument("--chat-replay-preserve-timestamps", action="store_true")
    parser.add_argument("--chat-replay-insert-control-messages", action="store_true")
    parser.add_argument("--profile-name", default="echo-profile")
    parser.add_argument("--profile-log-check-path", default="")
    parser.add_argument("--echo-build-type", default="")
    parser.add_argument("--cache-state", default="")
    parser.add_argument("--network-state", default="")
    parser.add_argument("--fixture-name", default="")
    parser.add_argument("--notes", default="")
    args = parser.parse_args()

    run_dir = initialize_run_dir(
        run_dir=args.run_dir,
        output_root=args.output_root,
        scenario=args.scenario,
        echo_path=args.launch_path,
        echo_build_type=args.echo_build_type,
        cache_state=args.cache_state,
        network_state=args.network_state,
        fixture_name=args.fixture_name,
        notes=args.notes,
    )
    launch_path = resolve_executable(args.launch_path)
    working_dir = resolve_working_directory(args.launch_working_directory)
    profile_name = slug(args.profile_name, "echo-profile")

    stdout_path = run_dir / f"{profile_name}.stdout.raw.txt"
    stderr_path = run_dir / f"{profile_name}.stderr.raw.txt"
    profile_log_path = resolve_log_path(run_dir, profile_name)
    profile_log_start = profile_log_path.stat().st_size if profile_log_path.exists() else 0
    log_path = run_dir / f"{profile_name}.log"
    run_manifest_path = run_dir / "echo-profile-run.json"
    profile_log_check_raw_path = run_dir / "profile-log-check.raw.txt"
    chat_replay_report_path = run_dir / "echo-chat-replay-run.json"

    profile_env = dict(PROFILE_ENV)
    profile_env["ECHO_LOG_FILE_PATH"] = str(profile_log_path)
    apply_network_state(profile_env, args.network_state)
    if args.app_owned_quit_after_ms > 0:
        profile_env["ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS"] = str(
            args.app_owned_quit_after_ms
        )
    if args.enable_scroll_smoke:
        profile_env["ECHO_BENCH_SCROLL_SMOKE"] = "1"
        if args.scroll_smoke_sequence:
            profile_env["ECHO_BENCH_SCROLL_SMOKE_SEQUENCE"] = args.scroll_smoke_sequence
        if args.scroll_smoke_delay_ms > 0:
            profile_env["ECHO_BENCH_SCROLL_SMOKE_DELAY_MS"] = str(args.scroll_smoke_delay_ms)
        if args.scroll_smoke_step_delay_ms > 0:
            profile_env["ECHO_BENCH_SCROLL_SMOKE_STEP_DELAY_MS"] = str(args.scroll_smoke_step_delay_ms)

    chat_replay_path: Path | None = None
    if args.chat_replay:
        chat_replay_path = Path(args.chat_replay).expanduser().resolve()
        if not chat_replay_path.exists():
            raise SystemExit(f"chat replay fixture does not exist: {chat_replay_path}")
        profile_env["ECHO_CHAT_REPLAY"] = str(chat_replay_path)
        profile_env["ECHO_CHAT_REPLAY_REPORT"] = str(chat_replay_report_path)
        if args.chat_replay_channel:
            profile_env["ECHO_CHAT_REPLAY_CHANNEL"] = args.chat_replay_channel
        if args.chat_replay_delay_ms > 0:
            profile_env["ECHO_CHAT_REPLAY_DELAY_MS"] = str(args.chat_replay_delay_ms)
        if args.chat_replay_speedup > 0:
            profile_env["ECHO_CHAT_REPLAY_SPEEDUP"] = str(args.chat_replay_speedup)
        if args.chat_replay_max_events > 0:
            profile_env["ECHO_CHAT_REPLAY_MAX_EVENTS"] = str(args.chat_replay_max_events)
        if args.chat_replay_clear_channel:
            profile_env["ECHO_CHAT_REPLAY_CLEAR_CHANNEL"] = "1"
        if args.chat_replay_preserve_timestamps:
            profile_env["ECHO_CHAT_REPLAY_PRESERVE_TIMESTAMPS"] = "1"
        if args.chat_replay_insert_control_messages:
            profile_env["ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES"] = "1"

    child_env = os.environ.copy()
    child_env.update(profile_env)
    launched_at = utc_stamp()
    started = time.monotonic()
    stop_result = None
    process: subprocess.Popen[Any] | None = None
    exited_before_deadline = False
    with stdout_path.open("w", encoding="utf-8") as stdout, stderr_path.open("w", encoding="utf-8") as stderr:
        process = subprocess.Popen(
            [str(launch_path), *args.launch_argument],
            cwd=str(working_dir),
            env=child_env,
            stdout=stdout,
            stderr=stderr,
            text=True,
        )
        deadline = time.monotonic() + args.duration_seconds
        while time.monotonic() < deadline:
            if process_exited(process):
                exited_before_deadline = True
                break
            time.sleep(min(0.25, max(0.0, deadline - time.monotonic())))
        stop_result = stop_profile_process(process, args.stop_mode, args.exit_wait_seconds)
    observed_duration = round(time.monotonic() - started, 3)

    if profile_log_path.exists():
        with profile_log_path.open("rb") as source:
            source.seek(min(profile_log_start, profile_log_path.stat().st_size))
            log_path.write_bytes(source.read())

    process_exited_value = True
    process_exit_code = None
    if process is not None:
        process_exited_value = process_exited(process)
        if process_exited_value:
            process_exit_code = process.returncode

    log_check_exit_code = None
    if log_path.exists():
        checker = find_profile_log_checker(args.profile_log_check_path)
        if checker is not None:
            log_check_exit_code = run_profile_log_check(log_path, checker, profile_log_check_raw_path)

    chat_replay_valid = True
    chat_replay_validation = "not requested"
    if chat_replay_path is not None:
        chat_replay_valid, chat_replay_validation = validate_chat_replay_report(
            chat_replay_report_path, chat_replay_path
        )

    benchmark_log_valid = False
    benchmark_log_validation = "runtime log was not produced"
    app_owned_shutdown_valid = args.app_owned_quit_after_ms == 0
    app_owned_shutdown_validation = "not requested"
    if log_path.exists():
        benchmark_log_valid, benchmark_log_validation = validate_benchmark_log(log_path)
        if args.app_owned_quit_after_ms > 0:
            app_owned_shutdown_valid, app_owned_shutdown_validation = (
                validate_app_owned_shutdown(log_path)
            )

    app_owned_exit_valid = (
        exited_before_deadline and process_exit_code == 0
        if args.app_owned_quit_after_ms > 0
        else not exited_before_deadline
    )
    if args.app_owned_quit_after_ms > 0 and stop_result is not None:
        stop_result.update(
            {
                "requested": True,
                "method": "app-owned-quit",
                "graceful_close_requested": True,
            }
        )

    run_manifest = {
        "schema_version": 1,
        "app": "echo",
        "scenario": args.scenario,
        "profile_name": profile_name,
        "launched_at_utc": launched_at,
        "duration_seconds": args.duration_seconds,
        "app_owned_quit_after_ms": args.app_owned_quit_after_ms or None,
        "observed_duration_seconds": observed_duration,
        "launch_path": str(launch_path),
        "launch_argument": args.launch_argument,
        "launch_working_directory": str(working_dir),
        "process_id": process.pid if process is not None else None,
        "process_exited": process_exited_value,
        "process_exit_code": process_exit_code,
        "exited_before_deadline": exited_before_deadline,
        "stop": stop_result,
        "environment": profile_env,
        "artifacts": {
            "stdout": str(stdout_path),
            "stderr": str(stderr_path),
            "log": str(log_path),
            "profile_log": str(profile_log_path),
            "profile_log_check": str(profile_log_check_raw_path) if profile_log_check_raw_path.exists() else None,
            "chat_replay": str(chat_replay_path) if chat_replay_path is not None else None,
            "echo_chat_replay_run": str(chat_replay_report_path) if chat_replay_path is not None else None,
        },
        "chat_replay": {
            "enabled": chat_replay_path is not None,
            "path": str(chat_replay_path) if chat_replay_path is not None else None,
            "report": str(chat_replay_report_path) if chat_replay_path is not None else None,
            "channel": args.chat_replay_channel or None,
            "delay_ms": args.chat_replay_delay_ms,
            "speedup": args.chat_replay_speedup if args.chat_replay_speedup > 0 else None,
            "max_events": args.chat_replay_max_events if args.chat_replay_max_events > 0 else None,
            "clear_channel": args.chat_replay_clear_channel,
            "preserve_timestamps": args.chat_replay_preserve_timestamps,
            "insert_control_messages": args.chat_replay_insert_control_messages,
            "valid": chat_replay_valid,
            "validation": chat_replay_validation,
        },
        "profile_log_check_exit_code": log_check_exit_code,
        "benchmark_log_validation": {
            "valid": benchmark_log_valid,
            "detail": benchmark_log_validation,
        },
        "app_owned_shutdown_validation": {
            "valid": app_owned_shutdown_valid,
            "detail": app_owned_shutdown_validation,
        },
        "decision": "Echo-only profile run; not compared",
    }
    write_json(run_manifest_path, run_manifest)

    summary_path = run_dir / "summary.json"
    summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": args.scenario,
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(summary, "raw_artifacts")
    metrics = ensure_object(summary, "metrics")
    raw_artifacts["echo_profile_run"] = str(run_manifest_path)
    raw_artifacts["echo_profile_stdout"] = str(stdout_path)
    raw_artifacts["echo_profile_stderr"] = str(stderr_path)
    raw_artifacts["echo_profile_log"] = str(log_path)
    if chat_replay_path is not None:
        raw_artifacts["chat_replay"] = str(chat_replay_path)
        raw_artifacts["echo_chat_replay_run"] = str(chat_replay_report_path)
    if profile_log_check_raw_path.exists():
        raw_artifacts["profile_log_check"] = str(profile_log_check_raw_path)
    metrics["echo_profile"] = run_manifest
    profile_valid = (
        app_owned_exit_valid
        and log_path.exists()
        and log_path.stat().st_size > 0
        and (log_check_exit_code is None or log_check_exit_code == 0)
        and chat_replay_valid
        and benchmark_log_valid
        and app_owned_shutdown_valid
    )
    summary["measurements_recorded"] = profile_valid
    summary["status"] = "echo-profiled" if profile_valid else "echo-profile-invalid"
    summary["decision"] = (
        "Echo-only profile run; not compared"
        if profile_valid
        else "Invalid Echo profile run; lifecycle, runtime log, benchmark marker, shutdown, or replay validation failed"
    )
    write_json(summary_path, summary)
    finalize_notes_status(run_dir, summary["status"], profile_valid)

    append_notes(
        run_dir,
        "Echo profile run",
        [
            f"- Run manifest: {run_manifest_path}",
            f"- Stdout: {stdout_path}",
            f"- Stderr: {stderr_path}",
            f"- Runtime log: {log_path}",
            f"- Duration seconds: {args.duration_seconds}",
            f"- Stop mode: {(stop_result or {}).get('method', args.stop_mode)}",
            f"- Benchmark log validation: {benchmark_log_validation}",
            f"- App-owned shutdown validation: {app_owned_shutdown_validation}",
            *(
                [
                    f"- Chat replay fixture: {chat_replay_path}",
                    f"- Chat replay report: {chat_replay_report_path}",
                    f"- Chat replay validation: {chat_replay_validation}",
                ]
                if chat_replay_path is not None
                else []
            ),
            "- Decision: Echo-only profile run; not compared",
        ],
    )

    print(run_manifest_path)
    if not profile_valid:
        print(
            "Echo profile run was invalid: lifecycle, runtime log, benchmark marker, shutdown, "
            f"or replay validation failed. See {run_manifest_path}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
