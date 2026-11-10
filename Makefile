# =============================================================================
# Echo Development Makefile
# =============================================================================
# Common development commands for Linux
# =============================================================================

.PHONY: all build run release check test lint fmt clean
.PHONY: profile flame heap memcheck asan install-tools
.PHONY: watch logs help audit
.PHONY: dist install uninstall
.PHONY: linux-deps setup-gpui linux-verify linux-test quick-test nextest coverage-root

# Local install layout (user-level, no root; CachyOS/Arch-friendly). The
# binary is never put on PATH as `echo`, which would shadow coreutils' echo.
# Every Echo window uses APP_ID as its Wayland app_id / X11 WM_CLASS, so the
# desktop entry must be named $(APP_ID).desktop for KDE and GNOME to match it.
APP_ID       := com.echo.chat
INSTALL_ROOT := $(HOME)/.local/share/echo
BIN_LINK     := $(HOME)/.local/bin/echo-gpui
DESKTOP_FILE := $(HOME)/.local/share/applications/$(APP_ID).desktop
ICON_FILE    := $(HOME)/.local/share/icons/hicolor/scalable/apps/$(APP_ID).svg

# Default target
all: check

# =============================================================================
# Build Commands
# =============================================================================

## Build in debug mode
build: setup-gpui
	cargo build

## Build in release mode
release: setup-gpui
	cargo build --release

## Build with profiling enabled (chrome://tracing output)
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

## Reconstruct or validate the pinned sibling GPUI fork on Linux
setup-gpui:
	./scripts/setup-gpui-fork.sh

## Run the non-test Linux compile, dist-build, and ELF/linkage harness
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

## Remove the user-level install
uninstall:
	rm -f $(BIN_LINK) $(DESKTOP_FILE) $(ICON_FILE)
	rm -rf $(INSTALL_ROOT)/bin
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null
	-kbuildsycoca6 2>/dev/null || kbuildsycoca5 2>/dev/null
	@echo "Uninstalled Echo from user directories"

# =============================================================================
# Run Commands
# =============================================================================

## Run in debug mode
run:
	cargo run

## Run in release mode
rr:
	cargo run --release

## Run with profiling (generates echo_trace.json)
run-profile:
	cargo run --release --features profiling

## Run with verbose logging
run-debug:
	RUST_LOG=debug cargo run

## Run with trace logging (very verbose)
run-trace:
	RUST_LOG=trace cargo run

## Run with specific module logging
run-log:
	RUST_LOG=warn,echo_gpui=info,echo_gpui::backend::irc=debug cargo run

# =============================================================================
# Quality Checks
# =============================================================================

## Quick compile check
check: setup-gpui
	cargo check --locked --bin echo

## Run clippy lints (fails on correctness and suspicious lints)
lint: setup-gpui
	cargo clippy --locked -p echo_gpui -p echo-autocomplete-core -p echo-cache-policy -p echo-chat-render -p echo-core -p echo-cosmetics-core -p echo-emote-core -p echo-link-core -p echo-media-cache -p echo-render-core -p echo-text-core --lib -- -D clippy::correctness -D clippy::suspicious

## Fail on every Clippy warning (currently a cleanup/ratchet target)
lint-strict: setup-gpui
	cargo clippy --locked -p echo_gpui -p echo-autocomplete-core -p echo-cache-policy -p echo-chat-render -p echo-core -p echo-cosmetics-core -p echo-emote-core -p echo-link-core -p echo-media-cache -p echo-render-core -p echo-text-core --lib -- -D warnings

## Format code
fmt:
	cargo fmt

## Check formatting without modifying
fmt-check:
	cargo fmt -- --check

## Run the authoritative Linux tests plus build/artifact verification
ci: linux-test linux-verify
	@echo "Linux tests, build, and artifact verification passed!"

## Run security audit on dependencies
audit:
	cargo audit

## Run full dependency checks (advisories, bans, licenses)
deny:
	cargo deny check

# =============================================================================
# Testing
# =============================================================================

## Run all tests
test:
	./scripts/test-linux.sh cargo

## Run tests with output
test-verbose: setup-gpui
	RUST_MIN_STACK=268435456 cargo test --locked -p echo_gpui --lib -- --nocapture

