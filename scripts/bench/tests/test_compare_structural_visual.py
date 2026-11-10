#!/usr/bin/env python3
"""Adversarial tests for structural and pixel visual comparison."""

from __future__ import annotations

import binascii
import hashlib
import importlib.util
import json
import math
import struct
import sys
import tempfile
import unittest
import zlib
from copy import deepcopy
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "scripts/bench/compare-structural-visual.py"
SPEC = importlib.util.spec_from_file_location("compare_structural_visual", MODULE_PATH)
assert SPEC and SPEC.loader
COMPARE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = COMPARE
SPEC.loader.exec_module(COMPARE)


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", binascii.crc32(kind + payload) & 0xFFFFFFFF)
    )


def write_png(
    path: Path, width: int, height: int, pixels: bytes, *, color_type: int = 2
) -> None:
    channels = {2: 3, 3: 1, 6: 4}[color_type]
    rows = b"".join(
        b"\0" + pixels[row * width * channels : (row + 1) * width * channels]
        for row in range(height)
    )
    palette = png_chunk(b"PLTE", b"\0\0\0") if color_type == 3 else b""
    path.write_bytes(
        COMPARE.PNG_SIGNATURE
        + png_chunk(
            b"IHDR", struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
        )
        + palette
        + png_chunk(b"IDAT", zlib.compress(rows))
        + png_chunk(b"IEND", b"")
    )


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def document(png: Path) -> dict:
    return {
        "viewport": {
            "x": 0,
            "y": 0,
            "width": 20,
            "height": 20,
            "scale": 1.0,
            "refresh_hz": 60.0,
            "output": "DP-1",
        },
        "screenshot_sha256": digest(png),
        "rectangles": [
            {"id": "root", "x": 0, "y": 0, "width": 20, "height": 20, "role": "window"},
            {
                "id": "left",
                "x": 0,
                "y": 0,
                "width": 10,
                "height": 20,
                "role": "split",
                "parent": "root",
                "hitbox": True,
            },
            {
                "id": "right",
                "x": 10,
                "y": 0,
                "width": 10,
                "height": 20,
                "role": "split",
                "parent": "root",
                "hitbox": True,
            },
        ],
        "masks": [
            {
                "id": "clock",
                "reason": "nondeterministic clock",
                "provenance": "fixture-v1",
                "x": 0,
                "y": 0,
                "width": 1,
                "height": 1,
            }
        ],
    }


class StructuralVisualTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.reference_png = self.root / "reference.png"
        self.actual_png = self.root / "actual.png"
        black = bytes(20 * 20 * 3)
        write_png(self.reference_png, 20, 20, black)
        write_png(self.actual_png, 20, 20, black)
        self.reference = document(self.reference_png)
        self.actual = document(self.actual_png)
        self.reference_path = self.root / "reference.json"
        self.actual_path = self.root / "actual.json"

    def tearDown(self) -> None:
        self.temp.cleanup()

    def save(self) -> None:
        self.reference_path.write_text(json.dumps(self.reference), encoding="utf-8")
        self.actual_path.write_text(json.dumps(self.actual), encoding="utf-8")

    def run_compare(self) -> dict:
        self.save()
        return COMPARE.compare(
            self.root,
            self.reference_path,
            self.actual_path,
            self.reference_png,
            self.actual_png,
            Path("diff.json"),
            Path("diff.svg"),
        )

    def test_identical_visual_passes_and_emits_artifacts(self) -> None:
        result = self.run_compare()
        self.assertEqual(result["status"], "PASS")
        self.assertTrue((self.root / "diff.json").is_file())
        self.assertIn("<svg", (self.root / "diff.svg").read_text(encoding="utf-8"))
        self.assertTrue((self.root / "visual-diff.png").is_file())

    def test_one_pixel_geometry_delta_is_allowed(self) -> None:
        self.actual["rectangles"][1]["width"] = 9
        self.actual["rectangles"][2]["x"] = 9
        self.actual["rectangles"][2]["width"] = 11
        self.assertEqual(self.run_compare()["structural"]["status"], "PASS")

    def test_more_than_one_pixel_geometry_delta_fails(self) -> None:
        self.actual["rectangles"][1]["width"] = 8
        self.actual["rectangles"][2]["x"] = 8
        self.actual["rectangles"][2]["width"] = 12
        result = self.run_compare()
        self.assertEqual(result["status"], "FAIL")
        self.assertTrue(any("1px" in error for error in result["structural"]["errors"]))

    def test_unexplained_extra_rectangle_fails(self) -> None:
        self.actual["rectangles"].append(
            {
                "id": "extra",
                "x": 0,
                "y": 0,
                "width": 1,
                "height": 1,
                "role": "ghost",
                "parent": "left",
            }
        )
        result = self.run_compare()
        self.assertEqual(result["status"], "FAIL")
        self.assertTrue(
            any(
                "unexplained rectangles" in error
                for error in result["structural"]["errors"]
            )
        )

    def test_overlap_is_contract_error(self) -> None:
        self.actual["rectangles"][2]["x"] = 9
        self.save()
        with self.assertRaisesRegex(COMPARE.ContractError, "overlap"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_clipped_rectangle_is_contract_error(self) -> None:
        self.actual["rectangles"][2]["width"] = 11
        with self.assertRaisesRegex(COMPARE.ContractError, "out of bounds"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_child_escaping_parent_is_contract_error(self) -> None:
        self.actual["rectangles"].append(
            {
                "id": "escaped",
                "x": 9,
                "y": 0,
                "width": 2,
                "height": 2,
                "role": "button",
                "parent": "left",
            }
        )
        with self.assertRaisesRegex(COMPARE.ContractError, "parent bounds"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_invalid_hitbox_is_contract_error(self) -> None:
        self.actual["rectangles"][1]["width"] = 0.5
        with self.assertRaisesRegex(COMPARE.ContractError, "hitbox"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_mask_escape_is_contract_error(self) -> None:
        self.actual["masks"] = [
            {
                "id": "bad",
                "reason": "test",
                "provenance": "test",
                "x": 19,
                "y": 19,
                "width": 2,
                "height": 2,
            }
        ]
        with self.assertRaisesRegex(COMPARE.ContractError, "escapes viewport"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_masks_must_match_exactly(self) -> None:
        self.actual["masks"] = [
            {
                "id": "clock",
                "reason": "nondeterministic clock",
                "provenance": "fixture-v1",
                "x": 1,
                "y": 0,
                "width": 1,
                "height": 1,
            }
        ]
        result = self.run_compare()
        self.assertEqual(result["status"], "FAIL")
        self.assertIn(
            "masks differ; masks must match exactly", result["structural"]["errors"]
        )

    def test_scale_mismatch_fails(self) -> None:
        self.actual["viewport"]["scale"] = 1.25
        self.assertEqual(self.run_compare()["status"], "FAIL")

    def test_refresh_mismatch_fails(self) -> None:
        self.actual["viewport"]["refresh_hz"] = 59.94
        self.assertEqual(self.run_compare()["status"], "FAIL")

    def test_output_mismatch_fails(self) -> None:
        self.actual["viewport"]["output"] = "HDMI-1"
        self.assertEqual(self.run_compare()["status"], "FAIL")

    def test_mask_requires_reason_and_provenance(self) -> None:
        del self.actual["masks"][0]["reason"]
        with self.assertRaisesRegex(COMPARE.ContractError, "provenance and geometry"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_mask_area_is_bounded(self) -> None:
        mask = self.actual["masks"][0]
        mask.update({"width": 5, "height": 5})
        with self.assertRaisesRegex(COMPARE.ContractError, "5%"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_masks_cannot_overlap(self) -> None:
        self.actual["masks"].append(
            {
                "id": "other",
                "reason": "test",
                "provenance": "fixture-v1",
                "x": 0,
                "y": 0,
                "width": 1,
                "height": 1,
            }
        )
        with self.assertRaisesRegex(COMPARE.ContractError, "overlap"):
            COMPARE.compare_structure(self.reference, self.actual)

    def test_png_dimensions_must_match_viewport_scale(self) -> None:
        pixels = bytes(10 * 10 * 3)
        write_png(self.reference_png, 10, 10, pixels)
        write_png(self.actual_png, 10, 10, pixels)
        self.reference["screenshot_sha256"] = digest(self.reference_png)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        self.assertEqual(self.run_compare()["status"], "FAIL")

    def test_pixel_delta_at_half_percent_passes(self) -> None:
        self.reference["masks"] = []
        self.actual["masks"] = []
        pixels = bytearray(20 * 20 * 3)
        for pixel in (1, 2):
            pixels[pixel * 3] = 255
        write_png(self.actual_png, 20, 20, bytes(pixels))
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        result = self.run_compare()
        self.assertEqual(result["status"], "PASS")

    def test_pixel_delta_above_half_percent_fails(self) -> None:
        pixels = bytearray(20 * 20 * 3)
        for pixel in (1, 2, 3):
            pixels[pixel * 3] = 255
        write_png(self.actual_png, 20, 20, bytes(pixels))
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        result = self.run_compare()
        self.assertEqual(result["status"], "FAIL")
        self.assertGreater(result["pixel"]["mismatch_ratio"], 0.005)

    def test_logical_masks_scale_to_only_the_intended_physical_pixels(self) -> None:
        for scale in (1.25, 1.5, 2.0):
            with self.subTest(scale=scale):
                dimension = round(20 * scale)
                start = math.floor(scale)
                end = math.ceil(3 * scale)
                mask = {
                    "id": "clock",
                    "reason": "fixture",
                    "provenance": "fixture-v1",
                    "x": 1,
                    "y": 1,
                    "width": 2,
                    "height": 2,
                }
                self.reference["viewport"]["scale"] = scale
                self.actual["viewport"]["scale"] = scale
                self.reference["masks"] = [mask]
                self.actual["masks"] = [mask]
                reference = bytes(dimension * dimension * 3)
                actual = bytearray(reference)
                for y in range(start, end):
                    for x in range(start, end):
                        actual[(y * dimension + x) * 3] = 255
                write_png(self.reference_png, dimension, dimension, reference)
                write_png(self.actual_png, dimension, dimension, bytes(actual))
                self.reference["screenshot_sha256"] = digest(self.reference_png)
                self.actual["screenshot_sha256"] = digest(self.actual_png)
                result = self.run_compare()
                masked_pixels = (end - start) ** 2
                self.assertEqual(result["status"], "PASS")
                self.assertEqual(result["pixel"]["masked_pixels"], masked_pixels)
                self.assertEqual(
                    result["pixel"]["compared_pixels"], dimension**2 - masked_pixels
                )
                self.assertEqual(result["pixel"]["mismatched_pixels"], 0)

                actual[(start * dimension + end) * 3] = 255
                write_png(self.actual_png, dimension, dimension, bytes(actual))
                self.actual["screenshot_sha256"] = digest(self.actual_png)
                self.assertEqual(self.run_compare()["pixel"]["mismatched_pixels"], 1)

    def test_adjacent_logical_masks_count_physical_union_once(self) -> None:
        scale = 1.25
        dimension = round(20 * scale)
        masks = [
            {
                "id": "left-mask",
                "reason": "fixture",
                "provenance": "fixture-v1",
                "x": 0,
                "y": 0,
                "width": 1,
                "height": 1,
            },
            {
                "id": "right-mask",
                "reason": "fixture",
                "provenance": "fixture-v1",
                "x": 1,
                "y": 0,
                "width": 1,
                "height": 1,
            },
        ]
        for document in (self.reference, self.actual):
            document["viewport"]["scale"] = scale
            document["masks"] = masks
        pixels = bytes(dimension * dimension * 3)
        write_png(self.reference_png, dimension, dimension, pixels)
        write_png(self.actual_png, dimension, dimension, pixels)
        self.reference["screenshot_sha256"] = digest(self.reference_png)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        result = self.run_compare()
        self.assertEqual(result["pixel"]["masked_pixels"], 6)
        self.assertEqual(
            result["pixel"]["compared_pixels"] + result["pixel"]["masked_pixels"],
            dimension**2,
        )

    def test_unsupported_png_is_blocked_not_pass(self) -> None:
        write_png(self.reference_png, 20, 20, bytes(20 * 20), color_type=3)
        write_png(self.actual_png, 20, 20, bytes(20 * 20), color_type=3)
        self.reference["screenshot_sha256"] = digest(self.reference_png)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        result = self.run_compare()
        self.assertEqual(result["status"], "BLOCKED")
        self.assertEqual(result["structural"]["status"], "PASS")

    def test_missing_screenshot_hash_fails_closed(self) -> None:
        del self.actual["screenshot_sha256"]
        with self.assertRaisesRegex(COMPARE.ContractError, "lacks screenshot_sha256"):
            self.run_compare()

    def test_forged_screenshot_hash_mismatch_fails(self) -> None:
        self.actual["screenshot_sha256"] = "0" * 64
        with self.assertRaisesRegex(COMPARE.ContractError, "hash mismatch"):
            self.run_compare()

    def test_output_path_escape_fails(self) -> None:
        self.save()
        with self.assertRaisesRegex(COMPARE.ContractError, "escapes"):
            COMPARE.compare(
                self.root,
                self.reference_path,
                self.actual_path,
                self.reference_png,
                self.actual_png,
                Path("../diff.json"),
                Path("diff.svg"),
            )

    def test_output_cannot_alias_reference_metadata(self) -> None:
        self.save()
        with self.assertRaisesRegex(COMPARE.ContractError, "path alias"):
            COMPARE.compare(
                self.root,
                self.reference_path,
                self.actual_path,
                self.reference_png,
                self.actual_png,
                self.reference_path,
                Path("diff.svg"),
            )

    def test_symlink_output_cannot_alias_actual_png(self) -> None:
        self.save()
        alias = self.root / "alias.png"
        alias.symlink_to(self.actual_png)
        with self.assertRaisesRegex(COMPARE.ContractError, "path alias"):
            COMPARE.compare(
                self.root,
                self.reference_path,
                self.actual_path,
                self.reference_png,
                self.actual_png,
                Path("diff.json"),
                Path("diff.svg"),
                alias,
            )

    def test_malformed_png_is_fail_not_blocked(self) -> None:
        data = bytearray(self.actual_png.read_bytes())
        data[-1] ^= 1
        self.actual_png.write_bytes(data)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        self.assertEqual(self.run_compare()["status"], "FAIL")

    def test_color_semantic_chunks_are_typed_blocked(self) -> None:
        semantic_chunks = (
            png_chunk(b"tRNS", b"\0\0\0\0\0\0"),
            png_chunk(b"gAMA", struct.pack(">I", 45455)),
            png_chunk(b"cHRM", bytes(32)),
            png_chunk(b"sRGB", b"\0"),
            png_chunk(b"iCCP", b"profile\0\0compressed"),
        )
        original = self.actual_png.read_bytes()
        insert = 8 + 12 + 13
        for semantic_chunk in semantic_chunks:
            with self.subTest(kind=semantic_chunk[4:8]):
                self.actual_png.write_bytes(
                    original[:insert] + semantic_chunk + original[insert:]
                )
                self.actual["screenshot_sha256"] = digest(self.actual_png)
                self.assertEqual(self.run_compare()["status"], "BLOCKED")

    def test_unknown_critical_chunk_is_typed_blocked(self) -> None:
        data = self.actual_png.read_bytes()
        insert = 8 + 12 + 13
        data = data[:insert] + png_chunk(b"ABCD", b"") + data[insert:]
        self.actual_png.write_bytes(data)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        self.assertEqual(self.run_compare()["status"], "BLOCKED")

    def test_transparent_vs_opaque_rgba_never_passes(self) -> None:
        transparent = bytes([0, 0, 0, 0]) * (20 * 20)
        opaque = bytes([0, 0, 0, 255]) * (20 * 20)
        write_png(self.reference_png, 20, 20, transparent, color_type=6)
        write_png(self.actual_png, 20, 20, opaque, color_type=6)
        self.reference["screenshot_sha256"] = digest(self.reference_png)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        self.assertEqual(self.run_compare()["status"], "FAIL")

    def test_semantic_role_change_fails(self) -> None:
        self.actual = deepcopy(self.reference)
        self.actual["screenshot_sha256"] = digest(self.actual_png)
        self.actual["rectangles"][1]["role"] = "label"
        result = self.run_compare()
        self.assertEqual(result["status"], "FAIL")
        self.assertTrue(
            any(
                "semantic attributes" in error
                for error in result["structural"]["errors"]
            )
        )


if __name__ == "__main__":
    unittest.main()
