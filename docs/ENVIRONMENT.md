# Environment variables

Every `ECHO_*` variable Echo reads, grouped by purpose, plus the few non-`ECHO_`
runtime switches it honors. None is needed for normal use; they exist for
diagnostics, tuning experiments, and the benchmark harness. The build-time
variables at the end are read by `build.rs` when Echo is compiled. Unless
noted, a variable is read once at startup (or on first use), so set it before
launching Echo.

Every boolean switch follows one rule, implemented in `echo_core::env`
(`crates/echo-core/src/env.rs`): it is on when its value is `1`, `true`, `yes`
or `on`, ignoring case and surrounding whitespace, and off for any other value,
including `0`, an empty value and an unset variable. Numeric and path values
are trimmed; a blank or malformed one counts as unset, so the default applies.
The entries that deviate from this rule say so.

The variables used only by the Linux test and smoke scripts
(`ECHO_LINUX_*_TIMEOUT_SECONDS`, `ECHO_APP_SMOKE_*`) are described in
[LINUX_DEVELOPMENT.md](LINUX_DEVELOPMENT.md).

## Logging and tracing

| Variable | Effect |
| --- | --- |
| `ECHO_LOG` | Comma-separated log categories to enable: `irc`, `emotes`, `auth`, `cache`, `perf`, `perf_render`, `perf_cache`, `perf_emotes`, `ui`, `net`, `app`, `persist`, `update` (`src/log.rs`, `CATEGORY_TABLE`). Names are matched exactly; spaces around entries are ignored. `perf` also enables every `perf_*` category. An enabled category's debug and info lines go to stdout as well as WARN+. A set `RUST_LOG` replaces the category filter entirely, so don't combine the two. |
| `ECHO_LOG_FILE` | Write the enabled categories to `echo.log` too: `%LOCALAPPDATA%\echo\echo.log` on Windows, `~/.local/share/echo/echo.log` on Linux, `<profile>/logs/echo.log` under `ECHO_BENCH_PROFILE_ROOT` (release builds otherwise log WARN+ there). |
| `ECHO_LOG_FILE_PATH` | With `ECHO_LOG_FILE` on, write the log file to this path instead. Ignored under `ECHO_BENCH_PROFILE_ROOT`. |
| `ECHO_LOG_APP`, `ECHO_LOG_AUTH`, `ECHO_LOG_CACHE`, `ECHO_LOG_EMOTES`, `ECHO_LOG_IRC`, `ECHO_LOG_NET`, `ECHO_LOG_PERSIST`, `ECHO_LOG_UI`, `ECHO_LOG_UPDATE` | Enable one log category each, same as naming it in `ECHO_LOG`. |
| `ECHO_LOG_PERF`, `ECHO_LOG_PERF_CACHE`, `ECHO_LOG_PERF_EMOTES`, `ECHO_LOG_PERF_RENDER` | Enable performance timing logs (all `perf` categories, or one area). |
| `ECHO_EMOTE_TRACE`, `ECHO_LOG_EMOTE_TRACE` | High-volume emote resolution tracing; also enables the `emotes` category. |
| `ECHO_LAYOUT_TRACE`, `ECHO_LAYOUT_TRACE_VERBOSE` | Trace chat layout and text shaping. The verbose switch implies the plain one. Either also enables the `perf_render` category. |
| `ECHO_RENDER_TRACE`, `ECHO_RENDER_TRACE_VERBOSE` | Trace canvas painting per frame. The verbose switch implies the plain one. Either also enables the `perf_render` category. |
| `ECHO_RELAYOUT_TRACE` | Record why chat rows were re-laid out. |
| `ECHO_PICKER_TIMING` | Log emote picker open and prefetch timings. |
| `ECHO_LOGFILE_RELAUNCH` | Set internally on a process started by `/debug-relaunch-with-logfile`; not meant to be set by hand. |

## Diagnostics and profiling

