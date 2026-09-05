#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

crates=(
  "nanocodex-oai-api:crates/nanocodex-oai-api"
  "nanocodex-tools-macros:crates/nanocodex-tools/macros"
  "nanocodex-observability:crates/nanocodex-observability"
  "nanocodex-tools:crates/nanocodex-tools"
  "nanocodex-agent:crates/nanocodex-agent"
  "nanocodex-durability:crates/nanocodex-durability"
  "nanocodex-subagents:crates/nanocodex-subagents"
  "nanocodex:crates/nanocodex"
)

case "${1:-}" in
  names)
    for crate in "${crates[@]}"; do
      printf '%s\n' "${crate%%:*}"
    done
    ;;
  paths)
    for crate in "${crates[@]}"; do
      printf '%s\n' "${crate#*:}"
    done
    ;;
  check)
    diff -u \
      <(for crate in "${crates[@]}"; do printf '%s\n' "${crate%%:*}"; done | sort) \
      <(
        cargo metadata --manifest-path "$repository_root/Cargo.toml" \
          --no-deps --format-version 1 |
          jq -r '.packages[] | select(.name | startswith("nanocodex")) | select(.publish != []) | .name' |
          sort
      )
    ;;
  *)
    echo "usage: $0 {names|paths|check}" >&2
    exit 2
    ;;
esac
