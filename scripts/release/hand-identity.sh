#!/usr/bin/env bash
# Write the release reuse key for one platform's Hand.
#
#   scripts/release/hand-identity.sh CLI_BINARY TARGET SIGNING OUTPUT_DIR
#
# The CLI prints the "Hand Identity" of the Hand built from the same tree: a
# digest of exactly the sources, locked dependencies, toolchain, target,
# profile and embedded payloads linked into nanocodex-hand, without a commit
# hash or timestamp. The key adds what changes the shipped Hand outside the
# binary: its signing mode and the packaging inputs. Writes
# OUTPUT_DIR/nanocodex2-TARGET.identity ("<key>  nanocodex2-TARGET"). A CLI
# without a Hand Identity writes nothing, so its Hand is never reused.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 CLI_BINARY TARGET SIGNING OUTPUT_DIR" >&2
  exit 2
fi
cli=$1
target=$2
signing=$3
output=$4
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

identities="$("$cli" --version | sed -n 's/^Hand Identity: //p')"
if [[ -z "$identities" ]]; then
  echo "::notice::This CLI reports no Hand Identity; its Hand is always published as built"
  exit 0
fi
[[ "$identities" =~ ^[0-9a-f]{64}$ ]] || {
  echo "expected exactly one 64-hex Hand Identity line, got: $identities" >&2
  exit 1
}

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum | awk '{ print $1 }'; }
else
  sha256() { shasum -a 256 | awk '{ print $1 }'; }
fi

packaging=()
case "$target" in
  *-apple-darwin) packaging=(scripts/release/macos-sign-hand.sh nanocodex-vm.entitlements) ;;
esac
key="$({
  printf 'hand-identity %s\ntarget %s\nsigning %s\n' "$identities" "$target" "$signing"
  for file in "${packaging[@]}"; do
    printf 'packaging %s %s\n' "$file" "$(sha256 < "$root/$file")"
  done
} | sha256)"
mkdir -p "$output"
printf '%s  nanocodex2-%s\n' "$key" "$target" > "$output/nanocodex2-$target.identity"
echo "Hand Identity $identities; release reuse key $key"
