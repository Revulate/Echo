#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
launch_path="${ECHO_APP_SMOKE_BINARY:-$repo_root/target/debug/echo}"
output_root="${ECHO_APP_SMOKE_OUTPUT_ROOT:-$repo_root/target/linux-app-smoke}"
quit_after_seconds="${ECHO_APP_SMOKE_DURATION_SECONDS:-8}"
timeout_seconds="${ECHO_APP_SMOKE_TIMEOUT_SECONDS:-20}"
fixture="$repo_root/benchmarks/fixtures/chat-replay-normal.ndjson"

if [[ "$(uname -s)" != "Linux" ]]; then
    printf 'Echo application smoke is Linux-only.\n' >&2
    exit 1
fi
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    printf 'Echo application smoke requires a real X11 or Wayland display.\n' >&2
    exit 1
fi

cd "$repo_root"
if [[ "${ECHO_APP_SMOKE_SKIP_BUILD:-0}" != "1" ]]; then
    cargo build --locked -p echo_gpui --bin echo
fi
cargo build --release --locked -p profile-log-check

if [[ ! -x "$launch_path" ]]; then
    printf 'Echo application smoke binary is not executable: %s\n' "$launch_path" >&2
    exit 1
fi

mkdir -p "$output_root"
run_root="$(mktemp -d "$output_root/run.XXXXXX")"
profile_root="$run_root/profile"
ECHO_BENCH_PROFILE_ROOT="$profile_root" \
python3 scripts/bench/run-echo-profile.py \
    --run-dir "$run_root" \
    --scenario "Linux application smoke" \
    --launch-path "$launch_path" \
    --duration-seconds "$timeout_seconds" \
    --app-owned-quit-after-ms "$((quit_after_seconds * 1000))" \
    --exit-wait-seconds 5 \
    --stop-mode close-main-window \
    --profile-log-check-path target/release/profile-log-check \
    --echo-build-type debug \
    --cache-state isolated \
    --network-state offline-fixture \
    --fixture-name linux-app-smoke \
    --chat-replay "$fixture" \
    --chat-replay-channel echo-bench \
    --chat-replay-speedup 1000 \
    --chat-replay-max-events 3 \
    --chat-replay-clear-channel

printf 'Echo Linux application smoke passed. Artifacts: %s\n' "$run_root"
