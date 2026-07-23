#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

wasm_target=wasm32-unknown-unknown
cargo build --locked -p nanocodex-wasm --target "$wasm_target" --profile wasm
wasm-bindgen "target/$wasm_target/wasm/nanocodex_wasm.wasm" \
  --target nodejs \
  --out-dir js/bindings/pkg-node \
  --out-name nanocodex
wasm-bindgen "target/$wasm_target/wasm/nanocodex_wasm.wasm" \
  --target web \
  --out-dir js/bindings/pkg-web \
  --out-name nanocodex
node js/bindings/scripts/write-package-types.mjs
