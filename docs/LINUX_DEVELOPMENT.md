# Linux development and build verification

Echo supports native X11 and Wayland GPUI backends. Linux validation uses Rust
unit and integration tests, compile and distribution-build checks, executable
linkage validation, and a separate real-GPUI runtime smoke.
Install Rust through `rustup`; the repository's `rust-toolchain.toml` selects the
exact supported toolchain automatically.

## Linux test suite

Linux is the authoritative platform for Echo's Rust tests: the root `echo_gpui`
library tests, the extracted crates, doctests, the standalone contract tools, and
the real-GPUI smoke all run here.

`scripts/test-linux.sh <mode>` runs one of these modes:

| Mode | Stages |
| --- | --- |
| `cargo` (default) | Test inventory check, then root and extracted-crate library tests and doctests with plain `cargo test`. Needs no extra tools. |
| `quick` | `cargo check --workspace --bins`, the test inventory check, the benchmark-profile storage-root self-test, the `cargo` test stages, the tool registry check and every contract tool, and `cargo check -p echo-update-core`. |
| `quick-nextest` | `quick` with nextest running the library tests. This is what CI runs. |
| `nextest` | The test inventory check and the nextest and doctest stages only. |
| `ci` | `quick-nextest` plus compile checks of the three criterion benches. |
| `coverage-root` | LCOV for the root library and every extracted crate. Manual, not a CI gate. |

