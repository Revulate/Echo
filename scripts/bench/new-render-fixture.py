#!/usr/bin/env python3
"""Create deterministic render fixture artifacts."""

from __future__ import annotations

import argparse
import json
import random
from collections import Counter
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, initialize_run_dir, load_json, slug, utc_stamp, write_json


FIXTURE_KINDS = (
    "static-emotes",
    "animated-emotes",
    "mixed-emotes",
    "large-scrollback",
    "rapid-scrolling",
    "multi-split",
)
DEFAULT_MESSAGE_COUNTS = {
    "static-emotes": 500,
    "animated-emotes": 500,
    "mixed-emotes": 750,
    "large-scrollback": 20_000,
    "rapid-scrolling": 8_000,
    "multi-split": 2_000,
}
COLORS = ["#1f77b4", "#2ca02c", "#d62728", "#9467bd", "#17becf"]


def emote_token(code: str, provider: str, animated: bool, *, wide: bool = False, scale: int = 2) -> dict[str, Any]:
    return {
        "kind": "emote",
        "code": code,
        "provider": provider,
        "animated": animated,
        "wide": wide,
        "preferred_scale": scale,
    }


def emote_pool(kind: str) -> list[dict[str, Any]]:
    static = [
        emote_token("Kappa", "twitch", False, scale=4),
        emote_token("PogChamp", "twitch", False, scale=4),
        emote_token("LUL", "twitch", False, scale=4),
        emote_token("BibleThump", "twitch", False, scale=4),
    ]
    animated = [
        emote_token("peepoDance", "7tv", True),
        emote_token("catJAM", "bttv", True),
        emote_token("HYPERCLAP", "ffz", True),
        emote_token("PETTHEMODS", "7tv", True),
    ]
    mixed = [
        emote_token("Kappa", "twitch", False, scale=4),
        emote_token("OMEGALUL", "bttv", False),
        emote_token("FeelsGoodMan", "ffz", False),
        emote_token("WidePeepoHappy", "7tv", False, wide=True),
        emote_token("peepoDance", "7tv", True),
        emote_token("catJAM", "bttv", True),
    ]
    if kind == "static-emotes":
        return static
    if kind == "animated-emotes":
        return animated
    return mixed


def render_message(
    index: int,
    kind: str,
    split_count: int,
    pool: list[dict[str, Any]],
    rng: random.Random,
) -> dict[str, Any]:
    channel_index = index % split_count if kind == "multi-split" else 0
    channel = f"#echo-render-{channel_index:02d}"
    display_name = f"RenderUser{rng.randrange(250):04d}"
    long_tail = ""
    if kind in {"large-scrollback", "rapid-scrolling"}:
        long_tail = " " + ("scrollback payload " * ((index % 5) + 1)).strip()

    tokens: list[dict[str, Any]] = [
        {
            "kind": "text",
            "value": f"message {index} from {display_name}",
        },
        dict(pool[index % len(pool)]),
        {
            "kind": "text",
            "value": f"render fixture{long_tail}",
        },
    ]

    if kind in {"animated-emotes", "mixed-emotes", "static-emotes"}:
        tokens.append(dict(pool[(index + 3) % len(pool)]))
        tokens.append(
            {
                "kind": "mention",
                "value": "@RenderUser0001",
            }
        )

    if index % 17 == 0:
        tokens.append(
            {
                "kind": "link",
                "value": f"https://example.invalid/render-fixture/{index}",
            }
        )

    return {
        "schema_version": 1,
        "sequence": index,
        "kind": "render-message",
        "fixture_kind": kind,
        "timestamp_ms": index * 100,
        "channel": channel,
        "split_index": channel_index,
        "id": f"render-fixture-{index:08d}",
        "login": f"renderuser{rng.randrange(250):04d}",
        "display_name": display_name,
        "badges": ["moderator/1", "subscriber/24"] if index % 11 == 0 else ["subscriber/12"],
        "color": COLORS[index % len(COLORS)],
        "tokens": tokens,
        "expected": {
            "contains_animated_emote": any(token.get("kind") == "emote" and token.get("animated") for token in tokens),
            "contains_wide_emote": any(token.get("kind") == "emote" and token.get("wide") for token in tokens),
            "contains_link": any(token.get("kind") == "link" for token in tokens),
        },
    }


def write_ndjson(path: Path, events: list[dict[str, Any]]) -> None:
    with path.open("w", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event, separators=(",", ":")) + "\n")


