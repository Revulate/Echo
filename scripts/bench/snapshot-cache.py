#!/usr/bin/env python3
"""Record a structured cache directory snapshot.

This mirrors scripts/bench/snapshot-cache.ps1 for environments where PowerShell
is not installed. It records cache inventory only, not performance data.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def utc_stamp(dt: datetime) -> str:
    return dt.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def slug(value: str, fallback: str = "value") -> str:
    normalized = re.sub(r"[^a-z0-9._-]+", "-", value.lower()).strip("-")
    return normalized or fallback


def git_value(repo_root: Path, *args: str) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    value = result.stdout.strip()
    return value or None


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_json(path: Path, data: Any) -> None:
    path.write_text(json.dumps(data, indent=2, sort_keys=False) + "\n", encoding="utf-8")


def init_run_dir(repo_root: Path, args: argparse.Namespace) -> Path:
    if args.run_dir:
        run_dir = Path(args.run_dir).expanduser()
        run_dir.mkdir(parents=True, exist_ok=True)
        return run_dir.resolve()

    output_root = Path(args.output_root).expanduser() if args.output_root else repo_root / "benchmarks" / "results"
    output_root.mkdir(parents=True, exist_ok=True)
    run_id = utc_now().strftime("%Y%m%d-%H%M%S")
    run_root = output_root / run_id
    scenario_slug = slug(args.scenario, "scenario")
    scenario_dir = run_root / scenario_slug
    suffix = 1
    while scenario_dir.exists():
        scenario_dir = run_root / f"{scenario_slug}-{suffix:02d}"
        suffix += 1
    scenario_dir.mkdir(parents=True)

    dirty_output = git_value(repo_root, "status", "--porcelain") or ""
    dirty_paths = [line for line in dirty_output.splitlines() if line.strip()]
    environment = {
        "schema_version": 1,
        "run_id": run_id,
        "scenario": args.scenario,
        "scenario_slug": scenario_dir.name,
        "captured_at_utc": utc_stamp(utc_now()),
        "artifacts": {
            "root": str(scenario_dir),
            "environment": str(scenario_dir / "environment.json"),
            "summary": str(scenario_dir / "summary.json"),
            "notes": str(scenario_dir / "notes.md"),
        },
        "repo": {
            "root": str(repo_root),
            "branch": git_value(repo_root, "branch", "--show-current"),
            "echo_commit": git_value(repo_root, "rev-parse", "HEAD"),
            "echo_commit_short": git_value(repo_root, "rev-parse", "--short", "HEAD"),
            "dirty": bool(dirty_paths),
            "dirty_paths": dirty_paths,
        },
        "run_context": {
            "cache_state": args.phase,
            "network_state": "",
            "fixture_name": "",
        },
        "machine": {
            "platform": os.uname().sysname if hasattr(os, "uname") else None,
            "release": os.uname().release if hasattr(os, "uname") else None,
            "python_version": ".".join(str(part) for part in tuple(__import__("sys").version_info[:3])),
        },
    }
    summary = {
        "schema_version": 1,
        "run_id": run_id,
        "scenario": args.scenario,
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {
            "echo": None,
        },
        "decision": "not measured",
    }
    notes = [
        "# Benchmark Run Notes",
        "",
        f"- Run ID: {run_id}",
        f"- Scenario: {args.scenario}",
        "- Status: initialized",
        "- Measurements recorded: false",
        "",
        "This helper only records cache inventory metadata.",
        "Do not use this initialized run as performance evidence until raw measurements are added.",
        "",
    ]
    write_json(scenario_dir / "environment.json", environment)
    write_json(scenario_dir / "summary.json", summary)
    (scenario_dir / "notes.md").write_text("\n".join(notes), encoding="utf-8")
    return scenario_dir


def collect_snapshot(cache_path: Path, app: str, cache_name: str, phase: str, largest_file_count: int) -> dict[str, Any]:
    cache_exists = cache_path.exists()
    resolved_cache_path = cache_path.resolve() if cache_exists else cache_path
    files: list[Path] = []
    directory_count = 0

    if cache_exists:
        for root, dirs, names in os.walk(resolved_cache_path):
            directory_count += len(dirs)
            root_path = Path(root)
            for name in names:
                path = root_path / name
                if path.is_file():
                    files.append(path)

    total_bytes = sum(path.stat().st_size for path in files)
    extension_groups: dict[str, dict[str, int]] = defaultdict(lambda: {"file_count": 0, "bytes": 0})
    for path in files:
        ext = path.suffix.lower() if path.suffix else "[none]"
        extension_groups[ext]["file_count"] += 1
        extension_groups[ext]["bytes"] += path.stat().st_size

    extension_summary = [
        {
            "extension": ext,
            "file_count": values["file_count"],
            "bytes": values["bytes"],
        }
        for ext, values in extension_groups.items()
    ]
    extension_summary.sort(key=lambda item: (-item["bytes"], -item["file_count"], item["extension"]))

    largest_files = []
    for path in sorted(files, key=lambda item: item.stat().st_size, reverse=True)[:largest_file_count]:
        stat = path.stat()
        relative_path = os.path.relpath(path, resolved_cache_path) if cache_exists else str(path)
        largest_files.append(
            {
                "relative_path": relative_path,
                "bytes": stat.st_size,
                "extension": path.suffix.lower() if path.suffix else "[none]",
                "last_write_time_utc": utc_stamp(datetime.fromtimestamp(stat.st_mtime, timezone.utc)),
            }
        )

    write_times = [path.stat().st_mtime for path in files]
    oldest_write = utc_stamp(datetime.fromtimestamp(min(write_times), timezone.utc)) if write_times else None
    newest_write = utc_stamp(datetime.fromtimestamp(max(write_times), timezone.utc)) if write_times else None

    return {
        "schema_version": 1,
        "created_at_utc": utc_stamp(utc_now()),
        "app": app,
        "cache_name": cache_name,
        "phase": phase,
        "cache_path": str(resolved_cache_path),
        "cache_exists": cache_exists,
        "file_count": len(files),
        "directory_count": directory_count,
        "total_bytes": total_bytes,
        "oldest_file_write_utc": oldest_write,
        "newest_file_write_utc": newest_write,
        "extension_summary": extension_summary,
        "largest_files": largest_files,
        "decision": "cache snapshot recorded; not a benchmark comparison",
    }


def ensure_object(data: dict[str, Any], key: str) -> dict[str, Any]:
    value = data.get(key)
    if not isinstance(value, dict):
        value = {}
        data[key] = value
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-path", required=True)
    parser.add_argument("--app", choices=["echo", "other"], default="other")
    parser.add_argument("--cache-name", default="cache")
    parser.add_argument("--phase", default="snapshot")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--scenario", default="cache snapshot")
    parser.add_argument("--largest-file-count", type=int, default=20)
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[2]
    run_dir = init_run_dir(repo_root, args)
    snapshot = collect_snapshot(
        Path(args.cache_path).expanduser(),
        args.app,
        args.cache_name,
        args.phase,
        max(args.largest_file_count, 0),
    )

    artifact_name = f"{slug(args.app)}-{slug(args.cache_name)}-{slug(args.phase)}.cache.json"
    snapshot_path = run_dir / artifact_name
    write_json(snapshot_path, snapshot)

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
    cache_snapshots = ensure_object(raw_artifacts, "cache_snapshots")
    cache_key = f"{slug(args.app)}_{slug(args.cache_name)}_{slug(args.phase)}"
    cache_snapshots[cache_key] = str(snapshot_path)
    cache_summary = ensure_object(summary, "cache")
    cache_summary[cache_key] = snapshot
    summary["decision"] = "cache snapshot recorded; not compared"
    write_json(summary_path, summary)

    notes_path = run_dir / "notes.md"
    with notes_path.open("a", encoding="utf-8") as notes:
        notes.write(f"\n## Cache snapshot: {args.app}/{args.cache_name}/{args.phase}\n\n")
        notes.write(f"- Snapshot artifact: {snapshot_path}\n")
        notes.write(f"- Cache path: {snapshot['cache_path']}\n")
        notes.write(f"- File count: {snapshot['file_count']}\n")
        notes.write(f"- Total bytes: {snapshot['total_bytes']}\n")
        notes.write("- Decision: cache snapshot recorded; not compared\n")

    print(snapshot_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