The library tests cover the root crate plus the ten extracted crates: the eight
GPUI-free policy and domain crates (`echo-autocomplete-core`, `echo-cache-policy`,
`echo-core`, `echo-cosmetics-core`, `echo-emote-core`, `echo-link-core`,
`echo-render-core` and `echo-text-core`), the GPUI-dependent `echo-media-cache`
(RenderImage decoding and the media runtime) and `echo-chat-render` (the canvas
renderer's layout, hit-testing and selection model), and the `cache-preload-check`
tool's library tests.

With nextest, the root and extracted-crate tests go through a *single* invocation
so they share one JUnit report at `target/nextest/ci/junit.xml`. nextest overwrites
that file on every run, so a second invocation would discard the first. No mode
runs the root target twice.

Every compile, test, doctest, benchmark-check and coverage stage runs through
`scripts/run-linux-stage.sh`. The runner prints START/PASS/FAIL/TIMEOUT records,
emits a heartbeat every 30 seconds with a compact child-process snapshot, keeps a
per-stage log under `target/linux-test-stages/`, and terminates timed-out or
interrupted process groups with TERM then KILL. Default stage limits are 15
minutes. Override them with `ECHO_LINUX_COMPILE_TIMEOUT_SECONDS`,
`ECHO_LINUX_TEST_TIMEOUT_SECONDS`, `ECHO_LINUX_COVERAGE_TIMEOUT_SECONDS` or
`ECHO_LINUX_TOOL_TIMEOUT_SECONDS`, and the heartbeat with
`ECHO_LINUX_STAGE_HEARTBEAT_SECONDS`. The harness sets `RUST_MIN_STACK` to 256 MiB
unless it is already set.

The nextest modes require cargo-nextest 0.9.143 or newer. Its configuration is
`.config/nextest.toml`. Retries are disabled and the CI profile fails flaky
tests: deterministic compile or assertion failures must be fixed, not hidden.
nextest does not run doctests, so the harness keeps an explicit
`cargo test --doc` pass for both backends.

The eight policy and domain crates stay GPUI-free; `echo-media-cache` depends on
GPUI for `RenderImage` decoding and `echo-chat-render` for the canvas renderer's
geometry model. Benchmark compilation uses three explicit stages
(`--bench layout_bench`, `--bench filter_bench` and `--bench loading_bench`)
rather than `--benches`, which would also select the root libtest target.

### Extracted crates and ports

`echo-media-cache` owns the in-memory decoded-image layer, the animation frame
layer, and the two process-wide registries every layer shares: the deferred GPUI
sprite-atlas release queue and the emote picker's published visible-media sets.
`echo-cache-policy` owns the bounded loaded-URL dedup registry, `echo-render-core`
the `AnimationMetadata` timing model that the dynamic tick scheduler reads,
`echo-link-core` the image-uploader configuration and response-template policy,
and `echo-core` the message deduplicator, channel observer registry, channel
lifecycle state machine, hotkey model, OAuth token sanitizing, custom-command
trigger matching, notification truncation, and slot resolver. Their root modules
are compatibility facades, so every `crate::backend::...` and `crate::cache::...`
path still resolves.

Root lookups that the extracted code still needs are installed fn-pointer ports,
each set from the site that first needs it: `echo_core::install_log_category_enabled`
from `log::init`; `echo_core::channel_lifecycle::install_whispers_channel_predicate`
from the root lifecycle static; `echo_core::slot_resolver::install_twitch_emote_lookup`,
`install_emoji_style`, and `install_emote_trace_enabled` from
`backend::bridge::init::initialize`; and
`echo_media_cache::frames::install_retained_decoded_url_predicate`,
`install_animated_frame_prefetch`, and `install_visible_animation_count` from the
loader's `FRAME_LAYER` initializer. Every default is the inert or already-reachable
behavior (an emote-map miss, the default emoji style, tracing off, nothing
retained, no prefetch, no visible animations), so a crate that runs standalone
degrades rather than guesses.

### Inventory and tool registry

`scripts/check-linux-test-inventory.sh` compares the repository against the
baseline in `scripts/linux-test-inventory.tsv`: the exact path and name of every
test attribute in the root crate, each extracted crate and the `cache-preload-check`
tool, the number of contract tools, and the runnable and ignored doctest counts.
Adding, removing, moving or renaming a test requires updating the baseline.

`scripts/linux-test-targets.tsv` classifies every `tools/*/Cargo.toml` binary
target as `contract`, `fixture`, `runtime`, `benchmark` or `helper`.
`scripts/test-linux-tools.sh` first checks that the registry matches the manifests
exactly, then runs all 38 contracts (deterministic, no-argument checks) and writes
one report per tool under `target/linux-tool-checks/`.

### Clippy and coverage

Clippy checks `echo_gpui` and the ten extracted crates and blocks the
`correctness` and `suspicious` lint groups in Linux CI (`make lint`). Existing
style and complexity warnings, including intentional high-arity render
boundaries, are not a gate; `make lint-strict` fails on every warning.

Coverage is a manual command:

```sh
./scripts/test-linux.sh coverage-root
```

It requires cargo-llvm-cov and writes `target/coverage/echo.lcov` for the root
library *and* every extracted crate in one `cargo llvm-cov --lib` run, so the
report covers the same code the suite tests. Instrumented builds are much slower
than the ordinary run, so the workflow has no coverage job and there is no
percentage threshold.

Upstream references:

- Cargo test: <https://doc.rust-lang.org/cargo/commands/cargo-test.html>
- nextest configuration: <https://nexte.st/docs/configuring-nextest/>
- cargo-llvm-cov: <https://github.com/taiki-e/cargo-llvm-cov>
- GitHub Rust CI: <https://docs.github.com/en/actions/tutorials/build-and-test-code/rust>

## One-command verification

```sh
./scripts/verify-linux-build.sh
```

The harness checks native build dependencies, reconstructs or validates the pinned
GPUI fork, compiles the locked Echo binary, builds the `dist` Echo and `echo-version`
executables, and verifies their ELF type, runtime linkage, executable stack policy,
and version identity. It does not launch a GPUI window.

`.github/workflows/linux.yml` runs on pull requests and pushes to the development
branches: `cargo fmt --check`, `./scripts/test-linux.sh quick-nextest`, Clippy,
`./scripts/verify-linux-build.sh`, and a startup smoke of the `dist` binary under
Xvfb. It uploads the test reports and the two verified Linux binaries as
short-lived CI artifacts. It does not publish a release or an update.

## Native dependencies

Run the preflight directly when diagnosing a machine:

```sh
./scripts/check-linux-build-deps.sh
```

Arch/CachyOS:

```sh
sudo pacman -S --needed base-devel pkgconf python perl util-linux alsa-lib fontconfig openssl vulkan-headers vulkan-icd-loader wayland libxcb libxkbcommon libxkbcommon-x11
```

Debian/Ubuntu:

```sh
sudo apt install build-essential pkg-config python3 perl util-linux libasound2-dev libfontconfig1-dev libssl-dev libvulkan-dev libwayland-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev binutils file
```

## GPUI fork reconstruction

Linux and other Bash environments use:

```sh
./scripts/setup-gpui-fork.sh
```

Windows and PowerShell environments use:

```pwsh
./scripts/setup-gpui-fork.ps1
```

Both scripts consume the ordered `scripts/gpui-fork-patches.txt` manifest and reject
a tracked or stale `../zed-gpui-fork` branch. They never silently overwrite a fork
whose source tree differs from the expected pinned patch series.

Useful narrower commands are exposed through `make linux-deps`, `make setup-gpui`,
`make check`, `make dist`, and `make linux-verify`.

## Desktop use on CachyOS and KDE Plasma

Echo runs on Plasma 6 under Wayland (the CachyOS default) and under X11.

1. Install the build dependencies above, `rustup` (or another way to get the
   pinned toolchain), a Vulkan driver (`vulkan-radeon`, `vulkan-intel`, or
   `nvidia-utils`), `pipewire-alsa` (Echo plays sounds through ALSA), and fonts
   (`noto-fonts` for text, `ttf-dejavu` for wide symbol and braille coverage in
   ASCII art).
2. Clone Echo next to an empty `../zed-gpui-fork` path and run
   `./scripts/setup-gpui-fork.sh`. It fetches Zed at the pinned revision and
   applies `patches/`; Linux cannot reuse a Windows fork checkout.
3. Run `make install`. It builds the `dist` profile and installs
   `~/.local/share/echo/bin/echo`, a `~/.local/bin/echo-gpui` link, the
   `com.echo.chat.desktop` launcher entry, and its icon. Never put the binary on
   `PATH` as `echo`: that name shadows coreutils' `echo`. Rerun `make install`
   after `git pull` to update; Linux has no automatic updater. `make uninstall`
   removes the files again and leaves settings and caches in place.

Every Echo window uses the app id `com.echo.chat` (the Wayland `app_id` and the
X11 `WM_CLASS`), so Plasma groups popouts under one task-manager entry and takes
the icon and name from the launcher entry. Plasma behavior:

- KWin draws the title bar (server-side decorations), so Echo hides its own
  minimize, maximize, and close buttons. Dragging or double-clicking Echo's top
  bar still moves or maximizes the window. A compositor that refuses
  server-side decorations, such as GNOME's, gets Echo's buttons instead, but
  Echo does not draw resize borders there.
- Always on top, and the task-manager attention flag for mentions, go through
  KWin's scripting D-Bus API (`org.kde.KWin /Scripting`), because neither
  Wayland nor GPUI can request them. Echo writes a short script to its runtime
  directory that matches the main window by process id, app id, and title.
  Outside KWin both are unavailable.
- Echo runs one instance per profile. The lock and the activation socket live
  in `$XDG_RUNTIME_DIR/echo/`. A second launch forwards its
  `XDG_ACTIVATION_TOKEN`, so KWin raises the running window instead of only
  flagging it. Launching again while Echo is still shutting down waits up to
  10 seconds for it to exit, then starts normally. X11 sessions forward no
  startup id, so KWin may only flag the window there.
- Closing the main window quits Echo even while popouts are open. SIGTERM,
  SIGINT, and SIGHUP (session logout, `kill`, a closed terminal) quit through
  the same settings, layout, and history shutdown path; a second signal exits at
  once.
- Desktop notifications appear only while no Echo window is active. They are
  filed under the `com.echo.chat` launcher entry, so System Settings >
  Notifications > Applications can configure them, and their text is escaped
  because Plasma renders notification bodies as markup.
- Text uses the General and Fixed width fonts from System Settings >
  Text & Fonts (read from `~/.config/kdeglobals`), falling back to Noto Sans and
  a common monospace family. GPUI's own default, IBM Plex Sans, is not bundled
  with Echo.
- Copy and paste go through GPUI's native Wayland (or X11) clipboard rather
  than XWayland, because KWin lets X11 clients read the clipboard only while an
  X11 window is focused. Clipboard file lists are not read on Linux; drag files
  onto the composer instead.
- Tokens stay in `~/.config/echo/tokens.json` with mode `0600`; KWallet and the
  Secret Service are not used yet.
- Not available on Linux: the tray icon (minimize or close to tray), the browser
  extension's Watching split, and a click-through stream overlay.

Settings live in `~/.config/echo/`, local data and logs in `~/.local/share/echo/`,
and caches in `~/.cache/echo/`. Windows keeps them under `%APPDATA%\echo` and
`%LOCALAPPDATA%\echo`. To carry settings over from a Windows install, copy
`settings.json` and `sounds.json` from the Windows profile into
`~/.config/echo/` while Echo is closed. Sign in again on Linux, because Windows
tokens are sealed to the Windows user. Custom sound files with Windows paths
need re-selecting.

Before trusting a build, run `./scripts/verify-linux-build.sh`,
`./scripts/test-linux.sh quick`, and the `./scripts/test-linux-app-smoke.sh`
runtime smoke described below inside the Plasma session.

## Bounded runtime smoke

For the ordinary "does the application work?" check, run:

```sh
./scripts/test-linux-app-smoke.sh
```

This is one realistic happy-path smoke, not a feature matrix. It builds the
current debug binary, opens the real GPUI application under an isolated profile,
injects three deterministic chat messages from
`benchmarks/fixtures/chat-replay-normal.ndjson` through the production replay
path, validates the runtime log and replay report, then asks Echo to quit through
GPUI so backend, settings, history, and cache shutdown markers are required before
exit. It requires a real X11 or Wayland session and keeps artifacts under
`target/linux-app-smoke/`, including the isolated profile and runtime log. Set
`ECHO_APP_SMOKE_SKIP_BUILD=1` only when deliberately reusing an existing binary, or
`ECHO_APP_SMOKE_BINARY` to exercise another build.

The underlying profiler runner, `scripts/bench/run-echo-profile.py`, is the
lower-level runtime smoke for profiling or custom fixtures (see
`docs/BENCHMARKING.md`). It launches the real GPUI binary with isolated Echo-owned
state, requires the process to stay alive for the requested observation, captures
artifacts, resolves the profile log, and fails if startup exits early, produces no
runtime log, or fails log validation. Run `./scripts/verify-linux-build.sh` first
so `target/dist/echo` exists. A real X11 or Wayland session is required; for
headless automation, start a virtual display with Xvfb first.

```sh
echo_smoke_root="$(mktemp -d)"
cargo build --release --locked -p profile-log-check
ECHO_BENCH_PROFILE_ROOT="$echo_smoke_root/profile" \
python3 scripts/bench/run-echo-profile.py \
  --output-root "$echo_smoke_root/artifacts" \
  --scenario "Linux dist startup smoke" \
  --launch-path target/dist/echo \
  --duration-seconds 8 \
  --exit-wait-seconds 5 \
  --stop-mode close-main-window \
  --profile-log-check-path target/release/profile-log-check \
  --echo-build-type dist \
  --cache-state isolated \
  --fixture-name linux-startup
```

On Linux, `close-main-window` uses a bounded `SIGTERM` stop because the Python
standard library cannot request a compositor-native window close. The manifest
records `graceful_close_requested=false`. Use `--app-owned-quit-after-ms`, as the
application smoke does, when graceful GPUI and backend shutdown must be verified.

## The root test target and `gpui::test`

### `gpui::test` shadows the built-in `#[test]`

`gpui` re-exports the proc-macro attribute `gpui::test` from its crate root. Any module
that does `use gpui::*;`, or the brace form `use gpui::{HighlightStyle, StyledText, *};`,
therefore binds the name `test`, and a glob-imported macro name shadows the built-in
attribute from the standard prelude. gpui's macro expands to `#[test]` again
(`gpui_macros/src/test.rs` emits `parse_quote! { #[test] ... }`), which resolves back to
`gpui::test` at the same call site, so expansion recurses without a fixed point. In a
crate at the default recursion limit this fails immediately with "recursion limit
reached while expanding `#[test]`". The root crate sets `#![recursion_limit = "4096"]`
(`src/lib.rs`), which turns that fast error into a 4096-deep expansion: minutes of
100%-CPU rustc per affected test module, and eventually a stack overflow.

Every test module therefore imports the built-in attribute explicitly:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    // ...
}
```

An explicit, non-glob import always wins over a glob import regardless of source order, so
`rustfmt` is free to sort this line wherever it likes. With every root test module carrying
it, `cargo check --locked -p echo_gpui --lib --tests` completes in well under a minute.

### The guard

`scripts/check-linux-test-inventory.sh` ends with `check_test_shadow_guard`, which requires
`use core::prelude::v1::test;` inside **every** `#[cfg(test)] mod` block under `src/`. The
rule is deliberately over-approximate: deciding which module chains can observe a gpui glob
is not worth the fragility, and the import is harmless where it is not strictly needed. It
does not skip anything silently; it reports, and fails, on each of:

