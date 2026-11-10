# Benchmarking and profiling Echo

This guide covers the tools for measuring Echo itself: isolated profiles, local
chat replay, the diagnostic log lines, the scripts under `scripts/bench/`, and
how to run an A/B between two Echo builds. Every environment variable mentioned
here is listed in `docs/ENVIRONMENT.md`; memory-specific tools are in
`docs/MEMORY_PROFILING.md`.

## Ground rules

- Measure a build that matches what you are asking about. `--release` is fat
  LTO with one codegen unit and takes a long time to build incrementally; the
  `dist` profile (`cargo build --profile dist --locked --bin echo`) is the
  shipped configuration and builds much faster. Debug builds are only useful
  for smoke tests.
- Run every observation in an isolated profile (below) so a benchmark never
  reads or writes your real settings, caches, tokens or logs.
- Prefer a local chat replay over a live channel. Live chat rate, emote sets and
  network timing change from run to run.
- Compare variants on the same machine, in alternating order (ABBA), with a
  warm-up run first. Report medians and the run-to-run spread, not one run.
- Scripts write run directories under `benchmarks/results/` by default. They are
  local output; don't commit them.

## Isolated profiles

`ECHO_BENCH_PROFILE_ROOT=<dir>` moves every Echo-owned config, data, cache, log
and keyring path under `<dir>` (`src/storage_roots.rs`):

| Path | Contents |
| --- | --- |
| `<dir>/config` | settings, layouts, history logs, media disk cache |
| `<dir>/data` | local data and the runtime directory |
| `<dir>/cache` | update cache |
| `<dir>/logs` | `echo.log` and profiling artifacts such as `dhat-heap.json` |

Integrations that read other applications' data are disabled while a profile is
active. The profile starts empty, so the first launch is a cold start. To measure
warm starts, run once to populate the profile, then copy that directory for each
measured run.

Echo enforces a single running instance. `ECHO_BENCH_ALLOW_MULTI_INSTANCE=1`
skips that check so a benchmark instance can start next to another Echo.

`ECHO_BENCH_OFFLINE=1` keeps Echo's own background network lanes disabled for the
run. It is honored only together with `ECHO_BENCH_PROFILE_ROOT`.

## Local chat replay

`ECHO_CHAT_REPLAY=<file>` replays a recorded chat file into a channel, so every
run sees the same messages. Replayed rows go through the same message buffer and
observer path as other messages (`src/backend/chat_replay.rs`). The companion
variables set the target channel (`ECHO_CHAT_REPLAY_CHANNEL`), clear it first
(`ECHO_CHAT_REPLAY_CLEAR_CHANNEL`), delay the start (`ECHO_CHAT_REPLAY_DELAY_MS`),
speed it up (`ECHO_CHAT_REPLAY_SPEEDUP`), cap it (`ECHO_CHAT_REPLAY_MAX_EVENTS`),
and write a report when it ends (`ECHO_CHAT_REPLAY_REPORT`).

A replay file is NDJSON, one event per line. Fixtures:

- `benchmarks/fixtures/chat-replay-normal.ndjson`: 600 deterministic messages in
  `#echo-bench` covering plain text, mentions, Twitch and third-party emotes,
  badges, links, long messages and reply-like rows. The Linux application smoke
  uses it.
- `benchmarks/fixtures/perf-rig/`: the three-split tab layout and mixed-rate
  animated-emote replay used by `run-perf-rig.ps1`.
- `benchmarks/fixtures/7tv-eventapi/`: recorded 7TV EventAPI traffic for
  `ECHO_7TV_EVENTAPI_REPLAY` (see [7TV_EVENTAPI_REPLAY.md](7TV_EVENTAPI_REPLAY.md)).
- Generate others with `scripts/bench/new-chat-replay-fixture.ps1` (or `.py`):
  `-ReplayScenario normal | high-volume | burst | reconnect`, with
  `-MessageCount`, `-DurationSeconds`, `-UserCount`, `-Seed` and the burst
  settings. `new-render-fixture.*` generates render inputs (static, animated and
  mixed emotes, large scrollback, rapid scrolling, multi-split), and
  `new-fixture-suite.*` builds and validates a full set in one run directory.

## Diagnostic log lines

Perf output is written as `key=value` log lines, which `tools/profile-log-check`
and the scripts parse. Enable file logging with `ECHO_LOG_FILE=1` and the
categories with `ECHO_LOG=perf` (plus others as needed). The log is
`<profile>/logs/echo.log` under an isolated profile, or `ECHO_LOG_FILE_PATH`.

