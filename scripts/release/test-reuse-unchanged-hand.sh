#!/usr/bin/env bash
# Offline test of reuse-unchanged-hand.sh against a fake gh release store.
#
#   scripts/release/test-reuse-unchanged-hand.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
reuse="$root/scripts/release/reuse-unchanged-hand.sh"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/bin" "$work/releases"

# gh release download TAG --repo R --dir D --pattern P...: copy matching assets
# of $work/releases/TAG; fail when nothing matches, as gh does.
cat > "$work/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2" == "release download" ]] || { echo "unexpected gh $*" >&2; exit 2; }
tag=$3; shift 3
dir=; patterns=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) shift 2 ;;
    --dir) dir=$2; shift 2 ;;
    --pattern) patterns+=("$2"); shift 2 ;;
    *) echo "unexpected gh argument $1" >&2; exit 2 ;;
  esac
done
release="$FAKE_RELEASES/$tag"
[[ -d "$release" ]] || { echo "release not found" >&2; exit 1; }
mkdir -p "$dir"
found=0
shopt -s nullglob
for pattern in "${patterns[@]}"; do
  for file in "$release"/$pattern; do cp "$file" "$dir/"; found=1; done
done
[[ "$found" -eq 1 ]] || { echo "no assets match" >&2; exit 1; }
GH
chmod 755 "$work/bin/gh"
export PATH="$work/bin:$PATH" FAKE_RELEASES="$work/releases" GITHUB_REPOSITORY=example/nanocodex
unset GITHUB_STEP_SUMMARY

linux=nanocodex2-x86_64-unknown-linux-gnu
mac=nanocodex2-aarch64-apple-darwin
windows=nanocodex2-x86_64-pc-windows-msvc
app=nanocodex-app-aarch64-apple-darwin.tar.gz
key() { printf '%s  %s\n' "$(printf '%s' "$1" | sha256sum | awk '{ print $1 }')" "$2"; }

# make_app DIR HAND_BYTES [incomplete]: write DIR/$app laid out like
# macos-sign-hand.sh output. Signing inside a bundle changes the executable,
# so the bundled bytes differ from the standalone Hand.
make_app() {
  local contents="$1/app/Nanocodex.app/Contents"
  mkdir -p "$contents/MacOS" "$contents/_CodeSignature"
  printf '%s' "$2" > "$contents/MacOS/nanocodex2"
  chmod 755 "$contents/MacOS/nanocodex2"
  printf '<plist/>' > "$contents/Info.plist"
  [[ "${3:-}" == incomplete ]] || printf 'sealed' > "$contents/_CodeSignature/CodeResources"
  COPYFILE_DISABLE=1 tar --no-xattrs -czf "$1/$app" -C "$1/app" Nanocodex.app
  rm -rf "$1/app"
}

# resum DIR: rewrite DIR/SHA256SUMS over every other file.
resum() {
  rm -f "$1/SHA256SUMS"
  (cd "$1" && sha256sum -- *) > "$work/sums"
  mv "$work/sums" "$1/SHA256SUMS"
}

# A published release: Hand bytes "old-<target>", keys for linux+mac+windows.
publish_previous() {
  local release="$work/releases/$1"
  rm -rf "$release"; mkdir -p "$release"
  for name in "$linux" "$mac"; do
    printf 'old-%s' "$name" | gzip -n > "$release/$name.gz"
    key same "$name" > "$release/$name.identity"
  done
  printf 'old-cli' | gzip -n > "$release/nanocodex-x86_64-unknown-linux-gnu.gz"
  printf 'old-windows' > "$release/$windows.exe"
  key same "$windows" > "$release/$windows.identity"
  make_app "$release" "bundle-signed-old-$mac"
  resum "$release"
}

# A fresh build: Hand bytes "new-<target>", keys from $1 (linux) $2 (mac).
fresh_dist() {
  rm -rf "$work/dist"; mkdir -p "$work/dist"
  for name in "$linux" "$mac"; do
    printf 'new-%s' "$name" > "$work/dist/$name"
    gzip -n -c "$work/dist/$name" > "$work/dist/$name.gz"
  done
  key "$1" "$linux" > "$work/dist/$linux.identity"
  key "$2" "$mac" > "$work/dist/$mac.identity"
  printf 'new-cli' | gzip -n > "$work/dist/nanocodex-x86_64-unknown-linux-gnu.gz"
  printf 'new-windows' > "$work/dist/$windows.exe"
  key same "$windows" > "$work/dist/$windows.identity"
  make_app "$work/dist" "bundle-signed-new-$mac"
}

