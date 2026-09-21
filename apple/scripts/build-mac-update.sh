#!/bin/bash
# Build with the Mac's existing Xcode signing identities and registered devices.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
if [[ $# -lt 2 ]]; then
  echo 'Usage: build-mac-update.sh BUILD ASSETS_DIR [publish-mac-update.py options, e.g. --deploy --notes TEXT]' >&2
  exit 2
fi
BUILD="$1"
ASSETS="$2"
shift 2
[[ "$BUILD" =~ ^[1-9][0-9]*$ ]] || { echo 'BUILD must be a positive integer' >&2; exit 2; }
[[ "$(uname -s)" == Darwin ]] || { echo 'Run on a Mac with Xcode and configured signing.' >&2; exit 2; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nanocodex-ota.XXXXXX")"
echo "Build artifacts: $WORK"
trap 'echo "Build artifacts retained: $WORK"' EXIT
bash "$ROOT/apple/NanocodexVoice/scripts/build-core.sh"
cat > "$WORK/ExportOptions.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>method</key><string>debugging</string><key>signingStyle</key><string>automatic</string><key>destination</key><string>export</string></dict></plist>
PLIST
xcodebuild -project "$ROOT/apple/NanocodexInbox.xcodeproj" -scheme NanocodexInbox \
  -configuration Release -destination 'generic/platform=iOS' \
  -derivedDataPath "$WORK/DerivedData" -archivePath "$WORK/Nanocodex.xcarchive" \
  -allowProvisioningUpdates CURRENT_PROJECT_VERSION="$BUILD" archive
xcodebuild -exportArchive -archivePath "$WORK/Nanocodex.xcarchive" \
  -exportPath "$WORK/export" -exportOptionsPlist "$WORK/ExportOptions.plist" -allowProvisioningUpdates
shopt -s nullglob
IPAS=("$WORK/export/"*.ipa)
[[ ${#IPAS[@]} -eq 1 ]] || { echo 'Expected exactly one exported IPA' >&2; exit 1; }
python3 "$ROOT/apple/scripts/publish-mac-update.py" --ipa "${IPAS[0]}" --assets-dir "$ASSETS" "$@"
