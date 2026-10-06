#!/usr/bin/env bash
# Real native/portable Claude journeys. Only remote providers are fixtures.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.97.0}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
for command in python3 git; do
  command -v "$command" >/dev/null || { echo "$command is required" >&2; exit 1; }
done
for helper in "${NANOCODEX_TEST_PDFINFO:-pdfinfo}" "${NANOCODEX_TEST_PDFTOPPM:-pdftoppm}"; do
  command -v "$helper" >/dev/null || { echo "Real Poppler helper required: $helper" >&2; exit 1; }
done
cargo test --locked -p nanocodex-claude --all-features
cargo test --locked -p nanocodex-claude-tools --all-features -- --include-ignored
cargo test --locked -p nanocodex-durability --features claude,sqlite --test 'claude*' --test checkpoint_branch
cargo test --locked -p nanocodex-oai-tools --test it native_mcp -- --nocapture
cargo test --locked -p nanocodex-bin --test 'claude_*' --test harness_routing -- --nocapture
cargo check --locked -p nanocodex-claude -p nanocodex-claude-tools --all-features --target wasm32-unknown-unknown
cargo check --locked -p nanocodex-wasm --target wasm32-unknown-unknown
