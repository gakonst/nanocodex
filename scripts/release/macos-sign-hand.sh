#!/usr/bin/env bash
# Sign the macOS Hand for a release.
#
#   scripts/release/macos-sign-hand.sh HAND_BINARY VERSION OUTPUT_DIR
#
# Produces, in OUTPUT_DIR:
#   nanocodex2                                the standalone Hand companion that
#                                             updaters install as versions/<key>/nanocodex2
#   nanocodex-app-aarch64-apple-darwin.tar.gz Nanocodex.app carrying the same
#                                             program as Contents/MacOS/nanocodex2
#   SIGNING                                   "developer-id <TEAM>" or "ad-hoc"
#
# Both are signed with identifier com.nanocodex.hand and the hypervisor
# entitlement its libkrun VMM children need. With MACOS_DEVELOPER_ID_P12_BASE64,
# MACOS_DEVELOPER_ID_P12_PASSWORD and MACOS_DEVELOPER_ID_TEAM_ID the signature is
# a Developer ID Application certificate, so macOS privacy grants follow the
# identifier and team across Hand builds. Without them the Hand is signed ad
# hoc: it runs, but every new build is a new identity to macOS privacy controls.
# No hardened runtime or notarization yet.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 HAND_BINARY VERSION OUTPUT_DIR" >&2
  exit 2
fi
hand=$1
version=$2
output=$3
identifier=com.nanocodex.hand
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
entitlements="$root/nanocodex-vm.entitlements"

test -x "$hand" || { echo "Hand binary is not executable: $hand" >&2; exit 1; }
# CFBundleVersion and CFBundleShortVersionString accept only numeric versions.
bundle_version=${version%%-*}
bundle_version=${bundle_version%%+*}
[[ "$bundle_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "expected a MAJOR.MINOR.PATCH version, got $version" >&2
  exit 1
}

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/nanocodex-hand-sign.XXXXXX")"
keychain=""
existing=()
search_list_changed=false
cleanup() {
  if [[ "$search_list_changed" == true ]]; then
    security list-keychains -d user -s "${existing[@]}" >/dev/null 2>&1 || true
  fi
  if [[ -n "$keychain" ]]; then
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$work"
}
trap cleanup EXIT

team=""
signing=(--sign -)
if [[ -n "${MACOS_DEVELOPER_ID_P12_BASE64:-}" ]]; then
  : "${MACOS_DEVELOPER_ID_P12_PASSWORD:?MACOS_DEVELOPER_ID_P12_PASSWORD is required with a certificate}"
  : "${MACOS_DEVELOPER_ID_TEAM_ID:?MACOS_DEVELOPER_ID_TEAM_ID is required with a certificate}"
  team=$MACOS_DEVELOPER_ID_TEAM_ID
  [[ "$team" =~ ^[A-Z0-9]{10}$ ]] || { echo "MACOS_DEVELOPER_ID_TEAM_ID is not a team identifier" >&2; exit 1; }
  # codesign resolves the certificate chain through the user search list.
  while IFS= read -r entry; do
    entry=${entry#"${entry%%[![:space:]]*}"}
    entry=${entry#\"}
    entry=${entry%\"}
    [[ -n "$entry" ]] && existing+=("$entry")
  done < <(security list-keychains -d user)
  search_list_changed=true
  keychain="$work/signing.keychain-db"
  keychain_password="$(openssl rand -hex 24)"
  printf '%s' "$MACOS_DEVELOPER_ID_P12_BASE64" | base64 --decode > "$work/identity.p12"
  security create-keychain -p "$keychain_password" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$keychain_password" "$keychain"
  security import "$work/identity.p12" -k "$keychain" -f pkcs12 \
    -P "$MACOS_DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign >/dev/null
  rm -f "$work/identity.p12"
  security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null
  security list-keychains -d user -s "$keychain" "${existing[@]}"
  identity="$(security find-identity -v -p codesigning "$keychain" \
    | awk -v team="($team)" 'index($0, "Developer ID Application") && index($0, team) { print $2; exit }')"
  [[ -n "$identity" ]] || {
    echo "the certificate has no valid Developer ID Application identity for team $team" >&2
    security find-identity -v -p codesigning "$keychain" >&2 || true
    exit 1
  }
  signing=(--sign "$identity" --keychain "$keychain" --timestamp)
  echo "Signing the Hand with Developer ID Application ($team)"
else
  echo "::warning title=Unsigned macOS Hand::Developer ID secrets are not configured; the Hand is signed ad hoc and macOS privacy grants will not persist across builds"
fi

sign() {
  codesign --force "${signing[@]}" --identifier "$identifier" \
    --entitlements "$entitlements" "$1"
}

verify() {
  local target=$1 requirement
  codesign --verify --strict --verbose=2 "$target"
  requirement="$(codesign --display --requirements - "$target" 2>&1)"
  printf '%s\n' "$requirement"
  if [[ -n "$team" ]]; then
    grep -Fq "identifier \"$identifier\"" <<<"$requirement"
    grep -Eq "certificate leaf\[subject\.OU\] = \"?$team\"?" <<<"$requirement"
  fi
  codesign --display --entitlements - --xml "$target" 2>/dev/null \
    | grep -Fq 'com.apple.security.hypervisor'
}

mkdir -p "$output"
cp "$hand" "$work/nanocodex2"
chmod 755 "$work/nanocodex2"
sign "$work/nanocodex2"
verify "$work/nanocodex2"

app="$work/Nanocodex.app"
mkdir -p "$app/Contents/MacOS"
cp "$hand" "$app/Contents/MacOS/nanocodex2"
chmod 755 "$app/Contents/MacOS/nanocodex2"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$identifier</string>
  <key>CFBundleName</key><string>Nanocodex</string>
  <key>CFBundleDisplayName</key><string>Nanocodex</string>
  <key>CFBundleExecutable</key><string>nanocodex2</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>$bundle_version</string>
  <key>CFBundleVersion</key><string>$bundle_version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
sign "$app"
verify "$app"
# Both forms must answer the Hand service probe that installers and updaters use.
"$app/Contents/MacOS/nanocodex2" __device-hand --service-protocol >/dev/null
"$work/nanocodex2" __device-hand --service-protocol >/dev/null

cp "$work/nanocodex2" "$output/nanocodex2"
# Archive without extended attributes or AppleDouble files, which would break
# the sealed bundle after extraction.
COPYFILE_DISABLE=1 tar --no-xattrs -czf "$output/nanocodex-app-aarch64-apple-darwin.tar.gz" \
  -C "$work" Nanocodex.app
extracted="$work/extracted"
mkdir -p "$extracted"
tar -xzf "$output/nanocodex-app-aarch64-apple-darwin.tar.gz" -C "$extracted"
verify "$extracted/Nanocodex.app" >/dev/null
if [[ -n "$team" ]]; then
  printf 'developer-id %s\n' "$team" > "$output/SIGNING"
else
  printf 'ad-hoc\n' > "$output/SIGNING"
fi
echo "macOS Hand signing: $(cat "$output/SIGNING")"
