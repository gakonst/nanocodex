#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

crates=(
  "nanocodex-hand:crates/nanocodex-hand"
  "nanocodex-home:crates/nanocodex-home"
  "nanocodex-oai-api:crates/nanocodex-oai-api"
  "nanocodex-observability:crates/nanocodex-observability"
  "nanocodex-oai-tools-macros:crates/nanocodex-oai-tools/macros"
  "nanocodex-voice-native:crates/nanocodex-voice-native"
  "nanocodex-voice-protocol:crates/nanocodex-voice-protocol"
  "nanocodex-computer:crates/experimental/nanocodex-computer"
  "nanocodex-oai-tools:crates/nanocodex-oai-tools"
  "nanocodex-voice-ffi:crates/nanocodex-voice-ffi"
  "nanocodex-agent:crates/nanocodex-agent"
  "nanocodex-claude-tools:crates/nanocodex-claude-tools"
  "nanocodex-claude:crates/nanocodex-claude"
  "nanocodex-durability:crates/nanocodex-durability"
  "nanocodex-managed:crates/nanocodex-managed"
  "nanocodex-subagents:crates/nanocodex-subagents"
  "nanocodex:crates/nanocodex"
  "nanocodex-voice:crates/nanocodex-voice"
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
      <(for crate in "${crates[@]}"; do printf '%s\n' "${crate%%:*}"; done | LC_ALL=C sort) \
      <(
        cargo metadata --manifest-path "$repository_root/Cargo.toml" \
          --locked --no-deps --format-version 1 |
          jq -r '.packages[] | select(.name | startswith("nanocodex")) | select(.publish != []) | .name' |
          LC_ALL=C sort
      )
    ;;
  *)
    echo "usage: $0 {names|paths|check}" >&2
    exit 2
    ;;
esac
