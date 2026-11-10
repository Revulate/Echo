#!/usr/bin/env python3
"""Focused regressions for the lean Echo profile runner."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "scripts/bench/run-echo-profile.py"
SPEC = importlib.util.spec_from_file_location("run_echo_profile", MODULE_PATH)
assert SPEC and SPEC.loader
PROFILE = importlib.util.module_from_spec(SPEC)
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC.loader.exec_module(PROFILE)
from benchlib import initialize_run_dir  # noqa: E402


class RunEchoProfileTests(unittest.TestCase):
    def test_offline_fixture_enforces_application_offline_mode(self) -> None:
        profile_env: dict[str, str] = {}
        PROFILE.apply_network_state(profile_env, "offline-fixture")
        self.assertEqual(profile_env["ECHO_BENCH_OFFLINE"], "1")

    def test_live_network_state_does_not_force_offline_mode(self) -> None:
        profile_env: dict[str, str] = {}
        PROFILE.apply_network_state(profile_env, "live")
        self.assertNotIn("ECHO_BENCH_OFFLINE", profile_env)

    def test_benchmark_failure_marker_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "echo.log"
            log.write_text(
                "INFO bench.ui_fixture surface=settings passed=true\n"
                "ERROR bench.ui_fixture failed stage=settings_open reason=state_mismatch\n",
                encoding="utf-8",
            )
            valid, detail = PROFILE.validate_benchmark_log(log)
            self.assertFalse(valid)
            self.assertIn("stage=settings_open", detail)

    def test_clean_benchmark_log_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "echo.log"
            log.write_text(
                "INFO bench.ui_fixture surface=settings stage=complete passed=true\n",
                encoding="utf-8",
            )
            self.assertEqual(
                PROFILE.validate_benchmark_log(log),
                (True, "no benchmark failure markers"),
            )

    def test_explicit_run_dir_adds_metadata_without_overwriting_results(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            run_dir = Path(temp) / "explicit-run"
            run_dir.mkdir()
            summary = run_dir / "summary.json"
            summary.write_text('{"status":"sampled"}\n', encoding="utf-8")

            initialize_run_dir(run_dir=str(run_dir), scenario="explicit metadata")

            self.assertTrue((run_dir / "environment.json").is_file())
            self.assertEqual(json.loads(summary.read_text(encoding="utf-8"))["status"], "sampled")


if __name__ == "__main__":
    unittest.main()