def provider_summary(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_provider: dict[str, Counter[str]] = {}
    for message in messages:
        for token in message["tokens"]:
            if token.get("kind") != "emote":
                continue
            provider = str(token.get("provider", ""))
            counts = by_provider.setdefault(provider, Counter())
            counts["emote_token_count"] += 1
            if token.get("animated"):
                counts["animated_count"] += 1
            if token.get("wide"):
                counts["wide_count"] += 1

    return [
        {
            "provider": provider,
            "emote_token_count": counts["emote_token_count"],
            "animated_count": counts["animated_count"],
            "wide_count": counts["wide_count"],
        }
        for provider, counts in sorted(by_provider.items())
    ]


def manifest_layout(kind: str, split_count: int, viewport_width: int, viewport_height: int) -> dict[str, Any]:
    channel_count = split_count if kind == "multi-split" else 1
    return {
        "split_count": channel_count,
        "viewport": {
            "width": viewport_width,
            "height": viewport_height,
        },
        "channels": [
            {
                "split_index": split,
                "channel": f"#echo-render-{split:02d}",
            }
            for split in range(channel_count)
        ],
    }


def manifest_actions(kind: str) -> list[dict[str, Any]]:
    if kind != "rapid-scrolling":
        return []
    return [
        {"at_ms": 0, "kind": "scroll-to-bottom", "target": "all"},
        {"at_ms": 250, "kind": "wheel", "delta_y": -2400, "repeat": 20},
        {"at_ms": 1250, "kind": "wheel", "delta_y": 2400, "repeat": 20},
        {"at_ms": 2250, "kind": "scroll-to-bottom", "target": "all"},
    ]


def positive_range(value: int, name: str, minimum: int, maximum: int) -> None:
    if value < minimum or value > maximum:
        raise SystemExit(f"{name} must be in range {minimum}..{maximum}.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-kind", choices=FIXTURE_KINDS, default="static-emotes")
    parser.add_argument("--message-count", type=int, default=0)
    parser.add_argument("--split-count", type=int, default=4)
    parser.add_argument("--viewport-width", type=int, default=1280)
    parser.add_argument("--viewport-height", type=int, default=720)
    parser.add_argument("--seed", type=int, default=4242)
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--benchmark-scenario", default="render fixture")
    args = parser.parse_args()

    positive_range(args.message_count, "--message-count", 0, 1_000_000)
    positive_range(args.split_count, "--split-count", 1, 32)
    positive_range(args.viewport_width, "--viewport-width", 320, 16_384)
    positive_range(args.viewport_height, "--viewport-height", 240, 16_384)
    positive_range(args.seed, "--seed", 1, 2_147_483_647)

    run_dir = initialize_run_dir(
        run_dir=args.run_dir,
        output_root=args.output_root,
        scenario=args.benchmark_scenario,
        fixture_name=f"render-{args.fixture_kind}",
        notes=f"Deterministic render fixture for '{args.fixture_kind}'. This is input data, not frame-time evidence.",
    )

    message_count = args.message_count if args.message_count > 0 else DEFAULT_MESSAGE_COUNTS[args.fixture_kind]
    rng = random.Random(args.seed)
    pool = emote_pool(args.fixture_kind)
    messages = [render_message(index, args.fixture_kind, args.split_count, pool, rng) for index in range(message_count)]

    artifact_slug = slug(f"render-{args.fixture_kind}")
    messages_path = run_dir / f"{artifact_slug}.messages.ndjson"
    manifest_path = run_dir / f"{artifact_slug}.manifest.json"
    summary_path = run_dir / f"{artifact_slug}.summary.json"
    write_ndjson(messages_path, messages)

    layout = manifest_layout(args.fixture_kind, args.split_count, args.viewport_width, args.viewport_height)
    actions = manifest_actions(args.fixture_kind)
    channel_count = args.split_count if args.fixture_kind == "multi-split" else 1
    fixture_summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "fixture_kind": args.fixture_kind,
        "message_count": message_count,
        "split_count": channel_count,
        "viewport_width": args.viewport_width,
        "viewport_height": args.viewport_height,
        "seed": args.seed,
        "provider_summary": provider_summary(messages),
        "action_count": len(actions),
        "decision": "render fixture recorded; not frame-time evidence",
    }
    write_json(summary_path, fixture_summary)

    manifest = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "fixture_kind": args.fixture_kind,
        "messages_path": str(messages_path),
        "summary_path": str(summary_path),
        "layout": layout,
        "actions": actions,
        "decision": "render fixture recorded; not measured",
    }
    write_json(manifest_path, manifest)

    run_summary_path = run_dir / "summary.json"
    run_summary = load_json(run_summary_path) or {
        "schema_version": 1,
        "scenario": args.benchmark_scenario,
        "status": "initialized",
        "measurements_recorded": False,
        "raw_artifacts": {},
        "decision": "not measured",
    }
    raw_artifacts = ensure_object(run_summary, "raw_artifacts")
    raw_artifacts["render_fixture"] = str(messages_path)
    raw_artifacts["render_fixture_manifest"] = str(manifest_path)
    raw_artifacts["render_fixture_summary"] = str(summary_path)
    run_summary["render_fixture"] = fixture_summary
    run_summary["decision"] = "render fixture recorded; not measured"
    write_json(run_summary_path, run_summary)

    append_notes(
        run_dir,
        "Render fixture",
        [
            f"- Messages artifact: {messages_path}",
            f"- Manifest artifact: {manifest_path}",
            f"- Fixture kind: {args.fixture_kind}",
            f"- Messages: {message_count}",
            f"- Splits: {channel_count}",
            "- Decision: render fixture recorded; not measured",
        ],
    )

    print(manifest_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
