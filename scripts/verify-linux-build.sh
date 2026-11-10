#!/usr/bin/env bash
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
cargo check --locked --bin echo
cargo build --profile dist --locked --bin echo
cargo build --profile dist --locked -p echo-version

for artifact in "$binary" "$version_binary"; do
    [[ -x "$artifact" ]] || {
        printf 'Expected executable was not produced: %s\n' "$artifact" >&2
        exit 1
    }
    file "$artifact" | grep -q 'ELF 64-bit'
    linkage="$(ldd -r "$artifact" 2>&1)"
    if grep -Eqi 'not found|undefined symbol' <<<"$linkage"; then
        printf '%s\n' "$linkage" >&2
        exit 1
    fi
    readelf -W -l "$artifact" | grep -Eq 'GNU_STACK[[:space:]].*RW[[:space:]]'
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