| Variable | Output |
| --- | --- |
| `ECHO_LOG_PERF_RENDER=1` | One "Canvas render/layout aggregate" line per split every 2 s while it renders: frames, layout and element time, cache hits, animation reuse, rebuild reasons. |
| `ECHO_FPS_PROFILER=1` | A frame, GPU and present line every 30 root frames (`cadence_fps`, `work_ms`, `canvas_cpu_ms`, `canvas_paint_ms`, `gpu_ms`, `present_latency_ms`, jank and frame classes). `ECHO_FPS_OVERLAY=1` shows the FPS overlay as well. |
| `ECHO_MEM_REPORT_SECS=N` | A "memory breakdown" line every N seconds: decoded media, layout caches, GPUI image entries, private and working-set bytes. |
| `ECHO_PERF_MONITOR=1` | Backend counters (UI notify sources, emote stages and events) read through the metrics overlay and the metrics JSON. |
| `ECHO_LOG_PERF`, `ECHO_LOG_PERF_EMOTES`, `ECHO_LOG_PERF_CACHE` | Timing logs for all perf areas, emotes, or the cache. |

Animation, layout and image wake loops only repaint a split when something
visible changes, so an idle window should show few frames. A high frame count
on an idle split is a finding, not noise.

## Scripts

Most scripts under `scripts/bench/` write into a run directory created by
`new-benchmark-run.ps1` (or `.py`): `environment.json` (machine, commit, dirty
paths), `summary.json` (artifact paths and metrics) and `notes.md`.
`validate-benchmark-run.ps1` (or `.py`) checks that the artifacts a run claims
to have exist and parse, for example `-RequireEchoProfile`, `-RequireChatReplay`
or `-RequireFrameTrace`.

