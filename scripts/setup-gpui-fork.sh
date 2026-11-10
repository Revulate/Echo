#!/usr/bin/env bash
# Reconstruct, or validate, the sibling GPUI fork that Cargo.toml's
# [patch."https://github.com/zed-industries/zed"] section points at: Zed at the
# revision Cargo.toml pins, plus scripts/gpui-fork-patches.txt applied with
# `git am`, checked against the manifest's fork-tree. See docs/GPUI_FORK.md.
#
# Usage: scripts/setup-gpui-fork.sh [fork-path]   (default: ../zed-gpui-fork)
# setup-gpui-fork.ps1 is the PowerShell twin; keep their checks and messages alike.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
branch="echo-paint-fork"
fork_path="${1:-$(dirname -- "$repo_root")/zed-gpui-fork}"
patch_manifest="$repo_root/scripts/gpui-fork-patches.txt"
# `git am` records a committer; the tree check below does not depend on it, so
# a fixed identity keeps fresh machines and CI runners without one working.
git_am_identity=(-c user.name="Echo GPUI fork" -c user.email="gpui-fork@echo.invalid")

fail() {
    printf 'GPUI fork setup failed: %s\n' "$*" >&2
    exit 1
}

require_clean_fork() {
    [[ -z "$(git -C "$fork_path" status --porcelain --untracked-files=no)" ]] ||
        fail "$fork_path contains tracked working-tree changes. Restore or commit them, then rerun."
}

for command_name in git grep sed; do
    command -v "$command_name" >/dev/null 2>&1 || fail "missing command: $command_name"
done

[[ -f "$patch_manifest" ]] || fail "missing patch manifest: $patch_manifest"

mapfile -t cargo_revs < <(
    grep -oE 'github\.com/zed-industries/zed", rev = "[0-9a-f]{40}"|^[[:space:]]*rev = "[0-9a-f]{40}"' \
        "$repo_root/Cargo.toml" | grep -oE '[0-9a-f]{40}' | sort -u
)
((${#cargo_revs[@]} > 0)) || fail "could not read the GPUI revision from Cargo.toml"
((${#cargo_revs[@]} == 1)) || fail "Cargo.toml pins more than one Zed revision: ${cargo_revs[*]}"
pinned_rev="${cargo_revs[0]}"

expected_tree="$(sed -nE 's/^fork-tree[[:space:]]*=[[:space:]]*([0-9a-f]{40})[[:space:]]*$/\1/p' "$patch_manifest")"
[[ "$expected_tree" =~ ^[0-9a-f]{40}$ ]] || fail "missing or malformed fork-tree in $patch_manifest"

mapfile -t patch_files < <(
    sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' -e '/=/d' "$patch_manifest"
)
((${#patch_files[@]} > 0)) || fail "patch manifest lists no patches: $patch_manifest"
for patch_file in "${patch_files[@]}"; do
    [[ -f "$repo_root/$patch_file" ]] || fail "missing patch: $repo_root/$patch_file"
done

if [[ ! -d "$fork_path/.git" ]]; then
    printf 'Cloning Zed into %s (blob-filtered)...\n' "$fork_path"
    git clone --filter=blob:none https://github.com/zed-industries/zed "$fork_path"
fi

require_clean_fork

if git -C "$fork_path" show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$fork_path" switch "$branch" >/dev/null
    current_tree="$(git -C "$fork_path" rev-parse 'HEAD^{tree}')"
    [[ "$current_tree" == "$expected_tree" ]] || fail \
        "existing $branch has tree $current_tree; expected $expected_tree. Preserve or rename the stale branch, then rerun."
    require_clean_fork
    printf 'GPUI fork ready at %s (tree %s).\n' "$fork_path" "$current_tree"
    exit 0
fi

if ! git -C "$fork_path" cat-file -e "$pinned_rev^{commit}" 2>/dev/null; then
    git -C "$fork_path" remote get-url origin >/dev/null 2>&1 ||
        fail "the GPUI fork has no origin remote for fetching $pinned_rev"
    git -C "$fork_path" fetch origin "$pinned_rev"
fi

git -C "$fork_path" switch --detach "$pinned_rev" >/dev/null
git -C "$fork_path" switch -c "$branch" >/dev/null

for patch_file in "${patch_files[@]}"; do
    printf 'Applying %s...\n' "$patch_file"
    if ! git "${git_am_identity[@]}" -C "$fork_path" am "$repo_root/$patch_file"; then
        fail "git am stopped at $patch_file. Run git am --abort in $fork_path and inspect the patch."
    fi
done

current_tree="$(git -C "$fork_path" rev-parse 'HEAD^{tree}')"
[[ "$current_tree" == "$expected_tree" ]] ||
    fail "reconstructed tree is $current_tree; expected $expected_tree. Regenerate the patch series or update fork-tree."
require_clean_fork

printf 'GPUI fork ready at %s (tree %s).\n' "$fork_path" "$current_tree"