| Variable | Effect |
| --- | --- |
| `ECHO_FPS_OVERLAY` | Show the FPS overlay (turns the FPS profiler on). |
| `ECHO_FPS_PROFILER` | Enable the FPS profiler without the overlay. |
| `ECHO_MEASUREMENTS` (or `ZED_MEASUREMENTS`) | Enable the profiler's measurements output (turns the FPS profiler on). |
| `ECHO_JANK_THRESHOLD` | Frame time in milliseconds (a decimal number, default 16.67) above which the profiler counts a frame as jank. Read only while the profiler is on. |
| `ECHO_PERF_MONITOR` | Enable the backend performance monitor. |
| `ECHO_MEM_REPORT_SECS` | Log a memory breakdown every N seconds (0 or unset: off). |
| `ECHO_ALLOC_PROFILE` | Enable allocation counting. Only builds with the `alloc-profile` feature read it. |
| `ECHO_DHAT_FILE`, `ECHO_DHAT_SECS` | Output path (default `dhat-heap.json`) and run length in seconds (default 90) for `dhat-heap` builds. Under `ECHO_BENCH_PROFILE_ROOT` the file always goes to `<profile>/logs/dhat-heap.json`. See [MEMORY_PROFILING.md](MEMORY_PROFILING.md). |
| `ECHO_AUDIT_CHANNEL`, `ECHO_AUDIT_EMOTES` | Log how the comma-separated emote tokens in `ECHO_AUDIT_EMOTES` resolve in the channel `ECHO_AUDIT_CHANNEL`, each time that channel's emote index is rebuilt. Both must be set. |
| `ECHO_AUDIT_EMOTE_IDS` | Comma-separated URL substrings (for example 7TV emote ids); log direct-paint state for matching images. |
| `ECHO_DIAGNOSTIC_TRIM_WORKING_SET_ON_PICKER_CLOSE` | Trim the process working set after the emote picker purges its cache. |

## Runtime, rendering, and theme

| Variable | Effect |
| --- | --- |
| `ECHO_BACKEND_THREADS` | Backend Tokio worker threads (default 4, clamped 1-8). |
| `ECHO_BACKEND_BLOCKING_THREADS` | Backend blocking-pool threads (default 12, clamped 2-32). |
| `ECHO_ANIM_MAX_FPS` | Cap emote animation frame rate (clamped 30-360) instead of following the display refresh rate. |
| `ECHO_ANIMATION_PATCH` | On by default. Set to anything other than an on value (for example `0`) to repaint the chat for every animation frame instead of patching the new frame into the presented scene. |
| `ECHO_GPU_NAME_PAINT` | On by default. Set to anything other than an on value (for example `0`) to composite 7TV name paints on the CPU (one cached image per name and animation frame) instead of rebuilding them on the GPU from a per-name mask. Renderers without GPU name paints always use the CPU. |
| `ECHO_INACTIVE_THROTTLE` | Three states, an exception to the boolean rule: an on value caps unfocused windows at 30 fps; `0`, `off`, `no` or `false` (any case) removes the cap; unset or any other value keeps the default interval (see [GPUI_FORK.md](GPUI_FORK.md), "Upstream features Echo relies on"). |
| `ECHO_MIMALLOC_TRIM_INTERVAL_SECS` | mimalloc builds only (Linux, or Windows with `windows-mimalloc`). Run `mi_collect` every N seconds; `0` or unset leaves it off, and a value that is not a non-negative integer logs a warning and leaves it off. It only collects the trim thread's own heap. |
| `MIMALLOC_PAGE_COMMIT_ON_DEMAND` | mimalloc builds only. mimalloc's own option. Echo sets it to `1` at startup unless the variable is set; `0` restores committing whole pages. |
| `ECHO_UPDATE_RECHECK_SECS` | Re-check the update manifest every N seconds (N > 0) while Echo runs, instead of every 4 hours (testing). |
| `ECHO_SYSTEM_THEME` | Linux and the BSDs. Override the detected system theme, checked before `GTK_THEME`, `GTK_APPLICATION_PREFER_DARK_THEME` and `COLORSCHEME`: `1`, `true`, `yes`, `on`, `dark` or `prefer-dark` mean dark; `0`, `false`, `no`, `off`, `light`, `default` or `prefer-light` mean light; any other value is read as a theme name, and one containing `dark`, `night` or `black` counts as dark. |
| `FORCE_ANON_IRC` | Boolean. Connect to IRC anonymously (read-only) even when an account is logged in. |

## Settings import

| Variable | Effect |
| --- | --- |
| `ECHO_CHATTERINO_IMPORT_DIR` | Folder that Settings > Advanced > Import from Chatterino reads instead of auto-detecting one: the Chatterino root (for example `%APPDATA%\Chatterino2`), its `Settings` folder, or a portable install. Because it names a path explicitly it also works under `ECHO_BENCH_PROFILE_ROOT`, which otherwise turns auto-detection off; setting it there also lets the first-run import card appear, which still needs a profile with no Echo settings file and a card that wasn't dismissed. Read each time the import panel scans. |

## Media cache tuning

`ECHO_MEDIA_CACHE_MB`, `ECHO_CACHE_BUDGET_MB` and `ECHO_PICKER_CACHE_BUDGET_MB`
take a whole number of megabytes; a malformed value is ignored.

