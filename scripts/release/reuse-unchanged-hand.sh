#!/usr/bin/env bash
# Republish the previous release's exact Hand when nothing that reaches it changed.
#
#   scripts/release/reuse-unchanged-hand.sh DIST_DIR PREVIOUS_TAG
#
# For every DIST_DIR/nanocodex2-<target>.identity whose reuse key equals the
# checksum-verified key published on PREVIOUS_TAG, replace the freshly built
# Hand assets with that release's checksum-verified bytes:
#
#   x86_64-unknown-linux-gnu  nanocodex2-<target>.gz
#   aarch64-apple-darwin      nanocodex2-<target>.gz and
#                             nanocodex-app-aarch64-apple-darwin.tar.gz
#
# Updaters then see a byte-identical Hand and change only the CLI: no Hand
# restart, and on macOS no new code identity. The CLI (nanocodex-*) is never
# touched. Any other target (Windows ships a raw .exe and an installer that
# embeds the Hand) is published as built. Reuse fails closed: a missing,
# malformed or unverifiable previous key or asset publishes the new build, and
# a target's assets are swapped only once all of them verified. Run before
# SHA256SUMS is generated. Requires gh with GH_TOKEN and GITHUB_REPOSITORY.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 DIST_DIR PREVIOUS_TAG" >&2
  exit 2
fi
dist=$1
previous=$2
repository=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}

if command -v sha256sum >/dev/null 2>&1; then
  sha256_check() { sha256sum --check --strict --status "$1"; }
else
  sha256_check() { shasum -a 256 --check --strict --status "$1"; }
fi

summary() {
  echo "$1"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then echo "- $1" >> "$GITHUB_STEP_SUMMARY"; fi
}

# A reuse key file holds exactly one "<64 hex>  nanocodex2-<target>" line.
valid_key() {
  local file=$1 name=$2
  [[ -f "$file" && "$(wc -l < "$file")" -eq 1 ]] &&
    grep -Eqx "[0-9a-f]{64}  $name" "$file"
}

# Exactly one checksum line for $1 in manifest $2, written to $3.
checksum_line() {
  awk -v asset="$1" '$2 == asset || $2 == "*" asset { print; count++ } END { if (count != 1) exit 1 }' \
    "$2" > "$3"
}

shopt -s nullglob
identities=("$dist"/nanocodex2-*.identity)
if [[ ${#identities[@]} -eq 0 ]]; then
  summary "This build has no Hand reuse keys; publishing the Hand as built"
  exit 0
fi
if [[ -z "$previous" ]]; then
  summary "No previous release; publishing the Hand as built"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
mkdir "$work/previous"
if ! gh release download "$previous" --repo "$repository" --dir "$work/previous" \
  --pattern SHA256SUMS --pattern 'nanocodex2-*.identity'; then
  echo "::notice::could not download reuse keys from $previous"
fi
if [[ ! -s "$work/previous/SHA256SUMS" ]]; then
  summary "$previous has no checksum manifest; publishing the Hand as built"
  exit 0
fi

for identity in "${identities[@]}"; do
  name="$(basename "$identity" .identity)"
  target=${name#nanocodex2-}
  case "$target" in
    x86_64-unknown-linux-gnu) assets=("$name.gz") ;;
    aarch64-apple-darwin) assets=("$name.gz" nanocodex-app-aarch64-apple-darwin.tar.gz) ;;
    *)
      summary "$name: Hand reuse is unsupported for $target; publishing the new build"
      continue
      ;;
  esac
  if ! valid_key "$identity" "$name"; then
    echo "$identity is not a single '<sha256>  $name' line" >&2
    exit 1
  fi
  previous_key="$work/previous/$name.identity"
  if ! valid_key "$previous_key" "$name" ||
    ! checksum_line "$name.identity" "$work/previous/SHA256SUMS" "$work/previous/$name.identity.sha256" ||
    ! (cd "$work/previous" && sha256_check "$name.identity.sha256"); then
    summary "$name: $previous has no verified reuse key; publishing the new build"
    continue
  fi
  if ! cmp -s "$identity" "$previous_key"; then
    summary "$name changed since $previous; publishing the new build"
    continue
  fi

  rm -rf "$work/assets"
  mkdir "$work/assets"
  patterns=()
  for asset in "${assets[@]}"; do
    patterns+=(--pattern "$asset")
  done
  verified=true
  gh release download "$previous" --repo "$repository" --dir "$work/assets" "${patterns[@]}" ||
    verified=false
  for asset in "${assets[@]}"; do
    "$verified" || break
    if ! checksum_line "$asset" "$work/previous/SHA256SUMS" "$work/assets/$asset.sha256" ||
      ! (cd "$work/assets" && sha256_check "$asset.sha256") ||
      ! gzip -t "$work/assets/$asset"; then
      echo "::warning::$previous has no verified $asset"
      verified=false
    fi
  done
  # Both assets come from one checksum manifest, signed together by
  # macos-sign-hand.sh. The bundled executable carries its own bundle-bound
  # signature, so it never byte-matches the standalone Hand; require instead
  # the sealed layout the updater accepts.
  if "$verified" && [[ "$target" == aarch64-apple-darwin ]]; then
    mkdir "$work/assets/app"
    bundle="$work/assets/app/Nanocodex.app/Contents"
    if ! tar -xzf "$work/assets/nanocodex-app-aarch64-apple-darwin.tar.gz" -C "$work/assets/app" ||
      [[ "$(ls -A "$work/assets/app")" != Nanocodex.app ]] ||
      [[ -n "$(find "$work/assets/app" ! -type f ! -type d)" ]] ||
      [[ ! -f "$bundle/Info.plist" || ! -f "$bundle/_CodeSignature/CodeResources" ]] ||
      [[ ! -f "$bundle/MacOS/nanocodex2" || ! -x "$bundle/MacOS/nanocodex2" ]]; then
      echo "::warning::$previous Nanocodex.app is not a complete signed bundle"
      verified=false
    fi
  fi
  if ! "$verified"; then
    summary "$name: unchanged since $previous, but its assets did not verify; publishing the new build"
    continue
  fi
  for asset in "${assets[@]}"; do
    cp "$work/assets/$asset" "$dist/$asset"
  done
  # Raw compatibility executables are the same bytes, uncompressed.
  if [[ -e "$dist/$name" ]]; then
    gzip -dc "$dist/$name.gz" > "$dist/$name"
    chmod 755 "$dist/$name"
  fi
  echo "::notice title=Unchanged Hand::$name is unchanged since $previous; republishing its exact bytes"
  summary "$name: reused the exact Hand of $previous"
done
