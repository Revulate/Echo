#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
registry="$repo_root/scripts/linux-test-targets.tsv"
stage_runner="$repo_root/scripts/run-linux-stage.sh"
report_dir="${ECHO_LINUX_TOOL_REPORT_DIR:-$repo_root/target/linux-tool-checks}"
tool_timeout="${ECHO_LINUX_TOOL_TIMEOUT_SECONDS:-900}"
heartbeat="${ECHO_LINUX_STAGE_HEARTBEAT_SECONDS:-30}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf -- "$tmp_dir"' EXIT

mkdir -p "$report_dir"
cd "$repo_root"

cargo metadata --locked --no-deps --format-version 1 >"$tmp_dir/metadata.json"
python3 - "$repo_root" "$tmp_dir/metadata.json" <<'PY' | sort >"$tmp_dir/manifests.tsv"
import json
import pathlib
import sys

repo_root = pathlib.Path(sys.argv[1]).resolve()
tool_root = repo_root / "tools"
metadata = json.loads(pathlib.Path(sys.argv[2]).read_text(encoding="utf-8"))

for package in metadata["packages"]:
    manifest = pathlib.Path(package["manifest_path"]).resolve()
    try:
        directory = manifest.parent.relative_to(tool_root)
    except ValueError:
        continue
    for target in package["targets"]:
        if "bin" in target["kind"]:
            print(f"{directory}\t{package['name']}\t{target['name']}")
PY

awk -F '\t' '
    $1 !~ /^#/ {
        if (NF != 4) {
            printf "Malformed registry row %d\n", NR > "/dev/stderr";
            exit 1;
        }
        if ($4 !~ /^(contract|fixture|runtime|benchmark|helper)$/) {
            printf "Unknown registry category %s on row %d\n", $4, NR > "/dev/stderr";
            exit 1;
        }
        print $1 "\t" $2 "\t" $3;
    }
' "$registry" | sort >"$tmp_dir/registry.tsv"

if ! diff -u "$tmp_dir/manifests.tsv" "$tmp_dir/registry.tsv"; then
    printf 'Linux tool registry does not exactly match tools/*/Cargo.toml binary targets.\n' >&2
    exit 1
fi

contract_count="$(awk -F '\t' '$1 !~ /^#/ && $4 == "contract" { count++ } END { print count + 0 }' "$registry")"
if [[ "$contract_count" != 38 ]]; then
    printf 'Expected exactly 38 deterministic no-argument contracts, found %s.\n' "$contract_count" >&2
    exit 1
fi
printf 'Tool registry PASS binaries=%s contracts=%s\n' "$(wc -l <"$tmp_dir/registry.tsv")" "$contract_count"

while IFS=$'\t' read -r directory package target category; do
    [[ "$directory" == \#* || "$category" != "contract" ]] && continue
    "$stage_runner" \
        --timeout-seconds "$tool_timeout" \
        --heartbeat-seconds "$heartbeat" \
        --log "$report_dir/$package.txt" \
        -- "tool-$package" \
        env CARGO_TERM_COLOR=never cargo run --quiet --locked -p "$package" --bin "$target"
done <"$registry"

printf 'Echo Linux standalone tool checks passed: %s\n' "$contract_count"
