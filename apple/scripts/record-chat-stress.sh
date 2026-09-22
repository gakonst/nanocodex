#!/bin/bash
# Requires Xcode, an available iPhone simulator, and the built voice XCFramework.
# XCTest records each test, including successful runs; no live account is used.
set -euo pipefail
cd "$(dirname "$0")/../.."
if [[ $# -lt 3 ]]; then
  echo "Usage: $0 SIMULATOR_UDID DERIVED_DATA OUTPUT_DIRECTORY [ITERATIONS=3]" >&2
  exit 2
fi
device="$1"
derived="$2"
output="$3"
iterations="${4:-3}"
mkdir -p "$output"
xcodebuild build-for-testing -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox \
  -destination "platform=iOS Simulator,id=$device" -derivedDataPath "$derived" > "$output/build.log" 2>&1
xctestrun=$(python3 - "$derived" <<'PY'
import pathlib, plistlib, sys
paths = list((pathlib.Path(sys.argv[1]) / 'Build/Products').glob('NanocodexInbox_*.xctestrun'))
if len(paths) != 1:
    raise SystemExit(f'Expected one generated xctestrun, found {len(paths)}')
path = paths[0]
with path.open('rb') as source:
    run = plistlib.load(source)
targets = ([target for config in run['TestConfigurations'] for target in config['TestTargets']]
           if 'TestConfigurations' in run else [target for key,target in run.items() if not key.startswith('__')])
for target in targets:
    if target.get('IsUITestBundle'):
        target.update(PreferredScreenCaptureFormat='screenRecording',
                      SystemAttachmentLifetime='keepAlways', UserAttachmentLifetime='keepAlways')
with path.open('wb') as destination:
    plistlib.dump(run, destination)
print(path)
PY
)
set +e
xcodebuild test-without-building -xctestrun "$xctestrun" \
  -destination "platform=iOS Simulator,id=$device" -parallel-testing-enabled NO \
  -test-iterations "$iterations" -test-repetition-relaunch-enabled YES \
  -test-timeouts-enabled YES -default-test-execution-time-allowance 180 -maximum-test-execution-time-allowance 240 \
  -only-testing:NanocodexInboxUITests/InboxUITests/testStress200ToolArrivalsKeepCompletedTailVisible \
  -only-testing:NanocodexInboxUITests/InboxUITests/testNativeTranscriptBoundsMountedCellsFor2000Rows \
  -only-testing:NanocodexInboxUITests/InboxUITests/testNativeTranscriptPreservesExpandedToolAfterCellRecycling \
  -resultBundlePath "$output/stress.xcresult" > "$output/tests.log" 2>&1
result=$?
set -e
if [[ -f "$output/stress.xcresult/Info.plist" ]]; then
  xcrun xcresulttool export attachments --path "$output/stress.xcresult" --output-path "$output/attachments"
fi
exit "$result"