- a `#[cfg(test)] mod NAME { ... }` block that lacks the import (`missing`);
- the `#[cfg(test)] mod NAME;` file-module form, which it cannot inspect in place
  (`unhandled`);
- a `#[test]` or `#[tokio::test]` outside any `#[cfg(test)] mod` block (`stray`);
- a block whose canonically indented closing brace cannot be located (`unhandled`).

Block extent is anchored on rustfmt's indentation rather than brace counting, so braces
inside `format!` strings and string literals cannot confuse it. Crates under `crates/` are
not covered: they use explicit `gpui` imports instead of globs.

### Stale tests

Never add production API to make a stale test compile. Find the commit that moved or
removed the API with `git log -S'<symbol>' -- <file>`, then either adapt the test minimally
to the current API while keeping its name and intent, or delete it as obsolete. A
`cfg(test)` fixture constructor that a dependent crate's tests need goes behind that
crate's `test-support` feature (as `echo-cache-policy` does for
`BudgetManager::with_budget`), enabled only by the dependent's dev-dependency, so no
fixture ships in a normal build.

### Modules left in the root crate

The root target runs without further extraction. Two renderer modules were
assessed and left in place:

`text_shaping.rs` has the widest surface in the renderer: it needs
`backend::emoji::{TextSegment, parse_emoji}`,
`backend::emotes::{Emote, chat_display_url_shared}`,
`backend::slots::{MessageSlot, render_units::{EmoteRenderUnit, SlotRenderUnit,
resolve_render_units}, resolver::SlotResolver}`,
`backend::irc::{get_user_color, resolve_user_color}`,
`backend::cosmetics::get_cached_user_cosmetics`, `backend::badges::badge_key_matches`,
`backend::ascii_art::classify`,
`backend::embed_thumbnails::{should_render_thumbnail, thumbnail_dimensions}`,
`backend::link_types::EmbedMetadata`, `backend::bridge::is_embed_expanded_state`,
`backend::streamer_mode::{should_hide_link_urls, should_hide_link_thumbnails}`,
`backend::settings::{apply_nickname_rules, get_embed_display_mode, EmbedDisplayMode,
DeletedMessageMode}`, `theme::colors`, `log::emote_trace_enabled`, the `log_perf_render!`
and `log_emotes!` macros, and the four root-only `layout` items below. Moving it means
moving the emote/badge/cosmetic resolution and embed-policy layers first, or accepting a
port surface of roughly twenty callbacks.

