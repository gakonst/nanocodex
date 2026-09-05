#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 <pull-request-number>" >&2
    exit 2
fi

pr=$1
if [[ ! "$pr" =~ ^[1-9][0-9]*$ ]]; then
    echo "pull request number must be a positive integer" >&2
    exit 2
fi

repository=${GH_REPO:-gakonst/nanocodex}
workflow=nightly.yml

command -v gh >/dev/null 2>&1 || {
    echo "gh is required to dispatch PR artifacts" >&2
    exit 2
}
command -v jq >/dev/null 2>&1 || {
    echo "jq is required to inspect the pull request" >&2
    exit 2
}

pull_request=$(
    gh pr view "$pr" --repo "$repository" \
        --json headRefName,headRefOid,isCrossRepository,state,url
)
state=$(jq -r '.state' <<<"$pull_request")
if [[ "$state" != "OPEN" ]]; then
    echo "pull request #$pr is $state; refusing to build a stale head" >&2
    exit 1
fi

head_ref=$(jq -r '.headRefName' <<<"$pull_request")
head_sha=$(jq -r '.headRefOid' <<<"$pull_request")
cross_repository=$(jq -r '.isCrossRepository' <<<"$pull_request")
if [[ "$cross_repository" == "true" ]]; then
    workflow_ref=$(
        gh repo view "$repository" --json defaultBranchRef \
            --jq '.defaultBranchRef.name'
    )
else
    workflow_ref=$head_ref
fi

gh workflow run "$workflow" --repo "$repository" --ref "$workflow_ref" \
    --field "pr=$pr"

printf 'Dispatched PR #%s CLI artifacts for %s.\n' "$pr" "$head_sha"
printf 'Track it with: gh run list --repo %s --workflow %s --event workflow_dispatch\n' \
    "$repository" "$workflow"
