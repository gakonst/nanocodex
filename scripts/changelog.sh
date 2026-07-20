#!/usr/bin/env bash
set -euo pipefail

run_unless_dry_run() {
    if [[ "${DRY_RUN:-false}" == "true" ]]; then
        echo "skipping due to dry run: $*" >&2
    else
        "$@"
    fi
}

workspace_root=${WORKSPACE_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}
crate_root=${CRATE_ROOT:-}
command=(git cliff --workdir "$workspace_root" --config "$workspace_root/cliff.toml" "$@")

if [[ -z "${GITHUB_TOKEN:-}" ]] && command -v gh >/dev/null; then
    github_token=$(gh auth token 2>/dev/null || true)
    if [[ -n "$github_token" ]]; then
        export GITHUB_TOKEN="$github_token"
    fi
fi

generate_crate_changelog() {
    local current_crate=$1
    local crate_glob="${current_crate#"$workspace_root/"}/**"

    run_unless_dry_run \
        "${command[@]}" \
        --include-path "$crate_glob" \
        --output "$current_crate/CHANGELOG.md"
}

# cargo-release invokes this hook once per crate. As in Alloy, every invocation
# refreshes the complete repository changelog and the current crate gets a
# path-filtered changelog of its own.
run_unless_dry_run "${command[@]}" --output "$workspace_root/CHANGELOG.md"

if [[ -n "$crate_root" ]]; then
    if [[ "$crate_root" == "$workspace_root"/crates/* ]]; then
        generate_crate_changelog "$crate_root"
    fi
else
    for crate_name in \
        nanocodex-core \
        nanocodex-macros \
        nanocodex-observability \
        nanocodex-service \
        nanocodex-tools \
        nanocodex-mcp \
        nanocodex; do
        generate_crate_changelog "$workspace_root/crates/$crate_name"
    done
fi