`async_layout.rs` is blocked only on that. Everything else it needs already
exists: `echo_core::message::ChatMessage`, `echo_core::sync::{MutexExt, RwLockExt}` (already
the body of `backend::lock_utils`), and `echo_core::version_registry::VersionRegistry` for
both `slots::resolver::get_emote_version` and `state::VersionRegistry::channel_emote_version`.
It would additionally need a `crossbeam-channel` dependency, a `layout_settings_version`
port, and a `perf_render` log port for its six `log_perf_render!` sites. Its one remaining
tie to the root is
`text_shaping::build_message_layout(&ChatMessage, &str, f32, bool, Option<&mut Window>) ->
CanvasMessageLayout`; installing that as a fn-pointer port would invert the dependency
direction.

For reference, `echo-chat-render` owns the layout model, hit testing, selection, paint-layer
ordering, width bucketing, RTL hit mapping, line buffering, font metrics, the 7TV name-paint
recipe, and GPUI text-run interning. Its ports (`ports.rs`, each set by an `install_*`
function) are `font_scale`, `chat_density_multiplier`, `badge_scale`, `emote_scale`,
`clipboard_writer` (read through `ports::copy_text`), `text_primary`, `decoded_image_size`,
and `badge_set_pending`, plus `echo_core::install_log_category_enabled`;
`install_render_ports()` binds them from `main.rs`. Theme-resolved row colors stay in the root as the `HighlightColors` and
`LowTrustColors` extension traits, and `get_cached_color`, `get_cached_timestamp`,
`timestamp_width`, and `refresh_timestamp_width_probe` stay in the root `layout` facade.
