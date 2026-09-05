#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/benchmarks/pr50_thresholds.tsv"
target_dir="${CARGO_TARGET_DIR:-target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$repo_root/$target_dir"
fi
criterion_root="$target_dir/criterion"

command -v jq >/dev/null || {
    echo "jq is required to check Criterion estimates" >&2
    exit 2
}

failures=0
checked=0
while IFS=$'\t' read -r relative_path baseline_ns maximum_ns contract; do
    [[ -z "$relative_path" || "$relative_path" == \#* ]] && continue
    estimate="$criterion_root/$relative_path/new/estimates.json"
    if [[ ! -f "$estimate" ]]; then
        echo "FAIL missing Criterion estimate: $relative_path" >&2
        failures=$((failures + 1))
        continue
    fi

    median_ns="$(jq -er '.median.point_estimate' "$estimate")"
    checked=$((checked + 1))
    if awk -v actual="$median_ns" -v maximum="$maximum_ns" \
        'BEGIN { exit !(actual <= maximum) }'; then
        printf 'PASS %-78s median=%12.3f ns max=%12.3f ns\n' \
            "$relative_path" "$median_ns" "$maximum_ns"
    else
        printf 'FAIL %-78s median=%12.3f ns max=%12.3f ns (%s)\n' \
            "$relative_path" "$median_ns" "$maximum_ns" "$contract" >&2
        failures=$((failures + 1))
    fi
done < "$manifest"

if (( failures > 0 )); then
    echo "$failures benchmark regression gate(s) failed" >&2
    exit 1
fi

echo "$checked benchmark regression gates passed"
