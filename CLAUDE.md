# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**Read `AGENTS.md` first.** It is the working contract: mission, the slice workflow, the proportional validation ladder (Level 0–4), the commit-message convention (`slice(<area>): ...`, one commit per slice), and the non-goals. This file only adds orientation that AGENTS.md does not cover.

## What Echo is

A native Rust chat client for Twitch built on Zed's **GPUI**. Workspace version lives in the root `Cargo.toml` (`[workspace.package]`); toolchain is pinned by `rust-toolchain.toml` (Rust 2024 edition). The root package is `echo_gpui` (lib + bin `echo`, bin has `test = false`).

## Build prerequisites: the GPUI fork

`Cargo.toml`'s `[patch."https://github.com/zed-industries/zed"]` redirects `gpui`, `gpui_platform`, `reqwest_client` to a **sibling checkout `../zed-gpui-fork`** (branch `echo-paint-fork`). Nothing compiles without it. Reconstruct/verify it from the pinned Zed rev plus the series in `patches/`:

- Windows: `pwsh -NoProfile -File scripts/setup-gpui-fork.ps1`
- Linux: `./scripts/setup-gpui-fork.sh` (or `make setup-gpui`)

Patch order and rationale: `docs/GPUI_FORK.md`, `scripts/gpui-fork-patches.txt`. Changing the fork means adding/updating a patch file there too. The Zed revision is pinned in the root `Cargo.toml`'s `[workspace.dependencies]` (with the other dependencies shared across members, which inherit them via `<name>.workspace = true`); `build.rs` and the setup scripts read it from the one-line `gpui = { ... rev = ... }` entry there.

Optional build-time variables `ECHO_API_PROXY_URL`/`ECHO_API_PROXY_KEY` (environment or a gitignored `echo-private.env`) configure a private badge/user-lookup proxy; builds without them print a warning and work without the proxy. See `docs/ENVIRONMENT.md`.

## Commands

Windows (primary dev machine):

