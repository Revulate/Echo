#!/usr/bin/env bash
# Guard: every Echo-owned config, data and cache path under src/ must go through
# src/storage_roots.rs so ECHO_BENCH_PROFILE_ROOT isolates all of Echo's
# state. --self-test first proves the scanner against built-in fixtures.
# check-storage-roots.ps1 is the PowerShell twin; keep the patterns alike.
set -euo pipefail

usage() {
    printf 'Usage: %s [--self-test]\n' "${0##*/}" >&2
}

self_test=0
case "${1:-}" in
    "")
        ;;
    --self-test)
        self_test=1
        ;;
    *)
        usage
        exit 2
        ;;
esac
if (($# > 1)); then
    usage
    exit 2
fi

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_root="$repo_root/src"
central_module="storage_roots.rs"
patterns=(
    'dirs::(config_dir|data_dir|data_local_dir|cache_dir)[[:space:]]*\('
    'PathBuf::from\("(settings|user_notes|sounds|echo_trace|dhat-heap)\.json"\)'
    'std::env::var\("HOME"\)'
)
descriptions=(
    'direct dirs storage root'
    'legacy cwd storage file'
    'manual HOME storage root'
)
self_test_fixture_root=""

cleanup_self_test_fixture() {
    if [[ -n "$self_test_fixture_root" ]]; then
        rm -rf -- "$self_test_fixture_root"
    fi
}

violations=()
scan_error=""

scan_source_root() {
    local scan_root="$1"
    local file relative_path index status
    local -a source_files=()

    violations=()
    scan_error=""
    if [[ ! -f "$scan_root/$central_module" ]]; then
        scan_error="Missing storage-roots module: $central_module"
        return 2
    fi

    mapfile -d '' -t source_files \
        < <(find "$scan_root" -type f -name '*.rs' -print0 | LC_ALL=C sort -z)
    for file in "${source_files[@]}"; do
        relative_path="${file#"$scan_root/"}"
        if [[ "$relative_path" == "$central_module" ]]; then
            continue
        fi
        for index in "${!patterns[@]}"; do
            # -z reads the file as one record, so [[:space:]] also matches the
            # line breaks rustfmt may put inside a call. Like the find above,
            # this ignores .gitignore and includes hidden files.
            if grep -E -z -q -- "${patterns[$index]}" "$file"; then
                status=0
            else
                status=$?
            fi
            if ((status == 0)); then
                violations+=("$relative_path (${descriptions[$index]})")
            elif ((status > 1)); then
                scan_error="Failed to scan $relative_path for storage-root violations."
                return "$status"
            fi
        done
    done

    ((${#violations[@]} == 0))
}

run_self_tests() {
    local fixture_file index expected missing_status
    local -a fixture_sources fixture_descriptions

    self_test_fixture_root="$(mktemp -d)"
    trap cleanup_self_test_fixture EXIT
    mkdir -p "$self_test_fixture_root/nested"
    printf '%s\n' \
        'let config = dirs::config_dir();' \
        'let data = dirs::data_local_dir();' \
        'let external = dirs::data_dir();' \
        'let cache = dirs::cache_dir();' \
        'let sound = PathBuf::from("sounds.json");' \
        'let home = std::env::var("HOME");' \
        >"$self_test_fixture_root/$central_module"
    printf '%s\n' \
        'let path = crate::storage_roots::config_root_or_current_dir();' \
        >"$self_test_fixture_root/nested/clean file.rs"

    if ! scan_source_root "$self_test_fixture_root"; then
        printf 'Internal clean/exclusion fixture failed: %s\n' \
            "${scan_error:-${violations[*]}}" >&2
        return 1
    fi

    if scan_source_root "$self_test_fixture_root/nested"; then
        printf 'Internal missing-central-module fixture unexpectedly passed.\n' >&2
        return 1
    else
        missing_status=$?
    fi
    if ((missing_status != 2)) \
        || [[ "$scan_error" != "Missing storage-roots module: $central_module" ]]; then
        printf 'Internal missing-module fixture returned the wrong error: %s\n' \
            "$scan_error" >&2
        return 1
    fi

    fixture_sources=(
        'let path = dirs::config_dir();'
        'let path = dirs::data_local_dir();'
        'let path = dirs::data_dir();'
        'let path = dirs::cache_dir();'
        'let path = PathBuf::from("settings.json");'
        'let path = PathBuf::from("user_notes.json");'
        'let path = PathBuf::from("sounds.json");'
        'let path = PathBuf::from("echo_trace.json");'
        'let path = PathBuf::from("dhat-heap.json");'
        'let home = std::env::var("HOME");'
        $'let path = dirs::config_dir\n    ();'
    )
    fixture_descriptions=(
        'direct dirs storage root'
        'direct dirs storage root'
        'direct dirs storage root'
        'direct dirs storage root'
        'legacy cwd storage file'
        'legacy cwd storage file'
        'legacy cwd storage file'
        'legacy cwd storage file'
        'legacy cwd storage file'
        'manual HOME storage root'
        'direct dirs storage root'
    )
    fixture_file="$self_test_fixture_root/nested/file with spaces.rs"
    for index in "${!fixture_sources[@]}"; do
        printf '%s\n' "${fixture_sources[$index]}" >"$fixture_file"
        if scan_source_root "$self_test_fixture_root"; then
            printf 'Internal negative fixture unexpectedly passed: %s\n' \
                "${fixture_descriptions[$index]}" >&2
            return 1
        fi
        expected="nested/file with spaces.rs (${fixture_descriptions[$index]})"
        if ((${#violations[@]} != 1)) || [[ "${violations[0]}" != "$expected" ]]; then
            printf 'Internal negative fixture returned the wrong violation: %s\n' \
                "${violations[*]}" >&2
            return 1
        fi
    done
    rm -f -- "$fixture_file"

    mkdir -p "$self_test_fixture_root/.hidden"
    printf '%s\n' '.hidden/' >"$self_test_fixture_root/.gitignore"
    printf '%s\n' 'let path = dirs::data_local_dir();' \
        >"$self_test_fixture_root/.hidden/ignored.rs"
    if scan_source_root "$self_test_fixture_root"; then
        printf 'Internal hidden/ignored fixture unexpectedly passed.\n' >&2
        return 1
    fi
    expected='.hidden/ignored.rs (direct dirs storage root)'
    if ((${#violations[@]} != 1)) || [[ "${violations[0]}" != "$expected" ]]; then
        printf 'Hidden/ignored fixture returned the wrong violation: %s\n' \
            "${violations[*]}" >&2
        return 1
    fi
    rm -rf -- "$self_test_fixture_root/.hidden" "$self_test_fixture_root/.gitignore"

    printf '%s\n' 'let path = dirs::cache_dir();' \
        >"$self_test_fixture_root/nested/$central_module"
    if scan_source_root "$self_test_fixture_root"; then
        printf 'Internal nested-central-module fixture unexpectedly passed.\n' >&2
        return 1
    fi
    expected="nested/$central_module (direct dirs storage root)"
    if ((${#violations[@]} != 1)) || [[ "${violations[0]}" != "$expected" ]]; then
        printf 'Nested central-module fixture returned the wrong violation: %s\n' \
            "${violations[*]}" >&2
        return 1
    fi

    cleanup_self_test_fixture
    self_test_fixture_root=""
    trap - EXIT
}

if ((self_test)); then
    run_self_tests
fi

if ! scan_source_root "$source_root"; then
    if [[ -n "$scan_error" ]]; then
        printf '%s\n' "$scan_error" >&2
    else
        printf 'Echo-owned storage must use storage_roots; isolated-root violations found:\n' >&2
        printf '  - %s\n' "${violations[@]}" >&2
    fi
    exit 1
fi

printf 'PASS: all Echo-owned storage roots route through %s.\n' "$central_module"
