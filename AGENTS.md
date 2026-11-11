# AGENTS.md

This file is the working contract for anyone (human or coding agent) changing
Echo. Read it before starting work.

## Mission

Echo is a native Rust chat client for Twitch, built on GPUI. It supports Twitch
only. The project is well into implementation: extend and refine what exists
rather than restarting from first principles, and keep the existing
architecture unless measurement shows it has to change.

Echo aims to be a complete, dependable chat client whose responsiveness, memory
usage, CPU and GPU usage, frame pacing and long-session stability are measured
rather than assumed.

## Before changing code

Inspect the current repository state and the relevant docs, then the code path
you are about to change. Prefer updating existing docs over creating new plans:

- `docs/ARCHITECTURE.md`: data flow, source map, renderer invariants
- `docs/GPUI_FORK.md`: the GPUI fork and its patch series
- `docs/ENVIRONMENT.md`: every `ECHO_*` variable
- `docs/BENCHMARKING.md`: the benchmark harness
- `docs/LINUX_DEVELOPMENT.md`: the Linux test platform
- `docs/LINUX_DESKTOP.md`: how Echo installs and behaves on the Linux desktop
- `docs/MEMORY_PROFILING.md`: memory attribution
- `docs/DEPENDENCY_POLICY.md`: dependency and supply-chain rules

## Rust architecture direction

Preserve and improve Echo's Rust-native stack. Do not introduce WebView,
Electron, DOM rendering, or browser-wrapper architecture. Optimize the hot path
rather than rewriting the application.

Every Echo-owned native GPUI window must be opened through
`components::ui::open_native_window`, which installs the bounded shared URI
image-cache provider at the outermost root. Run
`pwsh -NoProfile -File scripts/check-native-window-roots.ps1 -SelfTest` after
adding or changing a native-window root, and update that checker's explicit
caller list in the same slice.

Every Echo-owned config, local-data, or cache path must route through
`storage_roots`. `ECHO_BENCH_PROFILE_ROOT` isolates experiments from normal
user state, including the Windows keyring; do not add direct
`dirs::config_dir`, `dirs::data_local_dir`, `dirs::cache_dir`, manual
`HOME/.config`, or legacy cwd storage roots outside that module. Run
`pwsh -NoProfile -File scripts/check-storage-roots.ps1 -SelfTest`
after changing Echo-owned storage roots. OS-external discovery (for example
OBS and Discord logs) must remain outside that profile and skip while it is
active.

Preferred performance direction:

- one custom chat-log canvas rather than an element per token
- a virtualized message list with bounded scrollback
- low-allocation message parsing and layout, with shaped-layout caches
- async image and emote loading and decoding off the UI thread
- batched GPU textures, a bounded CPU image cache and bounded GPU residency
- LRU or expiry-based emote eviction
- animation ticks only for visible animated content
- event-driven redraws instead of constant repaint loops
- frame pacing and input responsiveness over raw throughput
- viewport-first loading and progressive refinement
- no synchronous full emote-set decode or upload at startup

## Performance rules

Performance claims require evidence. Do not describe a change as faster,
lighter, lower-memory, lower-CPU, lower-GPU or smoother unless it was measured.

When working on performance-sensitive slices, prefer measuring:

- startup time and time to the first usable chat view
- idle and active memory under chat load
- CPU under normal and high-volume chat
- GPU and frame time while rendering dense chat
- animated emote frame cost
- cold and warm emote cache behavior
- scrolling frame pacing
- multi-split and multi-channel behavior
- long-session memory growth and cleanup after leaving channels

Record the measurement method and results with the change (commit message or
pull request description). `docs/BENCHMARKING.md` describes the harness. A slice
that makes no performance claim does not need a benchmark run.

## Validation policy

### Canonical formatting

- The repository's pinned toolchain and `rustfmt.toml` define Rust 2024 language
  and style formatting.
- Use `cargo fmt --all` only for an intentional workspace-formatting pass.
- For a narrow slice, format exact files with `rustfmt <path> [<path> ...]`.
- Do not pass source paths after `cargo fmt --`; Cargo forwards arguments after
  `--` to rustfmt while still selecting Cargo targets, which can create unrelated
  formatter churn.
- Canonical `cargo fmt --all -- --check` output is authoritative. Do not revert a
  formatter-only delta merely to match noncanonical wrapping or import order.

### Platforms

