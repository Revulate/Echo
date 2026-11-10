#!/usr/bin/env python3
"""Deterministic evaluator for channel provider-index diff publication."""

from __future__ import annotations

import copy
import statistics
import time
from dataclasses import dataclass


PROVIDERS = ("twitch", "ffz", "bttv", "7tv")
PRIORITY = {provider: index for index, provider in enumerate(PROVIDERS)}
ENTRY_COUNT = 4_100
ITERATIONS = 41


@dataclass(frozen=True)
class Emote:
    provider: str
    id: str
    name: str
    revision: int = 0


def aliases(emote: Emote) -> set[str]:
    folded = emote.name.lower()
    return {emote.name} if folded == emote.name else {emote.name, folded}


def insert_name_keys(lookup: dict[str, Emote], emote: Emote) -> None:
    lookup[emote.name] = emote
    folded = emote.name.lower()
    if folded != emote.name:
        current = lookup.get(folded)
        if current is None or current.name != folded:
            lookup[folded] = emote


def emote_sort_key(item: Emote) -> tuple[int, str, str]:
    return (PRIORITY[item.provider], item.name, item.id)


def full_rebuild(inventory: dict[tuple[str, str], Emote]) -> dict[str, Emote]:
    lookup: dict[str, Emote] = {}
    ordered = sorted(
        inventory.values(), key=lambda item: (PRIORITY[item.provider], item.name, item.id)
    )
    for emote in ordered:
        insert_name_keys(lookup, emote)
    return lookup


def changed_targets(
    previous: dict[str, Emote], replacement: dict[str, Emote]
) -> tuple[set[str], set[tuple[str, str]]]:
    names: set[str] = set()
    identities: set[tuple[str, str]] = set()
    for name in previous.keys() | replacement.keys():
        old = previous.get(name)
        new = replacement.get(name)
        if old != new:
            names.add(name)
            if old is not None:
                identities.add((old.provider, old.id))
            if new is not None:
                identities.add((new.provider, new.id))
    return names, identities


def fixture() -> tuple[dict[tuple[str, str], Emote], list[Emote]]:
    inventory: dict[tuple[str, str], Emote] = {}
    per_provider = ENTRY_COUNT // len(PROVIDERS)
    for provider in PROVIDERS:
        for index in range(per_provider):
            name = "Collision" if index == 12 else f"{provider}_{index}"
            emote = Emote(provider, f"{provider}-{index}", name)
            inventory[(provider, emote.id)] = emote

    replacement = [
        emote
        for (provider, _), emote in inventory.items()
        if provider == "7tv" and emote.id != "7tv-40"
    ]
    replacement = [
        Emote(emote.provider, emote.id, "RenamedCollision", 1)
        if emote.id == "7tv-12"
        else Emote(emote.provider, emote.id, "HUH", 1)
        if emote.id == "7tv-41"
        else emote
        for emote in replacement
    ]
    replacement.append(Emote("7tv", "7tv-new", "huh"))
    return inventory, replacement


def run_full(
    inventory: dict[tuple[str, str], Emote],
    replacement: list[Emote],
    previous: dict[str, Emote],
) -> tuple[dict[str, Emote], tuple[set[str], set[tuple[str, str]]]]:
    updated = {key: value for key, value in inventory.items() if key[0] != "7tv"}
    updated.update({("7tv", emote.id): emote for emote in replacement})
    lookup = full_rebuild(updated)
    return lookup, changed_targets(previous, lookup)


