#!/usr/bin/env python3
"""Focused tests for the Linux PGO build/evaluation companion."""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "scripts/bench/run-pgo-build.py"
SPEC = importlib.util.spec_from_file_location("run_pgo_build", MODULE_PATH)
assert SPEC and SPEC.loader
PGO = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PGO
SPEC.loader.exec_module(PGO)


class RunPgoBuildTests(unittest.TestCase):
    def test_sysroot_profdata_candidates_are_host_specific(self) -> None:
        candidates = PGO.llvm_profdata_candidates(Path("/rust"), "x86_64-unknown-linux-gnu")
        self.assertEqual(candidates[0], Path("/rust/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata"))

    def test_cargo_json_requires_exact_package_target_and_kind(self) -> None:
        good = {
            "reason": "compiler-artifact",
            "package_id": "path+file:///repo#echo_gpui@0.9.11",
            "target": {"name": "echo", "kind": ["bin"]},
            "executable": "/tmp/target/echo",
        }
        noise = {
            "reason": "compiler-artifact",
            "package_id": "path+file:///repo#other@1.0.0",
            "target": {"name": "echo", "kind": ["bin"]},
            "executable": "/tmp/wrong",
        }
        actual = PGO.parse_cargo_executable(
            [json.dumps(noise), "compiler chatter", json.dumps(good)], "echo_gpui", "echo", "bin"
        )
        self.assertEqual(actual, Path("/tmp/target/echo").resolve())

    def test_cargo_json_rejects_ambiguous_executables(self) -> None:
        messages = []
        for path in ("/tmp/a", "/tmp/b"):
            messages.append(
                json.dumps(
                    {
                        "reason": "compiler-artifact",
                        "package_id": "path+file:///repo#echo_gpui@0.9.11",
                        "target": {"name": "echo", "kind": ["bin"]},
                        "executable": path,
                    }
                )
            )
        with self.assertRaisesRegex(PGO.PgoError, "expected one Cargo JSON executable"):
            PGO.parse_cargo_executable(messages, "echo_gpui", "echo", "bin")

    def test_fresh_profraw_rejects_stale_and_empty_data(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            stale = root / "stale.profraw"
            stale.write_bytes(b"profile")
            os.utime(stale, ns=(1, 1))
            with self.assertRaisesRegex(PGO.PgoError, "empty or stale"):
                PGO.fresh_profraw_files(root, 2)
            stale.unlink()
            empty = root / "empty.profraw"
            empty.touch()
            with self.assertRaisesRegex(PGO.PgoError, "empty or stale"):
                PGO.fresh_profraw_files(root, 0)

    def test_instrumented_build_profiles_are_recorded_then_cleared(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            profile = root / "build.profraw"
            profile.write_bytes(b"profile")
            removed = PGO.clear_instrumented_build_profiles(root)
            self.assertEqual(len(removed), 1)
            self.assertEqual(removed[0]["size_bytes"], 7)
            self.assertFalse(profile.exists())

    def test_training_environment_is_offline_isolated_and_no_send(self) -> None:
        env = PGO.training_environment(
            {
                "PRESERVE": "yes",
                "ECHO_BENCH_LIVE_CHANNEL": "must-be-removed",
                "ECHO_CHAT_REPLAY_MAX_EVENTS": "1",
                "LLVM_PROFILE_FILE": "stale",
            },
            Path("/tmp/profile"),
            Path("/tmp/raw/echo-%p.profraw"),
            Path("/tmp/replay.ndjson"),
            Path("/tmp/report.json"),
        )
        self.assertEqual(env["ECHO_BENCH_OFFLINE"], "1")
        self.assertEqual(env["ECHO_BENCH_PROFILE_ROOT"], str(Path("/tmp/profile")))
        self.assertEqual(env["ECHO_CHAT_REPLAY_SPEEDUP"], "0")
        self.assertFalse(any("SEND" in key for key in env))
        self.assertEqual(env["PRESERVE"], "yes")
        self.assertNotIn("ECHO_BENCH_LIVE_CHANNEL", env)
        self.assertNotIn("ECHO_CHAT_REPLAY_MAX_EVENTS", env)

    def test_evaluator_keeps_only_correct_non_regressing_candidate(self) -> None:
        lanes = {
            "startup": {"correctness": True, "cases": [{"name": "ready", "delta_percent": 4.99}]},
            "layout": {"correctness": True, "cases": [{"name": "layout", "delta_percent": -2.0}]},
            "search": {"correctness": True, "cases": [{"name": "search", "delta_percent": 0.0}]},
        }
        self.assertEqual(PGO.evaluate_ab(lanes)["decision"], "keep")

    def test_evaluator_hard_reverts_correctness_or_over_five_percent(self) -> None:
        lanes = {
            "startup": {"correctness": True, "cases": [{"name": "ready", "delta_percent": 5.01}]},
            "layout": {"correctness": False, "cases": [{"name": "layout", "delta_percent": -3.0}]},
        }
        verdict = PGO.evaluate_ab(lanes)
        self.assertEqual(verdict["decision"], "revert")
        self.assertTrue(verdict["hard_revert"])
        self.assertEqual(len(verdict["reasons"]), 3)
        self.assertIn("search: lane missing", verdict["reasons"])

    def test_evaluator_rejects_missing_empty_and_non_finite_lanes(self) -> None:
        lanes = {
            "startup": {"correctness": True, "cases": []},
            "layout": {
                "correctness": True,
                "cases": [{"name": "layout", "delta_percent": float("nan")}],
            },
        }
        verdict = PGO.evaluate_ab(lanes)
        self.assertEqual(verdict["decision"], "revert")
        self.assertIn("search: lane missing", verdict["reasons"])
        self.assertIn("startup: no performance cases", verdict["reasons"])
        self.assertIn("layout/layout: non-finite performance delta", verdict["reasons"])

    def test_search_csv_preserves_correctness_checksum(self) -> None:
        parsed = PGO.parse_search_csv(
            "kind,name,units,ns_per_unit,units_per_second,checksum\n"
            "match,term,100,12.5,8000000,42\n"
        )
        self.assertEqual(parsed, {"match/term": (12.5, 42)})

    def test_search_csv_rejects_non_finite_and_duplicate_rows(self) -> None:
        header = "kind,name,units,ns_per_unit,units_per_second,checksum\n"
        with self.assertRaisesRegex(PGO.PgoError, "invalid search benchmark measurement"):
            PGO.parse_search_csv(header + "match,term,1,nan,0,1\n")
        with self.assertRaisesRegex(PGO.PgoError, "duplicate search benchmark row"):
            PGO.parse_search_csv(
                header + "match,term,1,1.0,1,1\nmatch,term,1,2.0,1,1\n"
            )

    def test_percent_delta_rejects_non_positive_and_infinite_values(self) -> None:
        for baseline, candidate in [(0.0, 1.0), (1.0, -1.0), (1.0, float("inf"))]:
            with self.assertRaises(PGO.PgoError):
                PGO.percent_delta(baseline, candidate)

    def test_criterion_estimate_parser_supports_current_output(self) -> None:
        output = "layout time:   [15.900 µs 16.250 µs 16.700 µs]\n"
        self.assertEqual(PGO.parse_bencher_ns(output), 16_250.0)

    def test_stage_timeout_is_capped_at_ten_minutes(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            runner = PGO.StageRunner(Path(temp), 601, dry_run=True)
            with self.assertRaisesRegex(PGO.PgoError, "1..600"):
                runner.run("bad", ["true"])

    def test_run_artifact_path_filter_does_not_hide_other_changes(self) -> None:
        lines = [
            "?? benchmarks/results/run/linux-pgo/report.json",
            " M src/main.rs",
        ]
        ignored = "benchmarks/results/run/linux-pgo"
        filtered = [
            line
            for line in lines
            if not (
                line[3:].strip('"').replace("\\", "/") == ignored
                or line[3:].strip('"').replace("\\", "/").startswith(ignored + "/")
            )
        ]
        self.assertEqual(filtered, [" M src/main.rs"])


if __name__ == "__main__":
    unittest.main()
