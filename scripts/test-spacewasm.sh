#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
spacewasm_root="${SPACEWASM_ROOT:?set SPACEWASM_ROOT to a nasa/spacewasm checkout}"
guest="$repo_root/target/wasm32-wasip1/release/nanocodex-spacewasm.wasm"

cargo test -p nanocodex-spacewasm
RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=--max-memory=${NANOCODEX_SPACEWASM_MAX_MEMORY_BYTES:-67108864}" \
  cargo build --release --target wasm32-wasip1 -p nanocodex-spacewasm
"$repo_root/scripts/wasm2spacewasm.sh" "$guest"
wasm-dis "$guest" -o - | grep -Eq '^ \(memory \$0 [0-9]+ 1024\)$'
cargo build --manifest-path "$spacewasm_root/Cargo.toml" -p spacewasi

input="$(mktemp)"
output="$(mktemp)"
trap 'rm -f "$input" "$output"' EXIT

printf '%s\n' \
  '{"op":"init","expected_revision":0,"instructions":"Be terse","tools":[{"type":"function","name":"read_sensor","description":"Read one sensor","strict":false,"parameters":{"type":"object"}}]}' \
  '{"op":"prompt","expected_revision":1,"text":"Inspect channel 7"}' \
  '{"op":"model_output","expected_revision":2,"items":[{"type":"function_call","name":"read_sensor","arguments":"{\"channel\":7}","call_id":"call-1"}]}' \
  '{"op":"tool_output","expected_revision":3,"call_id":"call-1","output":"nominal"}' \
  '{"op":"model_output","expected_revision":4,"items":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Channel 7 is nominal."}]}]}' \
  '{"op":"snapshot"}' \
  '{"op":"shutdown"}' >"$input"

"$spacewasm_root/target/debug/spacewasi" "$guest" <"$input" >"$output"

grep -q '"kind":"model_request","revision":2' "$output"
grep -q '"kind":"tool_calls","revision":3' "$output"
grep -q '"kind":"complete","revision":5' "$output"
grep -q '"kind":"snapshot"' "$output"
grep -q '"kind":"shutdown","revision":5' "$output"

echo "nanocodex-spacewasm: SpaceWasm JSONL E2E passed"