- App-crate compile gate: `cargo check -p echo_gpui --bins`
- Lint: `cargo lint`, the one Clippy gate (alias in `.cargo/config.toml`: workspace libs and bins, `-D clippy::correctness -D clippy::suspicious`; no test targets, because the GPUI lib test target overflows rustc's stack on Windows). `make lint` and CI run the same alias; `cargo lint-strict` is the `-D warnings` ratchet.
- Run: `cargo run` / `cargo run --release`; fixture tools: `cargo run -p <tool-name>`
- Distribution build: `cargo build --profile dist --locked --bin echo` (thin LTO, ~4 min). `--release` is fat LTO with one codegen unit, so even an incremental release build of `echo_gpui` takes ~15 min. Both profiles use `panic = "abort"`, so `catch_unwind` recovery (e.g. the layout workers) only works in dev/test builds.
- Format: `rustfmt <exact files>` for a slice; `cargo fmt --all -- --check` is canonical (never pass paths after `cargo fmt --`)
- Do **not** run `cargo test` for the root crate on Windows. GPUI-free extracted crates test fine there, e.g. `cargo test -p echo-core --lib chat_snapshot`, `cargo test -p echo-render-core --lib animation_timing`.

Linux (authoritative test platform, see `docs/LINUX_DEVELOPMENT.md`):

- `./scripts/test-linux.sh quick` — root lib tests + all extracted crates + doctests + standalone contract tools
- other modes: `cargo`, `quick-nextest`, `nextest`, `ci`, `coverage-root`
- Single test: `RUST_MIN_STACK=268435456 cargo test --locked -p <crate> --lib <test_name>`

Guard scripts: `docs/ARCHITECTURE.md` ("Guard scripts") is the complete list. The two most often needed, run with `-SelfTest` after touching the relevant area:

- `scripts/check-native-window-roots.ps1` — every Echo native window must open via `components::ui::open_native_window` (update the checker's caller list in the same slice)
- `scripts/check-storage-roots.ps1` — every config/data/cache path must go through `src/storage_roots.rs` (`ECHO_BENCH_PROFILE_ROOT` isolation)

Runtime logging: stdout carries WARN+ unless `ECHO_LOG`, an `ECHO_LOG_*` switch or `RUST_LOG` enables more. A release build with no logging env vars writes WARN+ plus a session header to `%LOCALAPPDATA%\echo\echo.log` (5 MB, rotates to `echo.log.1`; under `<profile>\logs` with `ECHO_BENCH_PROFILE_ROOT`). For diagnostics set `ECHO_LOG_FILE=1` and `ECHO_LOG=<targets>` (categories incl. `app`, `persist`, `update`) and read the same file. Never log tokens, URLs with credentials/query (`log::redact_url`), message bodies or clipboard text. Every `ECHO_*` variable is listed in `docs/ENVIRONMENT.md`.

Perf diagnostics are `key=value` log lines, which is what `tools/profile-log-check` and the benchmark rigs parse: `ECHO_LOG_PERF_RENDER` emits a per-split "Canvas render/layout aggregate" line every 2 s (on render), `ECHO_FPS_PROFILER=1` a frame/GPU/present line every 30 root frames, and `ECHO_MEM_REPORT_SECS=N` a "memory breakdown" line. Other counters (`metrics::record_ui_notify_source`, `record_emote_stage`, `record_emote_event`) are gated by `ECHO_PERF_MONITOR`/the profiler and are read through the metrics JSON (`backend::bridge::events::get_metrics_json`) and the metrics overlay. For release A/B work, use `scripts/bench/run-perf-rig.ps1` (isolated `ECHO_BENCH_PROFILE_ROOT`, `ECHO_CHAT_REPLAY` fixture from `benchmarks/fixtures/perf-rig/`, warm-up, per-split summary) once per binary in ABBA order; `docs/BENCHMARKING.md` covers the rest of the harness.

## Architecture

`docs/ARCHITECTURE.md` is the full map: data flow (backend on Tokio → `src/backend/bridge/` → GPUI state entities in `src/state/` → the canvas renderer in `src/components/chat/canvas_renderer/`), the source map, cross-file flows (message ingress, duplicate-channel splits, per-canvas wake loops, animation patching, async layout, direct-canvas vs element-fallback rows), renderer invariants, manual renderer checks, settings UI conventions, and workspace members. Read the relevant section before touching the renderer.

Points that most often matter for a change:

- Backend work goes through `src/backend/bridge/`; UI code may import backend types and call read-only lookups directly. The settings UI (and any component persisting a preference) calls `crate::backend::settings` directly: that is the documented exception. Cross-thread change detection is **version polling** (`crates/echo-core/src/version_registry.rs`), not push notifications.
- Rendering is event-driven with mode-aware frame caps; don't add constant repaint loops.
- Split-specific actions carry the source `SplitId` (`find_open_split_target` in `src/app.rs`); channel-only actions resolve to the tab's active split.
- `tools/*` compile real Echo source via `#[path = "../../../src/..."]` alongside hand-written **stubs**, so changing a signature in `src/` can break a tool's stubs. Tools compile in workspace gates; the Linux test script runs the contract tools (`scripts/linux-test-targets.tsv` classifies every tool).

## Docs

- `docs/ARCHITECTURE.md` — architecture, invariants, manual renderer checks, settings UI conventions.
- `docs/GPUI_FORK.md` — the GPUI fork and its patch series.
- `docs/BENCHMARKING.md`, `docs/MEMORY_PROFILING.md` — performance measurement.
- `docs/LINUX_DEVELOPMENT.md` — Linux build and test platform.
- `docs/LINUX_DESKTOP.md` — installing and running Echo on the Linux desktop (KDE Plasma).
- `docs/ENVIRONMENT.md` — every environment variable.
- `docs/DEPENDENCY_POLICY.md` — `cargo deny` gate and advisory exceptions.
- `docs/PLUGINS.md` — why Echo has no scripting API.
- `docs/7TV_EVENTAPI_REPLAY.md` — replaying 7TV EventAPI fixtures.

Publishing is handled outside this repository. Only `.github/workflows/linux.yml` runs in CI. Never tag, publish, or enable workflows without explicit authorization.
