#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
log_dir="$PWD/apple/build/evidence/package-tests"
mkdir -p "$log_dir"
# Each package owns its .build tree. Two lanes avoid adding runner queue waits
# and cap compiler concurrency rather than oversubscribing the host four ways.
cpus=$(getconf _NPROCESSORS_ONLN)
jobs=$((cpus / 2))
if (( jobs < 1 )); then jobs=1; fi
lane() {
  local failed=0 package result
  for package in "$@"; do
    echo "Starting $package (compiler jobs=$jobs)"
    if swift test --package-path "apple/$package" --jobs "$jobs" > "$log_dir/$package.log" 2>&1; then
      result=0
    else
      result=$?
      failed=1
    fi
    echo "Finished $package (exit=$result); transcript: $log_dir/$package.log"
  done
  return "$failed"
}
# Balance the measured 52+35 and 59+31 second serial package steps.
lane InboxCore NanocodexContext &
first=$!
lane NanocodexVoice NanocodexHand &
second=$!
trap 'kill "$first" "$second" 2>/dev/null || true' EXIT
status=0
wait "$first" || status=1
wait "$second" || status=1
trap - EXIT
for package in InboxCore NanocodexVoice NanocodexContext NanocodexHand; do
  echo "::group::$package"
  cat "$log_dir/$package.log"
  echo '::endgroup::'
done
exit "$status"