| Variable | Effect |
| --- | --- |
| `ECHO_MEDIA_CACHE_MB` | Total decoded-media memory budget in MB (clamped 32-4096; default 72, 112 or 192 MB for up to 8 GB, up to 16 GB, or more RAM). |
| `ECHO_CACHE_BUDGET_MB` | Legacy name for `ECHO_MEDIA_CACHE_MB`, used only when that is unset or blank. |
| `ECHO_PICKER_CACHE_BUDGET_MB` | Media budget while the emote picker is open (clamped 64-4096, never below the main budget; default 128, 192 or 256 MB by RAM). |
| `ECHO_GPUI_URI_CACHE_MB` | GPUI URI image cache budget. The default is also the ceiling: a quarter of the decoded-media budget, kept between 16 and 64 MB, so this can only lower it (minimum 16 MB). |
| `ECHO_DECODED_ANIMATION_SOURCE_CACHE_MB` | Compressed animation sources held in RAM before they move to sidecar files (max 512; default 64, 96 or 128 by RAM). |
| `ECHO_DECODED_ANIMATION_DISK_CACHE_MB` | Animation sources kept in session sidecar files under the media cache's `anim` folder, read back to refill and rebuild frames (max 4096; default 512). |
| `ECHO_PICKER_DECODED_ANIMATION_SOURCE_CACHE_MB` | The same cache while the picker is open (default 128, 256 or 512 by RAM). |
| `ECHO_MAX_CONCURRENT_ANIMATED_DECODES` | Concurrent chat animation decodes (clamped 1-16). |
| `ECHO_MAX_CONCURRENT_PICKER_ANIMATED_DECODES` | Concurrent picker animation decodes (clamped 1-16). |
| `ECHO_CACHE_LOADER_THREADS` | Image loader runtime worker threads (default 1, clamped 1-8). |
| `ECHO_CACHE_BLOCKING_THREADS` | Image loader blocking threads for decode (clamped 2-16). |
| `ECHO_WARM_START_EAGER_URL_LIMIT` | Emote images decoded eagerly from the disk cache at startup (default 50, max 512). |

## Emotes and the emote picker

| Variable | Effect |
| --- | --- |
| `ECHO_EMOTE_MISS_REFRESH` | On while unset or set to an on value; any other value (for example `0`) disables refetching a channel's emotes when a message names an unknown one. |
| `ECHO_CHANNEL_STARTUP_REFRESH_GRACE_SECS` | How long disk-cached channel emotes count as fresh at startup before a refresh (default 120). |
| `ECHO_STARTUP_RECENT_EMOTE_PRELOAD_LIMIT` | How many recently opened channels have their emotes preloaded at startup (default 0: off). |
| `ECHO_STARTUP_RECENT_EMOTE_PRELOAD_DELAY_MS`, `ECHO_STARTUP_RECENT_EMOTE_PRELOAD_GAP_MS` | Delay before that preload (default 8000) and the gap between channels (default 2000). |
| `ECHO_PICKER_PAGE_EMOTE_LIMIT` | Emotes per picker page (default 10,000, clamped 80-50,000). Read each time a page is built. |
| `ECHO_PICKER_NATIVE_ANIMATED` | Let GPUI animate picker cells natively instead of Echo's animation path. |
| `ECHO_PICKER_COMPACT_ANIMATION_DENSITY` | Animated-cell count at which the picker switches to compact animation (default 256; 0 disables). |
| `ECHO_PICKER_STATIC_PREFETCH_EAGER_LIMIT`, `ECHO_PICKER_STATIC_PREFETCH_LAZY_LIMIT` | Static picker images prefetched eagerly (default 256) and lazily (default 512). |
| `ECHO_PICKER_ANIMATED_PREFETCH_LIMIT` | Animated picker images prefetched (default 96; 0 disables). |
| `ECHO_7TV_EVENTAPI_REPLAY*` | Replay recorded 7TV EventAPI traffic. See [7TV_EVENTAPI_REPLAY.md](7TV_EVENTAPI_REPLAY.md). |

The four picker prefetch and density limits are read each time the picker
plans a prefetch, so they can change while Echo runs.

## Chat replay

`ECHO_CHAT_REPLAY` replays a recorded chat file into a channel instead of
connecting live (`src/backend/chat_replay.rs`).

