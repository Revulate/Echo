#!/usr/bin/env python3
"""Shared helpers for cross-platform benchmark artifact scripts."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def utc_stamp(dt: datetime | None = None) -> str:
    return (dt or utc_now()).astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def slug(value: str, fallback: str = "scenario") -> str:
    normalized = re.sub(r"[^a-z0-9._-]+", "-", value.lower()).strip("-")
    return normalized or fallback


def git_value(*args: str) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(REPO_ROOT), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    value = result.stdout.strip()
    return value or None


def repo_dirty_paths() -> list[str]:
    output = git_value("status", "--porcelain") or ""
    return [line for line in output.splitlines() if line.strip()]


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=False) + "\n", encoding="utf-8")


def ensure_object(data: dict[str, Any], key: str) -> dict[str, Any]:
    value = data.get(key)
    if not isinstance(value, dict):
        value = {}
        data[key] = value
    return value


def append_notes(run_dir: Path, title: str, lines: list[str]) -> None:
    notes_path = run_dir / "notes.md"
    with notes_path.open("a", encoding="utf-8") as notes:
        notes.write(f"\n## {title}\n\n")
        for line in lines:
            notes.write(f"{line}\n")


def finalize_notes_status(run_dir: Path, status: str, measurements_recorded: bool) -> None:
    notes_path = run_dir / "notes.md"
    if not notes_path.exists():
        return
    content = notes_path.read_text(encoding="utf-8")
    content = content.replace("- Status: initialized", f"- Status: {status}", 1)
    content = content.replace(
        "- Measurements recorded: false",
        f"- Measurements recorded: {str(measurements_recorded).lower()}",
        1,
    )
    notes_path.write_text(content, encoding="utf-8")


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest().upper()


def resolve_executable(value: str) -> Path:
    expanded = Path(value).expanduser()
    if expanded.exists():
        return expanded.resolve()
    resolved = shutil.which(value)
    if resolved:
        return Path(resolved).resolve()
    raise FileNotFoundError(f"ExecutablePath '{value}' was not found as a file or executable command.")


def file_metadata(path: Path) -> dict[str, Any]:
    stat = path.stat()
    return {
        "length_bytes": stat.st_size,
        "last_write_time_utc": utc_stamp(datetime.fromtimestamp(stat.st_mtime, timezone.utc)),
        "sha256": file_sha256(path),
    }


def null_stats() -> dict[str, int | None]:
    return {
        "min": None,
        "avg": None,
        "max": None,
    }


def series_stats(values: list[Any]) -> dict[str, int | None]:
    clean: list[float] = []
    for value in values:
        if value is None or value == "":
            continue
        try:
            clean.append(float(value))
        except (TypeError, ValueError):
            continue
    if not clean:
        return null_stats()
    return {
        "min": round(min(clean)),
        "avg": round(sum(clean) / len(clean)),
        "max": round(max(clean)),
    }


def percentile(sorted_values: list[float], percent: float) -> float | None:
    """Percentile of ascending-sorted values by linear interpolation between the
    closest ranks (rank = percent / 100 * (n - 1); numpy "linear", Excel
    PERCENTILE.INC), rounded to 6 decimals. BenchLib.psm1's Get-Percentile uses
    the same definition. Returns None for empty input."""
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


def initialize_run_metadata(
    scenario_dir: Path,
    run_id: str,
    scenario: str = "benchmark run",
    echo_path: str = "",
    echo_build_type: str = "",
    cache_state: str = "",
    network_state: str = "",
    fixture_name: str = "",
    notes: str = "",
) -> None:
    dirty_paths = repo_dirty_paths()
    environment = {
        "schema_version": 1,
        "run_id": run_id,
        "scenario": scenario,
        "scenario_slug": scenario_dir.name,
        "captured_at_utc": utc_stamp(),
        "artifacts": {
            "root": str(scenario_dir),
            "environment": str(scenario_dir / "environment.json"),
            "summary": str(scenario_dir / "summary.json"),
            "notes": str(scenario_dir / "notes.md"),
            "expected_echo_raw": str(scenario_dir / "echo.raw.txt"),
        },
        "repo": {
            "root": str(REPO_ROOT),
            "branch": git_value("branch", "--show-current"),
            "echo_commit": git_value("rev-parse", "HEAD"),
            "echo_commit_short": git_value("rev-parse", "--short", "HEAD"),
            "dirty": bool(dirty_paths),
            "dirty_paths": dirty_paths,
        },
        "apps": {
            "echo": {
                "path": echo_path,
                "build_type": echo_build_type,
            },
        },
        "run_context": {
            "cache_state": cache_state,
            "network_state": network_state,
            "fixture_name": fixture_name,
        },
        "machine": {
            "computer_name": platform.node(),
            "os": {
                "system": platform.system(),
                "release": platform.release(),
                "version": platform.version(),
                "machine": platform.machine(),
                "platform": platform.platform(),
            },
            "cpu": {
                "processor": platform.processor(),
                "logical_processors": os.cpu_count(),
            },
            "gpu": [],
            "total_physical_memory_bytes": None,
            "power_scheme": None,
            "python_version": platform.python_version(),
        },
    }
    summary = {
        "schema_version": 1,
        "run_id": run_id,
        "scenario": scenario,
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {
            "echo": None,
        },
        "decision": "not measured",
    }
    note_lines = [
        "# Benchmark Run Notes",
        "",
        f"- Run ID: {run_id}",
        f"- Scenario: {scenario}",
        "- Status: initialized",
        "- Measurements recorded: false",
        "",
    ]
    if notes:
        note_lines.extend(["## Notes", "", notes, ""])
    note_lines.extend(
        [
            "This helper only creates the artifact directory and captures environment metadata.",
            "Do not use this initialized run as performance evidence until raw measurements are added.",
            "",
        ]
    )

    environment_path = scenario_dir / "environment.json"
    summary_path = scenario_dir / "summary.json"
    notes_path = scenario_dir / "notes.md"
    if not environment_path.exists():
        write_json(environment_path, environment)
    if not summary_path.exists():
        write_json(summary_path, summary)
    if not notes_path.exists():
        notes_path.write_text("\n".join(note_lines), encoding="utf-8")


def initialize_run_dir(
    run_dir: str = "",
    output_root: str = "",
    scenario: str = "benchmark run",
    echo_path: str = "",
    echo_build_type: str = "",
    cache_state: str = "",
    network_state: str = "",
    fixture_name: str = "",
    notes: str = "",
) -> Path:
    if run_dir:
        scenario_dir = Path(run_dir).expanduser().resolve()
        scenario_dir.mkdir(parents=True, exist_ok=True)
        if not (scenario_dir / "environment.json").exists():
            initialize_run_metadata(
                scenario_dir,
                scenario_dir.name,
                scenario,
                echo_path,
                echo_build_type,
                cache_state,
                network_state,
                fixture_name,
                notes,
            )
        return scenario_dir

    root = Path(output_root).expanduser() if output_root else REPO_ROOT / "benchmarks" / "results"
    root.mkdir(parents=True, exist_ok=True)
    run_id = utc_now().strftime("%Y%m%d-%H%M%S")
    run_root = root / run_id
    scenario_dir = run_root / slug(scenario)
    suffix = 1
    while scenario_dir.exists():
        scenario_dir = run_root / f"{slug(scenario)}-{suffix:02d}"
        suffix += 1
    scenario_dir.mkdir(parents=True)
    initialize_run_metadata(
        scenario_dir,
        run_id,
        scenario,
        echo_path,
        echo_build_type,
        cache_state,
        network_state,
        fixture_name,
        notes,
    )
    return scenario_dir.resolve()