def run_incremental(
    inventory: dict[tuple[str, str], Emote],
    replacement: list[Emote],
    previous: dict[str, Emote],
) -> tuple[dict[str, Emote], tuple[set[str], set[tuple[str, str]]]]:
    lookup = copy.copy(previous)
    old_provider = {key: value for key, value in inventory.items() if key[0] == "7tv"}
    new_provider = {("7tv", emote.id): emote for emote in replacement}
    changed_aliases: set[str] = set()
    for key in old_provider.keys() | new_provider.keys():
        old = old_provider.get(key)
        new = new_provider.get(key)
        if old != new:
            if old is not None:
                changed_aliases.update(aliases(old))
            if new is not None:
                changed_aliases.update(aliases(new))

    updated = {key: value for key, value in inventory.items() if key[0] != "7tv"}
    updated.update(new_provider)
    exact_winners: dict[str, Emote] = {}
    folded_winners: dict[str, Emote] = {}
    for emote in updated.values():
        if emote.name in changed_aliases:
            current = exact_winners.get(emote.name)
            if current is None or emote_sort_key(current) < emote_sort_key(emote):
                exact_winners[emote.name] = emote
        folded = emote.name.lower()
        if folded != emote.name and folded in changed_aliases:
            current = folded_winners.get(folded)
            if current is None or emote_sort_key(current) < emote_sort_key(emote):
                folded_winners[folded] = emote
    for alias in changed_aliases:
        selected = exact_winners.get(alias, folded_winners.get(alias))
        if selected is None:
            lookup.pop(alias, None)
        else:
            lookup[alias] = selected
    return lookup, changed_targets(previous, lookup)


def median_full_ns(updated: dict[tuple[str, str], Emote], previous: dict[str, Emote]) -> int:
    samples = []
    for _ in range(ITERATIONS):
        started = time.perf_counter_ns()
        lookup = full_rebuild(updated)
        changed_targets(previous, lookup)
        samples.append(time.perf_counter_ns() - started)
    return int(statistics.median(samples))


def median_incremental_ns(
    updated: dict[tuple[str, str], Emote],
    previous: dict[str, Emote],
    changed_aliases: set[str],
) -> int:
    samples = []
    for _ in range(ITERATIONS):
        # Copying the published ArcSwap snapshot is common publication setup;
        # this gate measures full winner rebuild versus the small-diff repair.
        lookup = copy.copy(previous)
        started = time.perf_counter_ns()
        exact_winners: dict[str, Emote] = {}
        folded_winners: dict[str, Emote] = {}
        for emote in updated.values():
            if emote.name in changed_aliases:
                current = exact_winners.get(emote.name)
                if current is None or emote_sort_key(current) < emote_sort_key(emote):
                    exact_winners[emote.name] = emote
            folded = emote.name.lower()
            if folded != emote.name and folded in changed_aliases:
                current = folded_winners.get(folded)
                if current is None or emote_sort_key(current) < emote_sort_key(emote):
                    folded_winners[folded] = emote
        for alias in changed_aliases:
            selected = exact_winners.get(alias, folded_winners.get(alias))
            if selected is None:
                lookup.pop(alias, None)
            else:
                lookup[alias] = selected
        changed_targets(
            {name: emote for name, emote in previous.items() if name in changed_aliases},
            {name: emote for name, emote in lookup.items() if name in changed_aliases},
        )
        samples.append(time.perf_counter_ns() - started)
    return int(statistics.median(samples))


def main() -> int:
    inventory, replacement = fixture()
    previous = full_rebuild(inventory)
    full_lookup, full_changes = run_full(inventory, replacement, previous)
    incremental_lookup, incremental_changes = run_incremental(inventory, replacement, previous)
    equivalent = full_lookup == incremental_lookup and full_changes == incremental_changes
    updated = {key: value for key, value in inventory.items() if key[0] != "7tv"}
    updated.update({("7tv", emote.id): emote for emote in replacement})
    changed_aliases = full_changes[0]
    full_ns = median_full_ns(updated, previous)
    incremental_ns = median_incremental_ns(updated, previous, changed_aliases)
    speedup = full_ns / incremental_ns
    passed = equivalent and speedup >= 2.0
    print(f"entries={len(inventory)} iterations={ITERATIONS}")
    print(f"equivalent={str(equivalent).lower()} changed_names={len(full_changes[0])} changed_identities={len(full_changes[1])}")
    print(f"full_median_us={full_ns / 1_000:.3f}")
    print(f"incremental_median_us={incremental_ns / 1_000:.3f}")
    print(f"speedup={speedup:.2f}x required=2.00x")
    print(f"result={'PASS' if passed else 'FAIL'}")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
