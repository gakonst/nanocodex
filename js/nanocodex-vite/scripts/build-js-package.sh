#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
script_path="$repository_root/js/nanocodex-vite/scripts/build-js-package.sh"
cd "$repository_root"

wasm_target=wasm32-unknown-unknown
target_dir="${CARGO_TARGET_DIR:-$repository_root/target}"
if [[ "$target_dir" != /* ]]; then
  target_dir="$repository_root/$target_dir"
fi

build_mode=development
cargo_profile=debug
if [[ "$#" -gt 0 ]]; then
  if [[ "$#" -ne 1 || "$1" != "--release" ]]; then
    echo "usage: $0 [--release]" >&2
    exit 1
  fi
  build_mode=release
  cargo_profile=wasm
fi

mkdir -p "$target_dir"
lock_file="$repository_root/.nanocodex-js-package.lock"
lock_timeout_seconds="${NANOCODEX_WASM_LOCK_TIMEOUT_SECONDS:-600}"
generated_dir=""

if [[ ! "$lock_timeout_seconds" =~ ^[0-9]+$ ]]; then
  echo "NANOCODEX_WASM_LOCK_TIMEOUT_SECONDS must be a non-negative integer" >&2
  exit 1
fi

if [[ "${NANOCODEX_WASM_LOCK_HELD:-}" != "$repository_root" ]]; then
  if command -v flock >/dev/null 2>&1; then
    if flock -E 75 -w "$lock_timeout_seconds" "$lock_file" \
      env NANOCODEX_WASM_LOCK_HELD="$repository_root" "$script_path" "$@"; then
      exit 0
    else
      lock_status=$?
    fi
  elif command -v lockf >/dev/null 2>&1; then
    if lockf -k -t "$lock_timeout_seconds" "$lock_file" \
      env NANOCODEX_WASM_LOCK_HELD="$repository_root" "$script_path" "$@"; then
      exit 0
    else
      lock_status=$?
    fi
  else
    echo "the Nanocodex WASM build requires flock or lockf for checkout-local serialization" >&2
    exit 1
  fi
  if [[ "$lock_status" -eq 75 ]]; then
    echo "timed out waiting for the checkout-local Nanocodex WASM build lock" >&2
  fi
  exit "$lock_status"
fi

cleanup() {
  if [[ -n "$generated_dir" && -d "$generated_dir" ]]; then
    rm -rf "$generated_dir"
  fi
}

trap cleanup EXIT

if [[ "$build_mode" == release ]]; then
  cargo build --locked -p nanocodex-wasm --target "$wasm_target" --profile wasm
else
  cargo build --locked -p nanocodex-wasm --target "$wasm_target"
fi
wasm_artifact="$target_dir/$wasm_target/$cargo_profile/nanocodex_wasm.wasm"
stamp_path="js/nanocodex/pkg-web/.nanocodex-bindgen-stamp"
binaryen=""
if [[ "$build_mode" == release ]]; then
  binaryen="$repository_root/js/nanocodex/node_modules/.bin/wasm-opt"
  if [[ ! -x "$binaryen" ]]; then
    echo "missing Binaryen dependency for the Nanocodex release WASM build" >&2
    exit 1
  fi
fi
fingerprint="$({
  wasm-bindgen --version
  printf 'build-mode=%s\n' "$build_mode"
  printf 'worker-bundler-v1-simd\n'
  if [[ "$build_mode" == release ]]; then
    "$binaryen" --version
  fi
  cksum < "$wasm_artifact"
})"
if [[ -f "$stamp_path" ]] \
  && [[ -f js/nanocodex/pkg-web/nanocodex_bg.wasm ]] \
  && [[ -f js/nanocodex/pkg-web/nanocodex_bg.js ]] \
  && [[ -f js/nanocodex/pkg-web/nanocodex_worker.js ]] \
  && [[ -f js/nanocodex/pkg-node/nanocodex.js ]] \
  && [[ -f js/nanocodex/pkg-node/nanocodex.d.ts ]] \
  && [[ -f js/nanocodex/pkg-node/package.json ]] \
  && [[ "$(<"$stamp_path")" == "$fingerprint" ]] \
  && node js/nanocodex/scripts/write-wasm-attestation.mjs --check-cache "$wasm_artifact" 2>/dev/null; then
  node js/nanocodex/scripts/write-wasm-attestation.mjs "$wasm_artifact"
  echo "wasm-bindgen outputs are current"
  exit 0
fi

generated_dir="$(mktemp -d)"
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
if [[ "$build_mode" == release ]]; then
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
fi
node js/nanocodex/scripts/deduplicate-wasm.mjs
node js/nanocodex/scripts/write-package-types.mjs
printf '%s\n' "$fingerprint" > "$stamp_path"
node js/nanocodex/scripts/write-wasm-attestation.mjs "$wasm_artifact"
