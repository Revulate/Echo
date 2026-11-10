# =============================================================================
# Echo Development Makefile (Linux)
# =============================================================================
# Convenience targets over the documented commands. `make help` lists every
# target, generated from the `## ` comment above it.
# =============================================================================

.PHONY: all help
.PHONY: build release build-profile build-debug-release dist
.PHONY: linux-deps setup-gpui linux-verify install uninstall
.PHONY: run rr run-profile run-debug run-trace run-log
.PHONY: check lint lint-strict fmt fmt-check deny ci
.PHONY: test test-verbose linux-test quick-test nextest coverage-root bench
.PHONY: install-tools flame heap perf perf-record memcheck cache asan tsan strace
.PHONY: clean watch logs

# Local install layout (user-level, no root; CachyOS/Arch-friendly). The
# binary is never put on PATH as `echo`, which would shadow coreutils' echo.
# Every Echo window uses APP_ID as its Wayland app_id / X11 WM_CLASS, so the
# desktop entry must be named $(APP_ID).desktop for KDE and GNOME to match it.
APP_ID       := com.echo.chat
INSTALL_ROOT := $(HOME)/.local/share/echo
BIN_LINK     := $(HOME)/.local/bin/echo-gpui
DESKTOP_FILE := $(HOME)/.local/share/applications/$(APP_ID).desktop
ICON_FILE    := $(HOME)/.local/share/icons/hicolor/scalable/apps/$(APP_ID).svg

# Log categories for `make run-log` (see docs/ENVIRONMENT.md), e.g.
# `make run-log LOG=irc,emotes`.
LOG ?= irc

# Default target
all: check

# =============================================================================
# Build
# =============================================================================

## Build in debug mode
build: setup-gpui
	cargo build

## Build in release mode (fat LTO, slow; prefer dist)
release: setup-gpui
	cargo build --release

## Build release with the Chrome tracing profiler (--features profiling)
build-profile: setup-gpui
	cargo build --release --features profiling

## Build with full debug symbols for profilers
build-debug-release: setup-gpui
	cargo build --profile release-with-debug

## Build the distribution binary (thin LTO, stripped, panic=abort)
dist: setup-gpui
	cargo build --profile dist --locked --bin echo

## Check native Linux headers and build tools before invoking Cargo
linux-deps:
	./scripts/check-linux-build-deps.sh

## Reconstruct or validate the pinned sibling GPUI fork
setup-gpui:
	./scripts/setup-gpui-fork.sh

## Build dist and verify ELF type, linkage, stack policy and version identity
linux-verify:
	./scripts/verify-linux-build.sh

## Build dist and install for the current user (binary, launcher entry, icon)
install: dist
	install -Dm755 target/dist/echo $(INSTALL_ROOT)/bin/echo
	install -Dm644 assets/icons/echo-logo.svg $(ICON_FILE)
	mkdir -p $(dir $(BIN_LINK)) $(dir $(DESKTOP_FILE))
	ln -sf $(INSTALL_ROOT)/bin/echo $(BIN_LINK)
	sed 's|@ECHO_EXEC@|$(INSTALL_ROOT)/bin/echo|g' assets/linux/$(APP_ID).desktop > $(DESKTOP_FILE)
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null
	-kbuildsycoca6 2>/dev/null || kbuildsycoca5 2>/dev/null
	@echo "Installed: $(INSTALL_ROOT)/bin/echo (CLI: echo-gpui, launcher: Echo)"

## Remove the user-level install (settings and caches stay)
uninstall:
	rm -f $(BIN_LINK) $(DESKTOP_FILE) $(ICON_FILE)
	rm -rf $(INSTALL_ROOT)/bin
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null
	-kbuildsycoca6 2>/dev/null || kbuildsycoca5 2>/dev/null
	@echo "Uninstalled Echo from user directories"

# =============================================================================
# Run
# =============================================================================

## Run in debug mode
run:
	cargo run

## Run in release mode
rr:
	cargo run --release

## Run with the Chrome tracing profiler (writes echo_trace.json)
run-profile:
	cargo run --release --features profiling

## Run with RUST_LOG=debug for every crate (ECHO_LOG categories are then ignored)
run-debug:
	RUST_LOG=debug cargo run

