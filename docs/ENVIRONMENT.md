# Environment variables

Every `ECHO_*` variable Echo reads, grouped by purpose. None is needed for
normal use; they exist for diagnostics, tuning experiments, and the benchmark
harness. The build-time variables at the end are read by `build.rs` when Echo
is compiled. Unless noted, a variable is read once at startup (or on first
use), so set it before launching Echo.

Numeric values that fail to parse fall back to the default. Boolean flags are
"set" when present unless the entry lists accepted values.

## Logging and tracing

| Variable | Effect |
| --- | --- |
| `ECHO_LOG` | Comma-separated log categories to enable (see `src/log.rs`). stdout carries WARN+ only unless this is set. |
| `ECHO_LOG_FILE` | Write the enabled `ECHO_LOG` categories to `%LOCALAPPDATA%\echo\echo.log` too (release builds otherwise log WARN+ there). |
| `ECHO_LOG_FILE_PATH` | Write the log file to this path instead. |
| `ECHO_LOG_APP`, `ECHO_LOG_AUTH`, `ECHO_LOG_CACHE`, `ECHO_LOG_EMOTES`, `ECHO_LOG_IRC`, `ECHO_LOG_NET`, `ECHO_LOG_PERSIST`, `ECHO_LOG_UI`, `ECHO_LOG_UPDATE` | Enable one log category each, same as naming it in `ECHO_LOG`. |
| `ECHO_LOG_PERF`, `ECHO_LOG_PERF_CACHE`, `ECHO_LOG_PERF_EMOTES`, `ECHO_LOG_PERF_RENDER` | Enable performance timing logs (all, or one area). |
| `ECHO_EMOTE_TRACE`, `ECHO_LOG_EMOTE_TRACE` | High-volume emote resolution tracing; also enables the `emotes` category. |
| `ECHO_LAYOUT_TRACE`, `ECHO_LAYOUT_TRACE_VERBOSE` | Trace chat layout and text shaping. |
| `ECHO_RENDER_TRACE`, `ECHO_RENDER_TRACE_VERBOSE` | Trace canvas painting per frame. |
| `ECHO_RELAYOUT_TRACE` | Record why chat rows were re-laid out. |
| `ECHO_PICKER_TIMING` | Log emote picker open and prefetch timings. |
| `ECHO_LOGFILE_RELAUNCH` | Set internally on a process started by `/debug-relaunch-with-logfile`; not meant to be set by hand. |

## Diagnostics and profiling

| Variable | Effect |
| --- | --- |
| `ECHO_FPS_OVERLAY` | Show the FPS overlay. |
| `ECHO_FPS_PROFILER` | Enable the FPS profiler without the overlay. |
| `ECHO_MEASUREMENTS` (or `ZED_MEASUREMENTS`) | Enable the profiler's measurements output. |
| `ECHO_JANK_THRESHOLD` | Frame time in milliseconds above which the profiler counts a frame as jank. |
| `ECHO_PERF_MONITOR` | Enable the backend performance monitor. |
| `ECHO_MEM_REPORT_SECS` | Log a memory breakdown every N seconds (0 or unset: off). |
| `ECHO_ALLOC_PROFILE` | Enable allocation counting (builds with the `alloc-profile` feature). |
| `ECHO_DHAT_FILE`, `ECHO_DHAT_SECS` | Output path and run length for `dhat-heap` builds. See `docs/MEMORY_PROFILING.md`. |
| `ECHO_AUDIT_CHANNEL`, `ECHO_AUDIT_EMOTES` | Log how the listed emote tokens resolve in one channel. |
| `ECHO_AUDIT_EMOTE_IDS` | Comma-separated URL substrings (for example 7TV emote ids); log direct-paint state for matching images. |
| `ECHO_DIAGNOSTIC_TRIM_WORKING_SET_ON_PICKER_CLOSE` | Trim the process working set after the emote picker purges its cache. |

## Runtime, rendering, and theme

