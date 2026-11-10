#!/usr/bin/env python3
"""Create and validate a deterministic chat/render fixture suite."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path
from typing import Any

from benchlib import REPO_ROOT, append_notes, ensure_object, initialize_run_dir, load_json, utc_stamp, write_json


CHAT_SCENARIOS = ("normal", "high-volume", "burst", "reconnect")
RENDER_KINDS = (
    "static-emotes",
    "animated-emotes",
    "mixed-emotes",
    "large-scrollback",
    "rapid-scrolling",
    "multi-split",
)


def run_checked(args: list[str]) -> Path:
    result = subprocess.run(
        args,
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"Script failed with exit code {result.returncode}: {' '.join(args)}\n"
            f"stdout:\n{result.stdout}\n"
            f"stderr:\n{result.stderr}"
        )
    output_lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not output_lines:
        raise SystemExit(f"Script did not print an artifact path: {' '.join(args)}")
    path = Path(output_lines[-1]).expanduser()
    if not path.is_absolute():
        path = (REPO_ROOT / path).resolve()
    if not path.exists():
        raise SystemExit(f"Script did not return an existing artifact path: {path}")
    return path


def validation_passed(path: Path) -> bool:
    data = load_json(path)
    return bool(isinstance(data, dict) and data.get("passed") is True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chat-scenarios", nargs="+", choices=CHAT_SCENARIOS, default=list(CHAT_SCENARIOS))
    parser.add_argument("--render-kinds", nargs="+", choices=RENDER_KINDS, default=list(RENDER_KINDS))
    parser.add_argument("--chat-message-count", type=int, default=0)
    parser.add_argument("--render-message-count", type=int, default=0)
    parser.add_argument("--seed", type=int, default=9001)
    parser.add_argument("--output-root", default="")
    parser.add_argument("--benchmark-scenario", default="benchmark fixture suite")
    args = parser.parse_args()

    if args.chat_message_count < 0 or args.chat_message_count > 1_000_000:
        raise SystemExit("--chat-message-count must be in range 0..1000000.")
    if args.render_message_count < 0 or args.render_message_count > 1_000_000:
        raise SystemExit("--render-message-count must be in range 0..1000000.")
    if args.seed < 1 or args.seed > 2_147_483_647:
        raise SystemExit("--seed must be in range 1..2147483647.")

    suite_dir = initialize_run_dir(
        output_root=args.output_root,
        scenario=args.benchmark_scenario,
        fixture_name="fixture-suite",
        notes="Deterministic fixture suite for chat replay and render benchmark inputs. This is not performance evidence.",
    )
    fixture_output_root = suite_dir / "fixtures"
    fixture_output_root.mkdir(parents=True, exist_ok=True)

    chat_script = REPO_ROOT / "scripts" / "bench" / "new-chat-replay-fixture.py"
    render_script = REPO_ROOT / "scripts" / "bench" / "new-render-fixture.py"
    validator_script = REPO_ROOT / "scripts" / "bench" / "validate-benchmark-run.py"
    for script in (chat_script, render_script, validator_script):
        if not script.exists():
            raise SystemExit(f"Required benchmark script not found: {script}")

    chat_fixtures: list[dict[str, Any]] = []
    render_fixtures: list[dict[str, Any]] = []
    suite_passed = True
    fixture_index = 0

    for scenario in args.chat_scenarios:
        fixture_seed = args.seed + fixture_index
        fixture_index += 1
        command = [
            sys.executable,
            str(chat_script),
            "--replay-scenario",
            scenario,
            "--output-root",
            str(fixture_output_root),
            "--benchmark-scenario",
            f"chat replay fixture suite {scenario}",
            "--seed",
            str(fixture_seed),
        ]
        if args.chat_message_count > 0:
            command.extend(["--message-count", str(args.chat_message_count)])

        manifest_path = run_checked(command)
        fixture_run_dir = manifest_path.parent
        validation_path = run_checked(
            [
                sys.executable,
                str(validator_script),
                "--run-dir",
                str(fixture_run_dir),
                "--require-chat-replay",
                "--minimum-samples",
                str(args.chat_message_count if args.chat_message_count > 0 else 1),
            ]
        )
        passed = validation_passed(validation_path)
        suite_passed = suite_passed and passed
        chat_fixtures.append(
            {
                "scenario": scenario,
                "seed": fixture_seed,
                "run_dir": str(fixture_run_dir),
                "manifest_path": str(manifest_path),
                "validation_path": str(validation_path),
                "validation_passed": passed,
            }
        )

    for kind in args.render_kinds:
        fixture_seed = args.seed + fixture_index
        fixture_index += 1
        command = [
            sys.executable,
            str(render_script),
            "--fixture-kind",
            kind,
            "--output-root",
            str(fixture_output_root),
            "--benchmark-scenario",
            f"render fixture suite {kind}",
            "--seed",
            str(fixture_seed),
        ]
        if args.render_message_count > 0:
            command.extend(["--message-count", str(args.render_message_count)])

        manifest_path = run_checked(command)
        fixture_run_dir = manifest_path.parent
        validation_path = run_checked(
            [
                sys.executable,
                str(validator_script),
                "--run-dir",
                str(fixture_run_dir),
                "--require-render-fixture",
                "--minimum-samples",
                str(args.render_message_count if args.render_message_count > 0 else 1),
            ]
        )
        passed = validation_passed(validation_path)
        suite_passed = suite_passed and passed
        render_fixtures.append(
            {
                "kind": kind,
                "seed": fixture_seed,
                "run_dir": str(fixture_run_dir),
                "manifest_path": str(manifest_path),
                "validation_path": str(validation_path),
                "validation_passed": passed,
            }
        )

    suite_manifest_path = suite_dir / "fixture-suite.manifest.json"
    suite_manifest = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "suite_dir": str(suite_dir),
        "fixture_output_root": str(fixture_output_root),
        "seed": args.seed,
        "chat_message_count_override": args.chat_message_count,
        "render_message_count_override": args.render_message_count,
        "chat_fixtures": chat_fixtures,
        "render_fixtures": render_fixtures,
        "validation_passed": suite_passed,
        "decision": "fixture suite recorded; not measured",
    }
    write_json(suite_manifest_path, suite_manifest)

    summary_path = suite_dir / "summary.json"
    summary = load_json(summary_path) or {
        "schema_version": 1,
        "scenario": args.benchmark_scenario,
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(summary, "raw_artifacts")
    raw_artifacts["fixture_suite_manifest"] = str(suite_manifest_path)
    summary["fixture_suite"] = {
        "chat_fixture_count": len(chat_fixtures),
        "render_fixture_count": len(render_fixtures),
        "validation_passed": suite_passed,
        "manifest_path": str(suite_manifest_path),
    }
    summary["decision"] = "fixture suite recorded; not measured"
    write_json(summary_path, summary)

    append_notes(
        suite_dir,
        "Fixture suite",
        [
            f"- Suite manifest: {suite_manifest_path}",
            f"- Chat fixtures: {len(chat_fixtures)}",
            f"- Render fixtures: {len(render_fixtures)}",
            f"- Child validations passed: {suite_passed}",
            "- Decision: fixture suite recorded; not measured",
        ],
    )

    print(suite_manifest_path)
    return 0 if suite_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
