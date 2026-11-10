#!/usr/bin/env python3
"""Compare native screenshot metadata and semantic rectangles fail-closed."""

from __future__ import annotations

import argparse
import binascii
import hashlib
import importlib.util
import json
import math
import struct
import sys
import zlib
from dataclasses import dataclass
from html import escape
from pathlib import Path
from typing import Any, Iterable, Sequence


DRIVER_PATH = Path(__file__).with_name("wayland_native_driver.py")
DRIVER_SPEC = importlib.util.spec_from_file_location(
    "echo_wayland_native_driver", DRIVER_PATH
)
assert DRIVER_SPEC and DRIVER_SPEC.loader
DRIVER = importlib.util.module_from_spec(DRIVER_SPEC)
sys.modules.setdefault(DRIVER_SPEC.name, DRIVER)
DRIVER_SPEC.loader.exec_module(DRIVER)


BLOCKED_EXIT = 3
FAILED_EXIT = 4
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
GEOMETRY_TOLERANCE_PX = 1.0
PIXEL_MISMATCH_LIMIT = 0.005


class ContractError(RuntimeError):
    pass


class PixelBlocked(RuntimeError):
    pass


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def confined(root: Path, candidate: Path, *, must_exist: bool = False) -> Path:
    root = canonical(root)
    candidate = canonical(candidate if candidate.is_absolute() else root / candidate)
    if candidate != root and root not in candidate.parents:
        raise ContractError(f"path escapes artifact root: {candidate}")
    if must_exist and not candidate.is_file():
        raise ContractError(f"missing file: {candidate}")
    return candidate


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ContractError(f"invalid JSON {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ContractError(f"JSON root must be an object: {path}")
    return value


def require_pairwise_distinct(paths: dict[str, Path]) -> None:
    items = list(paths.items())
    for index, (left_name, left) in enumerate(items):
        for right_name, right in items[index + 1 :]:
            same_resolved = left.resolve(strict=False) == right.resolve(strict=False)
            same_file = False
            if left.exists() and right.exists():
                try:
                    same_file = left.samefile(right)
                except OSError:
                    same_file = False
            if same_resolved or same_file:
                raise ContractError(
                    f"path alias rejected: {left_name} and {right_name}"
                )


@dataclass(frozen=True)
class Rect:
    id: str
    x: float
    y: float
    width: float
    height: float
    role: str
    hitbox: bool
    parent: str | None
    allow_overlap: bool

    @property
    def right(self) -> float:
        return self.x + self.width

    @property
    def bottom(self) -> float:
        return self.y + self.height

    def intersects(self, other: "Rect") -> bool:
        return (
            self.x < other.right
            and other.x < self.right
            and self.y < other.bottom
            and other.y < self.bottom
        )


def finite_number(value: Any, label: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise ContractError(f"{label} must be numeric")
    result = float(value)
    if result != result or result in {float("inf"), float("-inf")}:
        raise ContractError(f"{label} must be finite")
    return result


def parse_rects(document: dict[str, Any], label: str) -> tuple[int, int, list[Rect]]:
    viewport = document.get("viewport")
    if not isinstance(viewport, dict):
        raise ContractError(f"{label} lacks viewport")
    width = finite_number(viewport.get("width"), f"{label}.viewport.width")
    height = finite_number(viewport.get("height"), f"{label}.viewport.height")
    finite_number(viewport.get("x"), f"{label}.viewport.x")
    finite_number(viewport.get("y"), f"{label}.viewport.y")
    if width <= 0 or height <= 0 or not width.is_integer() or not height.is_integer():
        raise ContractError(f"{label} viewport must use positive integer pixels")
    scale = finite_number(viewport.get("scale"), f"{label}.viewport.scale")
    refresh_hz = finite_number(
        viewport.get("refresh_hz"), f"{label}.viewport.refresh_hz"
    )
    if scale <= 0 or refresh_hz <= 0:
        raise ContractError(f"{label} viewport scale and refresh must be positive")
    if not isinstance(viewport.get("output"), str) or not viewport["output"]:
        raise ContractError(f"{label} viewport output identity is required")
    raw = document.get("rectangles")
    if not isinstance(raw, list):
        raise ContractError(f"{label}.rectangles must be a list")
    rects: list[Rect] = []
    seen: set[str] = set()
    for index, item in enumerate(raw):
        if not isinstance(item, dict):
            raise ContractError(f"{label}.rectangles[{index}] must be an object")
        rect_id = item.get("id")
        role = item.get("role")
        if not isinstance(rect_id, str) or not rect_id or rect_id in seen:
            raise ContractError(f"{label} has missing or duplicate rectangle id")
        if not isinstance(role, str) or not role:
            raise ContractError(f"{label}.{rect_id} lacks role")
        seen.add(rect_id)
        rect = Rect(
            id=rect_id,
            x=finite_number(item.get("x"), f"{label}.{rect_id}.x"),
            y=finite_number(item.get("y"), f"{label}.{rect_id}.y"),
            width=finite_number(item.get("width"), f"{label}.{rect_id}.width"),
            height=finite_number(item.get("height"), f"{label}.{rect_id}.height"),
            role=role,
            hitbox=item.get("hitbox", False),
            parent=item.get("parent"),
            allow_overlap=item.get("allow_overlap", False),
        )
        if not isinstance(rect.hitbox, bool) or not isinstance(
            rect.allow_overlap, bool
        ):
            raise ContractError(f"{label}.{rect_id} boolean fields are invalid")
        if rect.parent is not None and not isinstance(rect.parent, str):
            raise ContractError(f"{label}.{rect_id}.parent is invalid")
        if rect.width <= 0 or rect.height <= 0:
            raise ContractError(f"{label}.{rect_id} has non-positive dimensions")
        if rect.x < 0 or rect.y < 0 or rect.right > width or rect.bottom > height:
            raise ContractError(f"{label}.{rect_id} is clipped or out of bounds")
        if rect.hitbox and (rect.width < 1 or rect.height < 1):
            raise ContractError(f"{label}.{rect_id} hitbox is smaller than one pixel")
        rects.append(rect)
    by_id = {rect.id: rect for rect in rects}
    for rect in rects:
        if rect.parent is not None:
            parent = by_id.get(rect.parent)
            if parent is None:
                raise ContractError(f"{label}.{rect.id} refers to missing parent")
            if (
                rect.x < parent.x
                or rect.y < parent.y
                or rect.right > parent.right
                or rect.bottom > parent.bottom
            ):
                raise ContractError(f"{label}.{rect.id} escapes parent bounds")
    siblings: dict[str | None, list[Rect]] = {}
    for rect in rects:
        siblings.setdefault(rect.parent, []).append(rect)
    for group in siblings.values():
        for index, left in enumerate(group):
            for right in group[index + 1 :]:
                if left.intersects(right) and not (
                    left.allow_overlap and right.allow_overlap
                ):
                    raise ContractError(
                        f"{label} unexplained overlap: {left.id} and {right.id}"
                    )
    return int(width), int(height), rects


def parse_masks(
    document: dict[str, Any], width: int, height: int, label: str
) -> list[dict[str, Any]]:
    raw = document.get("masks", [])
    if not isinstance(raw, list):
        raise ContractError(f"{label}.masks must be a list")
    masks: list[dict[str, Any]] = []
    seen_ids: set[str] = set()
    for index, item in enumerate(raw):
        required_fields = {"id", "reason", "provenance", "x", "y", "width", "height"}
        if not isinstance(item, dict) or set(item) != required_fields:
            raise ContractError(
                f"{label}.masks[{index}] must have exact provenance and geometry fields"
            )
        if not isinstance(item["id"], str) or not item["id"] or item["id"] in seen_ids:
            raise ContractError(f"{label}.masks[{index}] has invalid id")
        if not isinstance(item["reason"], str) or not item["reason"].strip():
            raise ContractError(f"{label}.masks[{index}] lacks reason")
        if not isinstance(item["provenance"], str) or not item["provenance"].strip():
            raise ContractError(f"{label}.masks[{index}] lacks provenance")
        seen_ids.add(item["id"])
        values = tuple(
            finite_number(item[key], f"{label}.masks[{index}].{key}")
            for key in ("x", "y", "width", "height")
        )
        if any(not value.is_integer() for value in values):
            raise ContractError(f"{label}.masks[{index}] must use integer pixels")
        x, y, mask_width, mask_height = (int(value) for value in values)
        if (
            x < 0
            or y < 0
            or mask_width <= 0
            or mask_height <= 0
            or x + mask_width > width
            or y + mask_height > height
        ):
            raise ContractError(f"{label}.masks[{index}] escapes viewport")
        candidate = {**item, "x": x, "y": y, "width": mask_width, "height": mask_height}
        for other in masks:
            if (
                x < other["x"] + other["width"]
                and other["x"] < x + mask_width
                and y < other["y"] + other["height"]
                and other["y"] < y + mask_height
            ):
                raise ContractError(f"{label}.masks overlap")
        masks.append(candidate)
    masked_area = sum(mask["width"] * mask["height"] for mask in masks)
    if masked_area / (width * height) > 0.05:
        raise ContractError(f"{label}.masks exceed 5% viewport area")
    return masks


def compare_structure(
    reference: dict[str, Any], actual: dict[str, Any]
) -> dict[str, Any]:
    ref_width, ref_height, ref_rects = parse_rects(reference, "reference")
    act_width, act_height, act_rects = parse_rects(actual, "actual")
    errors: list[str] = []
    if (ref_width, ref_height) != (act_width, act_height):
        errors.append("viewport dimensions differ")
    for field in ("x", "y", "scale", "refresh_hz", "output"):
        if reference["viewport"].get(field) != actual["viewport"].get(field):
            errors.append(f"viewport {field} differs")
    ref_masks = parse_masks(reference, ref_width, ref_height, "reference")
    act_masks = parse_masks(actual, act_width, act_height, "actual")
    if ref_masks != act_masks:
        errors.append("masks differ; masks must match exactly")
    ref_by_id = {rect.id: rect for rect in ref_rects}
    act_by_id = {rect.id: rect for rect in act_rects}
    missing = sorted(ref_by_id.keys() - act_by_id.keys())
    extra = sorted(act_by_id.keys() - ref_by_id.keys())
    if missing:
        errors.append(f"missing rectangles: {', '.join(missing)}")
    if extra:
        errors.append(f"unexplained rectangles: {', '.join(extra)}")
    deltas: list[dict[str, Any]] = []
    for rect_id in sorted(ref_by_id.keys() & act_by_id.keys()):
        ref = ref_by_id[rect_id]
        act = act_by_id[rect_id]
        if (ref.role, ref.hitbox, ref.parent, ref.allow_overlap) != (
            act.role,
            act.hitbox,
            act.parent,
            act.allow_overlap,
        ):
            errors.append(f"{rect_id} semantic attributes differ")
        delta = {
            key: getattr(act, key) - getattr(ref, key)
            for key in ("x", "y", "width", "height")
        }
        deltas.append({"id": rect_id, **delta})
        if any(abs(value) > GEOMETRY_TOLERANCE_PX for value in delta.values()):
            errors.append(
                f"{rect_id} exceeds {GEOMETRY_TOLERANCE_PX:g}px geometry tolerance"
            )
    return {
        "status": "PASS" if not errors else "FAIL",
        "viewport": {"width": ref_width, "height": ref_height},
        "masks": ref_masks,
        "geometry_tolerance_px": GEOMETRY_TOLERANCE_PX,
        "deltas": deltas,
        "errors": errors,
    }


def masked(x: int, y: int, masks: Iterable[tuple[int, int, int, int]]) -> bool:
    return any(
        mx <= x < mx + width and my <= y < my + height
        for mx, my, width, height in masks
    )


def physical_masks(
    masks: list[dict[str, Any]], scale: float, pixel_width: int, pixel_height: int
) -> list[tuple[int, int, int, int]]:
    result: list[tuple[int, int, int, int]] = []
    for mask in masks:
        start_x = max(0, min(pixel_width, math.floor(mask["x"] * scale)))
        start_y = max(0, min(pixel_height, math.floor(mask["y"] * scale)))
        end_x = max(
            start_x, min(pixel_width, math.ceil((mask["x"] + mask["width"]) * scale))
        )
        end_y = max(
            start_y, min(pixel_height, math.ceil((mask["y"] + mask["height"]) * scale))
        )
        result.append((start_x, start_y, end_x - start_x, end_y - start_y))
    return result


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    crc = binascii.crc32(kind + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)


def write_diff_png(path: Path, width: int, height: int, rgba: bytes) -> None:
    rows = b"".join(
        b"\0" + rgba[row * width * 4 : (row + 1) * width * 4] for row in range(height)
    )
    payload = (
        PNG_SIGNATURE
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(rows, level=9))
        + png_chunk(b"IEND", b"")
    )
    path.write_bytes(payload)


def compare_pixels(
    reference_path: Path,
    actual_path: Path,
    masks: list[dict[str, Any]],
    diff_path: Path,
    expected_dimensions: tuple[int, int],
    scale: float,
) -> dict[str, Any]:
    ref_width, ref_height, ref_channels, reference = DRIVER.decode_png_strict(
        reference_path
    )
    act_width, act_height, act_channels, actual = DRIVER.decode_png_strict(actual_path)
    if (ref_width, ref_height, ref_channels) != (act_width, act_height, act_channels):
        return {"status": "FAIL", "errors": ["PNG dimensions or color types differ"]}
    if (ref_width, ref_height) != expected_dimensions:
        return {
            "status": "FAIL",
            "errors": ["PNG dimensions differ from viewport times scale"],
        }
    mask_tuples = physical_masks(masks, scale, ref_width, ref_height)
    masked_pixels = 0
    mismatches = 0
    compared = 0
    diff = bytearray(ref_width * ref_height * 4)
    stride = ref_width * ref_channels
    for y in range(ref_height):
        for x in range(ref_width):
            if masked(x, y, mask_tuples):
                masked_pixels += 1
                continue
            compared += 1
            start = y * stride + x * ref_channels
            if (
                reference[start : start + ref_channels]
                != actual[start : start + act_channels]
            ):
                mismatches += 1
                diff_start = (y * ref_width + x) * 4
                diff[diff_start : diff_start + 4] = b"\xff\x00\x00\xff"
    if compared == 0:
        raise PixelBlocked("masks exclude every pixel")
    ratio = mismatches / compared
    write_diff_png(diff_path, ref_width, ref_height, bytes(diff))
    return {
        "status": "PASS" if ratio <= PIXEL_MISMATCH_LIMIT else "FAIL",
        "mismatched_pixels": mismatches,
        "compared_pixels": compared,
        "masked_pixels": masked_pixels,
        "mismatch_ratio": ratio,
        "mismatch_limit": PIXEL_MISMATCH_LIMIT,
        "reference_sha256": sha256_file(reference_path),
        "actual_sha256": sha256_file(actual_path),
        "diff_png": str(diff_path),
        "diff_png_sha256": sha256_file(diff_path),
        "errors": []
        if ratio <= PIXEL_MISMATCH_LIMIT
        else ["unmasked pixel mismatch exceeds 0.5%"],
    }


def svg_document(structural: dict[str, Any]) -> str:
    viewport = structural["viewport"]
    delta_by_id = {delta["id"]: delta for delta in structural["deltas"]}
    shapes = []
    for delta in delta_by_id.values():
        changed = any(abs(delta[key]) > 0 for key in ("x", "y", "width", "height"))
        color = "#d00" if changed else "#0a0"
        shapes.append(
            f'<text x="8" y="{20 + len(shapes) * 18}" fill="{color}">{escape(delta["id"])}: dx={delta["x"]} dy={delta["y"]} dw={delta["width"]} dh={delta["height"]}</text>'
        )
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{viewport["width"]}" height="{viewport["height"]}" '
        f'viewBox="0 0 {viewport["width"]} {viewport["height"]}">'
        '<rect width="100%" height="100%" fill="#fff"/>' + "".join(shapes) + "</svg>\n"
    )


