#!/usr/bin/env python3
"""Create deterministic chat replay fixture artifacts."""

from __future__ import annotations

import argparse
import json
import random
from collections import Counter
from pathlib import Path
from typing import Any

from benchlib import append_notes, ensure_object, initialize_run_dir, load_json, slug, utc_stamp, write_json


DEFAULT_MESSAGE_COUNTS = {
    "normal": 600,
    "high-volume": 6000,
    "burst": 2000,
    "reconnect": 1200,
}
COLORS = ["#1f77b4", "#2ca02c", "#d62728", "#9467bd", "#8c564b", "#e377c2", "#17becf"]
VARIANTS = [
    "plain",
    "mention",
    "twitch-emote",
    "third-party-emote",
    "badge-heavy",
    "link",
    "long-message",
    "reply-like",
]


def message_timestamp_ms(
    scenario: str,
    index: int,
    total_messages: int,
    duration_ms: int,
    burst_size: int,
    burst_window_ms: int,
) -> int:
    if total_messages <= 1:
        return 0

    if scenario == "burst":
        active_burst_size = min(burst_size, total_messages)
        if index < active_burst_size:
            divisor = max(1, active_burst_size - 1)
            return int((index / divisor) * burst_window_ms)

        remaining = total_messages - active_burst_size
        divisor = max(1, remaining - 1)
        remaining_duration = max(1, duration_ms - burst_window_ms)
        return int(burst_window_ms + (((index - active_burst_size) / divisor) * remaining_duration))

    divisor = max(1, total_messages - 1)
    return int((index / divisor) * duration_ms)


def control_event(kind: str, timestamp_ms: int, channel: str, detail: str) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "sequence": 0,
        "kind": kind,
        "timestamp_ms": timestamp_ms,
        "channel": channel,
        "detail": detail,
    }


def message_text(variant: str, index: int) -> str:
    if variant == "plain":
        return f"deterministic chat message {index}"
    if variant == "mention":
        return f"@BenchUser0001 deterministic mention payload {index}"
    if variant == "twitch-emote":
        return f"Kappa deterministic emote payload {index}"
    if variant == "third-party-emote":
        return f"OMEGALUL 7TVWide FFZCool payload {index}"
    if variant == "badge-heavy":
        return f"subscriber badge-heavy message {index}"
    if variant == "link":
        return f"link payload https://example.invalid/echo-bench/{index}"
    if variant == "long-message":
        return f"long payload {index} " + ("abcdefghijklmnopqrstuvwxyz " * 8).strip()
    return f"reply-style payload {index} responding to echo-bench-{max(0, index - 1):08d}"


def message_event(index: int, timestamp_ms: int, channel: str, user_count: int, rng: random.Random) -> dict[str, Any]:
    user_index = rng.randrange(user_count)
    login = f"benchuser{user_index:04d}"
    display_name = f"BenchUser{user_index:04d}"
    user_id = 900000 + user_index
    message_id = f"echo-bench-{index:08d}"
    color = rng.choice(COLORS)
    variant = VARIANTS[index % len(VARIANTS)]
    text = message_text(variant, index)
    badges = "moderator/1,subscriber/24,founder/0" if variant == "badge-heavy" else "subscriber/12"
    emotes = "25:0-4" if variant == "twitch-emote" else ""
    channel_name = channel.removeprefix("#")
    irc_line = (
        f"@badge-info=;badges={badges};color={color};display-name={display_name};"
        f"emotes={emotes};flags=;id={message_id};mod=0;room-id=123456;"
        f"tmi-sent-ts={timestamp_ms};turbo=0;user-id={user_id};user-type= "
        f":{login}!{login}@{login}.tmi.twitch.tv PRIVMSG {channel} :{text}"
    )
    return {
        "schema_version": 1,
        "sequence": 0,
        "kind": "message",
        "timestamp_ms": timestamp_ms,
        "channel": channel,
        "source": "twitch-irc-fixture",
        "login": login,
        "display_name": display_name,
        "message": {
            "id": message_id,
            "variant": variant,
            "text": text,
        },
        "tags": {
            "badges": badges,
            "color": color,
            "display_name": display_name,
            "emotes": emotes,
            "room_id": "123456",
            "tmi_sent_ts": timestamp_ms,
            "user_id": str(user_id),
        },
        "irc": {
            "channel": channel_name,
            "line": irc_line,
        },
    }


