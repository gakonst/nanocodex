#!/usr/bin/env bash
set -uo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

timestamp="$(date -u '+%Y%m%dT%H%M%SZ')"
log_dir="$repository_root/.nanocodex/code-mode-logs"
log_file="${1:-$log_dir/batch-$timestamp.log}"
mkdir -p "$(dirname "$log_file")"

exec > >(tee -a "$log_file") 2>&1

failures=0

run_step() {
    local label=$1
    shift

    printf '\n===== %s =====\n' "$label"
    printf '$'
    printf ' %q' "$@"
    printf '\n'

    "$@"
    local status=$?
    printf '[exit status: %d]\n' "$status"
    if ((status != 0)); then
        failures=$((failures + 1))
    fi
}

run_shell_step() {
    local label=$1
    local source=$2

    printf '\n===== %s =====\n' "$label"
    printf '$ %s\n' "$source"

    bash -o pipefail -c "$source"
    local status=$?
    printf '[exit status: %d]\n' "$status"
    if ((status != 0)); then
        failures=$((failures + 1))
    fi
}

printf 'Nanocodex code-mode batch\n'
printf 'repository: %s\n' "$repository_root"
printf 'started UTC: %s\n' "$timestamp"
printf 'log: %s\n' "$log_file"

run_step "Git status" git status --short --branch
run_step "Recent commits" git log -10 --oneline --decorate
run_step "Rust compiler" rustc --version
run_step "Cargo" cargo --version
run_shell_step "Workspace packages" \
    "cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | [.name, .version, .edition, .manifest_path] | @tsv'"
run_step "Tracked and unignored files" rg --files
run_shell_step "Rust source line counts" \
    "rg --files -g '*.rs' -0 | xargs -0 wc -l | sort -n"
run_shell_step "Rust maintenance markers" \
    "rg -n -g '*.rs' 'TODO|FIXME|HACK|XXX' || true"
run_shell_step "Potential panic sites" \
    "rg -n -g '*.rs' 'unwrap\\(|expect\\(' || true"
run_step "Workspace test compilation" cargo test --workspace --no-run
run_step "Public example compilation" cargo check -p nanocodex-examples --bins

finished="$(date -u '+%Y%m%dT%H%M%SZ')"
printf '\n===== Summary =====\n'
printf 'finished UTC: %s\n' "$finished"
printf 'failed steps: %d\n' "$failures"
printf 'full log: %s\n' "$log_file"

if ((failures != 0)); then
    exit 1
fi
