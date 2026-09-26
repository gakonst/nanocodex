#!/bin/sh
# Queue Xcode work across Nanocodex agents sharing this Mac. The OS lock is
# held until xcodebuild exits, not merely until a Hand command yields.
set -eu

if [ "$#" -eq 0 ]; then
  echo "usage: scripts/xcodebuild-guard.sh [xcodebuild arguments...]" >&2
  exit 2
fi
if [ "$(uname -s)" != Darwin ]; then
  echo "xcodebuild-guard requires macOS" >&2
  exit 2
fi

jobs=0
tests=0
parallel_tests=0
sim_destinations=0
for arg in "$@"; do
  case "$arg" in
    -jobs) jobs=1 ;;
    test|test-without-building) tests=1 ;;
    -parallel-testing-enabled) parallel_tests=1 ;;
    -maximum-concurrent-test-simulator-destinations) sim_destinations=1 ;;
  esac
done

# Leave explicit caller limits alone; the defaults reserve CPU for the desktop.
if [ "$jobs" -eq 0 ]; then set -- -jobs 3 "$@"; fi
if [ "$tests" -eq 1 ]; then
  if [ "$parallel_tests" -eq 0 ]; then set -- "$@" -parallel-testing-enabled NO; fi
  if [ "$sim_destinations" -eq 0 ]; then
    set -- "$@" -maximum-concurrent-test-simulator-destinations 1
  fi
fi

lock_dir="${HOME}/Library/Caches/nanocodex"
mkdir -p "$lock_dir"
exec /usr/bin/lockf -t "${NANOCODEX_XCODE_LOCK_WAIT_SECONDS:-3600}" \
  "$lock_dir/xcodebuild.lock" "${NANOCODEX_XCODEBUILD_BIN:-$(command -v xcodebuild)}" "$@"
