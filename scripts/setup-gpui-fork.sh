#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
pinned_rev="9a8ab54db4f9f9af4dc6efb42b304c55c6a62bd3"
expected_tree="7a802743da3a4ba05833b47153b01f2cf567904f"
branch="echo-paint-fork"
fork_path="${1:-$(dirname -- "$repo_root")/zed-gpui-fork}"
patch_manifest="$repo_root/scripts/gpui-fork-patches.txt"

fail() {
    printf 'GPUI fork setup failed: %s\n' "$*" >&2
    exit 1
}

for command_name in git grep sed; do
    command -v "$command_name" >/dev/null 2>&1 || fail "missing command: $command_name"
done

[[ -f "$patch_manifest" ]] || fail "missing patch manifest: $patch_manifest"

cargo_rev="$(
    grep -m 1 -E 'rev = "[0-9a-f]{40}"' "$repo_root/Cargo.toml" |
        sed -E 's/.*rev = "([0-9a-f]{40})".*/\1/'
)"
[[ -n "$cargo_rev" ]] || fail "could not read the GPUI revision from Cargo.toml"
[[ "$cargo_rev" == "$pinned_rev" ]] ||
    fail "Cargo.toml pins $cargo_rev but this script expects $pinned_rev"

mapfile -t patch_files < <(
    sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$patch_manifest"
)
((${#patch_files[@]} > 0)) || fail "patch manifest is empty: $patch_manifest"
for patch_file in "${patch_files[@]}"; do
    [[ -f "$repo_root/$patch_file" ]] || fail "missing patch: $repo_root/$patch_file"
done

if [[ ! -d "$fork_path/.git" ]]; then
    printf 'Cloning Zed into %s (blob-filtered)...\n' "$fork_path"
    git clone --filter=blob:none https://github.com/zed-industries/zed "$fork_path"
fi

[[ -z "$(git -C "$fork_path" status --porcelain --untracked-files=no)" ]] ||
    fail "the GPUI fork contains tracked working-tree changes"

if git -C "$fork_path" show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$fork_path" switch "$branch" >/dev/null
    current_tree="$(git -C "$fork_path" rev-parse 'HEAD^{tree}')"
    [[ "$current_tree" == "$expected_tree" ]] || fail \
        "existing $branch has tree $current_tree; expected $expected_tree. Preserve or rename the stale branch, then rerun."
    [[ -z "$(git -C "$fork_path" status --porcelain --untracked-files=no)" ]] ||
        fail "the GPUI fork contains tracked working-tree changes"
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
    if ! git -C "$fork_path" am "$repo_root/$patch_file"; then
        printf 'Patch application stopped. Inspect %s and run git am --abort before retrying.\n' "$fork_path" >&2
        exit 1
    fi
done

current_tree="$(git -C "$fork_path" rev-parse 'HEAD^{tree}')"
[[ "$current_tree" == "$expected_tree" ]] ||
    fail "reconstructed tree is $current_tree; expected $expected_tree"
[[ -z "$(git -C "$fork_path" status --porcelain --untracked-files=no)" ]] ||
    fail "the reconstructed GPUI fork contains tracked working-tree changes"

printf 'GPUI fork ready at %s (tree %s).\n' "$fork_path" "$current_tree"