def compare(
    artifact_root: Path,
    reference_metadata: Path,
    actual_metadata: Path,
    reference_png: Path,
    actual_png: Path,
    output_metadata: Path,
    output_svg: Path,
    output_png: Path | None = None,
) -> dict[str, Any]:
    root = canonical(artifact_root)
    reference_metadata = confined(root, reference_metadata, must_exist=True)
    actual_metadata = confined(root, actual_metadata, must_exist=True)
    reference_png = confined(root, reference_png, must_exist=True)
    actual_png = confined(root, actual_png, must_exist=True)
    output_metadata = confined(root, output_metadata)
    output_svg = confined(root, output_svg)
    output_png = confined(root, output_png or Path("visual-diff.png"))
    require_pairwise_distinct(
        {
            "reference_metadata": reference_metadata,
            "actual_metadata": actual_metadata,
            "reference_png": reference_png,
            "actual_png": actual_png,
            "output_metadata": output_metadata,
            "output_svg": output_svg,
            "output_png": output_png,
        }
    )
    reference_document = load_object(reference_metadata)
    actual_document = load_object(actual_metadata)
    for label, document, png_path in (
        ("reference", reference_document, reference_png),
        ("actual", actual_document, actual_png),
    ):
        expected_hash = document.get("screenshot_sha256")
        if not isinstance(expected_hash, str) or len(expected_hash) != 64:
            raise ContractError(f"{label} metadata lacks screenshot_sha256")
        if expected_hash != sha256_file(png_path):
            raise ContractError(f"{label} screenshot hash mismatch")
    structural = compare_structure(reference_document, actual_document)
    scale = reference_document["viewport"]["scale"]
    expected_dimensions = (
        round(reference_document["viewport"]["width"] * scale),
        round(reference_document["viewport"]["height"] * scale),
    )
    try:
        pixel = compare_pixels(
            reference_png,
            actual_png,
            structural["masks"],
            output_png,
            expected_dimensions,
            scale,
        )
    except DRIVER.UnsupportedPng as exc:
        pixel = {"status": "BLOCKED", "reason": str(exc), "errors": []}
    except (OSError, DRIVER.Failed) as exc:
        pixel = {"status": "FAIL", "reason": str(exc), "errors": [str(exc)]}
    if structural["status"] == "FAIL" or pixel["status"] == "FAIL":
        status = "FAIL"
    elif pixel["status"] == "BLOCKED":
        status = "BLOCKED"
    else:
        status = "PASS"
    result = {
        "schema_version": 1,
        "status": status,
        "contracts": {
            "geometry_tolerance_px": GEOMETRY_TOLERANCE_PX,
            "pixel_mismatch_limit": PIXEL_MISMATCH_LIMIT,
            "masks_exact": True,
            "mask_coordinate_space": "logical",
            "mask_scaling": "floor-start-ceil-end-clip-to-physical-png",
        },
        "structural": structural,
        "pixel": pixel,
        "inputs": {
            "reference_metadata": str(reference_metadata),
            "reference_metadata_sha256": sha256_file(reference_metadata),
            "actual_metadata": str(actual_metadata),
            "actual_metadata_sha256": sha256_file(actual_metadata),
            "reference_png": str(reference_png),
            "actual_png": str(actual_png),
        },
    }
    output_metadata.parent.mkdir(parents=True, exist_ok=True)
    output_svg.parent.mkdir(parents=True, exist_ok=True)
    output_metadata.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    output_svg.write_text(svg_document(structural), encoding="utf-8")
    return result


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--artifact-root", required=True)
    result.add_argument("--reference-metadata", required=True)
    result.add_argument("--actual-metadata", required=True)
    result.add_argument("--reference-png", required=True)
    result.add_argument("--actual-png", required=True)
    result.add_argument("--output-metadata", default="visual-diff.json")
    result.add_argument("--output-svg", default="visual-diff.svg")
    result.add_argument("--output-png", default="visual-diff.png")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    root = Path(args.artifact_root)
    try:
        result = compare(
            root,
            Path(args.reference_metadata),
            Path(args.actual_metadata),
            Path(args.reference_png),
            Path(args.actual_png),
            Path(args.output_metadata),
            Path(args.output_svg),
            Path(args.output_png),
        )
    except ContractError as exc:
        print(
            json.dumps(
                {"schema_version": 1, "status": "FAIL", "reason": str(exc)},
                sort_keys=True,
            )
        )
        return FAILED_EXIT
    print(json.dumps(result, sort_keys=True))
    return (
        0
        if result["status"] == "PASS"
        else BLOCKED_EXIT
        if result["status"] == "BLOCKED"
        else FAILED_EXIT
    )


if __name__ == "__main__":
    sys.exit(main())
