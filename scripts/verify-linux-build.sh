#!/usr/bin/env bash
# Build the locked `dist` Echo and echo-version binaries on Linux and verify that
# each is a 64-bit ELF with resolvable runtime linkage and a non-executable
# stack, and that echo-version reports the workspace version and a Linux
# target. It does not launch a window.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$repo_root/target/dist/echo"
version_binary="$repo_root/target/dist/echo-version"

if [[ "$(uname -s)" != "Linux" ]]; then
    printf 'This verification harness must run on Linux.\n' >&2
    exit 1
fi

"$repo_root/scripts/check-linux-build-deps.sh"
"$repo_root/scripts/setup-gpui-fork.sh"

cd "$repo_root"
cargo build --profile dist --locked --bin echo
cargo build --profile dist --locked -p echo-version

for artifact in "$binary" "$version_binary"; do
    [[ -x "$artifact" ]] || {
        printf 'Expected executable was not produced: %s\n' "$artifact" >&2
        exit 1
    }
    if ! file "$artifact" | grep -q 'ELF 64-bit'; then
        printf 'Not a 64-bit ELF executable: %s\n%s\n' "$artifact" "$(file "$artifact")" >&2
        exit 1
    fi
    linkage="$(ldd -r "$artifact" 2>&1)"
    if grep -Eqi 'not found|undefined symbol' <<<"$linkage"; then
        printf '%s\n' "$linkage" >&2
        exit 1
    fi
    if ! readelf -W -l "$artifact" | grep -Eq 'GNU_STACK[[:space:]].*RW[[:space:]]'; then
        printf 'Expected a non-executable (RW) GNU_STACK segment in %s:\n' "$artifact" >&2
        readelf -W -l "$artifact" | grep -E 'GNU_STACK' >&2 || printf '  (no GNU_STACK segment)\n' >&2
        exit 1
    fi
done

reported_identity="$($version_binary)"
expected_version="$(
    sed -nE '/^\[workspace\.package\]$/,/^\[/ {
        s/^version = "([^"]+)"/\1/p
    }' "$repo_root/Cargo.toml"
)"
[[ -n "$expected_version" ]] || {
    printf 'Could not read [workspace.package] version from Cargo.toml.\n' >&2
    exit 1
}
grep -qx "Echo $expected_version" <<<"$reported_identity" || {
    printf 'echo-version output did not contain the expected Echo %s identity:\n%s\n' \
        "$expected_version" "$reported_identity" >&2
    exit 1
}
grep -Eq '^Target linux/(x86_64|aarch64)$' <<<"$reported_identity" || {
    printf 'echo-version output did not identify a supported Linux target:\n%s\n' \
        "$reported_identity" >&2
    exit 1
}

printf 'Echo Linux dist build is valid: %s\n' "$binary"
