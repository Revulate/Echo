#!/usr/bin/env bash
# Echo's authoritative Linux test harness. Every stage runs through
# run-linux-stage.sh (timeouts, heartbeats, per-stage logs under
# target/linux-test-stages/). Modes are described in usage() and in
# docs/LINUX_DEVELOPMENT.md.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-cargo}"
stage_runner="$repo_root/scripts/run-linux-stage.sh"
stage_log_dir="${ECHO_LINUX_STAGE_LOG_DIR:-$repo_root/target/linux-test-stages}"
compile_timeout="${ECHO_LINUX_COMPILE_TIMEOUT_SECONDS:-900}"
test_timeout="${ECHO_LINUX_TEST_TIMEOUT_SECONDS:-900}"
coverage_timeout="${ECHO_LINUX_COVERAGE_TIMEOUT_SECONDS:-900}"
heartbeat="${ECHO_LINUX_STAGE_HEARTBEAT_SECONDS:-30}"
# Packages whose library tests run next to the root crate's: every extracted
# crate under crates/ (directory name == package name) plus the one tool with a
# library test target.
library_test_package_args=(-p cache-preload-check)
for crate_manifest in "$repo_root"/crates/*/Cargo.toml; do
    crate_dir="${crate_manifest%/Cargo.toml}"
    library_test_package_args+=(-p "${crate_dir##*/}")
done
export RUST_MIN_STACK="${RUST_MIN_STACK:-268435456}"

usage() {
    cat >&2 <<USAGE
Usage: ${0##*/} [mode]

  cargo          (default) test inventory, then root and extracted-crate library
                 tests and doctests with plain cargo test
  quick          workspace bin check, inventory, guard self-test, bench script
                 tests, the cargo test stages, contract tools, update-core check
  quick-nextest  quick with nextest running the library tests (what CI runs)
  nextest        inventory plus the nextest and doctest stages
  ci             quick-nextest plus compile checks of the criterion benches
  coverage-root  LCOV for the root library and every extracted crate (manual)
USAGE
}

require_cargo_subcommand() {
    local command_name="$1"
    local install_hint="$2"
    if ! cargo "$command_name" --version >/dev/null 2>&1; then
        printf 'Required Cargo subcommand is missing: cargo %s\n%s\n' \
            "$command_name" "$install_hint" >&2
        exit 1
    fi
}

require_nextest() {
    # .config/nextest.toml's nextest-version is the one pin; nextest enforces it
    # too, but only after the build, so check it before compiling anything.
    local minimum_version
    local installed_version
    minimum_version="$(sed -nE 's/^nextest-version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' \
        "$repo_root/.config/nextest.toml")"
    if [[ -z "$minimum_version" ]]; then
        printf 'nextest-version is missing from .config/nextest.toml.\n' >&2
        exit 1
    fi
    require_cargo_subcommand nextest \
        'Install cargo-nextest from https://nexte.st/docs/installation/pre-built-binaries/.'
    installed_version="$(cargo nextest --version | awk 'NR == 1 { print $2 }')"
    if [[ -z "$installed_version" || "$(printf '%s\n%s\n' "$minimum_version" "$installed_version" | sort -V | head -1)" != "$minimum_version" ]]; then
        printf 'cargo-nextest %s or newer is required; found %s.\n' \
            "$minimum_version" "${installed_version:-unknown}" >&2
        exit 1
    fi
}

run_stage() {
    local timeout="$1"
    local name="$2"
    shift 2
    "$stage_runner" \
        --timeout-seconds "$timeout" \
        --heartbeat-seconds "$heartbeat" \
        --log "$stage_log_dir/$name.log" \
        -- "$name" "$@"
}

# Echo's own library tests plus every extracted crate. The root libtest target
# compiles and runs in about a minute, so it belongs in every mode, including
# the pull-request suite. Test modules must not let `use gpui::*` shadow the
# built-in `#[test]` with `gpui::test`, which recurses.
run_workspace_tests() {
    local backend="$1"
    case "$backend" in
        nextest)
            # One invocation, deliberately: nextest writes a single JUnit report
            # to target/nextest/ci/junit.xml, so a second run would overwrite the
            # first instead of adding to it. Root and extracted cases must share
            # this command to share that report.
            run_stage "$test_timeout" root-workspace-nextest \
                cargo nextest run --locked -p echo_gpui "${library_test_package_args[@]}" \
                    --lib --profile ci --retries 0 --flaky-result fail
            ;;
        cargo)
            run_stage "$test_timeout" root-lib-tests \
                cargo test --locked -p echo_gpui --lib --no-fail-fast
            run_stage "$test_timeout" extracted-lib-tests \
                cargo test --locked "${library_test_package_args[@]}" \
                    --lib --no-fail-fast
            ;;
        *)
            printf 'Unknown test backend: %s\n' "$backend" >&2
            exit 2
            ;;
    esac
    # Nextest does not run doctests, so both backends need these explicitly.
    run_stage "$test_timeout" root-doctests \
        cargo test --locked -p echo_gpui --doc
    run_stage "$test_timeout" extracted-doctests \
        cargo test --locked "${library_test_package_args[@]}" --doc
}

