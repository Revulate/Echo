#!/usr/bin/env python3
"""Record Echo executable identity metadata.

This mirrors scripts/bench/record-echo-baseline.ps1 for environments where
PowerShell is not installed. It records a baseline identity only; it is not a
benchmark measurement.
"""

from __future__ import annotations

import argparse

from benchlib import (
    REPO_ROOT,
    append_notes,
    ensure_object,
    file_metadata,
    git_value,
    initialize_run_dir,
    load_json,
    repo_dirty_paths,
    resolve_executable,
    utc_stamp,
    write_json,
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--scenario", default="echo baseline")
    parser.add_argument("--executable-path", required=True)
    parser.add_argument("--launch-argument", action="append", default=[])
    parser.add_argument("--version-label", default="")
    parser.add_argument("--commit", default="")
    parser.add_argument("--build-type", default="")
    parser.add_argument("--notes", default="")
    args = parser.parse_args()

    run_dir = initialize_run_dir(
        run_dir=args.run_dir,
        output_root=args.output_root,
        scenario=args.scenario,
        echo_path=args.executable_path,
        echo_build_type=args.build_type,
        notes=args.notes,
    )
    executable = resolve_executable(args.executable_path)
    file_info = file_metadata(executable)
    dirty_paths = repo_dirty_paths()
    repo_commit = args.commit or git_value("rev-parse", "HEAD")
    repo_short_commit = git_value("rev-parse", "--short", "HEAD")
    baseline = {
        "schema_version": 1,
        "recorded_at_utc": utc_stamp(),
        "app": "echo",
        "executable_path": str(executable),
        "executable_name": executable.name,
        "launch_argument": args.launch_argument,
        "version_label": args.version_label,
        "build_type": args.build_type,
        "repo": {
            "root": str(REPO_ROOT),
            "branch": git_value("branch", "--show-current"),
            "commit": repo_commit,
            "short_commit": repo_short_commit,
            "dirty": bool(dirty_paths),
            "dirty_paths": dirty_paths,
        },
        "file": file_info,
        "version_info": {
            "file_description": None,
            "file_version": None,
            "product_name": None,
            "product_version": None,
            "company_name": None,
            "original_filename": None,
        },
        "decision": "baseline recorded; not a benchmark measurement",
    }

    baseline_path = run_dir / "echo-baseline.json"
    write_json(baseline_path, baseline)

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
    raw_artifacts["echo_baseline"] = str(baseline_path)
    summary["echo_baseline"] = baseline
    summary["measurements_recorded"] = False
    summary["decision"] = "baseline recorded; not measured"
    write_json(summary_path, summary)

    environment_path = run_dir / "environment.json"
    environment = load_json(environment_path)
    if environment is not None:
        apps = ensure_object(environment, "apps")
        echo = ensure_object(apps, "echo")
        echo["path"] = str(executable)
        echo["build_type"] = args.build_type
        echo["version"] = args.version_label
        echo["sha256"] = file_info["sha256"]
        write_json(environment_path, environment)

    append_notes(
        run_dir,
        "Echo baseline",
        [
            f"- Baseline artifact: {baseline_path}",
            f"- Executable: {executable}",
            f"- Build type: {args.build_type}",
            f"- Commit: {repo_commit}",
            f"- Dirty: {bool(dirty_paths)}",
            f"- SHA256: {file_info['sha256']}",
            "- Decision: baseline recorded; not a benchmark measurement",
        ],
    )

    print(baseline_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
