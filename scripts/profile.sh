#!/usr/bin/env bash
# =============================================================================
# Echo Profiling & Analysis Scripts
# =============================================================================
# Usage: ./scripts/profile.sh [command]
#
# Commands:
#   flame       - Generate flamegraph (CPU profiling)
#   perf        - Run with perf stat (CPU counters)
#   perf-record - Record with perf for detailed analysis
#   heap        - Memory profiling with heaptrack
#   cache       - Cache analysis with Valgrind/Cachegrind
#   memcheck    - Memory error detection with Valgrind
#   asan        - AddressSanitizer build
#   tsan        - ThreadSanitizer build
#   strace      - System call tracing
#   install     - Install required tools
# =============================================================================

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

PROFILE="${PROFILE:-release-with-debug}"
BINARY="./target/${PROFILE}/echo"

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

# Build with debug symbols
build_profile() {
    info "Building with profile: $PROFILE"
    cargo build --profile "$PROFILE"
}

# Check if tool is installed
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

    info "Generating flamegraph..."
    build_profile

    # Flamegraph needs root for perf events
    cargo flamegraph --profile "$PROFILE" --root -o flamegraph.svg

    success "Flamegraph saved to: flamegraph.svg"

    # Try to open in browser
    if command -v xdg-open &> /dev/null; then
        xdg-open flamegraph.svg 2>/dev/null &
    fi
}

cmd_perf() {
    require_tool "perf" "sudo pacman -S perf"
    build_profile

    info "Running perf stat..."
    sudo perf stat -d "$BINARY"
}

cmd_perf_record() {
    require_tool "perf" "sudo pacman -S perf"
    build_profile

    info "Recording with perf (Ctrl+C to stop)..."
    sudo perf record -g --call-graph dwarf "$BINARY"

    info "Generating report..."
    sudo perf report --stdio > perf-report.txt

    success "Report saved to: perf-report.txt"
    info "Interactive: sudo perf report"
}

cmd_heap() {
    require_tool "heaptrack" "sudo pacman -S heaptrack"
    build_profile

    info "Running heaptrack memory profiler..."
    heaptrack "$BINARY"

    # Find the latest heaptrack output
    LATEST=$(ls -t heaptrack.echo.*.zst 2>/dev/null | head -1)
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
    valgrind --tool=cachegrind "$BINARY"

    LATEST=$(ls -t cachegrind.out.* 2>/dev/null | head -1)
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
        --log-file=valgrind.log "$BINARY"

    success "Memory check log saved to: valgrind.log"
}

cmd_asan() {
    info "Building with AddressSanitizer (requires nightly)..."

    RUSTFLAGS="-Zsanitizer=address" \
    cargo +nightly build --profile sanitize \
        -Zbuild-std --target x86_64-unknown-linux-gnu

    success "ASAN build complete"
    info "Run: ./target/x86_64-unknown-linux-gnu/sanitize/echo"

    # Run it
    ASAN_OPTIONS="detect_leaks=1" \
    ./target/x86_64-unknown-linux-gnu/sanitize/echo
}

cmd_tsan() {
    info "Building with ThreadSanitizer (requires nightly)..."

    RUSTFLAGS="-Zsanitizer=thread" \
    cargo +nightly build --profile sanitize \
        -Zbuild-std --target x86_64-unknown-linux-gnu

    success "TSAN build complete"
    ./target/x86_64-unknown-linux-gnu/sanitize/echo
}

cmd_strace() {
    require_tool "strace" "sudo pacman -S strace"
    build_profile

    info "Tracing system calls..."
    strace -c -f "$BINARY" 2>&1 | tee strace-summary.txt

    success "Summary saved to: strace-summary.txt"
}

cmd_install() {
    info "Installing profiling tools..."

    if command -v pacman &> /dev/null; then
        # Arch Linux
        sudo pacman -S --needed perf valgrind heaptrack strace
    elif command -v apt &> /dev/null; then
        # Ubuntu/Debian
        sudo apt install -y linux-tools-common linux-tools-generic valgrind \
            heaptrack heaptrack-gui strace
    elif command -v dnf &> /dev/null; then
        # Fedora
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

    # Check perf permissions
    if [[ -f /proc/sys/kernel/perf_event_paranoid ]]; then
        PARANOID=$(cat /proc/sys/kernel/perf_event_paranoid)
        if [[ "$PARANOID" -gt 1 ]]; then
            warn "perf_event_paranoid is $PARANOID (restrictive)"
            info "For user-space profiling without root:"
            echo "  sudo sysctl kernel.perf_event_paranoid=1"
            echo "  # Or permanently: add to /etc/sysctl.d/99-perf.conf"
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
    echo "  asan         Build with AddressSanitizer"
    echo "  tsan         Build with ThreadSanitizer"
    echo "  strace       System call tracing"
    echo "  install      Install required tools"
    echo ""
    echo "Environment:"
    echo "  PROFILE      Build profile (default: release-with-debug)"
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