| Variable | Effect |
| --- | --- |
| `ECHO_BACKEND_THREADS` | Backend Tokio worker threads (default 4, clamped 1-8). |
| `ECHO_BACKEND_BLOCKING_THREADS` | Backend blocking-pool threads (default 12, clamped 2-32). |
| `ECHO_ANIM_MAX_FPS` | Cap emote animation frame rate instead of following the display refresh rate. |
| `ECHO_ANIMATION_PATCH` | `0` repaints the chat for every animation frame instead of patching the new frame into the presented scene. |
| `ECHO_GPU_NAME_PAINT` | `0` composites 7TV name paints on the CPU (one cached image per name and animation frame) instead of rebuilding them on the GPU from a per-name mask. Renderers without GPU name paints always use the CPU. |
| `ECHO_INACTIVE_THROTTLE` | `1`/`true` caps unfocused windows at 30 fps; `0`/`off`/`false` disables the cap. |
| `ECHO_MIMALLOC_TRIM_INTERVAL_SECS` | mimalloc builds only (Linux, or Windows with `windows-mimalloc`). Run `mi_collect` every N seconds (off unless positive). It only collects the trim thread's own heap; stress A/Bs showed no saving. |
| `MIMALLOC_PAGE_COMMIT_ON_DEMAND` | mimalloc builds only. mimalloc's own option. Echo sets it to `1` at startup unless this is set; `0` restores committing whole pages. |
| `ECHO_UPDATE_RECHECK_SECS` | Re-check the update manifest every N seconds while Echo runs, instead of every 4 hours (testing). |
| `ECHO_SYSTEM_THEME` | Override the detected system theme with a dark flag or a theme name hint (checked before `GTK_THEME` and similar). |

## Settings import

| Variable | Effect |
| --- | --- |
| `ECHO_CHATTERINO_IMPORT_DIR` | Folder that Settings > Advanced > Import from Chatterino reads instead of auto-detecting one: the Chatterino root (for example `%APPDATA%\Chatterino2`), its `Settings` folder, or a portable install. Because it names a path explicitly it also works under `ECHO_BENCH_PROFILE_ROOT`, which otherwise turns auto-detection off; setting it there also lets the first-run import card appear, which still needs a profile with no Echo settings file and a card that wasn't dismissed. Read each time the import panel scans. |

## Media cache tuning

| Variable | Effect |
| --- | --- |
| `ECHO_MEDIA_CACHE_MB` | Total decoded-media memory budget in MB (clamped 32-4096; default scales with RAM). |
| `ECHO_CACHE_BUDGET_MB` | Legacy name for `ECHO_MEDIA_CACHE_MB`, used only when that is unset. |
| `ECHO_PICKER_CACHE_BUDGET_MB` | Media budget while the emote picker is open. |
| `ECHO_GPUI_URI_CACHE_MB` | GPUI URI image cache budget. |
| `ECHO_DECODED_ANIMATION_SOURCE_CACHE_MB` | Compressed animation sources held in RAM before they move to sidecar files (max 512; default 64/96/128 by RAM). |
| `ECHO_DECODED_ANIMATION_DISK_CACHE_MB` | Animation sources kept in session sidecar files under the media cache's `anim` folder, read back to refill and rebuild frames (max 4096; default 512). |
| `ECHO_PICKER_DECODED_ANIMATION_SOURCE_CACHE_MB` | The same cache while the picker is open (default 128/256/512 by RAM). |
| `ECHO_MAX_CONCURRENT_ANIMATED_DECODES` | Concurrent chat animation decodes (clamped 1-16). |
| `ECHO_MAX_CONCURRENT_PICKER_ANIMATED_DECODES` | Concurrent picker animation decodes (clamped 1-16). |
| `ECHO_CACHE_LOADER_THREADS` | Image loader runtime worker threads (default 1, clamped 1-8). |
| `ECHO_CACHE_BLOCKING_THREADS` | Image loader blocking threads for decode (clamped 2-16). |
| `ECHO_WARM_START_EAGER_URL_LIMIT` | Emote images decoded eagerly from the disk cache at startup (max 512). |

## Emotes and the emote picker

| Variable | Effect |
| --- | --- |
| `ECHO_EMOTE_MISS_REFRESH` | `0`/`false`/`off` disables refetching a channel's emotes when a message names an unknown one. |
| `ECHO_CHANNEL_STARTUP_REFRESH_GRACE_SECS` | How long disk-cached channel emotes count as fresh at startup before a refresh. |
| `ECHO_STARTUP_RECENT_EMOTE_PRELOAD_LIMIT` | Recently used emotes preloaded at startup (0 disables). |
| `ECHO_STARTUP_RECENT_EMOTE_PRELOAD_DELAY_MS`, `ECHO_STARTUP_RECENT_EMOTE_PRELOAD_GAP_MS` | Delay before that preload and the gap between its batches. |
| `ECHO_PICKER_PAGE_EMOTE_LIMIT` | Emotes per picker page (min 80). |
| `ECHO_PICKER_NATIVE_ANIMATED` | Let GPUI animate picker cells natively instead of Echo's animation path. |
| `ECHO_PICKER_COMPACT_ANIMATION_DENSITY` | Animated-cell count at which the picker switches to compact animation (0 disables). |
| `ECHO_PICKER_STATIC_PREFETCH_EAGER_LIMIT`, `ECHO_PICKER_STATIC_PREFETCH_LAZY_LIMIT` | Static picker images prefetched eagerly and lazily. |
| `ECHO_PICKER_ANIMATED_PREFETCH_LIMIT` | Animated picker images prefetched (0 disables). |
| `ECHO_7TV_EVENTAPI_REPLAY*` | Replay recorded 7TV EventAPI traffic. See `docs/7tv-eventapi-replay.md`. |