| Variable | Effect |
| --- | --- |
| `ECHO_CHAT_REPLAY` | Path of the NDJSON replay file. Setting it, even to an empty value, enables the replay. |
| `ECHO_CHAT_REPLAY_REPORT` | Path of a JSON report written when the replay ends. |
| `ECHO_CHAT_REPLAY_CHANNEL` | Replay every event into this channel instead of the channels named in the file. |
| `ECHO_CHAT_REPLAY_CLEAR_CHANNEL` | Boolean. Clear the target channel before replaying. |
| `ECHO_CHAT_REPLAY_DELAY_MS` | Wait this long before the first event (default 0). |
| `ECHO_CHAT_REPLAY_SPEEDUP` | Pace events by their recorded spacing divided by this positive factor. Unset, events are replayed without pauses. |
| `ECHO_CHAT_REPLAY_MAX_EVENTS` | Stop after this many events. |
| `ECHO_CHAT_REPLAY_PRESERVE_TIMESTAMPS` | Boolean. Keep the recorded message timestamps instead of rebasing them onto the replay's start time. |
| `ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES` | Boolean. Insert the file's non-moderation control events as local system rows. |

## Benchmark harness

These are set by the scripts under `scripts/bench/`; see
[BENCHMARKING.md](BENCHMARKING.md).

- `ECHO_BENCH_PROFILE_ROOT` (an absolute path) isolates every config, data,
  cache, log and keyring path under one directory (`src/storage_roots.rs`).
  Use it for any experiment that must not touch real user state.
- `ECHO_BENCH_OFFLINE` (boolean) keeps Echo's own background network lanes
  disabled. It is honored only together with `ECHO_BENCH_PROFILE_ROOT`.
- `ECHO_BENCH_ALLOW_MULTI_INSTANCE` (boolean) skips the single-instance check.
- Run identity and receipts: `ECHO_BENCH_TARGET_COMMIT`,
  `ECHO_BENCH_OBSERVATION_NONCE`, `ECHO_BENCH_PROFILE_RECEIPT` (an absolute
  path), and the live-channel workload `ECHO_BENCH_LIVE_CHANNEL` with its
  `ECHO_BENCH_LIVE_CHANNEL_RECEIPT`, which must name a file under
  `ECHO_BENCH_PROFILE_ROOT` and cannot be combined with the UI fixture.
- UI fixtures (booleans): `ECHO_BENCH_UI_FIXTURE`,
  `ECHO_BENCH_UI_FIXTURE_SETTINGS_ONLY`, `ECHO_BENCH_FEEDBACK_UI`,
  `ECHO_BENCH_RESPONSIVE_MATRIX`, `ECHO_BENCH_LINK_PREVIEW_MATRIX`, with
  `ECHO_BENCH_LINK_PREVIEW_MATRIX_DELAY_MS`.
- Scripted drivers. `ECHO_BENCH_UI_SMOKE`, `ECHO_BENCH_SCROLL_SMOKE` and
  `ECHO_BENCH_LIFECYCLE_CHURN` are booleans tuned by the millisecond and
  count values that share their prefix (`ECHO_BENCH_SCROLL_SMOKE_SEQUENCE`
  is a step list), plus `ECHO_BENCH_LIFECYCLE_CYCLES` and
  `ECHO_BENCH_LIFECYCLE_SETTLE_MS`. The emote-picker sweep runs when
  `ECHO_BENCH_PICKER_SWEEP_CHANNEL` names a channel and is tuned by the other
  `ECHO_BENCH_PICKER_SWEEP_*` and `ECHO_BENCH_PICKER_STABILITY_*` values and
  `ECHO_BENCH_PICKER_ANIMATION_MODE`. `ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS`
  quits Echo through GPUI after that many milliseconds.

## Build-time

`ECHO_BUILD_TARGET`, `ECHO_GPUI_VERSION`, and `ECHO_GPUI_ZED_REV` are set by
`build.rs` and read with `env!`; they are not runtime settings.

| Variable | Effect |
| --- | --- |
| `ECHO_API_PROXY_URL`, `ECHO_API_PROXY_KEY` | Optional. Base URL and access key of a private proxy for Twitch badge, user and stream lookups that need no login (`src/backend/constants.rs`). `build.rs` reads each from the environment, or from `KEY=VALUE` lines in a gitignored `echo-private.env` at the repository root (`#` comments, an `export ` prefix and surrounding quotes are accepted; a trailing `/` on the URL is dropped), and compiles them into the binary. Cargo watches `echo-private.env` only once a build has seen it, so after creating it, or restoring it after a build without it, run `touch build.rs` (or set the variables in the environment) before building. Public builds leave both unset: the build prints a warning that the proxy is disabled, and those lookups go straight to Twitch's Helix API or report that no proxy is configured. |
| `ECHO_REQUIRE_API_PROXY` | Exactly `1` (read by `build.rs`, an exception to the boolean rule) turns the missing-proxy warning into a build error, for builds that must ship with the proxy. |
