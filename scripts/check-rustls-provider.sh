#!/usr/bin/env bash
set -euo pipefail

shipped_tree=$(
  cargo tree --locked --package nanocodex-bin --features tempo \
    --edges normal,build --prefix none --color never
)
shipped_feature_tree=$(
  cargo tree --locked --package nanocodex-bin --features tempo \
    --edges features --prefix none --color never
)
workspace_tree=$(
  cargo tree --locked --workspace --all-features \
    --edges normal,build,dev --prefix none --color never
)
workspace_feature_tree=$(
  cargo tree --locked --workspace --all-features \
    --edges features --prefix none --color never
)

check_tree() {
  local label=$1
  local dependency_tree=$2
  local feature_tree=$3

  if grep -q '^aws-lc-rs v' <<<"$dependency_tree"; then
    echo "error: the ${label} graph must not contain aws-lc-rs" >&2
    exit 1
  fi

  local rustls_versions
  rustls_versions=$(
    awk '$1 == "rustls" && $2 ~ /^v/ { print $2 }' <<<"$dependency_tree" | sort -u
  )
  local rustls_version_count
  rustls_version_count=$(sed '/^$/d' <<<"$rustls_versions" | wc -l | tr -d ' ')
  if [[ "$rustls_version_count" != "1" ]]; then
    echo "error: expected exactly one rustls version in ${label}, found ${rustls_version_count}" >&2
    printf '%s\n' "$rustls_versions" >&2
    exit 1
  fi

  if ! grep -q '^ring v' <<<"$dependency_tree"; then
    echo "error: the ${label} graph must contain ring" >&2
    exit 1
  fi

  local reqwest_sources
  reqwest_sources=$(
    awk '$1 == "reqwest" && $2 ~ /^v/ {
      if (NF < 3 || $3 == "(*)") {
        print $2
      } else {
        print $2, $3
      }
    }' <<<"$dependency_tree" | sort -u
  )
  local reqwest_source_count
  reqwest_source_count=$(sed '/^$/d' <<<"$reqwest_sources" | wc -l | tr -d ' ')
  if [[ "$reqwest_source_count" != "1" ]]; then
    echo "error: expected exactly one reqwest source in ${label}, found ${reqwest_source_count}" >&2
    printf '%s\n' "$reqwest_sources" >&2
    exit 1
  fi

  if ! grep -q '^reqwest feature "rustls-no-provider"' <<<"$feature_tree"; then
    echo "error: the ${label} graph must enable reqwest's rustls-no-provider feature" >&2
    exit 1
  fi

  echo "rustls provider policy passed for ${label}: ring only (${rustls_versions}); one provider-neutral reqwest (${reqwest_sources})"
}

check_tree "shipped nanocodex" "$shipped_tree" "$shipped_feature_tree"
check_tree "complete workspace" "$workspace_tree" "$workspace_feature_tree"