## Chat replay

`ECHO_CHAT_REPLAY` names a recorded chat file to replay into a channel instead
of connecting live. Its companions tune the replay:
`ECHO_CHAT_REPLAY_CHANNEL`, `ECHO_CHAT_REPLAY_CLEAR_CHANNEL`,
`ECHO_CHAT_REPLAY_DELAY_MS`, `ECHO_CHAT_REPLAY_SPEEDUP`,
`ECHO_CHAT_REPLAY_MAX_EVENTS`, `ECHO_CHAT_REPLAY_PRESERVE_TIMESTAMPS`,
`ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES`, and `ECHO_CHAT_REPLAY_REPORT`
(path of a report written when the replay ends). See `src/backend/chat_replay.rs`.

## Benchmark harness

These are set by the scripts under `scripts/bench/`; see `docs/BENCHMARKING.md`.

- `ECHO_BENCH_PROFILE_ROOT` isolates every config, data, cache, and keyring
  path under one directory (see `src/benchmark_profile.rs`). Use it for any
  experiment that must not touch real user state.
- Run identity and receipts: `ECHO_BENCH_OFFLINE`, `ECHO_BENCH_TARGET_COMMIT`,
  `ECHO_BENCH_OBSERVATION_NONCE`, `ECHO_BENCH_PROFILE_RECEIPT`,
  `ECHO_BENCH_LIVE_CHANNEL`, `ECHO_BENCH_LIVE_CHANNEL_RECEIPT`,
  `ECHO_BENCH_ALLOW_MULTI_INSTANCE`.
- UI fixtures: `ECHO_BENCH_UI_FIXTURE`, `ECHO_BENCH_UI_FIXTURE_SETTINGS_ONLY`,
  `ECHO_BENCH_FEEDBACK_UI`, `ECHO_BENCH_RESPONSIVE_MATRIX`,
  `ECHO_BENCH_LINK_PREVIEW_MATRIX`, `ECHO_BENCH_LINK_PREVIEW_MATRIX_DELAY_MS`.
- Scripted drivers: `ECHO_BENCH_UI_SMOKE*`, `ECHO_BENCH_SCROLL_SMOKE*`,
  `ECHO_BENCH_LIFECYCLE_*`, `ECHO_BENCH_PICKER_SWEEP_*`,
  `ECHO_BENCH_PICKER_STABILITY_*`, `ECHO_BENCH_PICKER_ANIMATION_MODE`,
  `ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS`.

## Build-time

`ECHO_BUILD_TARGET`, `ECHO_GPUI_VERSION`, and `ECHO_GPUI_ZED_REV` are set by
`build.rs` and read with `env!`; they are not runtime settings.

| Variable | Effect |
| --- | --- |
| `ECHO_API_PROXY_URL`, `ECHO_API_PROXY_KEY` | Optional. Base URL and access key of a private proxy for Twitch badge, user and stream lookups that need no login (`src/backend/constants.rs`). `build.rs` reads each from the environment, or from `KEY=VALUE` lines in a gitignored `echo-private.env` at the repository root (`#` comments, an `export ` prefix and surrounding quotes are accepted; a trailing `/` on the URL is dropped), and compiles them into the binary. Cargo watches `echo-private.env` only once a build has seen it, so after creating it, or restoring it after a build without it, run `touch build.rs` (or set the variables in the environment) before building. Public builds leave both unset: the build prints a warning that the proxy is disabled, and those lookups go straight to Twitch's Helix API or report that no proxy is configured. |
| `ECHO_REQUIRE_API_PROXY` | `1` turns the missing-proxy warning into a build error, for builds that must ship with the proxy. |
