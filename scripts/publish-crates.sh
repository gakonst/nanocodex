#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

version="$(cargo metadata --no-deps --format-version 1 | jq -er '.packages[] | select(.name == "nanocodex") | .version')"
./scripts/release-crates.sh check
crates=()
while IFS= read -r crate; do
  crates+=("$crate")
done < <(./scripts/release-crates.sh names)

is_published() {
  local crate="$1"
  [[ "$(curl --connect-timeout 10 --max-time 30 \
    --user-agent "nanocodex-release/$version" --silent --output /dev/null \
    --write-out '%{http_code}' "https://crates.io/api/v1/crates/$crate/$version")" == "200" ]]
}

for crate in "${crates[@]}"; do
  if is_published "$crate"; then
    echo "$crate $version is already published"
    continue
  fi

  for attempt in 1 2 3 4 5 6; do
    echo "Publishing $crate $version (attempt $attempt/6)..."
    if cargo publish --locked --package "$crate"; then
      break
    fi
    if is_published "$crate"; then
      break
    fi
    if [[ "$attempt" == "6" ]]; then
      echo "Failed to publish $crate $version" >&2
      exit 1
    fi
    sleep "$((attempt * 10))"
  done
done
