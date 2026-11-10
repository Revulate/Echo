#!/usr/bin/env python3
"""Create a timestamped benchmark artifact directory.

This mirrors scripts/bench/new-benchmark-run.ps1 for environments where
PowerShell is not installed. It initializes metadata only; it is not a
performance measurement.
"""

from __future__ import annotations

import argparse

from benchlib import initialize_run_dir


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--output-root", default="")
    parser.add_argument("--echo-path", default="")
    parser.add_argument("--echo-build-type", default="")
    parser.add_argument("--cache-state", default="")
    parser.add_argument("--network-state", default="")
    parser.add_argument("--fixture-name", default="")
    parser.add_argument("--notes", default="")
    args = parser.parse_args()

    run_dir = initialize_run_dir(
        output_root=args.output_root,
        scenario=args.scenario,
        echo_path=args.echo_path,
        echo_build_type=args.echo_build_type,
        cache_state=args.cache_state,
        network_state=args.network_state,
        fixture_name=args.fixture_name,
        notes=args.notes,
    )
    print(run_dir)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
