#!/usr/bin/env bash
# =============================================================================
# Echo profiling wrappers (Linux): perf, flamegraph, heaptrack, Valgrind,
# sanitizers and strace around a locally built Echo.
# =============================================================================
# Usage: ./scripts/profile.sh [command]   (see cmd_help below, or `make help`)
#
# Every run uses an isolated Echo profile (ECHO_BENCH_PROFILE_ROOT, default
# target/profile/echo-profile, kept between runs so you can sign in once) and
# writes its artifacts to target/profile/. Nothing runs as root: perf needs
# kernel.perf_event_paranoid <= 2 for user-space sampling (`install` explains).
# =============================================================================

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

PROFILE="${PROFILE:-release-with-debug}"
# Cargo writes the dev profile to target/debug; every other profile uses its name.
if [[ "$PROFILE" == "dev" ]]; then
    PROFILE_DIR="debug"
else
    PROFILE_DIR="$PROFILE"
fi
BINARY="$PROJECT_ROOT/target/${PROFILE_DIR}/echo"
OUT_DIR="$PROJECT_ROOT/target/profile"
export ECHO_BENCH_PROFILE_ROOT="${ECHO_BENCH_PROFILE_ROOT:-$OUT_DIR/echo-profile}"
mkdir -p "$OUT_DIR"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { echo -e "${BLUE}[INFO]${NC} $*"; }
success() { echo -e "${GREEN}[SUCCESS]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

build_profile() {
    info "Building with profile: $PROFILE"
    cargo build --locked --profile "$PROFILE" --bin echo
}

require_tool() {
    if ! command -v "$1" &> /dev/null; then
        error "$1 is not installed. Run: $2"
        exit 1
    fi
}

# =============================================================================
# Commands
# =============================================================================

cmd_flame() {
    require_tool "flamegraph" "cargo install flamegraph"
    require_tool "perf" "sudo pacman -S perf (or linux-tools on Ubuntu)"
    build_profile

    info "Generating flamegraph..."
    (cd "$OUT_DIR" && flamegraph -o flamegraph.svg -- "$BINARY")
    success "Flamegraph saved to: $OUT_DIR/flamegraph.svg"

    if command -v xdg-open &> /dev/null; then
        xdg-open "$OUT_DIR/flamegraph.svg" 2>/dev/null &
    fi
}

cmd_perf() {
    require_tool "perf" "sudo pacman -S perf"
    build_profile

    info "Running perf stat..."
    perf stat -d "$BINARY"
}

cmd_perf_record() {
    require_tool "perf" "sudo pacman -S perf"
    build_profile

    info "Recording with perf (Ctrl+C to stop)..."
    perf record -g --call-graph dwarf -o "$OUT_DIR/perf.data" "$BINARY"

    info "Generating report..."
    perf report -i "$OUT_DIR/perf.data" --stdio > "$OUT_DIR/perf-report.txt"

    success "Report saved to: $OUT_DIR/perf-report.txt"
    info "Interactive: perf report -i $OUT_DIR/perf.data"
}

cmd_heap() {
    require_tool "heaptrack" "sudo pacman -S heaptrack"
    build_profile

    info "Running heaptrack memory profiler..."
    heaptrack -o "$OUT_DIR/heaptrack.echo" "$BINARY"

    LATEST=$(ls -t "$OUT_DIR"/heaptrack.echo*.zst 2>/dev/null | head -1)
    if [[ -n "$LATEST" ]]; then
        success "Heap trace saved to: $LATEST"
        info "Analyze with: heaptrack_gui $LATEST"

        if command -v heaptrack_gui &> /dev/null; then
            heaptrack_gui "$LATEST" &
        fi
    fi
}

cmd_cache() {
    require_tool "valgrind" "sudo pacman -S valgrind"
    build_profile

    info "Running Cachegrind cache analysis..."
    valgrind --tool=cachegrind --cachegrind-out-file="$OUT_DIR/cachegrind.out.%p" "$BINARY"

    LATEST=$(ls -t "$OUT_DIR"/cachegrind.out.* 2>/dev/null | head -1)
    if [[ -n "$LATEST" ]]; then
        success "Cache analysis saved to: $LATEST"
        info "Analyze with: cg_annotate $LATEST"
    fi
}

cmd_memcheck() {
    require_tool "valgrind" "sudo pacman -S valgrind"
    build_profile

    info "Running Valgrind memcheck..."
    valgrind --leak-check=full --show-leak-kinds=all --track-origins=yes \
        --log-file="$OUT_DIR/valgrind.log" "$BINARY"

    success "Memory check log saved to: $OUT_DIR/valgrind.log"
}

# Sanitizer builds need nightly and their own target directory layout, so they
# ignore PROFILE and always use the `sanitize` profile.
run_sanitized() {
    local sanitizer="$1"
    local options="$2"
    info "Building with -Zsanitizer=$sanitizer (requires nightly)..."
    RUSTFLAGS="-Zsanitizer=$sanitizer" \
        cargo +nightly build --profile sanitize --bin echo \
        -Zbuild-std --target x86_64-unknown-linux-gnu

    success "Sanitizer build complete"
    env "$options" ./target/x86_64-unknown-linux-gnu/sanitize/echo
}

cmd_asan() {
    run_sanitized address "ASAN_OPTIONS=${ASAN_OPTIONS:-detect_leaks=1}"
}

cmd_tsan() {
    run_sanitized thread "TSAN_OPTIONS=${TSAN_OPTIONS:-}"
}

cmd_strace() {
    require_tool "strace" "sudo pacman -S strace"
    build_profile

    info "Tracing system calls..."
    strace -c -f -o "$OUT_DIR/strace-summary.txt" "$BINARY"

    success "Summary saved to: $OUT_DIR/strace-summary.txt"
}

cmd_install() {
    info "Installing profiling tools..."

    if command -v pacman &> /dev/null; then
        sudo pacman -S --needed perf valgrind heaptrack strace
    elif command -v apt &> /dev/null; then
        sudo apt install -y linux-tools-common linux-tools-generic valgrind \
            heaptrack heaptrack-gui strace
    elif command -v dnf &> /dev/null; then
        sudo dnf install -y perf valgrind heaptrack strace
    else
        warn "Unknown package manager. Install manually:"
        echo "  - perf (linux-tools)"
        echo "  - valgrind"
        echo "  - heaptrack"
        echo "  - strace"
    fi

    info "Installing Rust tools..."
    cargo install flamegraph

    if [[ -f /proc/sys/kernel/perf_event_paranoid ]]; then
        PARANOID=$(cat /proc/sys/kernel/perf_event_paranoid)
        if [[ "$PARANOID" -gt 2 ]]; then
            warn "perf_event_paranoid is $PARANOID; perf cannot sample user-space as a normal user"
            info "Allow it for this boot (or persist it under /etc/sysctl.d/):"
            echo "  sudo sysctl kernel.perf_event_paranoid=2"
        fi
    fi

    success "Installation complete!"
}

cmd_help() {
    echo "Echo Profiling Scripts"
    echo ""
    echo "Usage: $0 <command>"
    echo ""
    echo "Commands:"
    echo "  flame        Generate CPU flamegraph (svg)"
    echo "  perf         Quick perf stat counters"
    echo "  perf-record  Detailed perf recording"
    echo "  heap         Memory profiling (heaptrack)"
    echo "  cache        Cache analysis (Cachegrind)"
    echo "  memcheck     Memory error check (Valgrind)"
    echo "  asan         Build and run with AddressSanitizer (nightly)"
    echo "  tsan         Build and run with ThreadSanitizer (nightly)"
    echo "  strace       System call tracing"
    echo "  install      Install required tools"
    echo ""
    echo "Environment:"
    echo "  PROFILE                  Cargo profile (default: release-with-debug; dev maps to target/debug)"
    echo "  ECHO_BENCH_PROFILE_ROOT  Echo profile to run under (default: target/profile/echo-profile)"
    echo ""
    echo "Artifacts are written to target/profile/."
    echo ""
    echo "Examples:"
    echo "  $0 flame                  # CPU flamegraph"
    echo "  $0 heap                   # Memory usage"
    echo "  PROFILE=dev $0 memcheck   # Debug build + memcheck"
}

# =============================================================================
# Main
# =============================================================================

case "${1:-help}" in
    flame)       cmd_flame ;;
    perf)        cmd_perf ;;
    perf-record) cmd_perf_record ;;
    heap)        cmd_heap ;;
    cache)       cmd_cache ;;
    memcheck)    cmd_memcheck ;;
    asan)        cmd_asan ;;
    tsan)        cmd_tsan ;;
    strace)      cmd_strace ;;
    install)     cmd_install ;;
    help|--help|-h) cmd_help ;;
    *)
        error "Unknown command: $1"
        cmd_help
        exit 1
        ;;
esac
