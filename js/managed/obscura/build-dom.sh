#!/bin/sh
set -eu
cd "$(dirname "$0")"
test "$(wasm-bindgen --version)" = 'wasm-bindgen 0.2.126'
revision=5ba6c05ed8dd848862a8a5a3d90e26fcae41ccd9
mkdir -p upstream
curl --fail --location "https://codeload.github.com/h4ckf0r0day/obscura/tar.gz/$revision" -o upstream.tar.gz
tar -xzf upstream.tar.gz --strip-components=1 -C upstream
cargo build --locked --release --target wasm32-unknown-unknown --manifest-path dom-wasm/Cargo.toml
wasm-bindgen dom-wasm/target/wasm32-unknown-unknown/release/obscura_dom_wasm.wasm --target web --out-dir dom-wasm/pkg
cp dom-wasm/pkg/obscura_dom_wasm.js runtime/obscura_dom_wasm.js
cp dom-wasm/pkg/obscura_dom_wasm_bg.wasm ../src/obscura-assets/dom.bin
cp upstream/crates/obscura-js/js/bootstrap.js ../src/obscura-assets/bootstrap.txt