## Run the Linux CI suite with nextest, doctests, nested-workspace tests, and target checks
linux-test:
	./scripts/test-linux.sh ci

## Run the pull-request Linux checks (root libtest target included)
quick-test:
	./scripts/test-linux.sh quick

## Run nextest for Echo and the extracted crates
nextest:
	./scripts/test-linux.sh nextest

## Generate LCOV for the root library plus every extracted crate (manual, not a CI gate)
coverage-root:
	./scripts/test-linux.sh coverage-root

## Run benchmarks
bench:
	cargo bench

# =============================================================================
# Profiling (Linux)
# =============================================================================

## Install profiling tools
install-tools:
	./scripts/profile.sh install

## Generate CPU flamegraph
flame:
	./scripts/profile.sh flame

## Memory profiling with heaptrack
heap:
	./scripts/profile.sh heap

## Run perf stat
perf:
	./scripts/profile.sh perf

## Detailed perf recording
perf-record:
	./scripts/profile.sh perf-record

## Valgrind memory check
memcheck:
	./scripts/profile.sh memcheck

## Cache analysis
cache:
	./scripts/profile.sh cache

## Build with AddressSanitizer
asan:
	./scripts/profile.sh asan

## Build with ThreadSanitizer
tsan:
	./scripts/profile.sh tsan

## System call trace
strace:
	./scripts/profile.sh strace

# =============================================================================
# Development Utilities
# =============================================================================

## Clean build artifacts
clean:
	cargo clean
	rm -f flamegraph.svg perf.data perf-report.txt valgrind.log
	rm -f echo_trace.json strace-summary.txt
	rm -f cachegrind.out.* heaptrack.echo.*

## Watch for changes and rebuild
watch:
	cargo watch -x check

## Show recent logs (if using file logging)
logs:
	@if [ -f ~/.local/share/echo/echo.log ]; then \
		tail -f ~/.local/share/echo/echo.log; \
	else \
		echo "No log file found. Run with RUST_LOG=... to see console logs"; \
	fi

# =============================================================================
# Help
# =============================================================================

## Show this help
help:
	@echo "Echo Development Commands"
	@echo ""
	@echo "Build:"
	@echo "  make build              Build debug"
	@echo "  make release            Build release"
	@echo "  make build-profile      Build with tracing profiler"
	@echo "  make build-debug-release Build release with debug symbols"
	@echo "  make linux-deps         Check native Linux build dependencies"
	@echo "  make setup-gpui         Reconstruct or validate the GPUI fork"
	@echo "  make linux-verify       Compile, build dist, and verify ELF linkage"
	@echo ""
	@echo "Run:"
	@echo "  make run                Run debug"
	@echo "  make rr                 Run release"
	@echo "  make run-profile        Run with profiling"
	@echo "  make run-debug          Run with RUST_LOG=debug"
	@echo "  make run-trace          Run with RUST_LOG=trace"
	@echo ""
	@echo "Quality:"
	@echo "  make check              Quick compile check"
	@echo "  make lint               Clippy lints"
	@echo "  make lint-strict        Pedantic clippy"
	@echo "  make fmt                Format code"
	@echo "  make test               Run Linux Cargo tests"
	@echo "  make quick-test         Run the Linux pull-request suite"
	@echo "  make nextest            Run nextest plus nested-workspace tests"
	@echo "  make coverage-root      Attempt monolithic root Echo LCOV"
	@echo "  make ci                 Full Linux tests, build, and artifact verification"
	@echo "  make audit              Security audit dependencies"
	@echo "  make deny               Full dependency checks"
	@echo ""
	@echo "Profiling (Linux):"
	@echo "  make install-tools      Install profiling tools"
	@echo "  make dist               Build distribution binary (dist profile)"
	@echo "  make install            Build dist and install for current user"
	@echo "  make uninstall          Remove the user-level install"
	@echo "  make flame              CPU flamegraph"
	@echo "  make heap               Memory profiling"
	@echo "  make perf               perf stat counters"
	@echo "  make memcheck           Valgrind memory check"
	@echo "  make asan               AddressSanitizer build"
	@echo "  make tsan               ThreadSanitizer build"
	@echo ""
	@echo "Other:"
	@echo "  make clean              Remove build artifacts"
	@echo "  make watch              Watch and rebuild"
