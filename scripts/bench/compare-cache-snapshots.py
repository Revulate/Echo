#!/usr/bin/env python3
"""Compare two cache snapshot JSON artifacts.

This mirrors scripts/bench/compare-cache-snapshots.ps1 for environments where
PowerShell is not installed. It records before/after cache inventory deltas only;
it is not an app performance comparison.
"""

from __future__ import annotations

import argparse
import json
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def slug(value: str, fallback: str = "value") -> str:
    normalized = re.sub(r"[^a-z0-9._-]+", "-", value.lower()).strip("-")
    return normalized or fallback


def read_snapshot(path: Path) -> tuple[Path, dict[str, Any]]:
    resolved = path.expanduser().resolve()
    with resolved.open("r", encoding="utf-8") as handle:
        data = json.load(handle)

    if "schema_version" not in data:
        raise ValueError(f"Cache snapshot is missing schema_version: {resolved}")
    for field in ("app", "cache_name", "phase", "file_count", "directory_count", "total_bytes"):
        if field not in data:
            raise ValueError(f"Cache snapshot is missing {field}: {resolved}")
    return resolved, data


def write_json(path: Path, data: Any) -> None:
    path.write_text(json.dumps(data, indent=2, sort_keys=False) + "\n", encoding="utf-8")


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def ensure_object(data: dict[str, Any], key: str) -> dict[str, Any]:
    value = data.get(key)
    if not isinstance(value, dict):
        value = {}
        data[key] = value
    return value


def extension_map(snapshot: dict[str, Any]) -> dict[str, dict[str, int | str]]:
    result: dict[str, dict[str, int | str]] = {}
    for entry in snapshot.get("extension_summary") or []:
        extension = str(entry.get("extension") or "[unknown]")
        result[extension] = {
            "extension": extension,
            "file_count": int(entry.get("file_count") or 0),
            "bytes": int(entry.get("bytes") or 0),
        }
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before-snapshot", required=True)
    parser.add_argument("--after-snapshot", required=True)
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-file", default="")
    args = parser.parse_args()

    before_path, before = read_snapshot(Path(args.before_snapshot))
    after_path, after = read_snapshot(Path(args.after_snapshot))

    run_dir = Path(args.run_dir).expanduser().resolve() if args.run_dir else after_path.parent
    run_dir.mkdir(parents=True, exist_ok=True)

    app = str(after.get("app") or "unknown")
    cache_name = str(after.get("cache_name") or "cache")
    before_phase = str(before.get("phase") or "before")
    after_phase = str(after.get("phase") or "after")

    before_ext = extension_map(before)
    after_ext = extension_map(after)
    extensions = sorted(set(before_ext) | set(after_ext))
    extension_deltas = []
    for extension in extensions:
        before_entry = before_ext.get(extension, {"file_count": 0, "bytes": 0})
        after_entry = after_ext.get(extension, {"file_count": 0, "bytes": 0})
        before_file_count = int(before_entry["file_count"])
        after_file_count = int(after_entry["file_count"])
        before_bytes = int(before_entry["bytes"])
        after_bytes = int(after_entry["bytes"])
        extension_deltas.append(
            {
                "extension": extension,
                "before_file_count": before_file_count,
                "after_file_count": after_file_count,
                "file_count_delta": after_file_count - before_file_count,
                "before_bytes": before_bytes,
                "after_bytes": after_bytes,
                "bytes_delta": after_bytes - before_bytes,
            }
        )

    before_file_count = int(before.get("file_count") or 0)
    after_file_count = int(after.get("file_count") or 0)
    before_directory_count = int(before.get("directory_count") or 0)
    after_directory_count = int(after.get("directory_count") or 0)
    before_bytes = int(before.get("total_bytes") or 0)
    after_bytes = int(after.get("total_bytes") or 0)

    comparison = {
        "schema_version": 1,
        "created_at_utc": utc_now(),
        "app": app,
        "cache_name": cache_name,
        "before_snapshot": str(before_path),
        "after_snapshot": str(after_path),
        "before_phase": before_phase,
        "after_phase": after_phase,
        "same_app": str(before.get("app")) == str(after.get("app")),
        "same_cache": str(before.get("cache_name")) == str(after.get("cache_name")),
        "before": {
            "cache_exists": bool(before.get("cache_exists")),
            "file_count": before_file_count,
            "directory_count": before_directory_count,
            "total_bytes": before_bytes,
        },
        "after": {
            "cache_exists": bool(after.get("cache_exists")),
            "file_count": after_file_count,
            "directory_count": after_directory_count,
            "total_bytes": after_bytes,
        },
        "delta": {
            "file_count": after_file_count - before_file_count,
            "directory_count": after_directory_count - before_directory_count,
            "total_bytes": after_bytes - before_bytes,
        },
        "extension_deltas": extension_deltas,
        "decision": "cache snapshots compared; not an app performance comparison",
    }

    output_file = Path(args.output_file).expanduser() if args.output_file else run_dir / (
        f"{slug(app)}-{slug(cache_name)}-{slug(before_phase)}-to-{slug(after_phase)}.cache-comparison.json"
    )
    output_file = output_file.resolve()
    write_json(output_file, comparison)

    summary_path = run_dir / "summary.json"
    summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": "cache snapshot comparison",
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(summary, "raw_artifacts")
    cache_comparisons = ensure_object(raw_artifacts, "cache_comparisons")
    comparison_key = f"{slug(app)}_{slug(cache_name)}_{slug(before_phase)}_to_{slug(after_phase)}"
    cache_comparisons[comparison_key] = str(output_file)
    cache_comparison_summary = ensure_object(summary, "cache_comparisons")
    cache_comparison_summary[comparison_key] = comparison
    summary["decision"] = "cache snapshots compared; not measured"
    write_json(summary_path, summary)

    notes_path = run_dir / "notes.md"
    with notes_path.open("a", encoding="utf-8") as notes:
        notes.write("\n## Cache snapshot comparison\n\n")
        notes.write(f"- Comparison artifact: {output_file}\n")
        notes.write(f"- Before snapshot: {before_path}\n")
        notes.write(f"- After snapshot: {after_path}\n")
        notes.write(f"- File delta: {after_file_count - before_file_count}\n")
        notes.write(f"- Byte delta: {after_bytes - before_bytes}\n")
        notes.write("- Decision: cache snapshots compared; not measured\n")

    print(output_file)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