Linux is Echo's authoritative Rust test platform. `cargo test`, `cargo nextest`,
unit tests, integration tests, and doctests are allowed on Linux when they are
proportional to the slice or part of an explicit milestone. Set
`RUST_MIN_STACK=268435456` for Echo's large GPUI test target on every platform:
without it rustc overflows its stack compiling that target. With it, the root
library tests also run on Windows (`cargo test -p echo_gpui --lib`), which is a
useful pre-push check but does not replace the Linux run, since platform-gated
code and tests differ. GPUI-free extracted crates test on Windows as they are.

### Proportional validation ladder

Do not run validation commands reflexively, and do not run full builds or test
suites at the end of every slice by default. Pick the level before making
changes.

#### Level 0: docs only

Use for documentation, planning notes, and benchmark documentation.

- inspect the relevant files
- review markdown consistency
- review `git diff`

Do not run builds or tests.

#### Level 1: low-risk code or config

Use for small refactors, comments, minor config cleanup, or non-hot-path changes.

- targeted file inspection and search
- review affected call sites
- review `git diff`

Do not run builds or tests unless the change touches public API or
compile-sensitive code.

#### Level 2: targeted compile-risk validation

Use when changing Rust types, public APIs, feature gates, module wiring, renderer
interfaces, async boundaries, or dependency-sensitive code.

- the smallest existing command that checks the affected area (for example
  `cargo check -p echo_gpui --bins` or `cargo check -p <crate>`)
- targeted Linux unit or integration tests that directly cover the change
- no full release build unless necessary

#### Level 3: performance-sensitive validation

Use when changing chat rendering, layout, text shaping, image decode, emote cache,
GPU upload, atlas behavior, animation ticking, batching, or memory ownership.

- the existing benchmark or profiling script for the affected scenario
- a targeted manual or scripted smoke run where the repository supports it
- before/after measurement when practical, recorded with the change

Do not run unrelated full test or build suites.

#### Level 4: milestone validation

Use only at explicit milestones, before major merge points, or after broad
architecture changes accumulate.

- broader build, check, smoke and benchmark commands already documented by the
  repository
- Linux `cargo test`, `cargo nextest`, doctests, coverage, and runtime smoke paths
  defined by the repository's test automation

## Slice workflow

Work in small vertical slices. A slice usually addresses one of:

- a missing or incomplete feature
- a rendering correctness issue
- an emote or cosmetics behavior gap
- a memory, CPU or GPU hot path
- a cache or eviction improvement
- a scroll or input responsiveness problem
- a benchmark or profiling harness improvement
- a small architecture cleanup that directly enables one of the above

Before each slice:

1. Read the relevant docs.
2. Inspect the current code path.
3. Identify the exact gap or hot path.
4. Choose the smallest useful implementation step.
5. Decide the validation level.

During each slice:

1. Keep changes narrow.
2. Preserve existing behavior unless intentionally changing it.
3. Avoid speculative rewrites and broad dependency changes.
4. Avoid touching unrelated formatting.
5. Update docs only where the slice changes what they describe.

After each slice:

1. Review `git diff`.
2. Update the affected docs.
3. Record the validation performed, or why no build, test or benchmark was run.
4. Commit the completed slice before starting the next one.

## Commit rules

Each completed slice ends with one commit. Do not commit broken or incoherent
states, and do not combine unrelated feature, UI, renderer, cache and benchmark
work in one commit.

Commit messages use `slice(<area>): <summary>`, for example:

- `slice(feature): add <feature>`
- `slice(render): fix <render behavior>`
- `slice(perf): reduce <hot path cost>`
- `slice(cache): bound <cache or resource>`
- `slice(bench): add <scenario>`
- `slice(ui): <interaction or layout change>`
- `slice(test): cover <behavior>`
- `slice(docs): update <document>`

A commit without a test or build run is fine when the chosen validation level did
not require one and the diff is coherent.

## Definition of done for a slice

- the intended gap or hot path was addressed
- existing behavior was preserved unless intentionally changed
- affected docs were updated
- validation was proportional to risk
- measurements were recorded if performance was claimed
- `git diff` was reviewed
- a commit was created

## Non-goals

Do not:

- restart the project plan from zero
- perform a full rewrite without proof it is necessary
- introduce Electron, WebView, DOM rendering, or browser-wrapper architecture
- remove implemented features to simplify the code
- optimize blindly without profiling or a clear hot-path reason
- run the root crate's Rust test graph on Windows
- run broad Linux test suites reflexively after every narrow slice
- run full builds at the end of every slice by default
- make performance claims without evidence
