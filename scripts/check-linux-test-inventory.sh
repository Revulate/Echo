#!/usr/bin/env bash
# Compare every test, contract tool and doctest in the root crate, the extracted
# crates and the cache-preload-check tool against the reviewed baseline in
# scripts/linux-test-inventory.tsv, then enforce the gpui::test shadowing guard
# (see docs/LINUX_DEVELOPMENT.md). Adding, moving or renaming a test means
# updating the baseline in the same change.
set -euo pipefail

# The baseline is byte-ordered. Pin collation so `sort` and `diff` agree on every
# workstation locale, not only on CI's C.UTF-8.
export LC_ALL=C.UTF-8

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
baseline="$repo_root/scripts/linux-test-inventory.tsv"
registry="$repo_root/scripts/linux-test-targets.tsv"
tmp_dir="$(mktemp -d)"
trap 'rm -rf -- "$tmp_dir"' EXIT
cd "$repo_root"

summary_value() {
    local key="$1"
    awk -F '\t' -v key="$key" '$1 == "summary" && $2 == key { print $3 }' "$baseline"
}

extract_tests() {
    find "$@" -type d -name target -prune -o -type f -name '*.rs' -print0 \
        | sort -z \
        | xargs -0 perl -ne '
            if (/^\s*#\[(?:tokio::)?test(?:\s|\(|\])/) { $pending = 1; }
            if ($pending && /(?:async\s+)?fn\s+([A-Za-z0-9_]+)/) {
                $file = $ARGV;
                $file =~ s|^\./||;
                print "$file\t$1\n";
                $pending = 0;
            }
        ' \
        | sort
}

check_test_scope() {
    local kind="$1"
    local output="$2"
    shift 2
    extract_tests "$@" | sed "s/^/$kind\t/" >"$output"
    awk -F '\t' -v kind="$kind" '$1 == kind' "$baseline" >"$output.expected"
    if [[ ! -s "$output" || ! -s "$output.expected" ]]; then
        printf 'Linux test inventory must not silently skip %s.\n' "$kind" >&2
        return 1
    fi
    if ! diff -u "$output.expected" "$output"; then
        printf 'Linux test inventory drift for %s. Review the diff and deliberately update %s.\n' \
            "$kind" "$baseline" >&2
        return 1
    fi
    printf 'Inventory PASS scope=%s count=%s\n' "$kind" "$(wc -l <"$output")"
}

check_test_scope root_test "$tmp_dir/root-tests.tsv" src
check_test_scope cache_preload_tool_test "$tmp_dir/cache-preload-tool-tests.tsv" tools/cache-preload-check/src
# Each extracted crate's baseline kind is its package name without the echo-
# prefix, in snake case: crates/echo-cache-policy -> cache_policy_test.
extracted_crate_dirs=()
for crate_manifest in crates/*/Cargo.toml; do
    extracted_crate_dirs+=("${crate_manifest%/Cargo.toml}")
done

crate_scope() {
    local crate_name="${1##*/}"
    crate_name="${crate_name#echo-}"
    printf '%s' "${crate_name//-/_}"
}

for crate_dir in "${extracted_crate_dirs[@]}"; do
    scope="$(crate_scope "$crate_dir")"
    check_test_scope "${scope}_test" "$tmp_dir/$scope-tests.tsv" "$crate_dir"
done

contract_count="$(awk -F '\t' '$1 !~ /^#/ && $4 == "contract" { count++ } END { print count + 0 }' "$registry")"
expected_contract_count="$(summary_value contract_tools)"
if [[ "$contract_count" != "$expected_contract_count" || "$contract_count" == 0 ]]; then
    printf 'Linux contract inventory drift: expected=%s actual=%s\n' "$expected_contract_count" "$contract_count" >&2
    exit 1
fi
printf 'Inventory PASS scope=contract_tools count=%s\n' "$contract_count"

check_doctest_scope() {
    local scope="$1"
    local expected_runnable
    local expected_ignored
    local runnable_doctests
    local ignored_doctests
    shift
    read -r runnable_doctests ignored_doctests < <(
        find "$@" -type d -name target -prune -o -type f -name '*.rs' -print0 \
        | sort -z \
        | xargs -0 perl -ne '
            if (/^\s*\/\/[!\/]\s*```([^`]*)/) {
                $label = $1;
                $label =~ s/\s+//g;
                if (!$open) {
                    $open = 1;
                    if ($label eq "" || $label =~ /^(?:rust(?:,.*)?|ignore(?:,.*)?|no_run(?:,.*)?)$/) {
                        if ($label =~ /(?:^|,)ignore(?:,|$)/) { $ignored++; }
                        else { $runnable++; }
                    }
                } else {
                    $open = 0;
                }
            }
            END { print(($runnable || 0) . " " . ($ignored || 0) . "\n"); }
        '
    )
    expected_runnable="$(summary_value "${scope}_doctests_runnable")"
    expected_ignored="$(summary_value "${scope}_doctests_ignored")"
    if [[ "$runnable_doctests" != "$expected_runnable" || "$ignored_doctests" != "$expected_ignored" ]]; then
        printf '%s doctest inventory drift: expected runnable=%s ignored=%s, actual runnable=%s ignored=%s\n' \
            "$scope" "$expected_runnable" "$expected_ignored" \
            "$runnable_doctests" "$ignored_doctests" >&2
        return 1
    fi
    printf 'Inventory PASS scope=%s_doctests runnable=%s ignored=%s\n' \
        "$scope" "$runnable_doctests" "$ignored_doctests"
}

