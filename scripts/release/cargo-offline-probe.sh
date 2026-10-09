#!/usr/bin/env bash
# Usage: cargo-offline-probe.sh TARGET
#
# Records NANOCODEX_CARGO_OFFLINE=true in GITHUB_ENV when the restored Cargo
# home already holds every locked package for TARGET. Windows release builds
# otherwise spent up to ~86s revalidating the sparse crates.io index before the
# first compile. Offline resolution still uses --locked and the Cargo.lock
# checksums, so it selects exactly the same sources; when anything is missing
# (cold cache or a lockfile change) the build stays online.
set -euo pipefail
target=${1:?usage: cargo-offline-probe.sh TARGET}
started=$SECONDS
if probe_log=$(cargo fetch --locked --offline --target "$target" 2>&1); then
  echo "NANOCODEX_CARGO_OFFLINE=true" >> "${GITHUB_ENV:-/dev/null}"
  echo "Restored registry is complete for $target; building offline ($((SECONDS - started))s probe)"
else
  echo "Restored registry is incomplete for $target; building online ($((SECONDS - started))s probe)"
  printf '%s\n' "$probe_log" | tail -n 5
fi
