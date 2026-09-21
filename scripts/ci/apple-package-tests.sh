#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
log_dir="$PWD/apple/build/evidence/package-tests"
mkdir -p "$log_dir"
python3 apple/NanocodexInboxUITests/verify_render_projection.py
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
# Keep native protocol and shared rendering coverage in the same two bounded lanes.
lane InboxCore NanocodexContext NanocodexUI &
first=$!
lane NanocodexVoice NanocodexHand &
second=$!
trap 'kill "$first" "$second" 2>/dev/null || true' EXIT
status=0
wait "$first" || status=1
wait "$second" || status=1
trap - EXIT
for package in InboxCore NanocodexVoice NanocodexContext NanocodexHand NanocodexUI; do
  echo "::group::$package"
  cat "$log_dir/$package.log"
  echo '::endgroup::'
done
exit "$status"
