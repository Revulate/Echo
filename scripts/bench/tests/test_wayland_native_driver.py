#!/usr/bin/env python3
"""Adversarial tests for the native Wayland adapter."""

from __future__ import annotations

import binascii
import hashlib
import importlib.util
import json
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import zlib
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "scripts/bench/wayland_native_driver.py"
SPEC = importlib.util.spec_from_file_location("wayland_native_driver", MODULE_PATH)
assert SPEC and SPEC.loader
DRIVER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = DRIVER
SPEC.loader.exec_module(DRIVER)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def chunk(kind: bytes, payload: bytes, *, bad_crc: bool = False) -> bytes:
    crc = (binascii.crc32(kind + payload) + int(bad_crc)) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)


def png_bytes(width: int = 100, height: int = 100) -> bytes:
    rows = b"".join(b"\0" + bytes(width * 3) for _ in range(height))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )


class WaylandNativeDriverTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.runtime = self.root / "runtime"
        self.runtime.mkdir()
        self.socket_path = self.runtime / "wayland-1"
        self.socket_path.touch()
        self.binary = self.root / "echo"
        self.binary.write_bytes(b"echo")
        self.run_root = self.root / "run"
        self.profile = self.run_root / "profile"
        self.profile.mkdir(parents=True)
        self.focus = self.root / "focus.json"
        self.tools = {
            "qdbus6": {
                "available": True,
                "path": "/usr/bin/qdbus6",
                "sha256": "d" * 64,
                "version": "6",
                "package": "qt6-tools",
            },
            "grim": {
                "available": True,
                "path": "/usr/bin/grim",
                "sha256": "a" * 64,
                "version": "1",
                "package": "grim",
            },
            "wtype": {
                "available": True,
                "path": "/usr/bin/wtype",
                "sha256": "b" * 64,
                "version": "1",
                "package": "wtype",
            },
            "kscreen-doctor": {
                "available": True,
                "path": "/usr/bin/kscreen-doctor",
                "sha256": "c" * 64,
                "version": "1",
                "package": "libkscreen",
            },
            "kwin_wayland": {
                "available": True,
                "path": "/usr/bin/kwin_wayland",
                "sha256": "e" * 64,
                "version": "1",
                "package": "kwin",
            },
        }
        self.expected = DRIVER.IdentityExpectation(
            321,
            777,
            self.binary,
            digest(b"echo"),
            self.run_root,
            self.profile,
            self.socket_path,
            "echo-main-1",
            self.focus,
        )
        self.write_focus()
        self.env = {
            "XDG_SESSION_TYPE": "wayland",
            "WAYLAND_DISPLAY": "wayland-1",
            "XDG_RUNTIME_DIR": str(self.runtime),
            "DISPLAY": ":0",
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def focus_value(self, **changes: object) -> dict:
        value = {
            "pid": 321,
            "process_start_ticks": 777,
            "executable_path": str(self.binary.resolve()),
            "executable_sha256": digest(b"echo"),
            "campaign_run_root": str(self.run_root.resolve()),
            "profile_root": str(self.profile.resolve()),
            "wayland_socket": str(self.socket_path.resolve()),
            "window_token": "echo-main-1",
            "focused": True,
            "geometry": {"x": 5, "y": 7, "width": 100, "height": 100},
            "foreign_toplevel_token": "echo-foreign-1",
            "captured_monotonic_ns": time.monotonic_ns(),
            "max_age_ns": 5_000_000_000,
            "provider": {
                "name": "qdbus6-kwin-active-window",
                "tool_path": "/usr/bin/qdbus6",
                "tool_sha256": "d" * 64,
                "argv": [
                    "/usr/bin/qdbus6",
                    "org.kde.KWin",
                    "/KWin",
                    "org.echo.Benchmark.activeWindowReceipt",
                ],
            },
        }
        value.update(changes)
        return value

    def write_focus(self, **changes: object) -> dict:
        value = self.focus_value(**changes)
        self.focus.write_text(json.dumps(value), encoding="utf-8")
        return value

    def identity_context(self, operational: dict | None = None):
        process_env = {
            "ECHO_BENCH_PROFILE_ROOT": str(self.profile),
            "WAYLAND_DISPLAY": "wayland-1",
            "XDG_RUNTIME_DIR": str(self.runtime),
            "XDG_SESSION_TYPE": "wayland",
        }
        return (
            mock.patch.object(DRIVER, "proc_executable", return_value=self.binary),
            mock.patch.object(DRIVER, "proc_start_ticks", return_value=777),
            mock.patch.object(DRIVER, "proc_environ", return_value=process_env),
            mock.patch.object(
                DRIVER,
                "session_probe",
                return_value={
                    "wayland_socket": str(self.socket_path),
                    "wayland_display": "wayland-1",
                    "x11_fallback": False,
                },
            ),
            mock.patch.object(
                DRIVER,
                "kwin_focus_probe",
                return_value=operational or json.loads(self.focus.read_text(encoding="utf-8")),
            ),
        )

    def verify(self) -> dict:
        patches = self.identity_context()
        with patches[0], patches[1], patches[2], patches[3], patches[4]:
            return DRIVER.verify_identity(self.expected, env=self.env, tools=self.tools)

    def test_exact_identity_passes(self) -> None:
        self.assertEqual(self.verify()["window_geometry"]["x"], 5)

    def test_regular_file_wayland_endpoint_is_blocked(self) -> None:
        regular = self.runtime / "fake"
        regular.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(DRIVER.Blocked, "not a socket"):
            DRIVER.session_probe({**self.env, "WAYLAND_DISPLAY": "fake"})

    def test_x11_never_falls_back(self) -> None:
        with self.assertRaisesRegex(DRIVER.Blocked, "native Wayland"):
            DRIVER.session_probe({**self.env, "XDG_SESSION_TYPE": "x11"})

    def test_pid_start_reuse_is_blocked(self) -> None:
        patches = self.identity_context()
        with (
            patches[0],
            mock.patch.object(DRIVER, "proc_start_ticks", return_value=778),
            patches[2],
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "start ticks"),
        ):
            DRIVER.verify_identity(self.expected, tools=self.tools)

    def test_executable_path_mismatch_is_blocked(self) -> None:
        patches = self.identity_context()
        with (
            mock.patch.object(
                DRIVER, "proc_executable", return_value=Path("/usr/bin/sleep")
            ),
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "path mismatch"),
        ):
            DRIVER.verify_identity(self.expected, tools=self.tools)

    def test_process_environment_socket_mismatch_is_blocked(self) -> None:
        patches = self.identity_context()
        bad_env = {
            "ECHO_BENCH_PROFILE_ROOT": str(self.profile),
            "WAYLAND_DISPLAY": "wayland-2",
            "XDG_RUNTIME_DIR": str(self.runtime),
        }
        with (
            patches[0],
            patches[1],
            mock.patch.object(DRIVER, "proc_environ", return_value=bad_env),
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "runtime/display"),
        ):
            DRIVER.verify_identity(self.expected, tools=self.tools)

    def test_profile_must_be_strictly_under_run(self) -> None:
        bad = DRIVER.IdentityExpectation(
            **{**self.expected.__dict__, "profile_root": self.run_root}
        )
        patches = self.identity_context()
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "strictly"),
        ):
            DRIVER.verify_identity(bad, tools=self.tools)

    def test_self_authored_focus_receipt_is_blocked(self) -> None:
        patches = self.identity_context(self.focus_value(window_token="different"))
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "operational compositor"),
        ):
            DRIVER.verify_identity(self.expected, tools=self.tools)

    def test_stale_focus_is_blocked(self) -> None:
        receipt = self.write_focus(captured_monotonic_ns=1, max_age_ns=1)
        patches = self.identity_context(receipt)
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "stale"),
        ):
            DRIVER.verify_identity(self.expected, tools=self.tools)

    def test_future_focus_timestamp_is_blocked(self) -> None:
        receipt = self.write_focus(captured_monotonic_ns=10**30)
        patches = self.identity_context(receipt)
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            self.assertRaisesRegex(DRIVER.Blocked, "future"),
        ):
            DRIVER.verify_identity(self.expected, tools=self.tools)

    def test_symlink_tool_substitution_is_rejected(self) -> None:
        fake = self.root / "grim"
        fake.symlink_to(sys.executable)
        with mock.patch.object(DRIVER.shutil, "which", return_value=str(fake)):
            self.assertFalse(DRIVER.probe_tools()["grim"]["available"])

    def test_failed_operational_probes_block_capabilities(self) -> None:
        matrix = DRIVER.capability_matrix(
            self.tools, probe=lambda argv: {"status": "BLOCKED", "argv": argv}
        )
        self.assertTrue(
            all(record["status"] == "BLOCKED" for record in matrix.values())
        )

    def test_capabilities_separate_installed_executable_compositor_capture_and_input(
        self,
    ) -> None:
        def probe(argv):
            return {
                "status": "PASS",
                "argv": argv,
                "stdout": "usage: grim -T token",
                "stderr": "",
            }

        matrix = DRIVER.capability_matrix(self.tools, probe=probe)
        self.assertEqual(
            matrix["screenshot_capture"]["capture_operational_status"], "PASS"
        )
        self.assertEqual(matrix["screenshot_capture"]["tool_executable_status"], "PASS")
        self.assertEqual(matrix["keyboard_input"]["installed_status"], "PASS")
        self.assertEqual(matrix["keyboard_input"]["operational_status"], "BLOCKED")

    def test_blocked_probe_preserves_native_session_tool_and_operational_records(
        self,
    ) -> None:
        capabilities = {
            "keyboard_input": {"status": "BLOCKED", "operational_status": "BLOCKED"}
        }
        outputs: list[str] = []
        with (
            mock.patch.object(
                DRIVER,
                "session_probe",
                return_value={
                    "session_type": "wayland",
                    "wayland_socket": "/run/user/1/wayland-0",
                },
            ),
            mock.patch.object(DRIVER, "probe_tools", return_value=self.tools),
            mock.patch.object(DRIVER, "capability_matrix", return_value=capabilities),
            mock.patch("builtins.print", side_effect=outputs.append),
        ):
            status = DRIVER.main(["--artifact-root", str(self.root), "probe"])
        record = json.loads(outputs[-1])
        self.assertEqual(status, DRIVER.BLOCKED_EXIT)
        self.assertTrue(record["native_wayland"])
        self.assertTrue(record["session_verified"])
        self.assertIn("tools", record)
        self.assertIn("capabilities", record)

    def test_pre_session_block_has_null_verification_and_no_contradiction(self) -> None:
        outputs: list[str] = []
        with (
            mock.patch.object(
                DRIVER, "session_probe", side_effect=DRIVER.Blocked("no socket")
            ),
            mock.patch("builtins.print", side_effect=outputs.append),
        ):
            status = DRIVER.main(["--artifact-root", str(self.root), "probe"])
        record = json.loads(outputs[-1])
        self.assertEqual(status, DRIVER.BLOCKED_EXIT)
        self.assertIsNone(record["session_verified"])
        self.assertIsNone(record["session"])
        self.assertFalse(record["native_wayland"])

    def test_post_session_failure_preserves_verified_session(self) -> None:
        outputs: list[str] = []
        session = {"session_type": "wayland", "wayland_socket": "/run/user/1/wayland-0"}
        with (
            mock.patch.object(DRIVER, "session_probe", return_value=session),
            mock.patch.object(
                DRIVER, "probe_tools", side_effect=OSError("tool failure")
            ),
            mock.patch("builtins.print", side_effect=outputs.append),
        ):
            status = DRIVER.main(["--artifact-root", str(self.root), "probe"])
        record = json.loads(outputs[-1])
        self.assertEqual(status, DRIVER.FAILED_EXIT)
        self.assertTrue(record["session_verified"])
        self.assertTrue(record["native_wayland"])
        self.assertEqual(record["session"], session)

    def test_success_output_overwrites_untrusted_session_fields(self) -> None:
        outputs: list[str] = []
        session = {"session_type": "wayland", "wayland_socket": "/run/user/1/wayland-0"}
        display = self.display_receipt()
        argv = [
            "--artifact-root",
            str(self.root),
            "capture",
            "--expected-pid",
            "321",
            "--expected-process-start-ticks",
            "777",
            "--expected-executable-path",
            str(self.binary),
            "--expected-executable-sha256",
            digest(b"echo"),
            "--campaign-run-root",
            str(self.run_root),
            "--expected-profile-root",
            str(self.profile),
            "--expected-wayland-socket",
            str(self.socket_path),
            "--expected-window-token",
            "echo-main-1",
            "--focus-receipt",
            str(self.focus),
            "--display-receipt",
            str(display),
        ]
        forged = {
            "schema_version": 1,
            "status": "PASS",
            "session_verified": False,
            "session": None,
        }
        with (
            mock.patch.object(DRIVER, "session_probe", return_value=session),
            mock.patch.object(DRIVER, "capture", return_value=forged),
            mock.patch("builtins.print", side_effect=outputs.append),
        ):
            status = DRIVER.main(argv)
        record = json.loads(outputs[-1])
        self.assertEqual(status, 0)
        self.assertTrue(record["session_verified"])
        self.assertTrue(record["native_wayland"])
        self.assertEqual(record["session"], session)

    def test_package_output_drift_is_rejected(self) -> None:
        result = subprocess.CompletedProcess(["pacman"], 0, "grim\nother\n", "")
        with (
            mock.patch.object(DRIVER.shutil, "which", return_value="/usr/bin/pacman"),
            mock.patch.object(DRIVER.subprocess, "run", return_value=result),
        ):
            version, package, _ = DRIVER.package_version(Path("/usr/bin/grim"))
        self.assertIsNone(version)
        self.assertIsNone(package)

    def test_package_integrity_requires_numeric_zero_altered_files(self) -> None:
        good = subprocess.CompletedProcess(
            ["pacman"], 0, "grim: 16 total files, 0 altered files\n", ""
        )
        bad = subprocess.CompletedProcess(
            ["pacman"], 0, "grim: 16 total files, 1 altered file\n", ""
        )
        with (
            mock.patch.object(DRIVER.shutil, "which", return_value="/usr/bin/pacman"),
            mock.patch.object(DRIVER.subprocess, "run", return_value=good),
        ):
            self.assertTrue(DRIVER.package_integrity("grim")[0])
        with (
            mock.patch.object(DRIVER.shutil, "which", return_value="/usr/bin/pacman"),
            mock.patch.object(DRIVER.subprocess, "run", return_value=bad),
        ):
            self.assertFalse(DRIVER.package_integrity("grim")[0])

    def test_trusted_qdbus_package_symlink_records_link_and_target_hashes(self) -> None:
        # Mirror the packaged layout under a temp root so the trust rules are
        # exercised without depending on the host's installed tools.
        system = self.root.resolve() / "system"
        specs = {}
        for name, (path_text, packages) in DRIVER.TRUSTED_TOOL_SPECS.items():
            path = system / path_text.lstrip("/")
            path.parent.mkdir(parents=True, exist_ok=True)
            specs[name] = (str(path), packages)
        target = system / DRIVER.QDBUS_TRUSTED_TARGET.lstrip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(b"qdbus")
        for name, (path_text, _) in specs.items():
            if name == "qdbus6":
                Path(path_text).symlink_to(target)
            else:
                Path(path_text).write_bytes(name.encode())

        def version(path):
            package = (
                "qt6-tools" if path.name == "qdbus6" else specs[path.name][1][0]
            )
            return "1", package, []

        with (
            mock.patch.dict(DRIVER.TRUSTED_TOOL_SPECS, specs),
            mock.patch.object(DRIVER, "QDBUS_TRUSTED_TARGET", str(target)),
            mock.patch.object(
                DRIVER.shutil, "which", side_effect=lambda name: specs[name][0]
            ),
            mock.patch.object(DRIVER, "package_version", side_effect=version),
            mock.patch.object(
                DRIVER, "package_integrity", return_value=(True, {"status": "PASS"})
            ),
        ):
            record = DRIVER.probe_tools()["qdbus6"]
        self.assertTrue(record["available"])
        self.assertEqual(record["canonical_target_path"], str(target))
        self.assertEqual(len(record["link_sha256"]), 64)

    def test_missing_tool_hash_is_blocked(self) -> None:
        with self.assertRaises(DRIVER.Blocked):
            DRIVER.select_tool({"grim": {"available": True, "version": "1"}}, ("grim",))

    def display_receipt(self, **changes: object) -> Path:
        value = {
            "geometry": {"x": 5, "y": 7, "width": 100, "height": 100},
            "scale": 1.0,
            "refresh_hz": 60.0,
            "output": "DP-1",
            "window_token": "echo-main-1",
            "pid": 321,
            "process_start_ticks": 777,
            "foreign_toplevel_token": "echo-foreign-1",
            "focused": True,
            "captured_monotonic_ns": time.monotonic_ns(),
            "provider": {
                "name": "qdbus6-kwin-active-display",
                "tool_path": "/usr/bin/qdbus6",
                "tool_sha256": "d" * 64,
                "argv": [
                    "/usr/bin/qdbus6",
                    "org.kde.KWin",
                    "/KWin",
                    "org.echo.Benchmark.activeDisplayReceipt",
                ],
            },
        }
        value.update(changes)
        path = self.root / "display.json"
        path.write_text(json.dumps(value), encoding="utf-8")
        return path

    def capture_runner(self, payload: bytes):
        def runner(argv, **kwargs):
            (self.root / "actual.png").write_bytes(payload)
            (self.root / "capture-stage.log").write_text("grim complete\n", encoding="utf-8")
            terminal = f"PASS stage=wayland-native-capture elapsed=0s log={self.root / 'capture-stage.log'}\n"
            return subprocess.CompletedProcess(argv, 0, terminal, "")

        return runner

    def run_capture(self, payload: bytes = b"") -> dict:
        patches = self.identity_context()
        display = self.display_receipt()
        display_value = json.loads(display.read_text(encoding="utf-8"))
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            mock.patch.object(DRIVER, "probe_tools", return_value=self.tools),
            mock.patch.object(DRIVER, "kwin_display_probe", return_value=display_value),
            mock.patch.object(DRIVER, "source_provenance", return_value={}),
        ):
            return DRIVER.capture(
                self.root,
                self.expected,
                display,
                runner=self.capture_runner(payload or png_bytes()),
            )

    def test_capture_uses_exact_window_region_and_records_log_hash(self) -> None:
        result = self.run_capture()
        self.assertEqual(
            result["command"]["command_argv"][1:3], ["-T", "echo-foreign-1"]
        )
        self.assertEqual(len(result["command"]["stage_log_sha256"]), 64)
        self.assertIn("PASS stage=", result["command"]["terminal"])
        self.assertTrue(result["session_verified"])
        persisted = json.loads((self.root / "actual.metadata.json").read_text(encoding="utf-8"))
        self.assertTrue(persisted["session_verified"])
        self.assertEqual(persisted["session"], result["identity"]["session"])

    def test_whole_output_window_mismatch_is_blocked(self) -> None:
        patches = self.identity_context()
        display = self.display_receipt(window_token="whole-output")
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            mock.patch.object(DRIVER, "probe_tools", return_value=self.tools),
            mock.patch.object(
                DRIVER,
                "kwin_display_probe",
                return_value=json.loads(display.read_text(encoding="utf-8")),
            ),
            self.assertRaisesRegex(DRIVER.Blocked, "window token"),
        ):
            DRIVER.capture(self.root, self.expected, display)

    def test_self_authored_display_receipt_is_rejected(self) -> None:
        display = self.display_receipt()
        patches = self.identity_context()
        live = json.loads(display.read_text(encoding="utf-8"))
        live["output"] = "HDMI-1"
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            mock.patch.object(DRIVER, "probe_tools", return_value=self.tools),
            mock.patch.object(DRIVER, "kwin_display_probe", return_value=live),
            self.assertRaisesRegex(DRIVER.Blocked, "live compositor"),
        ):
            DRIVER.capture(self.root, self.expected, display)

    def test_stale_png_is_deleted_before_stage(self) -> None:
        (self.root / "actual.png").write_bytes(b"stale")
        self.run_capture()
        self.assertNotEqual((self.root / "actual.png").read_bytes(), b"stale")

    def test_no_iend_png_is_rejected(self) -> None:
        path = self.root / "bad.png"
        path.write_bytes(png_bytes()[:-12])
        with self.assertRaisesRegex(DRIVER.Failed, "terminal"):
            DRIVER.validate_png(path)

    def test_bad_crc_png_is_rejected(self) -> None:
        data = bytearray(png_bytes())
        data[-1] ^= 1
        path = self.root / "bad.png"
        path.write_bytes(data)
        with self.assertRaisesRegex(DRIVER.Failed, "CRC"):
            DRIVER.validate_png(path)

    def test_bad_decompression_png_is_rejected(self) -> None:
        path = self.root / "bad.png"
        path.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", b"bad")
            + chunk(b"IEND", b"")
        )
        with self.assertRaisesRegex(DRIVER.Failed, "decompression"):
            DRIVER.validate_png(path)

    def test_truncated_png_chunk_is_rejected(self) -> None:
        path = self.root / "bad.png"
        path.write_bytes(b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 100) + b"IHDR")
        with self.assertRaisesRegex(DRIVER.Failed, "truncated"):
            DRIVER.validate_png(path)

    def test_malformed_color_semantic_chunk_is_failed_not_unsupported(self) -> None:
        base = png_bytes()
        insert = 8 + 12 + 13
        cases = (
            chunk(b"gAMA", b""),
            chunk(b"cHRM", b"short"),
            chunk(b"sRGB", b"\x04"),
            chunk(b"iCCP", b"name\0\x01bad"),
        )
        for index, semantic_chunk in enumerate(cases):
            with self.subTest(index=index):
                path = self.root / f"malformed-{index}.png"
                path.write_bytes(base[:insert] + semantic_chunk + base[insert:])
                with self.assertRaisesRegex(DRIVER.Failed, "malformed"):
                    DRIVER.validate_png(path)

    def test_color_semantic_chunk_after_idat_is_malformed(self) -> None:
        data = png_bytes()
        insert = len(data) - 12
        path = self.root / "late-gamma.png"
        path.write_bytes(
            data[:insert] + chunk(b"gAMA", struct.pack(">I", 45455)) + data[insert:]
        )
        with self.assertRaisesRegex(DRIVER.Failed, "malformed"):
            DRIVER.validate_png(path)

    def test_color_semantic_chunk_before_ihdr_is_malformed(self) -> None:
        data = png_bytes()
        path = self.root / "early-gamma.png"
        path.write_bytes(data[:8] + chunk(b"gAMA", struct.pack(">I", 45455)) + data[8:])
        with self.assertRaisesRegex(DRIVER.Failed, "begin with IHDR"):
            DRIVER.validate_png(path)

    def test_decompression_bomb_is_capped_at_exact_expected_output(self) -> None:
        path = self.root / "bomb.png"
        bomb = zlib.compress(b"A" * 20_000_000)
        path.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", bomb)
            + chunk(b"IEND", b"")
        )
        real_factory = zlib.decompressobj
        max_lengths: list[int] = []

        class Instrumented:
            def __init__(self):
                self.inner = real_factory()

            def decompress(self, data, max_length=0):
                max_lengths.append(max_length)
                return self.inner.decompress(data, max_length)

            def __getattr__(self, name):
                return getattr(self.inner, name)

        with (
            mock.patch.object(DRIVER.zlib, "decompressobj", side_effect=Instrumented),
            self.assertRaisesRegex(DRIVER.Failed, "exceeds exact bound"),
        ):
            DRIVER.validate_png(path)
        self.assertEqual(max_lengths, [5])

    def test_accumulated_idat_payload_is_bounded(self) -> None:
        path = self.root / "idat-limit.png"
        path.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", b"123456")
            + chunk(b"IDAT", b"789012")
            + chunk(b"IEND", b"")
        )
        with (
            mock.patch.object(DRIVER, "MAX_PNG_IDAT_BYTES", 10),
            self.assertRaisesRegex(DRIVER.Failed, "IDAT exceeds"),
        ):
            DRIVER.validate_png(path)

    def test_sparse_oversized_png_file_is_rejected_before_read(self) -> None:
        path = self.root / "oversized.png"
        with path.open("wb") as stream:
            stream.truncate(DRIVER.MAX_PNG_FILE_BYTES + 1)
        with self.assertRaisesRegex(DRIVER.Failed, "file exceeds"):
            DRIVER.validate_png(path)

    def test_file_growth_after_stat_remains_bounded(self) -> None:
        path = self.root / "growing.png"
        path.write_bytes(png_bytes())

        original_open = Path.open

        def grow_then_open(candidate, *args, **kwargs):
            if candidate == path:
                with original_open(candidate, "ab") as stream:
                    stream.truncate(DRIVER.MAX_PNG_FILE_BYTES + 1)
            return original_open(candidate, *args, **kwargs)

        with (
            mock.patch.object(DRIVER, "MAX_PNG_FILE_BYTES", 128),
            mock.patch.object(Path, "open", grow_then_open),
            self.assertRaisesRegex(DRIVER.Failed, "file exceeds"),
        ):
            DRIVER.validate_png(path)

    def test_capture_dimension_mismatch_is_rejected(self) -> None:
        with self.assertRaisesRegex(DRIVER.Failed, "dimensions"):
            self.run_capture(png_bytes(50, 50))

    def test_stage_missing_log_is_rejected(self) -> None:
        descriptor = DRIVER.stage_descriptor(
            "x", ["/bin/true"], self.root / "missing.log", 2
        )

        def runner(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 0, "PASS stage=x\n", "")

        with self.assertRaisesRegex(DRIVER.Failed, "retained log"):
            DRIVER.execute_stage(descriptor, runner=runner)

    def test_stage_terminal_distinguishes_failure_timeout_and_child_124(self) -> None:
        log = self.root / "terminal.log"
        descriptor = DRIVER.stage_descriptor("terminal", ["/bin/true"], log, 2)
        cases = (
            (9, f"FAIL stage=terminal status=9 elapsed=1s log={log}\n", "FAIL"),
            (124, f"TIMEOUT stage=terminal elapsed=2s log={log}\n", "TIMEOUT"),
            (124, f"FAIL stage=terminal status=124 elapsed=1s log={log}\n", "FAIL"),
        )
        for returncode, terminal, outcome in cases:
            with self.subTest(returncode=returncode, outcome=outcome):

                def runner(argv, **kwargs):
                    log.write_text("child output\n", encoding="utf-8")
                    return subprocess.CompletedProcess(argv, returncode, "", terminal)

                with self.assertRaises(DRIVER.Failed) as raised:
                    DRIVER.execute_stage(descriptor, runner=runner)
                record = json.loads(str(raised.exception))
                self.assertEqual(record["outcome"], outcome)
                self.assertEqual(record["terminal"], terminal.strip())

    def test_stage_terminal_rejects_returncode_mismatch_and_ambiguity(self) -> None:
        log = self.root / "terminal.log"
        descriptor = DRIVER.stage_descriptor("terminal", ["/bin/true"], log, 2)
        outputs = (
            (9, f"FAIL stage=terminal status=8 elapsed=1s log={log}\n", "mismatch"),
            (
                0,
                f"PASS stage=terminal elapsed=1s log={log}\nFAIL stage=terminal status=9 elapsed=1s log={log}\n",
                "final|ambiguous",
            ),
        )
        for returncode, output, reason in outputs:
            with self.subTest(reason=reason):

                def runner(argv, **kwargs):
                    log.write_text("child output\n", encoding="utf-8")
                    return subprocess.CompletedProcess(argv, returncode, output, "")

                with self.assertRaisesRegex(DRIVER.Failed, reason):
                    DRIVER.execute_stage(descriptor, runner=runner)

    def test_input_missing_post_receipt_is_blocked(self) -> None:
        route = self.root / "route.json"
        route.write_text(
            json.dumps(
                {
                    "action": "escape",
                    "pid": 321,
                    "process_start_ticks": 777,
                    "profile_root": str(self.profile.resolve()),
                    "window_token": "echo-main-1",
                    "focused": True,
                    "pre_semantic_sha256": "a" * 64,
                    "expected_post_correlation": "escaped",
                    "post_semantic_sha256": None,
                }
            ),
            encoding="utf-8",
        )

        def runner(argv, **kwargs):
            (self.root / "input-escape-stage.log").write_text("done\n", encoding="utf-8")
            return subprocess.CompletedProcess(
                argv, 0, "PASS stage=wayland-native-input-escape\n", ""
            )

        patches = self.identity_context()
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            mock.patch.object(DRIVER, "probe_tools", return_value=self.tools),
            self.assertRaisesRegex(DRIVER.Blocked, "disabled"),
        ):
            DRIVER.keyboard_input(
                self.root,
                self.expected,
                "escape",
                route,
                Path("post.json"),
                runner=runner,
            )

    def test_self_authored_post_receipt_cannot_enable_input_pass(self) -> None:
        route = self.root / "route.json"
        route_value = {
            "action": "escape",
            "pid": 321,
            "process_start_ticks": 777,
            "profile_root": str(self.profile.resolve()),
            "window_token": "echo-main-1",
            "focused": True,
            "pre_semantic_sha256": "a" * 64,
            "expected_post_correlation": "escaped",
            "post_semantic_sha256": None,
        }
        route.write_text(json.dumps(route_value), encoding="utf-8")
        post = {
            **route_value,
            "post_semantic_sha256": "f" * 64,
            "observed_transition": "escaped",
            "tool_sha256": "b" * 64,
            "provider": "echo-semantic-probe-v1",
            "provider_executable_sha256": digest(b"echo"),
            "captured_monotonic_ns": 10**30,
        }
        (self.root / "post.json").write_text(json.dumps(post), encoding="utf-8")
        patches = self.identity_context()
        with (
            patches[0],
            patches[1],
            patches[2],
            patches[3],
            patches[4],
            mock.patch.object(DRIVER, "probe_tools", return_value=self.tools),
            self.assertRaisesRegex(DRIVER.Blocked, "disabled"),
        ):
            DRIVER.keyboard_input(
                self.root, self.expected, "escape", route, Path("post.json")
            )

    def test_send_actions_are_forbidden(self) -> None:
        for action in ("send", "enter", "ban_user", "type_text"):
            with self.assertRaises(DRIVER.Blocked):
                DRIVER.input_command(
                    {"name": "wtype", "path": "/usr/bin/wtype"}, action
                )

    def test_source_provenance_hashes_adapter_stage_and_commit(self) -> None:
        value = DRIVER.source_provenance()
        self.assertEqual(len(value["adapter_sha256"]), 64)
        self.assertEqual(len(value["stage_runner_sha256"]), 64)
        self.assertTrue(value["repository_commit"])


if __name__ == "__main__":
    unittest.main()
