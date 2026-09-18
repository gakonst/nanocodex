#!/bin/bash
# Capture only an archived fixture build. Never opens an installed application.
set -euo pipefail
ROOT=$(git rev-parse --show-toplevel)
DOC="$ROOT/docs/ux/media/main-thread-396/macos"
BUILD=$(mktemp -d "$ROOT/.evidence-mac-fixture.XXXXXX")
echo "$BUILD" > "$DOC/build-location.txt"
mkdir -p "$BUILD/source" "$BUILD/data" "$DOC/frames"
git -C "$ROOT" archive f517a439 | tar -x -C "$BUILD/source"
mkdir -p "$BUILD/source/apple/NanocodexVoice/Artifacts"
if [ -n "${NANOCODEX_VOICE_ARTIFACT:-}" ]; then
  ditto "$NANOCODEX_VOICE_ARTIFACT" "$BUILD/source/apple/NanocodexVoice/Artifacts/NanocodexVoiceCore.xcframework"
else
  bash "$BUILD/source/apple/NanocodexVoice/scripts/build-core.sh"
fi
python3 "$DOC/prepare.py" "$BUILD/source" "$DOC/Fixture.swift"
xcodebuild -project "$BUILD/source/macos/Nanocodex.xcodeproj" -scheme Nanocodex -configuration Debug -derivedDataPath "$BUILD/derived" CODE_SIGNING_ALLOWED=NO SWIFT_ACTIVE_COMPILATION_CONDITIONS=DEBUG build > "$DOC/build.log" 2>&1
NANOCODEX_DESKTOP_DATA="$BUILD/data" NANOCODEX_NATIVE_UI_FIXTURE=1 NANOCODEX_EVIDENCE_OUTPUT="$DOC" "$BUILD/derived/Build/Products/Debug/Nanocodex.app/Contents/MacOS/Nanocodex" > "$DOC/capture.log" 2>&1
# Continuous samples of the actual native view, with no pointer or desktop input.
# Normalize dimensions because the native sheet is smaller than the main window.
python3 - "$DOC" <<'PYMEDIA'
import pathlib, subprocess, sys
root=pathlib.Path(sys.argv[1]); out=root/'frames-normalized'; out.mkdir(exist_ok=True)
for p in sorted((root/'frames').glob('*.png')):
 subprocess.run(['ffmpeg','-y','-loglevel','error','-i',str(p),'-vf','scale=1280:860:force_original_aspect_ratio=decrease,pad=1280:860:(ow-iw)/2:(oh-ih)/2:color=white','-frames:v','1',str(out/p.name)],check=True)
PYMEDIA
ffmpeg -hide_banner -loglevel error -y -framerate 10 -i "$DOC/frames-normalized/%05d.png" -c:v libx264 -pix_fmt yuv420p -movflags +faststart "$DOC/native-view-recording.mp4"
