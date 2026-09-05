#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

for command in cargo jq; do
  command -v "$command" >/dev/null || {
    echo "required command not found: $command" >&2
    exit 1
  }
done

public_packages='[
  "nanocodex",
  "nanocodex-agent",
  "nanocodex-durability",
  "nanocodex-managed",
  "nanocodex-oai-api",
  "nanocodex-observability",
  "nanocodex-subagents",
  "nanocodex-tools",
  "nanocodex-tools-macros"
]'
metadata="$(cargo metadata --locked --no-deps --format-version 1)"

assert_snapshot() {
  local label="$1"
  local expected="$2"
  local actual="$3"

  if [[ "$actual" == "$expected" ]]; then
    return
  fi

  echo "$label changed outside the allowed architecture:" >&2
  diff -u \
    -L "expected $label" \
    -L "actual $label" \
    <(printf '%s\n' "$expected") \
    <(printf '%s\n' "$actual") >&2 || true
  exit 1
}

expected_packages=$'nanocodex\nnanocodex-agent\nnanocodex-durability\nnanocodex-managed\nnanocodex-oai-api\nnanocodex-observability\nnanocodex-subagents\nnanocodex-tools\nnanocodex-tools-macros'
actual_packages="$(
  jq -r '
    .packages[]
    | select(.manifest_path | contains("/crates/nanocodex"))
    | .name
  ' <<<"$metadata" | LC_ALL=C sort
)"
assert_snapshot "public package set" "$expected_packages" "$actual_packages"

expected_edges=$'nanocodex\tnanocodex-agent\tnormal\tall\nnanocodex\tnanocodex-durability\tnormal\tall\nnanocodex\tnanocodex-managed\tnormal\tcfg(not(target_family = "wasm"))\nnanocodex\tnanocodex-oai-api\tnormal\tall\nnanocodex\tnanocodex-observability\tnormal\tcfg(not(target_family = "wasm"))\nnanocodex\tnanocodex-tools\tnormal\tall\nnanocodex-agent\tnanocodex-oai-api\tnormal\tall\nnanocodex-agent\tnanocodex-tools\tnormal\tall\nnanocodex-durability\tnanocodex-agent\tnormal\tall\nnanocodex-managed\tnanocodex-agent\tnormal\tall\nnanocodex-managed\tnanocodex-oai-api\tnormal\tall\nnanocodex-managed\tnanocodex-tools\tnormal\tall\nnanocodex-subagents\tnanocodex-agent\tnormal\tall\nnanocodex-subagents\tnanocodex-tools\tnormal\tall\nnanocodex-tools\tnanocodex-oai-api\tnormal\tall\nnanocodex-tools\tnanocodex-tools-macros\tnormal\tcfg(not(target_family = "wasm"))'
actual_edges="$(
  jq -r --argjson public "$public_packages" '
    .packages[]
    | select(.name as $name | $public | index($name))
    | .name as $from
    | .dependencies[]
    | select(.kind != "dev" and .path != null)
    | [$from, .name, (.kind // "normal"), (.target // "all")]
    | @tsv
  ' <<<"$metadata" | LC_ALL=C sort
)"
assert_snapshot "public crate dependency graph" "$expected_edges" "$actual_edges"

forbidden_dependencies="$(
  jq -r --argjson public "$public_packages" '
    .packages[]
    | select(.name as $name | $public | index($name))
    | .name as $from
    | .dependencies[]
    | select(.kind != "dev")
    | select(
        .name == "nanousd"
        or .name == "mpp"
        or .name == "tempo"
        or (.name | startswith("mpp-"))
        or (.name | startswith("tempo-"))
      )
    | "\($from) -> \(.name)"
  ' <<<"$metadata" | LC_ALL=C sort
)"
if [[ -n "$forbidden_dependencies" ]]; then
  echo "public crates must not depend on application-owned payment packages:" >&2
  printf '%s\n' "$forbidden_dependencies" >&2
  exit 1
fi

echo "crate boundaries match the public SDK architecture"
