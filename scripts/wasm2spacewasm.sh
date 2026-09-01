#!/usr/bin/env bash
set -euo pipefail

if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "wasm-opt is required (install Binaryen)" >&2
  exit 1
fi

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 input.wasm [output.wasm]" >&2
  exit 1
fi

input="$1"
output="${2:-$1}"
temporary=""
if [[ "$input" == "$output" ]]; then
  temporary="${input}.mvp.tmp"
  output="$temporary"
fi

trap '[[ -z "$temporary" ]] || rm -f "$temporary"' EXIT

# Binaryen must first accept the proposals emitted by current Rust before it
# can lower them. SpaceWasm's upstream helper omits --enable-bulk-memory, which
# makes recent wasm-opt versions reject Rust's memory.copy/memory.fill input
# before llvm-memory-copy-fill-lowering can run.
wasm-opt \
  --enable-bulk-memory \
  --llvm-memory-copy-fill-lowering \
  --signext-lowering \
  --llvm-nontrapping-fptoint-lowering \
  --disable-multivalue \
  --disable-simd \
  --disable-bulk-memory \
  "$input" \
  -o "$output"

if [[ -n "$temporary" ]]; then
  mv "$temporary" "$input"
  temporary=""
fi
