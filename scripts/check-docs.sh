#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

target_dir="${CARGO_TARGET_DIR:-$repository_root/target}"
if [[ "$target_dir" != /* ]]; then
  target_dir="$repository_root/$target_dir"
fi

version="$(cargo metadata --no-deps --format-version 1 | jq -er '.packages[] | select(.name == "nanocodex") | .version')"
./scripts/release-crates.sh check
crates=()
while IFS= read -r crate; do
  crates+=("$crate")
done < <(./scripts/release-crates.sh names)
temporary_dir="$(mktemp -d 2>/dev/null)" || {
  echo "failed to create a temporary docs.rs check directory" >&2
  exit 1
}
trap 'rm -rf -- "$temporary_dir"' EXIT

for crate in "${crates[@]}"; do
  archive="$target_dir/package/${crate}-${version}.crate"
  test -f "$archive" || {
    echo "missing $archive; package the release crates first" >&2
    exit 1
  }
  tar -xzf "$archive" -C "$temporary_dir"
done

release_config="$temporary_dir/release.toml"
{
  echo '[patch.crates-io]'
  for crate in "${crates[@]}"; do
    printf '%s = { path = "%s/%s-%s" }\n' \
      "$crate" "$temporary_dir" "$crate" "$version"
  done
} > "$release_config"

for crate in "${crates[@]}"; do
  echo "Documenting packaged $crate $version..."
  RUSTDOCFLAGS="-D warnings" \
    CARGO_TARGET_DIR="$target_dir/docsrs-check" \
    cargo doc \
      --all-features \
      --no-deps \
      --config "$release_config" \
      --manifest-path "$temporary_dir/${crate}-${version}/Cargo.toml"
done
