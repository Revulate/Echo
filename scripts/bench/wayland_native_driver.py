#!/usr/bin/env python3
"""Fail-closed native Wayland capture and keyboard adapter.

The adapter intentionally has no X11 implementation.  Commands that cannot
prove the target process, Wayland socket, profile, focused window, and tool
provenance produce a machine-readable BLOCKED result and a non-zero exit.
"""

from __future__ import annotations

import argparse
import binascii
import hashlib
import json
import os
import re
import secrets
import shutil
import signal
import stat
import struct
import subprocess
import sys
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, Sequence


REPO_ROOT = Path(__file__).resolve().parents[2]
STAGE_RUNNER = REPO_ROOT / "scripts/run-linux-stage.sh"
KWIN_SCRIPT_TEMPLATE = REPO_ROOT / "scripts/bench/kwin/echo-benchmark-receipt.js"
KWIN_PROVIDER = REPO_ROOT / "target/debug/echo-kwin-receipt-provider"
BLOCKED_EXIT = 3
FAILED_EXIT = 4
MAX_PNG_FILE_BYTES = 128 * 1024 * 1024
MAX_PNG_IDAT_BYTES = 96 * 1024 * 1024
MAX_PNG_DECODED_BYTES = 100_000_000
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
WINDOW_TOKEN_RE = re.compile(r"^[A-Za-z0-9_.:@/-]{1,256}$")
TRUSTED_TOOL_SPECS: dict[str, tuple[str, tuple[str, ...]]] = {
    "wtype": ("/usr/bin/wtype", ("wtype",)),
    "ydotool": ("/usr/bin/ydotool", ("ydotool",)),
    "grim": ("/usr/bin/grim", ("grim",)),
    "spectacle": ("/usr/bin/spectacle", ("spectacle",)),
    "kscreen-doctor": ("/usr/bin/kscreen-doctor", ("libkscreen",)),
    "kwin_wayland": ("/usr/bin/kwin_wayland", ("kwin",)),
    "qdbus6": ("/usr/bin/qdbus6", ("qt6-tools",)),
}
QDBUS_TRUSTED_TARGET = "/usr/lib/qt6/bin/qdbus"
SAFE_ACTIONS: dict[str, tuple[str, ...]] = {
    "escape": ("ESC",),
    "focus_next": ("CTRL", "TAB"),
    "focus_previous": ("CTRL", "SHIFT", "TAB"),
    "move_up": ("UP",),
    "move_down": ("DOWN",),
    "move_left": ("LEFT",),
    "move_right": ("RIGHT",),
    "page_up": ("PAGEUP",),
    "page_down": ("PAGEDOWN",),
    "home": ("HOME",),
    "end": ("END",),
}
TOOL_CANDIDATES = tuple(TRUSTED_TOOL_SPECS)


class Blocked(RuntimeError):
    """A required native capability or identity proof is unavailable."""


class Failed(RuntimeError):
    """A supported operation ran but did not satisfy its contract."""


class UnsupportedPng(Blocked):
    """A well-formed PNG uses an intentionally unsupported encoding."""


def utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def decode_png_strict(
    path: Path, *, max_decoded_bytes: int = MAX_PNG_DECODED_BYTES
) -> tuple[int, int, int, bytes]:
    try:
        file_size = path.stat().st_size
        with path.open("rb") as stream:
            data = stream.read(MAX_PNG_FILE_BYTES + 1)
    except OSError as exc:
        raise Failed(f"cannot read PNG: {exc}") from exc
    if file_size > MAX_PNG_FILE_BYTES or len(data) > MAX_PNG_FILE_BYTES:
        raise Failed("PNG file exceeds bounded size")
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise Failed("capture artifact is not a PNG")
    offset = 8
    ihdr: tuple[int, int, int, int, int, int, int] | None = None
    idat = bytearray()
    seen_idat = False
    seen_iend = False
    chunk_index = 0
    while offset < len(data):
        if offset + 12 > len(data):
            raise Failed("capture PNG is truncated")
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        kind = data[offset + 4 : offset + 8]
        end = offset + 8 + length
        if end + 4 > len(data):
            raise Failed("capture PNG chunk is truncated")
        payload = data[offset + 8 : end]
        crc = struct.unpack(">I", data[end : end + 4])[0]
        if binascii.crc32(kind + payload) & 0xFFFFFFFF != crc:
            raise Failed("capture PNG chunk CRC mismatch")
        if (
            not all(65 <= byte <= 90 or 97 <= byte <= 122 for byte in kind)
            or kind[2] & 0x20
        ):
            raise Failed("capture PNG has an invalid chunk type")
        if ihdr is None and kind != b"IHDR":
            raise Failed("capture PNG must begin with IHDR")
        if kind == b"IHDR":
            if chunk_index != 0 or ihdr is not None or length != 13:
                raise Failed("capture PNG must contain exactly one first IHDR")
            ihdr = struct.unpack(">IIBBBBB", payload)
            width, height, depth, color_type, compression, filtering, interlace = ihdr
            valid_depths = {
                0: {1, 2, 4, 8, 16},
                2: {8, 16},
                3: {1, 2, 4, 8},
                4: {8, 16},
                6: {8, 16},
            }
            if (
                width <= 0
                or height <= 0
                or compression != 0
                or filtering != 0
                or interlace not in {0, 1}
                or depth not in valid_depths.get(color_type, set())
            ):
                raise Failed("capture PNG uses invalid encoding")
        elif kind == b"IDAT":
            if ihdr is None or seen_iend:
                raise Failed("capture PNG has invalid IDAT ordering")
            seen_idat = True
            if len(idat) + len(payload) > MAX_PNG_IDAT_BYTES:
                raise Failed("capture PNG accumulated IDAT exceeds bound")
            idat.extend(payload)
        elif kind == b"IEND":
            if length != 0 or seen_iend or not seen_idat:
                raise Failed("capture PNG has invalid IEND")
            seen_iend = True
            offset = end + 4
            if offset != len(data):
                raise Failed("capture PNG has trailing bytes")
            break
        elif kind in {b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP"}:
            assert ihdr is not None
            color_type = ihdr[3]
            malformed = (
                seen_idat
                or (kind == b"tRNS" and (color_type in {4, 6} or not 0 < length <= 256))
                or (kind == b"tRNS" and color_type == 0 and length != 2)
                or (kind == b"tRNS" and color_type == 2 and length != 6)
                or (
                    kind == b"gAMA"
                    and (length != 4 or struct.unpack(">I", payload)[0] == 0)
                )
                or (kind == b"cHRM" and length != 32)
                or (kind == b"sRGB" and (length != 1 or payload[0] > 3))
                or (
                    kind == b"iCCP"
                    and (
                        not 3 <= length
                        or b"\0" not in payload[:-2]
                        or not 1 <= len(payload.split(b"\0", 1)[0]) <= 79
                        or payload.split(b"\0", 1)[1][0] != 0
                    )
                )
            )
            if malformed:
                raise Failed(f"capture PNG has malformed {kind.decode()} chunk")
            raise UnsupportedPng(
                f"PNG color semantic chunk {kind.decode()} is unsupported"
            )
        elif kind and kind[0] & 0x20 == 0:
            raise UnsupportedPng(
                f"unknown PNG critical chunk {kind.decode(errors='replace')} is unsupported"
            )
        elif seen_idat:
            raise Failed("capture PNG IDAT chunks must be contiguous")
        offset = end + 4
        chunk_index += 1
    if ihdr is None or not seen_idat or not seen_iend:
        raise Failed("capture PNG lacks required terminal chunks")
    width, height, depth, color_type, compression, filtering, interlace = ihdr
    channels = {0: 1, 2: 3, 4: 2, 6: 4}.get(color_type)
    if depth != 8 or channels is None or interlace:
        raise UnsupportedPng("PNG encoding is well-formed but unsupported")
    expected_bytes = height * (1 + width * channels)
    if expected_bytes > max_decoded_bytes:
        raise Failed("capture PNG decoded size exceeds bound")
    try:
        decompressor = zlib.decompressobj()
        decoded = decompressor.decompress(bytes(idat), expected_bytes + 1)
    except zlib.error as exc:
        raise Failed(f"capture PNG decompression failed: {exc}") from exc
    if decompressor.unconsumed_tail:
        raise Failed("capture PNG decompressed output exceeds exact bound")
    if (
        len(decoded) != expected_bytes
        or not decompressor.eof
        or decompressor.unused_data
    ):
        raise Failed("capture PNG decoded byte count is invalid")
    stride = width * channels
    pixels = bytearray(height * stride)
    source = 0
    for row in range(height):
        filter_type = decoded[source]
        source += 1
        if filter_type not in range(5):
            raise Failed("capture PNG has invalid scanline filter")
        for column in range(stride):
            value = decoded[source]
            source += 1
            output_index = row * stride + column
            left = pixels[output_index - channels] if column >= channels else 0
            above = pixels[output_index - stride] if row else 0
            upper_left = (
                pixels[output_index - stride - channels]
                if row and column >= channels
                else 0
            )
            estimate = left + above - upper_left
            choices = (
                abs(estimate - left),
                abs(estimate - above),
                abs(estimate - upper_left),
            )
            paeth_value = (left, above, upper_left)[choices.index(min(choices))]
            predictor = (0, left, above, (left + above) // 2, paeth_value)[filter_type]
            pixels[output_index] = (value + predictor) & 0xFF
    return width, height, channels, bytes(pixels)


def validate_png(
    path: Path, *, max_decoded_bytes: int = MAX_PNG_DECODED_BYTES
) -> tuple[int, int]:
    width, height, _, _ = decode_png_strict(path, max_decoded_bytes=max_decoded_bytes)
    return width, height


def source_provenance() -> dict[str, Any]:
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )
    return {
        "adapter_path": str(Path(__file__).resolve()),
        "adapter_sha256": sha256_file(Path(__file__).resolve()),
        "stage_runner_path": str(STAGE_RUNNER.resolve(strict=True)),
        "stage_runner_sha256": sha256_file(STAGE_RUNNER),
        "repository_commit": commit.stdout.strip() if commit.returncode == 0 else None,
        "repository_commit_argv": ["git", "rev-parse", "HEAD"],
    }


def canonical(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def confined_path(root: Path, candidate: Path, *, must_exist: bool = False) -> Path:
    root = canonical(root)
    candidate = canonical(candidate if candidate.is_absolute() else root / candidate)
    if candidate != root and root not in candidate.parents:
        raise Blocked(f"path escapes artifact root: {candidate}")
    if must_exist and not candidate.is_file():
        raise Blocked(f"required file is missing: {candidate}")
    return candidate


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise Blocked(f"invalid JSON receipt {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise Blocked(f"receipt must be a JSON object: {path}")
    return value


def proc_environ(pid: int) -> dict[str, str]:
    try:
        raw = Path(f"/proc/{pid}/environ").read_bytes()
    except OSError as exc:
        raise Blocked(f"cannot read expected process environment: {exc}") from exc
    result: dict[str, str] = {}
    for item in raw.split(b"\0"):
        if b"=" in item:
            key, value = item.split(b"=", 1)
            result[key.decode(errors="replace")] = value.decode(errors="replace")
    return result


def proc_start_ticks(pid: int) -> int:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
        value = int(fields[21])
    except (OSError, ValueError, IndexError) as exc:
        raise Blocked(f"cannot read expected process start ticks: {exc}") from exc
    if value <= 0:
        raise Blocked("expected process start ticks are invalid")
    return value


def proc_executable(pid: int) -> Path:
    try:
        return Path(os.readlink(f"/proc/{pid}/exe")).resolve(strict=True)
    except OSError as exc:
        raise Blocked(f"cannot resolve expected executable: {exc}") from exc


def package_version(path: Path) -> tuple[str | None, str | None, list[list[str]]]:
    pacman = shutil.which("pacman")
    if pacman is None:
        return None, None, []
    owner_argv = [pacman, "-Qqo", str(path)]
    try:
        owner = subprocess.run(
            owner_argv, check=False, capture_output=True, text=True, timeout=5
        )
    except (OSError, subprocess.TimeoutExpired):
        return None, None, []
    package = owner.stdout.strip()
    if owner.returncode != 0 or not package or "\n" in package:
        return None, None, [owner_argv]
    version_argv = [pacman, "-Q", package]
    try:
        version = subprocess.run(
            version_argv, check=False, capture_output=True, text=True, timeout=5
        )
    except (OSError, subprocess.TimeoutExpired):
        return None, package, [owner_argv, version_argv]
    text = version.stdout.strip()
    if version.returncode != 0 or not text.startswith(package + " "):
        return None, package, [owner_argv, version_argv]
    return text[len(package) + 1 :], package, [owner_argv, version_argv]


def package_integrity(package: str) -> tuple[bool, dict[str, Any]]:
    pacman = shutil.which("pacman")
    if pacman is None:
        return False, {"status": "BLOCKED", "reason": "pacman unavailable"}
    argv = [pacman, "-Qkk", package]
    try:
        result = subprocess.run(
            argv, check=False, capture_output=True, text=True, timeout=15
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return False, {"status": "BLOCKED", "argv": argv, "reason": str(exc)}
    combined = result.stdout + result.stderr
    altered_counts = [
        int(value) for value in re.findall(r"\b(\d+) altered files?\b", combined)
    ]
    passed = (
        result.returncode == 0
        and bool(altered_counts)
        and all(value == 0 for value in altered_counts)
    )
    return passed, {
        "status": "PASS" if passed else "BLOCKED",
        "argv": argv,
        "returncode": result.returncode,
        "stdout_sha256": hashlib.sha256(result.stdout.encode()).hexdigest(),
        "stderr_sha256": hashlib.sha256(result.stderr.encode()).hexdigest(),
        "altered_file_counts": altered_counts,
    }


def command_version(path: Path) -> tuple[str | None, list[list[str]], str | None]:
    packaged, _, package_argv = package_version(path)
    if packaged:
        return packaged, package_argv, "package-manager"
    variants = ([str(path), "--version"], [str(path), "-v"])
    for argv in variants:
        try:
            result = subprocess.run(
                argv,
                check=False,
                capture_output=True,
                text=True,
                timeout=5,
                env={"PATH": os.environ.get("PATH", "")},
            )
        except (OSError, subprocess.TimeoutExpired):
            continue
        text = (result.stdout + result.stderr).strip()
        if result.returncode == 0 and text:
            return text.splitlines()[0][:500], [argv], "tool-cli"
    return None, package_argv, None


def probe_tools() -> dict[str, dict[str, Any]]:
    tools: dict[str, dict[str, Any]] = {}
    for name in TOOL_CANDIDATES:
        expected_path_text, allowed_packages = TRUSTED_TOOL_SPECS[name]
        expected_path = Path(expected_path_text)
        resolved = shutil.which(name)
        if resolved is None:
            tools[name] = {"installed": False, "available": False}
            continue
        link_path = Path(resolved)
        path = link_path.resolve(strict=True)
        if name == "qdbus6":
            trusted_target = Path(QDBUS_TRUSTED_TARGET)
            link_valid = (
                link_path == expected_path
                and link_path.is_symlink()
                and path == trusted_target
            )
        else:
            link_valid = (
                link_path == expected_path
                and not link_path.is_symlink()
                and path == expected_path
                and path.name == name
            )
        if not link_valid:
            tools[name] = {
                "installed": True,
                "available": False,
                "rejected_path": str(path),
                "expected_path": expected_path_text,
            }
            continue
        version, package, version_argv = package_version(link_path)
        if package not in allowed_packages:
            tools[name] = {
                "installed": True,
                "available": False,
                "rejected_package": package,
                "allowed_packages": list(allowed_packages),
            }
            continue
        integrity_ok, integrity = package_integrity(package)
        if not integrity_ok:
            tools[name] = {
                "installed": True,
                "available": False,
                "rejected_package": package,
                "package_integrity": integrity,
            }
            continue
        tools[name] = {
            "installed": True,
            "available": True,
            "path": str(link_path),
            "sha256": sha256_file(path),
            "canonical_target_path": str(path),
            "canonical_target_sha256": sha256_file(path),
            "link_path": str(link_path) if link_path.is_symlink() else None,
            "link_target": os.readlink(link_path) if link_path.is_symlink() else None,
            "link_sha256": hashlib.sha256(os.readlink(link_path).encode()).hexdigest()
            if link_path.is_symlink()
            else None,
            "version": version,
            "version_argv": version_argv,
            "version_source": "package-manager" if version else None,
            "package": package,
            "package_integrity": integrity,
        }
    return tools


def session_probe(env: Mapping[str, str] = os.environ) -> dict[str, Any]:
    session_type = env.get("XDG_SESSION_TYPE", "")
    display = env.get("WAYLAND_DISPLAY", "")
    runtime = env.get("XDG_RUNTIME_DIR", "")
    if session_type != "wayland":
        raise Blocked(
            f"native Wayland requires XDG_SESSION_TYPE=wayland, got {session_type!r}"
        )
    if not display or "/" in display or display in {".", ".."}:
        raise Blocked("WAYLAND_DISPLAY must be a socket basename")
    if not runtime:
        raise Blocked("XDG_RUNTIME_DIR is missing")
    runtime_path = Path(runtime).resolve(strict=True)
    socket_path = runtime_path / display
    try:
        socket_stat = socket_path.stat()
    except OSError as exc:
        raise Blocked(f"Wayland socket is unavailable: {exc}") from exc
    if not stat.S_ISSOCK(socket_stat.st_mode):
        raise Blocked(f"Wayland endpoint is not a socket: {socket_path}")
    if socket_stat.st_uid != os.getuid():
        raise Blocked("Wayland socket is not owned by the current UID")
    return {
        "session_type": session_type,
        "wayland_display": display,
        "wayland_socket": str(socket_path),
        "xdg_runtime_dir": str(runtime_path),
        "display_present_but_unused": bool(env.get("DISPLAY")),
        "x11_fallback": False,
    }


@dataclass(frozen=True)
class IdentityExpectation:
    pid: int
    process_start_ticks: int
    executable_path: Path
    executable_sha256: str
    campaign_run_root: Path
    profile_root: Path
    wayland_socket: Path
    window_token: str
    focus_receipt: Path


def kwin_focus_probe(tools: Mapping[str, dict[str, Any]]) -> dict[str, Any]:
    qdbus = select_tool(tools, ("qdbus6",))
    argv = [
        qdbus["path"],
        "org.kde.KWin",
        "/KWin",
        "org.echo.Benchmark.activeWindowReceipt",
    ]
    try:
        result = subprocess.run(
            argv, check=False, capture_output=True, text=True, timeout=5
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Blocked(f"operational KWin focus probe failed: {exc}") from exc
    if result.returncode != 0:
        raise Blocked("operational KWin focus probe is unavailable")
    try:
        receipt = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise Blocked("operational KWin focus probe returned invalid JSON") from exc
    if not isinstance(receipt, dict):
        raise Blocked("operational KWin focus probe returned non-object JSON")
    receipt["provider"] = {
        "name": "qdbus6-kwin-active-window",
        "tool_path": qdbus["path"],
        "tool_sha256": qdbus["sha256"],
        "argv": argv,
    }
    return receipt


def verify_identity(
    expected: IdentityExpectation,
    *,
    env: Mapping[str, str] = os.environ,
    tools: Mapping[str, dict[str, Any]] | None = None,
    operational_probe: Callable[[Mapping[str, dict[str, Any]]], dict[str, Any]]
    | None = None,
) -> dict[str, Any]:
    if expected.pid <= 1:
        raise Blocked("expected PID must identify a non-init process")
    if not SHA256_RE.fullmatch(expected.executable_sha256):
        raise Blocked("expected executable hash must be lowercase SHA-256")
    if not WINDOW_TOKEN_RE.fullmatch(expected.window_token):
        raise Blocked("window identity token has invalid syntax")
    session = session_probe(env)
    actual_socket = Path(session["wayland_socket"]).resolve(strict=True)
    expected_socket = expected.wayland_socket.resolve(strict=True)
    if actual_socket != expected_socket:
        raise Blocked("current Wayland socket differs from expected socket")

    executable = proc_executable(expected.pid)
    if executable != expected.executable_path.resolve(strict=True):
        raise Blocked("expected PID executable path mismatch")
    executable_hash = sha256_file(executable)
    if executable_hash != expected.executable_sha256:
        raise Blocked("expected PID executable hash mismatch")
    process_env = proc_environ(expected.pid)
    if proc_start_ticks(expected.pid) != expected.process_start_ticks:
        raise Blocked("expected PID process start ticks mismatch")
    run_root = canonical(expected.campaign_run_root)
    profile_root = canonical(expected.profile_root)
    if profile_root == run_root or run_root not in profile_root.parents:
        raise Blocked("profile root must be strictly inside campaign run root")
    process_profile = process_env.get("ECHO_BENCH_PROFILE_ROOT")
    if not process_profile:
        raise Blocked("expected process lacks ECHO_BENCH_PROFILE_ROOT")
    if canonical(Path(process_profile)) != profile_root:
        raise Blocked("expected process profile root mismatch")
    process_runtime = process_env.get("XDG_RUNTIME_DIR")
    process_display = process_env.get("WAYLAND_DISPLAY")
    if not process_runtime or not process_display:
        raise Blocked("expected process lacks Wayland runtime binding")
    if canonical(Path(process_runtime) / process_display) != expected_socket:
        raise Blocked(
            "expected process runtime/display does not resolve expected socket"
        )
    if process_env.get("WAYLAND_DISPLAY") != session["wayland_display"]:
        raise Blocked("expected process is attached to a different Wayland display")
    if process_env.get("XDG_SESSION_TYPE") not in {None, "wayland"}:
        raise Blocked("expected process declares a non-Wayland session")

    tools = probe_tools() if tools is None else tools
    receipt = read_json(expected.focus_receipt)
    operational_receipt = (operational_probe or kwin_focus_probe)(tools)
    if receipt != operational_receipt:
        raise Blocked("focus receipt does not match fixed operational compositor probe")
    required = {
        "pid": expected.pid,
        "process_start_ticks": expected.process_start_ticks,
        "executable_path": str(executable),
        "executable_sha256": expected.executable_sha256,
        "campaign_run_root": str(run_root),
        "profile_root": str(profile_root),
        "wayland_socket": str(expected_socket),
        "window_token": expected.window_token,
        "focused": True,
    }
    for key, value in required.items():
        if receipt.get(key) != value:
            raise Blocked(f"focus receipt {key!r} mismatch")
    captured_ns = receipt.get("captured_monotonic_ns")
    if not isinstance(captured_ns, int) or captured_ns <= 0:
        raise Blocked("focus receipt lacks captured_monotonic_ns")
    max_age_ns = receipt.get("max_age_ns", 5_000_000_000)
    if not isinstance(max_age_ns, int) or not 0 < max_age_ns <= 30_000_000_000:
        raise Blocked("focus receipt max_age_ns is invalid")
    now_ns = time.monotonic_ns()
    if captured_ns > now_ns or now_ns - captured_ns > max_age_ns:
        raise Blocked("focus receipt is stale or from the future")
    geometry = receipt.get("geometry")
    if not isinstance(geometry, dict):
        raise Blocked("focus receipt lacks exact active-window geometry")
    for key in ("x", "y", "width", "height"):
        if not isinstance(geometry.get(key), int) or (
            key in {"width", "height"} and geometry[key] <= 0
        ):
            raise Blocked(f"focus receipt geometry {key} is invalid")
    provider = receipt.get("provider")
    qdbus = select_tool(tools, ("qdbus6",))
    expected_provider = {
        "name": "qdbus6-kwin-active-window",
        "tool_path": qdbus["path"],
        "tool_sha256": qdbus["sha256"],
        "argv": [
            qdbus["path"],
            "org.kde.KWin",
            "/KWin",
            "org.echo.Benchmark.activeWindowReceipt",
        ],
    }
    if provider != expected_provider:
        raise Blocked("focus receipt provider provenance mismatch")
    foreign_token = receipt.get("foreign_toplevel_token")
    if not isinstance(foreign_token, str) or not WINDOW_TOKEN_RE.fullmatch(
        foreign_token
    ):
        raise Blocked("focus receipt lacks verified foreign-toplevel token")
    return {
        "session": session,
        "pid": expected.pid,
        "executable": str(executable),
        "executable_sha256": executable_hash,
        "profile_root": str(canonical(expected.profile_root)),
        "window_token": expected.window_token,
        "window_geometry": geometry,
        "foreign_toplevel_token": foreign_token,
        "focus_receipt": str(expected.focus_receipt.resolve(strict=True)),
        "focus_receipt_sha256": sha256_file(expected.focus_receipt),
        "verified_monotonic_ns": now_ns,
    }


def select_tool(
    tools: Mapping[str, dict[str, Any]], names: Sequence[str]
) -> dict[str, Any]:
    for name in names:
        record = tools.get(name, {})
        if record.get("available") and record.get("version") and record.get("sha256"):
            return {"name": name, **record}
    raise Blocked(f"no versioned tool available from: {', '.join(names)}")


def bounded_probe(argv: list[str]) -> dict[str, Any]:
    started_ns = time.monotonic_ns()
    try:
        result = subprocess.run(
            argv, check=False, capture_output=True, text=True, timeout=5
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"status": "BLOCKED", "argv": argv, "reason": str(exc)}
    return {
        "status": "PASS" if result.returncode == 0 else "BLOCKED",
        "argv": argv,
        "returncode": result.returncode,
        "started_monotonic_ns": started_ns,
        "ended_monotonic_ns": time.monotonic_ns(),
        "stdout_sha256": hashlib.sha256(result.stdout.encode()).hexdigest(),
        "stderr_sha256": hashlib.sha256(result.stderr.encode()).hexdigest(),
        "stdout": result.stdout[:4000],
        "stderr": result.stderr[:4000],
    }


def capability_matrix(
    tools: Mapping[str, dict[str, Any]],
    *,
    probe: Callable[[list[str]], dict[str, Any]] = bounded_probe,
) -> dict[str, dict[str, Any]]:
    groups = {
        "keyboard_input": ("wtype", "ydotool"),
        "screenshot_capture": ("grim",),
        "display_geometry": ("kscreen-doctor",),
        "kwin_compositor": ("kwin_wayland",),
    }
    kscreen = tools.get("kscreen-doctor", {})
    qdbus = tools.get("qdbus6", {})
    display_probe = (
        probe([kscreen.get("path", ""), "-o"])
        if kscreen.get("available")
        else {"status": "BLOCKED"}
    )
    compositor_probe = (
        probe([qdbus.get("path", ""), "org.kde.KWin", "/KWin", "supportInformation"])
        if qdbus.get("available")
        else {"status": "BLOCKED"}
    )
    result: dict[str, dict[str, Any]] = {}
    for capability, names in groups.items():
        available = [
            name
            for name in names
            if tools.get(name, {}).get("available")
            and tools.get(name, {}).get("version")
            and tools.get(name, {}).get("sha256")
        ]
        installed = [
            name
            for name in names
            if tools.get(name, {}).get(
                "installed", tools.get(name, {}).get("available", False)
            )
        ]
        executable_probes = {
            name: probe([tools[name]["path"], "-h"])
            for name in available
            if name in {"wtype", "grim"}
        }
        executable = bool(available) and all(
            record["status"] == "PASS" for record in executable_probes.values()
        )
        operational = executable and compositor_probe["status"] == "PASS"
        if capability == "keyboard_input":
            operational = False
        elif capability == "screenshot_capture":
            grim_probe = executable_probes.get("grim", {})
            operational = operational and "-T" in (
                grim_probe.get("stdout", "") + grim_probe.get("stderr", "")
            )
        elif capability == "display_geometry":
            operational = operational and display_probe["status"] == "PASS"
        result[capability] = {
            "installed_status": "PASS" if installed else "BLOCKED",
            "tool_executable_status": "PASS" if executable else "BLOCKED",
            "compositor_operational_status": compositor_probe["status"],
            "capture_operational_status": "PASS"
            if capability == "screenshot_capture" and operational
            else "BLOCKED",
            "operational_status": "PASS" if available and operational else "BLOCKED",
            "status": "PASS" if available and operational else "BLOCKED",
            "versioned_tools": available,
            "installed_tools": installed,
            "candidates": list(names),
            "operational_probes": {
                "compositor": compositor_probe,
                "display": display_probe if capability == "display_geometry" else None,
                "tool_executable": executable_probes,
            },
        }
    return result


def stage_descriptor(
    stage: str,
    command: Sequence[str],
    log_path: Path,
    timeout_seconds: int,
) -> dict[str, Any]:
    if not 1 <= timeout_seconds <= 60:
        raise Blocked("timeout must be between 1 and 60 seconds")
    runner_argv = [
        str(STAGE_RUNNER),
        "--timeout-seconds",
        str(timeout_seconds),
        "--heartbeat-seconds",
        str(min(5, timeout_seconds)),
        "--kill-after-seconds",
        "2",
        "--log",
        str(log_path),
        "--",
        stage,
        *command,
    ]
    return {
        "stage": stage,
        "command_argv": list(command),
        "runner_argv": runner_argv,
        "timeout_seconds": timeout_seconds,
        "log_path": str(log_path),
        "cleanup_contract": "run-linux-stage TERM/KILL process-group cleanup; adapter owns no persistent process",
    }


def execute_stage(
    descriptor: dict[str, Any],
    *,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> dict[str, Any]:
    started_wall = utc_now()
    started_wall_ns = time.time_ns()
    started_ns = time.monotonic_ns()
    result = runner(
        descriptor["runner_argv"],
        check=False,
        capture_output=True,
        text=True,
        timeout=descriptor["timeout_seconds"] + 5,
        env={
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "PATH": os.environ.get("PATH", ""),
            "WAYLAND_DISPLAY": os.environ.get("WAYLAND_DISPLAY", ""),
            "XDG_RUNTIME_DIR": os.environ.get("XDG_RUNTIME_DIR", ""),
            "XDG_SESSION_TYPE": "wayland",
        },
    )
    ended_ns = time.monotonic_ns()
    log_path = Path(descriptor["log_path"])
    if not log_path.is_file():
        raise Failed("bounded stage did not create its retained log")
    terminal_patterns = (
        (
            re.compile(
                rf"PASS stage={re.escape(descriptor['stage'])} elapsed=\d+s log={re.escape(str(log_path))}"
            ),
            0,
            "PASS",
        ),
        (
            re.compile(
                rf"TIMEOUT stage={re.escape(descriptor['stage'])} elapsed=\d+s log={re.escape(str(log_path))}"
            ),
            124,
            "TIMEOUT",
        ),
        (
            re.compile(
                rf"FAIL stage={re.escape(descriptor['stage'])} status=([1-9]\d{{0,2}}) elapsed=\d+s log={re.escape(str(log_path))}"
            ),
            None,
            "FAIL",
        ),
        (
            re.compile(
                rf"FAIL stage={re.escape(descriptor['stage'])} signal=(INT|TERM|HUP) action=TERM log={re.escape(str(log_path))}"
            ),
            None,
            "FAIL",
        ),
    )
    candidates: list[tuple[str, int, str, str]] = []
    for stream_name, text in (("stdout", result.stdout), ("stderr", result.stderr)):
        prefix = text.split("----- retained stage log:", 1)[0]
        lines = prefix.splitlines()
        last_nonempty = max(
            (index for index, line in enumerate(lines) if line), default=-1
        )
        for index, line in enumerate(lines):
            for pattern, fixed_code, outcome in terminal_patterns:
                match = pattern.fullmatch(line)
                if match is None:
                    continue
                if fixed_code is not None:
                    expected_code = fixed_code
                elif "signal=" in line:
                    expected_code = {"INT": 130, "TERM": 143, "HUP": 129}[
                        match.group(1)
                    ]
                else:
                    expected_code = int(match.group(1))
                    if expected_code > 255:
                        raise Failed("bounded stage terminal status is invalid")
                if index != last_nonempty:
                    raise Failed("bounded stage terminal is not final")
                candidates.append((line, expected_code, outcome, stream_name))
                break
    if len(candidates) != 1:
        raise Failed("bounded stage terminal is missing or ambiguous")
    terminal, expected_code, outcome, terminal_stream = candidates[0]
    if result.returncode != expected_code:
        raise Failed("bounded stage terminal and return code mismatch")
    record = {
        **descriptor,
        "started_at": started_wall,
        "started_monotonic_ns": started_ns,
        "started_wall_ns": started_wall_ns,
        "ended_at": utc_now(),
        "ended_monotonic_ns": ended_ns,
        "returncode": result.returncode,
        "runner_stdout": result.stdout[-4000:],
        "runner_stderr": result.stderr[-4000:],
        "runner_stdout_sha256": hashlib.sha256(result.stdout.encode()).hexdigest(),
        "runner_stderr_sha256": hashlib.sha256(result.stderr.encode()).hexdigest(),
        "stage_log_sha256": sha256_file(log_path),
        "terminal": terminal,
        "terminal_stream": terminal_stream,
        "outcome": outcome,
    }
    if outcome != "PASS":
        raise Failed(json.dumps(record, sort_keys=True))
    return record


def wait_for_file(
    path: Path, process: subprocess.Popen[str], timeout_seconds: int = 5
) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        if path.is_file():
            return
        if process.poll() is not None:
            raise Failed("KWin receipt provider exited before readiness")
        time.sleep(0.025)
    raise Failed("KWin receipt provider readiness timed out")


def validate_provider_receipt(
    receipt: dict[str, Any],
    expected: IdentityExpectation,
    nonce: str,
    script_hash: str,
    provider_hash: str,
) -> None:
    required = {
        "schema_version": 1,
        "campaign_nonce_sha256": hashlib.sha256(nonce.encode()).hexdigest(),
        "provider_executable_sha256": provider_hash,
        "script_sha256": script_hash,
        "process_start_ticks": expected.process_start_ticks,
        "executable_path": str(expected.executable_path.resolve(strict=True)),
        "executable_sha256": expected.executable_sha256,
        "profile_root": str(expected.profile_root.resolve(strict=True)),
    }
    for key, value in required.items():
        if receipt.get(key) != value:
            raise Blocked(f"provider receipt {key!r} mismatch")
    captured = receipt.get("captured_provider_monotonic_ns")
    if not isinstance(captured, int) or captured <= 0:
        raise Blocked("provider receipt lacks trusted monotonic capture time")


def live_display_record(
    tools: Mapping[str, dict[str, Any]], output_name: str, artifact_root: Path
) -> dict[str, Any]:
    qdbus = select_tool(tools, ("qdbus6",))
    kscreen = select_tool(tools, ("kscreen-doctor",))
    active_log = confined_path(artifact_root, Path("kwin-active-output.log"))
    active_command = [
        qdbus["path"],
        "org.kde.KWin",
        "/KWin",
        "org.kde.KWin.activeOutputName",
    ]
    active_record = execute_stage(
        stage_descriptor("kwin-active-output", active_command, active_log, 5)
    )
    if active_log.read_text(encoding="utf-8").strip() != output_name:
        raise Blocked("provider output differs from live KWin active output")
    display_log = confined_path(artifact_root, Path("kscreen-display.json"))
    display_record = execute_stage(
        stage_descriptor(
            "kscreen-display-query", [kscreen["path"], "-j"], display_log, 8
        )
    )
    try:
        document = json.loads(display_log.read_text(encoding="utf-8"))
        output = next(
            item
            for item in document["outputs"]
            if item.get("name") == output_name and item.get("enabled")
        )
        mode = next(
            item
            for item in output["modes"]
            if item.get("id") == output.get("currentModeId")
        )
    except (OSError, json.JSONDecodeError, KeyError, StopIteration, TypeError) as exc:
        raise Blocked("live KScreen output query is incomplete") from exc
    return {
        "output": output_name,
        "geometry": {
            "x": output["pos"]["x"],
            "y": output["pos"]["y"],
            "width": round(mode["size"]["width"] / output["scale"]),
            "height": round(mode["size"]["height"] / output["scale"]),
        },
        "scale": output["scale"],
        "refresh_hz": mode["refreshRate"],
        "active_output_command": active_record,
        "kscreen_command": display_record,
    }


def orchestrate_kwin_receipts(
    artifact_root: Path,
    expected: IdentityExpectation,
    tools: Mapping[str, dict[str, Any]],
    *,
    timeout_seconds: int = 15,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    provider = KWIN_PROVIDER.resolve(strict=True)
    if not os.access(provider, os.X_OK):
        raise Blocked("KWin receipt provider is not executable")
    provider_hash = sha256_file(provider)
    nonce = secrets.token_hex(32)
    campaign_id = f"echo-wayland-{os.getpid()}-{nonce[:12]}"
    plugin_name = f"echo_benchmark_{nonce[:16]}"
    script_dir = confined_path(artifact_root, Path("kwin"))
    script_dir.mkdir(parents=True, exist_ok=True)
    script_path = confined_path(artifact_root, Path(f"kwin/{plugin_name}.js"))
    template = KWIN_SCRIPT_TEMPLATE.read_text(encoding="utf-8")
    if (
        template.count("__ECHO_CAMPAIGN_NONCE__") != 1
        or template.count("__ECHO_EXPECTED_PID__") != 1
    ):
        raise Failed("KWin receipt template nonce contract drifted")
    script_path.write_text(
        template.replace("__ECHO_CAMPAIGN_NONCE__", nonce).replace(
            "__ECHO_EXPECTED_PID__", str(expected.pid)
        ),
        encoding="utf-8",
    )
    script_hash = sha256_file(script_path)
    focus_path = confined_path(artifact_root, Path("focus.json"))
    display_path = confined_path(artifact_root, Path("display.json"))
    ready_path = confined_path(artifact_root, Path("provider-ready.json"))
    for stale in (focus_path, display_path, ready_path):
        if stale.exists():
            stale.unlink()
    provider_log = confined_path(artifact_root, Path("kwin-provider.log"))
    provider_wrapper = confined_path(artifact_root, Path("kwin-provider.wrapper.log"))
    provider_command = [
        str(provider),
        "--artifact-root",
        str(artifact_root),
        "--nonce",
        nonce,
        "--campaign-id",
        campaign_id,
        "--expected-pid",
        str(expected.pid),
        "--expected-start-ticks",
        str(expected.process_start_ticks),
        "--expected-executable",
        str(expected.executable_path),
        "--expected-executable-sha256",
        expected.executable_sha256,
        "--expected-profile-root",
        str(expected.profile_root),
        "--script-sha256",
        script_hash,
        "--timeout-seconds",
        str(timeout_seconds),
    ]
    provider_descriptor = stage_descriptor(
        "kwin-receipt-provider", provider_command, provider_log, timeout_seconds + 2
    )
    qdbus = select_tool(tools, ("qdbus6",))
    process: subprocess.Popen[str] | None = None
    load_record: dict[str, Any] | None = None
    start_record: dict[str, Any] | None = None
    cleanup_record: dict[str, Any] | None = None
    try:
        wrapper_stream = provider_wrapper.open("w", encoding="utf-8")
        process = subprocess.Popen(
            provider_descriptor["runner_argv"],
            stdout=wrapper_stream,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
            env={
                "PATH": os.environ.get("PATH", ""),
                "XDG_RUNTIME_DIR": os.environ.get("XDG_RUNTIME_DIR", ""),
                "DBUS_SESSION_BUS_ADDRESS": os.environ.get(
                    "DBUS_SESSION_BUS_ADDRESS", ""
                ),
            },
        )
        wrapper_stream.close()
        wait_for_file(ready_path, process)
        ready = read_json(ready_path)
        if (
            ready.get("provider_executable_sha256") != provider_hash
            or ready.get("script_sha256") != script_hash
        ):
            raise Blocked("KWin receipt provider readiness provenance mismatch")
        load_log = confined_path(artifact_root, Path("kwin-script-load.log"))
        load_record = execute_stage(
            stage_descriptor(
                "kwin-script-load",
                [
                    qdbus["path"],
                    "org.kde.KWin",
                    "/Scripting",
                    "org.kde.kwin.Scripting.loadScript",
                    str(script_path),
                    plugin_name,
                ],
                load_log,
                8,
            )
        )
        if not load_log.read_text(encoding="utf-8").strip().isdigit():
            raise Blocked("KWin refused the temporary campaign script")
        start_log = confined_path(artifact_root, Path("kwin-script-start.log"))
        start_record = execute_stage(
            stage_descriptor(
                "kwin-script-start",
                [
                    qdbus["path"],
                    "org.kde.KWin",
                    "/Scripting",
                    "org.kde.kwin.Scripting.start",
                ],
                start_log,
                8,
            )
        )
        returncode = process.wait(timeout=timeout_seconds + 3)
        if returncode != 0:
            raise Failed("KWin receipt provider stage failed")
        focus = read_json(focus_path)
        display = read_json(display_path)
        validate_provider_receipt(focus, expected, nonce, script_hash, provider_hash)
        validate_provider_receipt(display, expected, nonce, script_hash, provider_hash)
        if (
            focus.get("pid") != expected.pid
            or not focus.get("focused")
            or not focus.get("active")
        ):
            raise Blocked(
                "KWin focus receipt does not identify the expected active Echo process"
            )
        live_display = live_display_record(
            tools, focus.get("output", ""), artifact_root
        )
        raw_display = {key: display.get(key) for key in ("output", "geometry", "scale")}
        expected_display = {
            key: live_display[key] for key in ("output", "geometry", "scale")
        }
        if raw_display != expected_display:
            raise Blocked(
                "KWin script display receipt differs from separate live display query"
            )
        validated_display = {**display, **live_display}
        return (
            focus,
            validated_display,
            {
                "provider_path": str(provider),
                "provider_sha256": provider_hash,
                "provider_stage": provider_descriptor,
                "provider_wrapper_log": str(provider_wrapper),
                "provider_wrapper_sha256": sha256_file(provider_wrapper),
                "script_path": str(script_path),
                "script_sha256": script_hash,
                "plugin_name": plugin_name,
                "load": load_record,
                "start": start_record,
            },
        )
    except subprocess.TimeoutExpired as exc:
        raise Failed("KWin receipt provider completion timed out") from exc
    finally:
        try:
            cleanup_log = confined_path(artifact_root, Path("kwin-script-unload.log"))
            cleanup_record = execute_stage(
                stage_descriptor(
                    "kwin-script-unload",
                    [
                        qdbus["path"],
                        "org.kde.KWin",
                        "/Scripting",
                        "org.kde.kwin.Scripting.unloadScript",
                        plugin_name,
                    ],
                    cleanup_log,
                    8,
                )
            )
        except (Blocked, Failed, OSError, subprocess.SubprocessError):
            cleanup_record = None
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=2)
        if cleanup_record is None and sys.exc_info()[0] is None:
            raise Failed("temporary KWin campaign script cleanup failed")


def kwin_display_probe(tools: Mapping[str, dict[str, Any]]) -> dict[str, Any]:
    qdbus = select_tool(tools, ("qdbus6",))
    argv = [
        qdbus["path"],
        "org.kde.KWin",
        "/KWin",
        "org.echo.Benchmark.activeDisplayReceipt",
    ]
    try:
        result = subprocess.run(
            argv, check=False, capture_output=True, text=True, timeout=5
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Blocked(f"operational KWin display probe failed: {exc}") from exc
    if result.returncode != 0:
        raise Blocked("operational KWin display probe is unavailable")
    try:
        receipt = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise Blocked("operational KWin display probe returned invalid JSON") from exc
    if not isinstance(receipt, dict):
        raise Blocked("operational KWin display probe returned non-object JSON")
    receipt["provider"] = {
        "name": "qdbus6-kwin-active-display",
        "tool_path": qdbus["path"],
        "tool_sha256": qdbus["sha256"],
        "argv": argv,
    }
    return receipt


def validate_display_receipt(
    path: Path,
    tools: Mapping[str, dict[str, Any]],
    operational_probe: Callable[[Mapping[str, dict[str, Any]]], dict[str, Any]]
    | None = None,
) -> dict[str, Any]:
    receipt = read_json(path)
    if receipt != (operational_probe or kwin_display_probe)(tools):
        raise Blocked("display receipt does not match fixed live compositor probe")
    geometry = receipt.get("geometry")
    if not isinstance(geometry, dict):
        raise Blocked("display receipt lacks geometry")
    for key in ("x", "y", "width", "height"):
        value = geometry.get(key)
        if not isinstance(value, int) or (key in {"width", "height"} and value <= 0):
            raise Blocked(f"display geometry {key} is invalid")
    scale = receipt.get("scale")
    refresh = receipt.get("refresh_hz")
    if not isinstance(scale, (int, float)) or scale <= 0:
        raise Blocked("display scale is invalid")
    if not isinstance(refresh, (int, float)) or refresh <= 0:
        raise Blocked("display refresh_hz is invalid")
    if not receipt.get("output") or not isinstance(receipt.get("output"), str):
        raise Blocked("display output identity is missing")
    qdbus = select_tool(tools, ("qdbus6",))
    expected_provider = {
        "name": "qdbus6-kwin-active-display",
        "tool_path": qdbus["path"],
        "tool_sha256": qdbus["sha256"],
        "argv": [
            qdbus["path"],
            "org.kde.KWin",
            "/KWin",
            "org.echo.Benchmark.activeDisplayReceipt",
        ],
    }
    if receipt.get("provider") != expected_provider:
        raise Blocked("display receipt provider provenance mismatch")
    captured_ns = receipt.get("captured_monotonic_ns")
    now_ns = time.monotonic_ns()
    if (
        not isinstance(captured_ns, int)
        or captured_ns <= 0
        or captured_ns > now_ns
        or now_ns - captured_ns > 5_000_000_000
    ):
        raise Blocked("display receipt is stale or invalid")
    return receipt


def capture(
    artifact_root: Path,
    expected: IdentityExpectation,
    display_receipt_path: Path,
    *,
    timeout_seconds: int = 15,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> dict[str, Any]:
    artifact_root = canonical(artifact_root)
    artifact_root.mkdir(parents=True, exist_ok=True)
    tools = probe_tools()
    identity = verify_identity(expected, tools=tools)
    display_receipt = validate_display_receipt(display_receipt_path, tools)
    if display_receipt.get("window_token") != expected.window_token:
        raise Blocked("display receipt window token mismatch")
    required_display_identity = {
        "pid": expected.pid,
        "process_start_ticks": expected.process_start_ticks,
        "foreign_toplevel_token": identity["foreign_toplevel_token"],
        "focused": True,
    }
    for key, value in required_display_identity.items():
        if display_receipt.get(key) != value:
            raise Blocked(f"display receipt {key!r} mismatch")
    if display_receipt["geometry"] != identity["window_geometry"]:
        raise Blocked(
            "display receipt does not match verified active Echo window geometry"
        )
    tool = select_tool(tools, ("grim",))
    actual = confined_path(artifact_root, Path("actual.png"))
    metadata = confined_path(artifact_root, Path("actual.metadata.json"))
    log = confined_path(artifact_root, Path("capture-stage.log"))
    if actual.exists():
        actual.unlink()
    command = [tool["path"], "-T", identity["foreign_toplevel_token"], str(actual)]
    descriptor = stage_descriptor(
        "wayland-native-capture", command, log, timeout_seconds
    )
    command_record = execute_stage(descriptor, runner=runner)
    if not actual.is_file() or actual.stat().st_size < 24:
        raise Failed("capture tool did not create a plausible PNG artifact")
    actual_stat = actual.stat()
    if actual_stat.st_mtime_ns < command_record["started_wall_ns"]:
        raise Failed("capture artifact predates stage start")
    actual_width, actual_height = validate_png(actual)
    expected_width = round(
        display_receipt["geometry"]["width"] * display_receipt["scale"]
    )
    expected_height = round(
        display_receipt["geometry"]["height"] * display_receipt["scale"]
    )
    if (actual_width, actual_height) != (expected_width, expected_height):
        raise Failed("capture dimensions do not match display geometry and scale")
    evidence = {
        "schema_version": 1,
        "status": "PASS",
        "native_wayland": True,
        "session_verified": True,
        "session": identity["session"],
        "identity": identity,
        "display": display_receipt,
        "display_receipt_sha256": sha256_file(display_receipt_path),
        "tool": tool,
        "command": command_record,
        "actual_path": str(actual),
        "actual_sha256": sha256_file(actual),
        "actual_bytes": actual.stat().st_size,
        "actual_pixel_dimensions": {"width": actual_width, "height": actual_height},
        "captured_at": utc_now(),
        "artifact_inode": actual_stat.st_ino,
        "artifact_mtime_ns": actual_stat.st_mtime_ns,
        "source_provenance": source_provenance(),
    }
    metadata.write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return evidence


def capture_with_provider(
    artifact_root: Path,
    expected: IdentityExpectation,
    *,
    timeout_seconds: int = 15,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> dict[str, Any]:
    artifact_root = canonical(artifact_root)
    artifact_root.mkdir(parents=True, exist_ok=True)
    session = session_probe()
    expected_socket = expected.wayland_socket.resolve(strict=True)
    if Path(session["wayland_socket"]).resolve(strict=True) != expected_socket:
        raise Blocked("current Wayland socket differs from expected socket")
    executable = proc_executable(expected.pid)
    if executable != expected.executable_path.resolve(strict=True):
        raise Blocked("expected PID executable path mismatch")
    if proc_start_ticks(expected.pid) != expected.process_start_ticks:
        raise Blocked("expected PID process start ticks mismatch")
    if sha256_file(executable) != expected.executable_sha256:
        raise Blocked("expected PID executable hash mismatch")
    process_env = proc_environ(expected.pid)
    if canonical(Path(process_env.get("ECHO_BENCH_PROFILE_ROOT", ""))) != canonical(
        expected.profile_root
    ):
        raise Blocked("expected process profile root mismatch")
    process_socket = Path(process_env.get("XDG_RUNTIME_DIR", "")) / process_env.get(
        "WAYLAND_DISPLAY", ""
    )
    if canonical(process_socket) != expected_socket:
        raise Blocked("expected process Wayland binding mismatch")
    tools = probe_tools()
    focus, display, provider = orchestrate_kwin_receipts(
        artifact_root, expected, tools, timeout_seconds=timeout_seconds
    )
    internal_id = focus.get("internal_id")
    if not isinstance(internal_id, str) or not WINDOW_TOKEN_RE.fullmatch(internal_id):
        raise Blocked("KWin receipt lacks a usable window identifier")
    tool = select_tool(tools, ("grim",))
    actual = confined_path(artifact_root, Path("actual.png"))
    metadata = confined_path(artifact_root, Path("actual.metadata.json"))
    log = confined_path(artifact_root, Path("capture-stage.log"))
    if actual.exists():
        actual.unlink()
    command = [tool["path"], "-T", internal_id, str(actual)]
    command_record = execute_stage(
        stage_descriptor("wayland-native-capture", command, log, timeout_seconds),
        runner=runner,
    )
    if not actual.is_file() or actual.stat().st_size < 24:
        raise Failed(
            "grim -T did not produce a capture; KWin internalId is not a usable foreign token"
        )
    actual_width, actual_height = validate_png(actual)
    geometry = focus.get("geometry", {})
    expected_dimensions = (
        round(geometry.get("width", 0) * display["scale"]),
        round(geometry.get("height", 0) * display["scale"]),
    )
    if (actual_width, actual_height) != expected_dimensions:
        raise Failed(
            "capture dimensions do not match active-window geometry and display scale"
        )
    evidence = {
        "schema_version": 1,
        "status": "PASS",
        "native_wayland": True,
        "session_verified": True,
        "session": session,
        "focus": focus,
        "display": display,
        "provider": provider,
        "tool": tool,
        "command": command_record,
        "actual_path": str(actual),
        "actual_sha256": sha256_file(actual),
        "actual_bytes": actual.stat().st_size,
        "actual_pixel_dimensions": {"width": actual_width, "height": actual_height},
        "captured_at": utc_now(),
        "source_provenance": source_provenance(),
    }
    metadata.write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return evidence


def input_command(tool: Mapping[str, Any], action: str) -> list[str]:
    keys = SAFE_ACTIONS.get(action)
    if keys is None:
        raise Blocked("input action is not in the non-sending navigation allowlist")
    if tool["name"] == "wtype":
        argv = [tool["path"]]
        for modifier in keys[:-1]:
            argv.extend(["-M", modifier.lower()])
        argv.extend(["-k", keys[-1].lower()])
        for modifier in reversed(keys[:-1]):
            argv.extend(["-m", modifier.lower()])
        return argv
    codes = {
        "ESC": 1,
        "TAB": 15,
        "CTRL": 29,
        "SHIFT": 42,
        "HOME": 102,
        "UP": 103,
        "PAGEUP": 104,
        "LEFT": 105,
        "RIGHT": 106,
        "END": 107,
        "DOWN": 108,
        "PAGEDOWN": 109,
    }
    events = [f"{codes[key]}:1" for key in keys]
    events += [f"{codes[key]}:0" for key in reversed(keys)]
    return [tool["path"], "key", *events]


def keyboard_input(
    artifact_root: Path,
    expected: IdentityExpectation,
    action: str,
    route_receipt_path: Path,
    post_receipt_path: Path,
    *,
    timeout_seconds: int = 10,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> dict[str, Any]:
    artifact_root = canonical(artifact_root)
    artifact_root.mkdir(parents=True, exist_ok=True)
    tools = probe_tools()
    identity = verify_identity(expected, tools=tools)
    raise Blocked(
        "claimable native input is disabled until a fixed Echo-owned post-receipt provider exists; "
        f"verified target was {identity['window_token']}"
    )


def expectation_from_args(args: argparse.Namespace, root: Path) -> IdentityExpectation:
    return IdentityExpectation(
        pid=args.expected_pid,
        process_start_ticks=args.expected_process_start_ticks,
        executable_path=Path(args.expected_executable_path),
        executable_sha256=args.expected_executable_sha256,
        campaign_run_root=Path(args.campaign_run_root),
        profile_root=Path(args.expected_profile_root),
        wayland_socket=Path(args.expected_wayland_socket),
        window_token=args.expected_window_token,
        focus_receipt=confined_path(
            root,
            Path(args.focus_receipt or "focus.json"),
            must_exist=bool(args.focus_receipt),
        ),
    )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--artifact-root", required=True)
    sub = result.add_subparsers(dest="command", required=True)
    sub.add_parser("probe")
    for name in ("capture", "input"):
        command = sub.add_parser(name)
        command.add_argument("--expected-pid", required=True, type=int)
        command.add_argument("--expected-process-start-ticks", required=True, type=int)
        command.add_argument("--expected-executable-path", required=True)
        command.add_argument("--expected-executable-sha256", required=True)
        command.add_argument("--campaign-run-root", required=True)
        command.add_argument("--expected-profile-root", required=True)
        command.add_argument("--expected-wayland-socket", required=True)
        command.add_argument("--expected-window-token", default="provider-discovered")
        command.add_argument("--focus-receipt")
        command.add_argument(
            "--timeout-seconds", type=int, default=15 if name == "capture" else 10
        )
    sub.choices["capture"].add_argument("--display-receipt")
    sub.choices["input"].add_argument("--action", required=True)
    sub.choices["input"].add_argument("--route-receipt", required=True)
    sub.choices["input"].add_argument("--post-receipt", required=True)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    root = canonical(Path(args.artifact_root))
    session_verified: bool | None = None
    session_evidence: dict[str, Any] | None = None
    try:
        root.mkdir(parents=True, exist_ok=True)
        session_evidence = session_probe()
        session_verified = True
        if args.command == "probe":
            tools = probe_tools()
            capabilities = capability_matrix(tools)
            blocked = [
                name
                for name, record in capabilities.items()
                if record["status"] == "BLOCKED"
            ]
            result: dict[str, Any] = {
                "schema_version": 1,
                "status": "BLOCKED" if blocked else "PASS",
                "native_wayland": True,
                "session_verified": session_verified,
                "session": session_evidence,
                "tools": tools,
                "capabilities": capabilities,
                "blockers": [
                    f"unsupported native Wayland capability: {name}" for name in blocked
                ],
                "probed_at": utc_now(),
            }
        else:
            expected = expectation_from_args(args, root)
            if args.command == "capture":
                if args.focus_receipt or args.display_receipt:
                    if not args.focus_receipt or not args.display_receipt:
                        raise Blocked("receipt capture requires both receipt paths")
                    receipt = confined_path(
                        root, Path(args.display_receipt), must_exist=True
                    )
                    result = capture(
                        root, expected, receipt, timeout_seconds=args.timeout_seconds
                    )
                else:
                    result = capture_with_provider(
                        root, expected, timeout_seconds=args.timeout_seconds
                    )
            else:
                receipt = confined_path(root, Path(args.route_receipt), must_exist=True)
                post_receipt = confined_path(root, Path(args.post_receipt))
                result = keyboard_input(
                    root,
                    expected,
                    args.action,
                    receipt,
                    post_receipt,
                    timeout_seconds=args.timeout_seconds,
                )
        result["session_verified"] = session_verified
        result["session"] = session_evidence
        result["native_wayland"] = bool(session_verified)
    except Blocked as exc:
        result = {
            "schema_version": 1,
            "status": "BLOCKED",
            "native_wayland": bool(session_verified),
            "session_verified": session_verified,
            "session": session_evidence,
            "reason": str(exc),
            "recorded_at": utc_now(),
        }
        print(json.dumps(result, sort_keys=True))
        return BLOCKED_EXIT
    except (Failed, OSError, subprocess.SubprocessError) as exc:
        result = {
            "schema_version": 1,
            "status": "FAIL",
            "native_wayland": bool(session_verified),
            "session_verified": session_verified,
            "session": session_evidence,
            "reason": str(exc),
            "recorded_at": utc_now(),
        }
        print(json.dumps(result, sort_keys=True))
        return FAILED_EXIT
    print(json.dumps(result, sort_keys=True))
    return BLOCKED_EXIT if result["status"] == "BLOCKED" else 0


if __name__ == "__main__":
    sys.exit(main())
