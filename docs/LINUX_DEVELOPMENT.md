# Linux development and build verification

Echo supports native X11 and Wayland GPUI backends, and Linux is the
authoritative platform for its Rust tests. This page covers building on Linux
and the test, verification and smoke harness. Running Echo as a desktop
application on Linux (install, KDE Plasma behavior, where settings live) is in
[LINUX_DESKTOP.md](LINUX_DESKTOP.md).

Install Rust through `rustup`; the repository's `rust-toolchain.toml` selects the
exact supported toolchain automatically.

## Native dependencies

```sh
./scripts/check-linux-build-deps.sh   # or: make linux-deps
```

The preflight checks the commands and pkg-config modules the build needs and,
when any is missing, prints the install command for Arch/CachyOS and for
Debian/Ubuntu. The script is the one list; CI installs the same Debian packages
in `.github/workflows/linux.yml`.

## GPUI fork reconstruction

```sh
./scripts/setup-gpui-fork.sh   # or: make setup-gpui
```

The PowerShell twin is `pwsh -NoProfile -File scripts/setup-gpui-fork.ps1`.
Both reconstruct or validate `../zed-gpui-fork` from the revision Cargo.toml
pins and the ordered series in `scripts/gpui-fork-patches.txt`, and reject a
fork with tracked changes or a source tree other than the manifest's
`fork-tree`. They never overwrite a stale branch. Linux cannot reuse a Windows
fork checkout. [GPUI_FORK.md](GPUI_FORK.md) describes the patches.

## Linux test suite

`scripts/test-linux.sh <mode>` runs one of these modes:

| Mode | Stages |
| --- | --- |
| `cargo` (default) | Test inventory check, then root and extracted-crate library tests and doctests with plain `cargo test`. Needs no extra tools. |
| `quick` | `cargo check --workspace --bins`, the test inventory check, the storage-roots guard self-test, the Python bench-script tests (`python3 -m unittest discover -s scripts/bench/tests`), the `cargo` test stages, the tool registry check and every contract tool, and `cargo check -p echo-update-core`. |
| `quick-nextest` | `quick` with nextest running the library tests. This is what CI runs. |
| `nextest` | The test inventory check and the nextest and doctest stages only. |
| `ci` | `quick-nextest` plus compile checks of the three criterion benches. |
| `coverage-root` | LCOV for the root library and every extracted crate. Manual, not a CI gate. |

The library tests cover the root crate, every extracted crate under `crates/`
(the script reads the directory list, so a new crate joins automatically), and
the `cache-preload-check` tool's library tests. To run one test:

```sh
RUST_MIN_STACK=268435456 cargo test --locked -p <crate> --lib <test_name>
```

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

The nextest modes require at least the cargo-nextest version named by
`nextest-version` in `.config/nextest.toml`, which is also the version CI
installs. Retries are disabled and the CI profile fails flaky tests:
deterministic compile or assertion failures must be fixed, not hidden. nextest
does not run doctests, so the harness keeps an explicit `cargo test --doc` pass
for both backends.

Benchmark compilation uses three explicit stages (`--bench layout_bench`,
`--bench parse_bench` and `--bench loading_bench`) rather than `--benches`,
which would also select the root libtest target.

### Inventory and tool registry

`scripts/check-linux-test-inventory.sh` compares the repository against the
baseline in `scripts/linux-test-inventory.tsv`: the exact path and name of every
test attribute in the root crate, each extracted crate and the `cache-preload-check`
tool, the number of contract tools, and the runnable and ignored doctest counts.
Adding, removing, moving or renaming a test requires updating the baseline.

`scripts/linux-test-targets.tsv` classifies every `tools/*/Cargo.toml` binary
target as `contract`, `fixture`, `runtime`, `benchmark` or `helper`.
`scripts/test-linux-tools.sh` first checks that the registry matches the manifests
exactly, then runs every contract (deterministic, no-argument checks) and writes
one report per tool under `target/linux-tool-checks/`.

### Clippy and coverage

`make lint` (the `cargo lint` alias in `.cargo/config.toml`) runs Clippy over
every workspace library and binary and blocks the `correctness` and
`suspicious` lint groups; CI runs it. Existing style and complexity warnings,
including intentional high-arity render boundaries, are not a gate;
`make lint-strict` fails on every warning.

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

## Build verification and CI

```sh
./scripts/verify-linux-build.sh   # or: make linux-verify
./scripts/package-linux-release.sh   # stripped release tarball under target/dist/package/
```

The harness checks native build dependencies, reconstructs or validates the pinned
GPUI fork, builds the locked `dist` Echo and `echo-version` executables, and
verifies their ELF type, runtime linkage, non-executable stack and version
identity. It does not launch a GPUI window.

`.github/workflows/linux.yml` runs on pull requests, pushes to `main` and
`master`, a weekly schedule and manual dispatch: `cargo fmt --all -- --check`,
`./scripts/test-linux.sh quick-nextest`, `make lint`,
`./scripts/verify-linux-build.sh`, and a startup smoke of the `dist` binary under
Xvfb. It caches the GPUI fork checkout and Cargo's registry and build output,
and uploads the test reports and the two verified Linux binaries as short-lived
CI artifacts. It does not publish a release or an update.

Before trusting a build on a desktop, run `./scripts/verify-linux-build.sh`,
`./scripts/test-linux.sh quick`, and the application smoke below inside a real
session.

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
[BENCHMARKING.md](BENCHMARKING.md)). It launches the real GPUI binary with
isolated Echo-owned state, requires the process to stay alive for the requested
observation, captures artifacts, resolves the profile log, and fails if startup
exits early, produces no runtime log, or fails log validation. Run
`./scripts/verify-linux-build.sh` first so `target/dist/echo` exists. A real X11
or Wayland session is required; for headless automation, start a virtual display
with Xvfb first. The CI step "Run real GPUI startup smoke under Xvfb" in
`.github/workflows/linux.yml` is the reference invocation.

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
