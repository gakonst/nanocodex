#!/usr/bin/env bash
# Deterministic memory contract/behavior evaluation. Requires installed workspace dependencies.
set -euo pipefail
cd "$(dirname "$0")/../.."
if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /path/to/codex-at-pinned-revision" >&2
  exit 2
fi
python3 scripts/codex-parity/memory.py "$1"
node --test js/nanocodex-tools/test/extensions.test.mjs
cargo test --locked -p nanocodex-bin --test managed_memory
cargo test --locked -p nanocodex-voice-protocol
cargo test --locked -p nanocodex2-bin --bin nanocodex2 voice::tests
pnpm --filter nanocodex-vite run build:wasm
node --test js/nanocodex/test/managed-voice.test.mjs js/nanocodex/test/browser-voice.test.mjs
pnpm --filter nanocodex-managed-service run test:memory
