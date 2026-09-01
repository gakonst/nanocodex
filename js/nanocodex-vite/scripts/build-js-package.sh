#!/usr/bin/env bash
set -euo pipefail

script_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"
# Vite consumers start this builder in separate processes. Serialize the whole
# generation so the cache check and in-place Node glue rewrite are atomic.
if [[ "${NANOCODEX_WASM_BUILD_LOCK_HELD:-}" != "1" ]]; then
  exec node "$repository_root/js/nanocodex-vite/scripts/build-lock.mjs" \
    "$repository_root/js/nanocodex/pkg-web/.nanocodex-bindgen.lock" \
    "$script_path" "$@"
fi


wasm_target=wasm32-unknown-unknown
target_dir="${CARGO_TARGET_DIR:-$repository_root/target}"
if [[ "$target_dir" != /* ]]; then
  target_dir="$repository_root/$target_dir"
fi

cargo build --locked -p nanocodex-wasm --target "$wasm_target" --profile wasm
wasm_artifact="$target_dir/$wasm_target/wasm/nanocodex_wasm.wasm"
binaryen="$repository_root/js/nanocodex/node_modules/.bin/wasm-opt"
if [[ ! -x "$binaryen" ]]; then
  echo "missing Binaryen dependency for the nanocodex WASM build" >&2
  exit 1
fi
stamp_path="js/nanocodex/pkg-web/.nanocodex-bindgen-stamp"
fingerprint="$(wasm-bindgen --version; "$binaryen" --version; printf 'worker-bundler-v1-simd\n'; cksum < "$wasm_artifact")"
if [[ -f "$stamp_path" ]] \
  && [[ -f js/nanocodex/pkg-web/nanocodex_bg.wasm ]] \
  && [[ -f js/nanocodex/pkg-web/nanocodex_bg.js ]] \
  && [[ -f js/nanocodex/pkg-web/nanocodex_worker.js ]] \
  && [[ -f js/nanocodex/pkg-node/nanocodex.js ]] \
  && [[ "$(<"$stamp_path")" == "$fingerprint" ]] \
  && node js/nanocodex/scripts/write-wasm-attestation.mjs --check-cache "$wasm_artifact" 2>/dev/null; then
  node js/nanocodex/scripts/write-wasm-attestation.mjs "$wasm_artifact"
  echo "wasm-bindgen outputs are current"
  exit 0
fi

generated_dir="$(mktemp -d)"
trap 'rm -rf "$generated_dir"' EXIT
worker_bindings="$generated_dir/worker"
mkdir "$worker_bindings"
wasm-bindgen "$wasm_artifact" \
  --target nodejs \
  --out-dir js/nanocodex/pkg-node \
  --out-name nanocodex
wasm-bindgen "$wasm_artifact" \
  --target web \
  --out-dir js/nanocodex/pkg-web \
  --out-name nanocodex
wasm-bindgen "$wasm_artifact" \
  --target bundler \
  --out-dir "$worker_bindings" \
  --out-name nanocodex
cmp "$worker_bindings/nanocodex_bg.wasm" js/nanocodex/pkg-web/nanocodex_bg.wasm
cp "$worker_bindings/nanocodex_bg.js" js/nanocodex/pkg-web/nanocodex_bg.js
cp "$worker_bindings/nanocodex.js" js/nanocodex/pkg-web/nanocodex_worker.js
generated_wasm="js/nanocodex/pkg-web/nanocodex_bg.wasm"
optimized_wasm="$generated_dir/nanocodex.wasm"
"$binaryen" -Oz \
  --enable-bulk-memory \
  --enable-bulk-memory-opt \
  --enable-nontrapping-float-to-int \
  --enable-simd \
  --strip-debug \
  --strip-producers \
  --strip-toolchain-annotations \
  "$generated_wasm" \
  -o "$optimized_wasm"
mv "$optimized_wasm" "$generated_wasm"
node js/nanocodex/scripts/deduplicate-wasm.mjs
node js/nanocodex/scripts/write-package-types.mjs
printf '%s\n' "$fingerprint" > "$stamp_path"
node js/nanocodex/scripts/write-wasm-attestation.mjs "$wasm_artifact"