## Run with RUST_LOG=trace for every crate (very verbose)
run-trace:
	RUST_LOG=trace cargo run

## Run with Echo log categories LOG (default irc), also written to the log file
run-log:
	ECHO_LOG=$(LOG) ECHO_LOG_FILE=1 cargo run

# =============================================================================
# Quality
# =============================================================================

## Quick compile check
check: setup-gpui
	cargo check --locked --bin echo

## Clippy gate: workspace libs and bins, failing on correctness and suspicious lints
lint: setup-gpui
	cargo lint

## Clippy over the same targets, failing on every warning (cleanup ratchet, not a gate)
lint-strict: setup-gpui
	cargo lint-strict

## Format the whole workspace
fmt:
	cargo fmt --all

## Check workspace formatting (the canonical gate)
fmt-check:
	cargo fmt --all -- --check

## Dependency gate: advisories, licenses, bans and sources (docs/DEPENDENCY_POLICY.md)
deny:
	cargo deny --locked --all-features check advisories licenses bans sources

## Run the Linux CI test suite plus build and artifact verification
ci: linux-test linux-verify
	@echo "Linux tests, build, and artifact verification passed!"

# =============================================================================
# Testing
# =============================================================================

## Run the Linux test suite with plain cargo test
test:
	./scripts/test-linux.sh cargo

## Run the root library tests with output
test-verbose: setup-gpui
	RUST_MIN_STACK=268435456 cargo test --locked -p echo_gpui --lib -- --nocapture

## Run the CI mode: quick-nextest plus criterion bench compile checks
linux-test:
	./scripts/test-linux.sh ci

## Run the pull-request Linux checks (root libtest target included)
quick-test:
	./scripts/test-linux.sh quick

## Run nextest and doctests for Echo and the extracted crates
nextest:
	./scripts/test-linux.sh nextest

## Generate LCOV for the root library plus every extracted crate (manual, not a CI gate)
coverage-root:
	./scripts/test-linux.sh coverage-root

## Run the criterion benchmarks
bench:
	cargo bench

# =============================================================================
# Profiling (scripts/profile.sh; artifacts under target/profile/)
# =============================================================================

## Install profiling tools
install-tools:
	./scripts/profile.sh install

## Generate a CPU flamegraph
flame:
	./scripts/profile.sh flame

## Memory profiling with heaptrack
heap:
	./scripts/profile.sh heap

## perf stat counters
perf:
	./scripts/profile.sh perf

## Detailed perf recording and report
perf-record:
	./scripts/profile.sh perf-record

## Valgrind memory check
memcheck:
	./scripts/profile.sh memcheck

## Cachegrind cache analysis
cache:
	./scripts/profile.sh cache

## Build and run with AddressSanitizer (nightly)
asan:
	./scripts/profile.sh asan

## Build and run with ThreadSanitizer (nightly)
tsan:
	./scripts/profile.sh tsan

## System call summary
strace:
	./scripts/profile.sh strace

# =============================================================================
# Other
# =============================================================================

## Remove build and profiling artifacts
clean:
	cargo clean
	rm -f echo_trace.json

## Watch for changes and re-run cargo check
watch:
	cargo watch -x check

## Follow the opt-in debug log (ECHO_LOG_FILE=1)
logs:
	@if [ -f ~/.local/share/echo/echo.log ]; then \
		tail -f ~/.local/share/echo/echo.log; \
	else \
		echo "No log file found. Run with ECHO_LOG_FILE=1 (see docs/ENVIRONMENT.md)."; \
	fi

## Show this help
help:
	@awk ' \
		/^# =+$$/ { rule = 1; next } \
		rule && /^# [A-Z]/ { section = substr($$0, 3); rule = 0; next } \
		{ rule = 0 } \
		/^## / { desc = substr($$0, 4); next } \
		/^[a-zA-Z0-9_-]+:/ && desc != "" { \
			if (section != "") { printf "\n%s:\n", section; section = "" } \
			split($$0, t, ":"); printf "  make %-20s %s\n", t[1], desc \
		} \
		{ desc = "" } \
	' $(firstword $(MAKEFILE_LIST))