Where a script has both a `.ps1` and a `.py` version, the two produce the same
artifact layout: use the PowerShell version on Windows and the `.py` version
anywhere else Python 3 is available. Where one version does more, its table row
says so. The exception is
`run-pgo-build.py`, which is Linux-only (see the table). The PowerShell
scripts share their helpers through `BenchLib.psm1`, the counterpart of
`benchlib.py`. Every percentile either language reports is linear
interpolation between the closest ranks (rank = p/100 x (n-1), as numpy's
default and Excel's PERCENTILE.INC), rounded to 6 decimals.

The Python helpers have unit tests under `scripts/bench/tests`; the Linux
`quick` suite runs them, or run
`python3 -m unittest discover -s scripts/bench/tests` directly.

### Launch and sample

| Script | Purpose |
| --- | --- |
| `run-echo-profile.ps1` / `.py` | Launch Echo with profiler logging, keep it alive for `-DurationSeconds`, stop it (`-StopMode close-main-window`), and validate the log with `profile-log-check`. The Python version also drives chat replay (`--chat-replay*`), an isolated offline mode (`--network-state offline-fixture`), and an app-owned graceful quit (`--app-owned-quit-after-ms`). |
| `sample-process.ps1` / `.py` | Sample working set, private bytes, handles, threads and CPU of a launched or running process at a fixed interval. |
| `measure-startup.ps1` / `.py` | Time process creation to readiness (`-ReadyMode` `process-start`, `main-window`, `input-idle` or `app-log`). With `-App echo -AppLogPath <log>` it also records Echo's `startup_marker` milestones. |
| `run-startup-campaign.ps1` | Repeat `measure-startup.ps1` N times (`-ObservationCount`, at least 10) and report median and p95 (linear interpolation, see above) for each startup milestone. |
| `run-lifecycle-churn.ps1` | After a warm plateau, run `-Cycles` cycles that each open a split on a fixture channel, a popout and a scroll, then settle and check for growing handles, threads or private memory. Requires `-ReplayPath`. |
| `snapshot-cache.ps1` / `.py`, `compare-cache-snapshots.ps1` / `.py` | Inventory a cache directory before and after a run and diff the two. |
| `record-echo-baseline.ps1` / `.py` | Record an executable's size, SHA-256, commit and build type. |
| `record-performance-provenance.ps1` | Tie CPU and GPU evidence files, cache trees and the profiled executable together by hash. |

### Windows rigs

| Script | Purpose |
| --- | --- |
| `run-perf-rig.ps1` | Release perf rig. Takes `-Exe`, or builds `cargo build --profile dist --locked --bin echo`, warms an isolated profile, opens the three-split layout from `benchmarks/fixtures/perf-rig/`, replays mixed-rate animated emotes, and writes into `-OutDir` one `summary.json` (its `run` object holds process and main-thread CPU, per-thread CPU, GPU engine time, profiler frames and presents per second, per-split canvas counters and the memory breakdown) plus `perf-lines.txt` and `replay-report.json`. It writes its own output directory rather than a `new-benchmark-run` run directory. |
| `run-echo-ab.ps1` | Balanced A/B of two Echo binaries (`-BaselinePath`, `-CandidatePath`, `-Sequence ABBA`, `-RepeatCount`) through `sample-process.ps1`, optionally under a chat replay (`-EchoChatReplay`). Writes per-metric medians and deltas. |
| `run-live-ui-suite.ps1` | Launches Echo (`-LaunchPath`, default `target\release\echo.exe`) and drives its window with native input: scripted UI fixture checkpoints (`-UiFixtureInteractions`, `-UiFixtureSettingsOnly`), feedback, cursor and popup checks, the emote-picker sweep and the link-preview matrix. It uses your normal profile except for `-LinkPreviewMatrix`, which runs in an isolated one. It never types chat text, presses Enter or sends. |
| `capture-responsive-ui-matrix.ps1` | Screenshots of the UI fixture at the current, half-width, half-height and half-width-and-height window sizes. |
| `capture-gpu-frame-times.ps1` | PresentMon frame-time capture for a launched Echo (`-PresentMonPath`); `install-presentmon.ps1` downloads a pinned PresentMon release. |
| `capture-windows-gpu-counters.ps1` | Per-process GPU memory counters plus process samples over a long run, with optional periodic memory maps (`-MemoryMapIntervalSeconds`). `watch-owned-process.ps1` is its stop watchdog. |
| `capture-process-memory-map.ps1` | One virtual-memory map of a running process (`-TargetProcessId`), with the largest allocations listed. |
| `run-pgo-build.ps1` | Build a PGO-optimized Echo: instrumented build, training run under chat replay, profile merge, optimized build. `run-pgo-build.py` is the Linux counterpart. |
| `measure-dist-build.ps1` | Time `cargo build --profile dist --locked --bin echo` (`-CacheState` records whether `target/` was warm). |

### Summaries of saved traces

`summarize-frame-trace.*`, `summarize-gpu-trace.*`, `summarize-connection-trace.*`,
`summarize-long-session-samples.*` and `summarize-cleanup-samples.*` turn a raw
CSV or log trace into a structured summary inside a run directory.

### Linux

| Script | Purpose |
| --- | --- |
| `scripts/test-linux-app-smoke.sh` | Launch a debug build under an isolated profile, replay three messages from the normal fixture, quit through GPUI and validate the log. Needs a real X11 or Wayland session. |
| `scripts/profile.sh` | perf, flamegraph, heaptrack, Valgrind and sanitizer wrappers (`make flame`, `make heap`, ...). Runs under an isolated profile (`target/profile/echo-profile` unless `ECHO_BENCH_PROFILE_ROOT` is set) and writes artifacts to `target/profile/`. |

### Microbenchmarks

- Criterion benches in `benches/`: `cargo bench --bench layout_bench`
  (layout, height tree, scroll markers), `--bench parse_bench` (message-ingest
  parsing: emote tags, slot parsing, structure hashing) and
  `--bench loading_bench` (image decode, GPU format conversion, disk cache). Check one at a time with `cargo check -p echo_gpui --bench <name>`;
  `--benches` also selects the root libtest target.
- Standalone benchmark tools registered as `benchmark` in
  `scripts/linux-test-targets.tsv`: `cargo run --release -p resource-bench`,
  `-p search-bench` and `-p provider-index-bench`. `tools/gpui-scene-bench`
  is a Criterion bench of the production-shaped GPUI chat scene in a real
  native window (`cargo bench -p gpui-scene-bench`).
- `evaluate-provider-index-diff.py` models the incremental provider-index update
  in Python and checks it against a full rebuild.

## Running an A/B

1. Build both variants with the same profile and copy each binary out of
   `target/` so a rebuild can't replace it:

   ```powershell
   cargo build --profile dist --locked --bin echo
   Copy-Item target\dist\echo.exe $env:TEMP\echo-a.exe
   # switch to the candidate change, rebuild, then:
   Copy-Item target\dist\echo.exe $env:TEMP\echo-b.exe
   ```

2. Close other Echo instances and heavy applications, and keep the machine on
   the same power plan for every run.
3. For process CPU and memory, run the balanced A/B:

   ```powershell
   pwsh -NoProfile -File scripts\bench\run-echo-ab.ps1 `
     -BaselinePath $env:TEMP\echo-a.exe -CandidatePath $env:TEMP\echo-b.exe `
     -Sequence ABBA -RepeatCount 2 -DurationSeconds 60 `
     -EchoChatReplay benchmarks\fixtures\chat-replay-normal.ndjson
   ```

4. For render, frame and per-split cost, run the rig once per binary in ABBA
   order with identical settings, each into its own output directory:

   ```powershell
   pwsh -NoProfile -File scripts\bench\run-perf-rig.ps1 -Exe $env:TEMP\echo-a.exe -OutDir benchmarks\results\ab\run1-a
   pwsh -NoProfile -File scripts\bench\run-perf-rig.ps1 -Exe $env:TEMP\echo-b.exe -OutDir benchmarks\results\ab\run2-b
   pwsh -NoProfile -File scripts\bench\run-perf-rig.ps1 -Exe $env:TEMP\echo-b.exe -OutDir benchmarks\results\ab\run3-b
   pwsh -NoProfile -File scripts\bench\run-perf-rig.ps1 -Exe $env:TEMP\echo-a.exe -OutDir benchmarks\results\ab\run4-a
   ```

   Compare the `summary.json` files. A difference smaller than the gap between
   the two runs of the same binary is not a result.

5. Record the commands, both binaries' SHA-256 (`run-echo-ab.ps1` writes them to
   `echo-ab-run.json`), the fixture and the machine with the numbers.