hand() { gzip -dc "$work/dist/$1.gz"; }
app_hand() {
  tar -xzOf "$work/dist/$app" Nanocodex.app/Contents/MacOS/nanocodex2
}
expect() {
  if [[ "$2" != "$3" ]]; then echo "FAIL $1: expected '$3', got '$2'" >&2; exit 1; fi
  echo "ok $1"
}
run() { "$reuse" "$work/dist" "$1" > "$work/log" 2>&1; }

publish_previous v1

fresh_dist same same
run ""
expect "no previous release keeps the build" "$(hand "$linux")" "new-$linux"

fresh_dist same same
run v1
expect "unchanged Linux Hand is reused" "$(hand "$linux")" "old-$linux"
expect "raw Linux compatibility executable follows" "$(cat "$work/dist/$linux")" "old-$linux"
expect "unchanged macOS Hand is reused" "$(hand "$mac")" "old-$mac"
expect "Nanocodex.app is reused with it" "$(app_hand)" "bundle-signed-old-$mac"
expect "CLI is never reused" "$(gzip -dc "$work/dist/nanocodex-x86_64-unknown-linux-gnu.gz")" "new-cli"
expect "Windows Hand is unsupported" "$(cat "$work/dist/$windows.exe")" "new-windows"
grep -q "unsupported for x86_64-pc-windows-msvc" "$work/log"

fresh_dist changed same
run v1
expect "changed Linux key publishes the build" "$(hand "$linux")" "new-$linux"
expect "independent macOS key still reuses" "$(hand "$mac")" "old-$mac"

publish_previous v2
key changed "$linux" > "$work/releases/v2/$linux.identity"
fresh_dist changed same
run v2
expect "previous key not matching SHA256SUMS is ignored" "$(hand "$linux")" "new-$linux"
grep -q "$linux: v2 has no verified reuse key" "$work/log"

publish_previous v3
printf 'tampered' | gzip -n > "$work/releases/v3/$linux.gz"
fresh_dist same same
run v3
expect "previous Hand failing its checksum is not reused" "$(hand "$linux")" "new-$linux"

publish_previous v4
make_app "$work/releases/v4" "bundle-signed-old-$mac" incomplete
resum "$work/releases/v4"
fresh_dist same same
run v4
expect "unsealed previous app swaps neither asset (Hand)" "$(hand "$mac")" "new-$mac"
expect "unsealed previous app swaps neither asset (app)" "$(app_hand)" "bundle-signed-new-$mac"
grep -q "Nanocodex.app is not a complete signed bundle" "$work/log"

publish_previous v5
grep -v "$app" "$work/releases/v5/SHA256SUMS" > "$work/releases/v5/sums"
mv "$work/releases/v5/sums" "$work/releases/v5/SHA256SUMS"
fresh_dist same same
run v5
expect "unlisted previous app is not reused" "$(hand "$mac")" "new-$mac"
expect "unlisted previous app keeps the new app" "$(app_hand)" "bundle-signed-new-$mac"
expect "Linux still reuses beside it" "$(hand "$linux")" "old-$linux"

publish_previous v6
rm "$work/releases/v6/SHA256SUMS"
fresh_dist same same
run v6
expect "previous release without SHA256SUMS keeps the build" "$(hand "$linux")" "new-$linux"

publish_previous v7
rm "$work/releases/v7/"*.identity
resum "$work/releases/v7"
fresh_dist same same
run v7
expect "previous release without reuse keys keeps the build" "$(hand "$linux")" "new-$linux"

fresh_dist same same
printf 'garbage\n' > "$work/dist/$linux.identity"
if run v1; then echo "FAIL malformed key was accepted" >&2; exit 1; fi
echo "ok malformed local reuse key fails the job"

fresh_dist same same
rm "$work/dist/"*.identity
run v1
expect "build without reuse keys keeps the build" "$(hand "$linux")" "new-$linux"

echo "reuse-unchanged-hand: all cases passed"
