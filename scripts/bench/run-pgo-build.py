#!/usr/bin/env python3
"""Build, train, and fail-closed evaluate Linux PGO for Echo.

The runner uses isolated Cargo target directories and Cargo JSON artifacts so
baseline, instrumented, and profile-use executables never depend on guessed
target paths.  An executed run requires a clean Git source tree.  Training is
offline, profile-root isolated, high-volume replay only, and never performs a
send action.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


REPO_ROOT = Path(__file__).resolve().parents[2]
MAX_STAGE_SECONDS = 600
LAYOUT_CASES = (
    "layout_build/base_message/720px",
    "height_tree/visible_range_10k",
)
BENCHER_NS_RE = re.compile(r"bench:\s*([0-9,]+)\s+ns/iter")
CRITERION_TIME_RE = re.compile(
    r"time:\s*\[[0-9.]+\s+\S+\s+([0-9.]+)\s+(ns|µs|us|ms|s)\s+[0-9.]+\s+\S+\]"
)


class PgoError(RuntimeError):
    """A fail-closed PGO workflow error."""


def utc_stamp() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def llvm_profdata_candidates(sysroot: Path, host: str) -> tuple[Path, ...]:
    base = sysroot / "lib" / "rustlib" / host / "bin"
    return (base / "llvm-profdata", base / "llvm-profdata.exe")


def resolve_llvm_profdata(explicit: str = "", rustc: str = "rustc") -> Path:
    if explicit:
        candidate = Path(explicit).expanduser()
        resolved = candidate.resolve() if candidate.exists() else shutil.which(explicit)
        if resolved and Path(resolved).is_file():
            return Path(resolved).resolve()
        raise PgoError(f"llvm-profdata was not found: {explicit}")

    rustc_path = shutil.which(rustc)
    if not rustc_path:
        raise PgoError(f"rustc was not found: {rustc}")
    sysroot_result = subprocess.run(
        [rustc_path, "--print", "sysroot"], capture_output=True, text=True, check=True
    )
    version_result = subprocess.run(
        [rustc_path, "-vV"], capture_output=True, text=True, check=True
    )
    host = next(
        (line.partition(":")[2].strip() for line in version_result.stdout.splitlines() if line.startswith("host:")),
        "",
    )
    if not host:
        raise PgoError("rustc -vV did not report a host triple")
    for candidate in llvm_profdata_candidates(Path(sysroot_result.stdout.strip()), host):
        if candidate.is_file():
            return candidate.resolve()
    raise PgoError("sysroot llvm-profdata is missing; install rustup component llvm-tools-preview")


def parse_cargo_executable(lines: Iterable[str], package: str, target: str, kind: str) -> Path:
    matches: list[Path] = []
    for line in lines:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        target_data = message.get("target") or {}
        package_id = str(message.get("package_id", ""))
        if target_data.get("name") != target or kind not in target_data.get("kind", []):
            continue
        if not (package_id.startswith("path+") and f"#{package}@" in package_id):
            continue
        matches.append(Path(message["executable"]).resolve())
    unique = list(dict.fromkeys(matches))
    if len(unique) != 1:
        raise PgoError(
            f"expected one Cargo JSON executable for {package}:{target}:{kind}, found {len(unique)}"
        )
    return unique[0]


def fresh_profraw_files(profile_dir: Path, started_ns: int) -> list[Path]:
    files = sorted(profile_dir.glob("*.profraw"))
    stale = [path for path in files if path.stat().st_size <= 0 or path.stat().st_mtime_ns < started_ns]
    if not files:
        raise PgoError("training produced no .profraw files")
    if stale:
        raise PgoError(f"training profile data is empty or stale: {', '.join(str(path) for path in stale)}")
    return files


def clear_instrumented_build_profiles(profile_dir: Path) -> list[dict[str, Any]]:
    """Record and remove profiles emitted while compiling instrumented tools.

    Instrumented build scripts and compiler-run helper binaries legitimately
    execute during `cargo build`. Their profiles are not representative Echo
    training input and must be removed before the measured application launch.
    """
    profile_dir.mkdir(parents=True, exist_ok=True)
    removed: list[dict[str, Any]] = []
    for path in sorted(profile_dir.iterdir()):
        if not path.is_file() or path.suffix != ".profraw":
            raise PgoError(f"unexpected entry in profile directory: {path}")
        removed.append({"path": str(path), "size_bytes": path.stat().st_size, "sha256": sha256(path)})
        path.unlink()
    return removed


def percent_delta(baseline: float, candidate: float) -> float:
    if not math.isfinite(baseline) or not math.isfinite(candidate):
        raise PgoError("measurements must be finite")
    if baseline <= 0 or candidate <= 0:
        raise PgoError("measurements must be positive")
    return ((candidate - baseline) / baseline) * 100.0


def evaluate_ab(lanes: dict[str, dict[str, Any]], threshold_percent: float = 5.0) -> dict[str, Any]:
    reasons: list[str] = []
    for required in ("startup", "layout", "search"):
        if required not in lanes:
            reasons.append(f"{required}: lane missing")
    for name, lane in lanes.items():
        if not lane.get("correctness", False):
            reasons.append(f"{name}: correctness failed")
        cases = lane.get("cases", [])
        if not cases:
            reasons.append(f"{name}: no performance cases")
        for case in cases:
            delta = case.get("delta_percent")
            if not isinstance(delta, (int, float)):
                reasons.append(f"{name}/{case.get('name', 'unknown')}: missing performance delta")
            elif not math.isfinite(delta):
                reasons.append(f"{name}/{case.get('name', 'unknown')}: non-finite performance delta")
            elif delta > threshold_percent:
                reasons.append(f"{name}/{case.get('name', 'unknown')}: regression {delta:.3f}% exceeds {threshold_percent:.1f}%")
    return {
        "decision": "revert" if reasons else "keep",
        "hard_revert": bool(reasons),
        "regression_threshold_percent": threshold_percent,
        "reasons": reasons,
    }


def training_environment(
    inherited: dict[str, str], profile_root: Path, profraw_pattern: Path, replay: Path, report: Path
) -> dict[str, str]:
    env = {
        key: value
        for key, value in inherited.items()
        if not key.startswith("ECHO_BENCH_")
        and not key.startswith("ECHO_CHAT_REPLAY")
        and key != "LLVM_PROFILE_FILE"
    }
    env.update(
        {
            "ECHO_BENCH_OFFLINE": "1",
            "ECHO_BENCH_PROFILE_ROOT": str(profile_root),
            "ECHO_BENCH_PROFILE_RECEIPT": str(profile_root / "profile-receipt.json"),
            "ECHO_CHAT_REPLAY": str(replay),
            "ECHO_CHAT_REPLAY_REPORT": str(report),
            "ECHO_CHAT_REPLAY_CLEAR_CHANNEL": "1",
            "ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES": "1",
            "ECHO_CHAT_REPLAY_SPEEDUP": "0",
            "LLVM_PROFILE_FILE": str(profraw_pattern),
        }
    )
    return env


@dataclass
class StageRunner:
    run_dir: Path
    timeout_seconds: int
    dry_run: bool = False

    def run(
        self,
        name: str,
        command: list[str],
        *,
        env: dict[str, str] | None = None,
        cwd: Path = REPO_ROOT,
    ) -> dict[str, Any]:
        if not 1 <= self.timeout_seconds <= MAX_STAGE_SECONDS:
            raise PgoError(f"stage timeout must be in 1..{MAX_STAGE_SECONDS}")
        log = self.run_dir / "logs" / f"{name}.log"
        log.parent.mkdir(parents=True, exist_ok=True)
        record: dict[str, Any] = {
            "name": name,
            "command": command,
            "cwd": str(cwd),
            "timeout_seconds": self.timeout_seconds,
            "log": str(log),
            "dry_run": self.dry_run,
        }
        if self.dry_run:
            log.write_text("DRY RUN: " + " ".join(command) + "\n", encoding="utf-8")
            return record
        started = time.monotonic()
        with log.open("w", encoding="utf-8") as output:
            try:
                result = subprocess.run(
                    command,
                    cwd=cwd,
                    env=env,
                    stdout=output,
                    stderr=subprocess.STDOUT,
                    text=True,
                    timeout=self.timeout_seconds,
                    check=False,
                )
            except subprocess.TimeoutExpired as error:
                raise PgoError(f"stage {name} exceeded {self.timeout_seconds}s; see {log}") from error
        record.update(exit_code=result.returncode, elapsed_seconds=round(time.monotonic() - started, 3))
        if result.returncode != 0:
            raise PgoError(f"stage {name} failed with {result.returncode}; see {log}")
        return record


def git_provenance(ignore_root: Path | None = None) -> dict[str, Any]:
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    if ignore_root is not None:
        try:
            ignored = ignore_root.resolve().relative_to(REPO_ROOT).as_posix().rstrip("/")
        except ValueError:
            ignored = ""
        if ignored:
            status = [
                line
                for line in status
                if not (
                    line[3:].strip('"').replace("\\", "/") == ignored
                    or line[3:].strip('"').replace("\\", "/").startswith(ignored + "/")
                )
            ]
    return {"commit": commit, "clean": not status, "dirty_paths": status}


def cargo_build(
    runner: StageRunner,
    flavor: str,
    target_dir: Path,
    rustflags: str,
    *,
    include_benchmarks: bool,
) -> tuple[dict[str, Path], list[dict[str, Any]]]:
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    if rustflags:
        env["RUSTFLAGS"] = rustflags
    else:
        env.pop("RUSTFLAGS", None)
    env.pop("LLVM_PROFILE_FILE", None)
    cargo = shutil.which("cargo") or "cargo"
    commands = [
        (
            f"build-{flavor}-echo",
            [cargo, "build", "--locked", "--profile", "dist", "--message-format=json-render-diagnostics", "-p", "echo_gpui", "--bin", "echo"],
        )
    ]
    if include_benchmarks:
        commands.append(
            (
                f"build-{flavor}-layout",
                [cargo, "bench", "--locked", "--no-run", "--message-format=json-render-diagnostics", "-p", "echo_gpui", "--bench", "layout_bench"],
            )
        )
        commands.append(
            (
                f"build-{flavor}-search",
                [cargo, "build", "--locked", "--profile", "dist", "--message-format=json-render-diagnostics", "-p", "search-bench", "--bin", "search-bench"],
            )
        )
    stages: list[dict[str, Any]] = []
    logs: list[Path] = []
    for name, command in commands:
        stage = runner.run(name, command, env=env)
        stages.append(stage)
        logs.append(Path(stage["log"]))
    if runner.dry_run:
        return {}, stages
    artifacts = {
        "echo": parse_cargo_executable(logs[0].read_text(encoding="utf-8").splitlines(), "echo_gpui", "echo", "bin")
    }
    if include_benchmarks:
        artifacts["layout"] = parse_cargo_executable(
            logs[1].read_text(encoding="utf-8").splitlines(), "echo_gpui", "layout_bench", "bench"
        )
        artifacts["search"] = parse_cargo_executable(
            logs[2].read_text(encoding="utf-8").splitlines(), "search-bench", "search-bench", "bin"
        )
    for name, path in artifacts.items():
        if not path.is_file():
            raise PgoError(f"Cargo reported missing {name} executable: {path}")
    return artifacts, stages


def copy_artifacts(artifacts: dict[str, Path], destination: Path) -> dict[str, Path]:
    destination.mkdir(parents=True, exist_ok=True)
    copied: dict[str, Path] = {}
    for name, source in artifacts.items():
        target = destination / name
        shutil.copy2(source, target)
        target.chmod(target.stat().st_mode | 0o111)
        copied[name] = target.resolve()
    return copied


def stop_process(process: subprocess.Popen[Any]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def run_training(
    binary: Path,
    replay: Path,
    profile_dir: Path,
    run_dir: Path,
    timeout_seconds: int,
    commit: str,
) -> tuple[list[Path], dict[str, Any]]:
    if list(profile_dir.glob("*.profraw")):
        raise PgoError("profile directory is not fresh")
    profile_dir.mkdir(parents=True, exist_ok=True)
    profile_root = run_dir / "profiles" / "training"
    report = run_dir / "training-replay-report.json"
    env = training_environment(
        os.environ,
        profile_root,
        profile_dir / "echo-%m-%p.profraw",
        replay,
        report,
    )
    env["ECHO_BENCH_TARGET_COMMIT"] = commit
    env["ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS"] = str(max(1000, (timeout_seconds - 5) * 1000))
    stdout_path = run_dir / "logs" / "training.log"
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    started_ns = time.time_ns()
    with stdout_path.open("w", encoding="utf-8") as output:
        process = subprocess.Popen(
            [str(binary)],
            cwd=REPO_ROOT,
            env=env,
            stdout=output,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        try:
            exit_code = process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            stop_process(process)
            raise PgoError(f"training exceeded {timeout_seconds}s; see {stdout_path}")
    if exit_code != 0:
        raise PgoError(f"training exited with {exit_code}; see {stdout_path}")
    files = fresh_profraw_files(profile_dir, started_ns)
    if not report.is_file():
        raise PgoError("training did not publish its replay report")
    replay_result = json.loads(report.read_text(encoding="utf-8"))
    if (
        not replay_result.get("replay_injected")
        or replay_result.get("inserted_messages", 0) <= 0
        or replay_result.get("parse_errors") != 0
        or replay_result.get("skipped_events") != 0
    ):
        raise PgoError(f"training replay correctness failed: {replay_result}")
    return files, replay_result


def startup_sample(binary: Path, root: Path, timeout_seconds: int, commit: str) -> float:
    root.mkdir(parents=True, exist_ok=True)
    receipt = root / "profile-receipt.json"
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("ECHO_BENCH_") and not key.startswith("ECHO_CHAT_REPLAY")
    }
    env.update(
        {
            "ECHO_BENCH_OFFLINE": "1",
            "ECHO_BENCH_PROFILE_ROOT": str(root),
            "ECHO_BENCH_PROFILE_RECEIPT": str(receipt),
            "ECHO_BENCH_TARGET_COMMIT": commit,
            "ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS": str(timeout_seconds * 1000),
        }
    )
    started = time.monotonic()
    process = subprocess.Popen(
        [str(binary)],
        cwd=REPO_ROOT,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        deadline = started + timeout_seconds
        while time.monotonic() < deadline:
            if receipt.is_file():
                elapsed_ms = (time.monotonic() - started) * 1000.0
                data = json.loads(receipt.read_text(encoding="utf-8"))
                if data.get("process_id") != process.pid:
                    raise PgoError("startup receipt process identity mismatch")
                if Path(data.get("executable_path", "")).resolve() != binary.resolve():
                    raise PgoError("startup receipt executable path mismatch")
                if data.get("executable_sha256", "").lower() != sha256(binary):
                    raise PgoError("startup receipt executable hash mismatch")
                return elapsed_ms
            if process.poll() is not None:
                raise PgoError(f"startup process exited before receipt with {process.returncode}")
            time.sleep(0.01)
        raise PgoError(f"startup receipt timed out after {timeout_seconds}s")
    finally:
        stop_process(process)


def startup_lane(
    baseline: Path, candidate: Path, run_dir: Path, runs: int, timeout_seconds: int, commit: str
) -> dict[str, Any]:
    samples = {"baseline": [], "candidate": []}
    sequence = ("baseline", "candidate", "candidate", "baseline")
    binaries = {"baseline": baseline, "candidate": candidate}
    counters = {"baseline": 0, "candidate": 0}
    for flavor in sequence * runs:
        if counters[flavor] >= runs:
            continue
        index = counters[flavor]
        counters[flavor] += 1
        samples[flavor].append(
            startup_sample(binaries[flavor], run_dir / "profiles" / f"startup-{flavor}-{index}", timeout_seconds, commit)
        )
    baseline_median = statistics.median(samples["baseline"])
    candidate_median = statistics.median(samples["candidate"])
    return {
        "correctness": True,
        "readiness": "atomic benchmark profile receipt with executable identity",
        "samples_ms": samples,
        "cases": [
            {
                "name": "profile_receipt_startup",
                "baseline_median": baseline_median,
                "candidate_median": candidate_median,
                "unit": "ms",
                "delta_percent": percent_delta(baseline_median, candidate_median),
            }
        ],
    }


def parse_bencher_ns(output: str) -> float:
    matches = BENCHER_NS_RE.findall(output)
    if len(matches) == 1:
        return float(matches[0].replace(",", ""))
    criterion_matches = CRITERION_TIME_RE.findall(output)
    if len(criterion_matches) != 1:
        raise PgoError(
            "expected one Criterion estimate, "
            f"found bencher={len(matches)} criterion={len(criterion_matches)}"
        )
    value, unit = criterion_matches[0]
    multiplier = {"ns": 1.0, "µs": 1_000.0, "us": 1_000.0, "ms": 1_000_000.0, "s": 1_000_000_000.0}[unit]
    return float(value) * multiplier


def layout_lane(runner: StageRunner, baseline: Path, candidate: Path) -> dict[str, Any]:
    cases = []
    for case_index, case in enumerate(LAYOUT_CASES):
        values: dict[str, float] = {}
        for flavor, binary in (("baseline", baseline), ("candidate", candidate)):
            cwd = runner.run_dir / "evaluation-work" / f"layout-{case_index}-{flavor}"
            cwd.mkdir(parents=True, exist_ok=True)
            stage = runner.run(
                f"layout-{case_index}-{flavor}",
                [str(binary), "--bench", case, "--sample-size", "10", "--warm-up-time", "0.2", "--measurement-time", "0.5", "--noplot"],
                cwd=cwd,
            )
            values[flavor] = parse_bencher_ns(Path(stage["log"]).read_text(encoding="utf-8"))
        cases.append(
            {
                "name": case,
                "baseline_median": values["baseline"],
                "candidate_median": values["candidate"],
                "unit": "ns/iter",
                "delta_percent": percent_delta(values["baseline"], values["candidate"]),
            }
        )
    return {"correctness": True, "cases": cases}


def parse_search_csv(text: str) -> dict[str, tuple[float, int]]:
    rows = list(csv.DictReader(text.splitlines()))
    if not rows:
        raise PgoError("search benchmark emitted no CSV rows")
    parsed: dict[str, tuple[float, int]] = {}
    for row in rows:
        key = f"{row['kind']}/{row['name']}"
        if key in parsed:
            raise PgoError(f"duplicate search benchmark row: {key}")
        measurement = float(row["ns_per_unit"])
        if not math.isfinite(measurement) or measurement <= 0:
            raise PgoError(f"invalid search benchmark measurement for {key}")
        parsed[key] = (measurement, int(row["checksum"]))
    return parsed


def search_lane(runner: StageRunner, baseline: Path, candidate: Path, runs: int) -> dict[str, Any]:
    samples: dict[str, dict[str, list[float]]] = {"baseline": {}, "candidate": {}}
    checksums: dict[str, dict[str, set[int]]] = {"baseline": {}, "candidate": {}}
    for run_index in range(runs):
        for flavor, binary in (("baseline", baseline), ("candidate", candidate)):
            cwd = runner.run_dir / "evaluation-work" / f"search-{run_index}-{flavor}"
            cwd.mkdir(parents=True, exist_ok=True)
            stage = runner.run(f"search-{run_index}-{flavor}", [str(binary)], cwd=cwd)
            parsed = parse_search_csv(Path(stage["log"]).read_text(encoding="utf-8"))
            for name, (ns, checksum) in parsed.items():
                samples[flavor].setdefault(name, []).append(ns)
                checksums[flavor].setdefault(name, set()).add(checksum)
    correctness = samples["baseline"].keys() == samples["candidate"].keys()
    cases = []
    for name in sorted(samples["baseline"]):
        correct = (
            checksums["baseline"].get(name) == checksums["candidate"].get(name)
            and len(checksums["baseline"].get(name, set())) == 1
        )
        correctness = correctness and correct
        baseline_median = statistics.median(samples["baseline"][name])
        candidate_median = statistics.median(samples["candidate"][name])
        cases.append(
            {
                "name": name,
                "correctness": correct,
                "baseline_median": baseline_median,
                "candidate_median": candidate_median,
                "unit": "ns/unit",
                "delta_percent": percent_delta(baseline_median, candidate_median),
            }
        )
    return {"correctness": correctness, "cases": cases}


def default_run_dir() -> Path:
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    return REPO_ROOT / "benchmarks" / "results" / stamp / "linux-pgo"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--llvm-profdata", default="")
    parser.add_argument("--replay", default="", help="Existing high-volume NDJSON replay; generated when omitted")
    parser.add_argument("--stage-timeout-seconds", type=int, default=600)
    parser.add_argument("--training-timeout-seconds", type=int, default=120)
    parser.add_argument("--startup-timeout-seconds", type=int, default=30)
    parser.add_argument("--startup-runs", type=int, default=3)
    parser.add_argument("--search-runs", type=int, default=3)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.stage_timeout_seconds <= MAX_STAGE_SECONDS:
        parser.error(f"--stage-timeout-seconds must be in 1..{MAX_STAGE_SECONDS}")
    if not 10 <= args.training_timeout_seconds <= MAX_STAGE_SECONDS:
        parser.error(f"--training-timeout-seconds must be in 10..{MAX_STAGE_SECONDS}")
    if not 1 <= args.startup_timeout_seconds <= 300:
        parser.error("--startup-timeout-seconds must be in 1..300")
    if not 2 <= args.startup_runs <= 5 or not 2 <= args.search_runs <= 5:
        parser.error("startup/search runs must be in 2..5")

    provenance = git_provenance()
    run_dir = Path(args.run_dir).expanduser().resolve() if args.run_dir else default_run_dir()
    run_dir.mkdir(parents=True, exist_ok=True)
    runner = StageRunner(run_dir, args.stage_timeout_seconds, args.dry_run)
    report: dict[str, Any] = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "platform": sys.platform,
        "dry_run": args.dry_run,
        "source": provenance,
        "same_source_provenance": False,
        "stages": [],
        "lanes": {},
        "decision": "revert",
        "hard_revert": True,
        "errors": [],
    }
    report_path = run_dir / "pgo-report.json"
    try:
        if sys.platform != "linux":
            raise PgoError("this companion runner supports Linux only")
        if not provenance["clean"] and not args.dry_run:
            raise PgoError("executed PGO runs require a clean Git source tree")
        llvm_profdata = Path("llvm-profdata") if args.dry_run else resolve_llvm_profdata(args.llvm_profdata)
        report["llvm_profdata"] = str(llvm_profdata)
        targets = run_dir / "cargo-targets"
        baseline, stages = cargo_build(runner, "baseline", targets / "baseline", "", include_benchmarks=True)
        report["stages"].extend(stages)
        instrumented, stages = cargo_build(
            runner,
            "instrumented",
            targets / "instrumented",
            f"-Cprofile-generate={run_dir / 'profraw'}",
            include_benchmarks=False,
        )
        report["stages"].extend(stages)
        if args.dry_run:
            replay = Path(args.replay).resolve() if args.replay else run_dir / "fixture" / "chat-replay-high-volume.ndjson"
            report.update(status="dry-run", replay=str(replay), decision="revert", hard_revert=True)
            write_json(report_path, report)
            print(report_path)
            return 0

        if args.replay:
            replay = Path(args.replay).expanduser().resolve()
            if not replay.is_file():
                raise PgoError(f"replay fixture does not exist: {replay}")
        else:
            fixture_dir = run_dir / "fixture"
            stage = runner.run(
                "generate-high-volume-fixture",
                [
                    sys.executable,
                    str(REPO_ROOT / "scripts/bench/new-chat-replay-fixture.py"),
                    "--run-dir",
                    str(fixture_dir),
                    "--replay-scenario",
                    "high-volume",
                    "--duration-seconds",
                    "30",
                ],
            )
            report["stages"].append(stage)
            fixtures = sorted(fixture_dir.glob("*.ndjson"))
            if len(fixtures) != 1:
                raise PgoError(f"expected one generated high-volume replay, found {len(fixtures)}")
            replay = fixtures[0]
        report["replay"] = {"path": str(replay), "sha256": sha256(replay)}

        binaries_root = run_dir / "binaries"
        baseline = copy_artifacts(baseline, binaries_root / "baseline")
        instrumented = copy_artifacts(instrumented, binaries_root / "instrumented")
        report["instrumented_build_profiles"] = clear_instrumented_build_profiles(
            run_dir / "profraw"
        )
        profraw, replay_result = run_training(
            instrumented["echo"], replay, run_dir / "profraw", run_dir, args.training_timeout_seconds, provenance["commit"]
        )
        report["training"] = {
            "offline": True,
            "profile_root": str(run_dir / "profiles" / "training"),
            "profraw": [str(path) for path in profraw],
            "replay_result": replay_result,
        }
        input_list = run_dir / "profraw-inputs.txt"
        input_list.write_text("\n".join(str(path) for path in profraw) + "\n", encoding="utf-8")
        merged = run_dir / "echo.profdata"
        stage = runner.run(
            "merge-profraw",
            [str(llvm_profdata), "merge", f"--output={merged}", f"--input-files={input_list}"],
        )
        report["stages"].append(stage)
        if not merged.is_file() or merged.stat().st_size <= 0:
            raise PgoError("llvm-profdata did not create a non-empty merged profile")

        candidate, stages = cargo_build(
            runner,
            "profile-use",
            targets / "profile-use",
            f"-Cprofile-use={merged} -Cllvm-args=-pgo-warn-missing-function",
            include_benchmarks=True,
        )
        report["stages"].extend(stages)
        candidate = copy_artifacts(candidate, binaries_root / "candidate")
        end_provenance = git_provenance(run_dir)
        report["source_after"] = end_provenance
        report["same_source_provenance"] = (
            end_provenance["commit"] == provenance["commit"] and end_provenance["clean"]
        )
        if not report["same_source_provenance"]:
            raise PgoError("source provenance changed during the PGO run")

        report["binaries"] = {
            flavor: {name: {"path": str(path), "sha256": sha256(path)} for name, path in values.items()}
            for flavor, values in (("baseline", baseline), ("candidate", candidate))
        }
        report["lanes"]["startup"] = startup_lane(
            baseline["echo"], candidate["echo"], run_dir, args.startup_runs, args.startup_timeout_seconds, provenance["commit"]
        )
        report["lanes"]["layout"] = layout_lane(runner, baseline["layout"], candidate["layout"])
        report["lanes"]["search"] = search_lane(runner, baseline["search"], candidate["search"], args.search_runs)
        verdict = evaluate_ab(report["lanes"])
        report.update(verdict)
        final_provenance = git_provenance(run_dir)
        report["source_final"] = final_provenance
        if final_provenance["commit"] != provenance["commit"] or not final_provenance["clean"]:
            raise PgoError("source provenance changed during A/B evaluation")
        report["status"] = "pass" if verdict["decision"] == "keep" else "fail"
    except Exception as error:  # retain a hard-revert artifact for every executed failure
        report["status"] = "fail"
        report["decision"] = "revert"
        report["hard_revert"] = True
        report["errors"].append(str(error))
    write_json(report_path, report)
    print(report_path)
    return 0 if report.get("status") in {"pass", "dry-run"} else 1


if __name__ == "__main__":
    raise SystemExit(main())
