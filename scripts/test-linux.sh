#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-cargo}"
stage_runner="$repo_root/scripts/run-linux-stage.sh"
stage_log_dir="${ECHO_LINUX_STAGE_LOG_DIR:-$repo_root/target/linux-test-stages}"
compile_timeout="${ECHO_LINUX_COMPILE_TIMEOUT_SECONDS:-900}"
test_timeout="${ECHO_LINUX_TEST_TIMEOUT_SECONDS:-900}"
coverage_timeout="${ECHO_LINUX_COVERAGE_TIMEOUT_SECONDS:-900}"
heartbeat="${ECHO_LINUX_STAGE_HEARTBEAT_SECONDS:-30}"
extracted_package_args=(
    -p cache-preload-check
    -p echo-autocomplete-core
    -p echo-cache-policy
    -p echo-chat-render
    -p echo-core
    -p echo-cosmetics-core
    -p echo-emote-core
    -p echo-link-core
    -p echo-media-cache
    -p echo-render-core
    -p echo-text-core
)
export RUST_MIN_STACK="${RUST_MIN_STACK:-268435456}"

usage() {
    printf 'Usage: %s [quick|quick-nextest|cargo|nextest|ci|coverage-root]\n' "${0##*/}" >&2
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
    local minimum_version="0.9.143"
    local installed_version
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
            require_nextest
            run_stage "$test_timeout" root-workspace-nextest \
                cargo nextest run --locked -p echo_gpui "${extracted_package_args[@]}" \
                    --lib --profile ci --retries 0 --flaky-result fail
            ;;
        cargo)
            run_stage "$test_timeout" root-lib-tests \
                cargo test --locked -p echo_gpui --lib --no-fail-fast
            run_stage "$test_timeout" extracted-lib-tests \
                cargo test --locked "${extracted_package_args[@]}" \
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
        cargo test --locked "${extracted_package_args[@]}" --doc
}

run_update_core_check() {
    run_stage "$compile_timeout" update-core-check \
        cargo check --locked -p echo-update-core --lib
}

run_bench_checks() {
    run_stage "$compile_timeout" bench-layout-check \
        cargo check --locked -p echo_gpui --bench layout_bench
    run_stage "$compile_timeout" bench-filter-check \
        cargo check --locked -p echo_gpui --bench filter_bench
    run_stage "$compile_timeout" bench-loading-check \
        cargo check --locked -p echo_gpui --bench loading_bench
}

run_quick_tests() {
    local backend="${1:-cargo}"
    run_stage "$compile_timeout" workspace-bin-check \
        cargo check --locked --workspace --bins
    run_stage "$test_timeout" test-inventory \
        "$repo_root/scripts/check-linux-test-inventory.sh"
    run_stage "$test_timeout" benchmark-profile-roots \
        "$repo_root/scripts/check-benchmark-profile-roots.sh" --self-test
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
            cargo llvm-cov --locked -p echo_gpui "${extracted_package_args[@]}" --lib \
                --lcov --output-path "$repo_root/target/coverage/echo.lcov"
        ;;
    *)
        usage
        exit 2
        ;;
esac

printf 'Echo Linux test mode completed: %s\n' "$mode"
