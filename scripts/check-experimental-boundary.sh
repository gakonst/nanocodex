#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
metadata_file="$(mktemp 2>/dev/null)" || {
  echo "failed to create a Cargo metadata file" >&2
  exit 1
}
trap 'rm -f -- "$metadata_file"' EXIT

cd "$repository_root"
cargo metadata --no-deps --format-version 1 > "$metadata_file"

stable_prefix="$repository_root/crates/"
experimental_prefix="$repository_root/crates/experimental/"

published="$(
  jq -r --arg experimental "$experimental_prefix" '
    .packages[]
    | select(.manifest_path | startswith($experimental))
    | select(.publish != [])
    | .name
  ' "$metadata_file"
)"
if [[ -n "$published" ]]; then
  echo "experimental packages must not be published:" >&2
  printf '%s\n' "$published" >&2
  exit 1
fi

violations="$(
  jq -r \
    --arg stable "$stable_prefix" \
    --arg experimental "$experimental_prefix" '
      [.packages[]
        | select(.manifest_path | startswith($experimental))
        | .name
      ] as $experimental_names
      | .packages[]
      | select(
          (.manifest_path | startswith($stable))
          and (.manifest_path | startswith($experimental) | not)
        )
      | . as $package
      | .dependencies[]
      | select(.source == null)
      | select(.name as $dependency | $experimental_names | index($dependency))
      | "\($package.name) -> \(.name)"
    ' "$metadata_file"
)"
if [[ -n "$violations" ]]; then
  echo "stable crates must not depend on experimental crates:" >&2
  printf '%s\n' "$violations" >&2
  exit 1
fi