run_update_core_check() {
    run_stage "$compile_timeout" update-core-check \
        cargo check --locked -p echo-update-core --lib
}

run_bench_checks() {
    run_stage "$compile_timeout" bench-layout-check \
        cargo check --locked -p echo_gpui --bench layout_bench
    run_stage "$compile_timeout" bench-filter-check \
        cargo check --locked -p echo_gpui --bench parse_bench
    run_stage "$compile_timeout" bench-loading-check \
        cargo check --locked -p echo_gpui --bench loading_bench
}

run_quick_tests() {
    local backend="${1:-cargo}"
    run_stage "$compile_timeout" workspace-bin-check \
        cargo check --locked --workspace --bins
    run_stage "$test_timeout" test-inventory \
        "$repo_root/scripts/check-linux-test-inventory.sh"
    run_stage "$test_timeout" storage-roots \
        "$repo_root/scripts/check-storage-roots.sh" --self-test
    run_stage "$test_timeout" bench-script-tests \
        python3 -m unittest discover -s "$repo_root/scripts/bench/tests"
    run_workspace_tests "$backend"
    "$repo_root/scripts/test-linux-tools.sh"
    run_update_core_check
}

if [[ "$(uname -s)" != "Linux" ]]; then
    printf 'Echo test-linux.sh is the authoritative Linux test harness.\n' >&2
    exit 1
fi

cd "$repo_root"
"$repo_root/scripts/check-linux-build-deps.sh"
"$repo_root/scripts/setup-gpui-fork.sh"
mkdir -p "$stage_log_dir"

case "$mode" in
    quick)
        run_quick_tests cargo
        ;;
    quick-nextest)
        require_nextest
        run_quick_tests nextest
        ;;
    cargo)
        run_stage "$test_timeout" test-inventory \
            "$repo_root/scripts/check-linux-test-inventory.sh"
        run_workspace_tests cargo
        ;;
    nextest)
        require_nextest
        run_stage "$test_timeout" test-inventory \
            "$repo_root/scripts/check-linux-test-inventory.sh"
        run_workspace_tests nextest
        ;;
    ci)
        require_nextest
        run_quick_tests nextest
        run_bench_checks
        ;;
    coverage-root)
        require_cargo_subcommand llvm-cov \
            'Install cargo-llvm-cov from https://github.com/taiki-e/cargo-llvm-cov.'
        mkdir -p "$repo_root/target/coverage"
        run_stage "$coverage_timeout" root-coverage \
            cargo llvm-cov --locked -p echo_gpui "${library_test_package_args[@]}" --lib \
                --lcov --output-path "$repo_root/target/coverage/echo.lcov"
        ;;
    *)
        usage
        exit 2
        ;;
esac

printf 'Echo Linux test mode completed: %s\n' "$mode"
