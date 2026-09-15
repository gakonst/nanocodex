#!/usr/bin/env bash
set -euo pipefail

# Run this from a Cloudflare Sandbox Hand. GitHub authentication is supplied by
# Nanocodex's scoped egress connection, never by a token written to /brain.
repo=${NANOCODEX_IOS_REPOSITORY:-gakonst/nanocodex}
workflow=${NANOCODEX_IOS_WORKFLOW:-ios-self-update.yml}
ref=${NANOCODEX_IOS_REF:-master}
version=${NANOCODEX_IOS_VERSION:-0.1.0}
build_version=${NANOCODEX_IOS_BUILD_VERSION:-$(date -u +%s)}
output=${NANOCODEX_IOS_OUTPUT:-/brain/ios-deployments}

command -v gh >/dev/null || { echo "gh is required" >&2; exit 127; }
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || { echo "invalid repository" >&2; exit 2; }
[[ "$ref" =~ ^[A-Za-z0-9._/-]+$ && "$ref" != *..* ]] || { echo "invalid ref" >&2; exit 2; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]] || { echo "invalid version" >&2; exit 2; }
[[ "$build_version" =~ ^[1-9][0-9]*$ ]] || { echo "invalid build version" >&2; exit 2; }

request_id=$(python3 -c 'import secrets; print(secrets.token_hex(12))')
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gh workflow run "$workflow" --repo "$repo" --ref "$ref" \
  -f request_id="$request_id" -f version="$version" -f build_version="$build_version"

run_id=""
for _ in $(seq 1 60); do
  run_id=$(gh run list --repo "$repo" --workflow "$workflow" --event workflow_dispatch \
    --created ">=$started" --json databaseId,displayTitle \
    --jq ".[] | select(.displayTitle == \"Nanocodex iPhone update $request_id\") | .databaseId" | head -n 1)
  [[ -n "$run_id" ]] && break
  sleep 2
done
[[ -n "$run_id" ]] || { echo "workflow run was not found" >&2; exit 1; }
gh run watch "$run_id" --repo "$repo" --exit-status
run_url=$(gh run view "$run_id" --repo "$repo" --json url --jq .url)

mkdir -p "$output"
python3 - "$request_id" "$version" "$build_version" "$run_id" "$run_url" > "$output/latest.json" <<'PY'
import json, sys
request, version, build, run, url = sys.argv[1:]
print(json.dumps({
    "build_version": build,
    "bundle_id": "xyz.paradigm.centaur",
    "channel": "testflight-internal",
    "provider": "apple",
    "request_id": request,
    "run_id": int(run),
    "run_url": url,
    "status": "uploaded",
    "version": version,
}, sort_keys=True, indent=2))
PY
cat "$output/latest.json"
