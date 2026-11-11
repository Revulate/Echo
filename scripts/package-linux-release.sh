#!/usr/bin/env bash
# Pack the verified Linux `dist` binary into the tarball attached to a GitHub
# release: echo-<version>-linux-<arch>.tar.gz, holding a stripped `echo` and
# the LICENSE. Run scripts/verify-linux-build.sh first; this script only
# packages what that step built. Prints the tarball path.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$repo_root/target/dist/echo"
output_dir="${1:-$repo_root/target/dist/package}"

[[ -x "$binary" ]] || {
    printf 'target/dist/echo is missing; run scripts/verify-linux-build.sh first.\n' >&2
    exit 1
}

version="$(
    sed -nE '/^\[workspace\.package\]$/,/^\[/ {
        s/^version = "([^"]+)"/\1/p
    }' "$repo_root/Cargo.toml"
)"
[[ -n "$version" ]] || {
    printf 'Could not read [workspace.package] version from Cargo.toml.\n' >&2
    exit 1
}

name="echo-$version-linux-$(uname -m)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

install -Dm755 "$binary" "$stage/$name/echo"
install -Dm644 "$repo_root/LICENSE" "$stage/$name/LICENSE"
# The dist profile keeps symbols so local builds can be profiled; a download
# does not need them.
strip --strip-all "$stage/$name/echo"

mkdir -p "$output_dir"
tarball="$output_dir/$name.tar.gz"
tar --owner=0 --group=0 --numeric-owner -C "$stage" -czf "$tarball" "$name"
(cd "$output_dir" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
printf '%s\n' "$tarball"