def write_ndjson(path: Path, events: list[dict[str, Any]]) -> None:
    with path.open("w", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event, separators=(",", ":")) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--replay-scenario", choices=["normal", "high-volume", "burst", "reconnect"], default="normal")
    parser.add_argument("--message-count", type=int, default=0)
    parser.add_argument("--duration-seconds", type=int, default=60)
    parser.add_argument("--user-count", type=int, default=50)
    parser.add_argument("--seed", type=int, default=1337)
    parser.add_argument("--burst-size", type=int, default=250)
    parser.add_argument("--burst-window-ms", type=int, default=1000)
    parser.add_argument("--channel", default="#echo-bench")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--output-root", default="")
    parser.add_argument("--benchmark-scenario", default="chat replay fixture")
    args = parser.parse_args()

    if args.message_count < 0 or args.message_count > 1_000_000:
        raise SystemExit("--message-count must be in range 0..1000000.")
    if args.duration_seconds < 1 or args.duration_seconds > 86_400:
        raise SystemExit("--duration-seconds must be in range 1..86400.")
    if args.user_count < 1 or args.user_count > 10_000:
        raise SystemExit("--user-count must be in range 1..10000.")
    if args.seed < 1:
        raise SystemExit("--seed must be positive.")
    if args.burst_size < 1 or args.burst_size > 1_000_000:
        raise SystemExit("--burst-size must be in range 1..1000000.")
    if args.burst_window_ms < 1 or args.burst_window_ms > 600_000:
        raise SystemExit("--burst-window-ms must be in range 1..600000.")

    run_dir = initialize_run_dir(
        run_dir=args.run_dir,
        output_root=args.output_root,
        scenario=args.benchmark_scenario,
        fixture_name=f"chat-replay-{args.replay_scenario}",
        notes=f"Deterministic chat replay fixture for '{args.replay_scenario}'. This is input data, not performance evidence.",
    )

    channel = args.channel if args.channel.startswith("#") else f"#{args.channel}"
    duration_ms = args.duration_seconds * 1000
    message_count = args.message_count if args.message_count > 0 else DEFAULT_MESSAGE_COUNTS[args.replay_scenario]
    rng = random.Random(args.seed)
    events: list[dict[str, Any]] = []

    for index in range(message_count):
        timestamp_ms = message_timestamp_ms(
            args.replay_scenario,
            index,
            message_count,
            duration_ms,
            args.burst_size,
            args.burst_window_ms,
        )
        events.append(message_event(index, timestamp_ms, channel, args.user_count, rng))

    if args.replay_scenario == "reconnect":
        disconnect_at = int(duration_ms * 0.40)
        attempt_at = min(duration_ms, disconnect_at + 1000)
        complete_at = min(duration_ms, disconnect_at + 2500)
        events.append(control_event("disconnect", disconnect_at, channel, "simulated transport disconnect"))
        events.append(control_event("reconnect-attempt", attempt_at, channel, "simulated reconnect attempt"))
        events.append(control_event("reconnect-complete", complete_at, channel, "simulated reconnect completion"))

    events.sort(key=lambda event: (int(event["timestamp_ms"]), str(event["kind"])))
    for sequence, event in enumerate(events):
        event["sequence"] = sequence

    artifact_slug = slug(f"chat-replay-{args.replay_scenario}")
    replay_path = run_dir / f"{artifact_slug}.ndjson"
    summary_path = run_dir / f"{artifact_slug}.summary.json"
    manifest_path = run_dir / f"{artifact_slug}.manifest.json"
    write_ndjson(replay_path, events)

    message_events = [event for event in events if event["kind"] == "message"]
    control_events = [event for event in events if event["kind"] != "message"]
    variant_counts = Counter(event["message"]["variant"] for event in message_events)
    fixture_summary = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "replay_scenario": args.replay_scenario,
        "channel": channel,
        "seed": args.seed,
        "duration_seconds": args.duration_seconds,
        "requested_message_count": args.message_count,
        "message_count": len(message_events),
        "control_event_count": len(control_events),
        "total_event_count": len(events),
        "user_count": args.user_count,
        "first_timestamp_ms": events[0]["timestamp_ms"] if events else None,
        "last_timestamp_ms": events[-1]["timestamp_ms"] if events else None,
        "average_message_rate_per_second": round(len(message_events) / float(args.duration_seconds), 3),
        "burst_size": min(args.burst_size, len(message_events)) if args.replay_scenario == "burst" else None,
        "burst_window_ms": args.burst_window_ms if args.replay_scenario == "burst" else None,
        "variants": [{"variant": key, "count": variant_counts[key]} for key in sorted(variant_counts)],
        "decision": "chat replay fixture recorded; not a benchmark comparison",
    }
    write_json(summary_path, fixture_summary)

    manifest = {
        "schema_version": 1,
        "created_at_utc": utc_stamp(),
        "replay_path": str(replay_path),
        "summary_path": str(summary_path),
        "replay_scenario": args.replay_scenario,
        "channel": channel,
        "seed": args.seed,
        "decision": "chat replay fixture recorded; not measured",
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
    raw_artifacts["chat_replay"] = str(replay_path)
    raw_artifacts["chat_replay_manifest"] = str(manifest_path)
    raw_artifacts["chat_replay_summary"] = str(summary_path)
    run_summary["chat_replay"] = fixture_summary
    run_summary["decision"] = "chat replay fixture recorded; not measured"
    write_json(run_summary_path, run_summary)

    append_notes(
        run_dir,
        "Chat replay fixture",
        [
            f"- Replay artifact: {replay_path}",
            f"- Manifest artifact: {manifest_path}",
            f"- Scenario: {args.replay_scenario}",
            f"- Messages: {len(message_events)}",
            f"- Control events: {len(control_events)}",
            "- Decision: chat replay fixture recorded; not measured",
        ],
    )

    print(manifest_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