check_doctest_scope root src
check_doctest_scope cache_preload_tool tools/cache-preload-check/src
for crate_dir in "${extracted_crate_dirs[@]}"; do
    check_doctest_scope "$(crate_scope "$crate_dir")" "$crate_dir/src"
done

# ----------------------------------------------------------------------------
# gpui::test shadowing guard
# ----------------------------------------------------------------------------
#
# `use gpui::*` (and the brace form `use gpui::{.., *}`) re-exports gpui's
# proc-macro attribute `gpui::test`, which shadows the built-in `#[test]`.
# gpui's macro emits `#[test]` again, so expansion recurses; the root crate's
# `#![recursion_limit = "4096"]` turns that into a multi-minute expansion and
# eventually a stack overflow rather than a fast error. That is the real cause
# of rustc crashing on the root test target.
#
# The fix is one explicit import per test module: an explicit (non-glob) import
# always wins over a glob, regardless of source order, so canonical rustfmt
# placement is fine. Rather than trying to decide which module chains can see a
# gpui glob, this rule is deliberately over-approximate and requires the import
# in EVERY root `#[cfg(test)] mod` block. Extracted crates under crates/ use
# explicit gpui imports instead and are not covered here.
#
# The rule reports, and never silently skips:
#   * a `#[cfg(test)] mod NAME { ... }` block without the import
#   * the `#[cfg(test)] mod NAME;` file-module form, which it cannot inspect
#   * a `#[test]`/`#[tokio::test]` outside any `#[cfg(test)] mod` block
#   * a block whose canonical closing brace cannot be located
check_test_shadow_guard() {
    local findings
    findings="$(
        find src -type d -name target -prune -o -type f -name '*.rs' -print0 \
        | sort -z \
        | xargs -0 perl -0777 -ne '
            my $src = $_;
            my $file = $ARGV;
            $file =~ s|^\./||;
            my @covered;

            while ($src =~ /^([ \t]*)#\[cfg\(test\)\][ \t]*\r?\n[ \t]*mod[ \t]+([A-Za-z0-9_]+)[ \t]*(\{|;)/gm) {
                my ($indent, $mod, $kind) = ($1, $2, $3);
                my $line = 1 + (substr($src, 0, $-[0]) =~ tr/\n//);
                if ($kind eq ";") {
                    print "$file:$line\tunhandled\t#[cfg(test)] mod $mod; cannot be inspected in place\n";
                    next;
                }
                my $body_at = $+[0];
                my $close = "\n" . $indent . "}";
                my $end = index($src, $close, $body_at);
                if ($end < 0) {
                    print "$file:$line\tunhandled\tno canonically indented close for mod $mod\n";
                    next;
                }
                my $body = substr($src, $body_at, $end - $body_at);
                push @covered, [$body_at, $end];
                unless ($body =~ /^[ \t]*use core::prelude::v1::test;[ \t]*\r?$/m) {
                    print "$file:$line\tmissing\tmod $mod lacks use core::prelude::v1::test;\n";
                }
            }

            while ($src =~ /^[ \t]*#\[(?:tokio::)?test(?:\s|\(|\])/gm) {
                my $at = $-[0];
                my $inside = 0;
                for my $r (@covered) { $inside = 1 if $at >= $r->[0] && $at < $r->[1]; }
                next if $inside;
                my $line = 1 + (substr($src, 0, $at) =~ tr/\n//);
                print "$file:$line\tstray\t#[test] outside any #[cfg(test)] mod block\n";
            }
        '
    )"

    if [[ -n "$findings" ]]; then
        printf 'gpui::test shadowing guard FAILED (%s finding(s)):\n' "$(printf '%s\n' "$findings" | wc -l)" >&2
        printf '%s\n' "$findings" >&2
        printf 'Add `use core::prelude::v1::test;` inside each reported #[cfg(test)] mod block.\n' >&2
        return 1
    fi
    printf 'Inventory PASS scope=test_shadow_guard violations=0\n'
}

check_test_shadow_guard
