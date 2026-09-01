# Nanocodex on SpaceWasm

SpaceWasm is a synchronous Wasm 1.0 interpreter, not a browser or an async
application server. This adapter therefore runs the deterministic Nanocodex
transcript and tool loop inside the guest and leaves capabilities with the
embedding flight host:

- the guest owns typed Responses history, turn phase, tool-call correlation,
  revision fencing, and checkpoints;
- the host owns the OpenAI connection, credentials, timers, durable storage,
  and every tool effect;
- the guest never receives a provider credential or opens a socket;
- every mutating command carries `expected_revision`, so a replay after an
  ambiguous host failure is rejected instead of duplicating an effect.

The `nanocodex-spacewasm` binary exposes this boundary as one JSON command and
one JSON action per line over WASI Preview 1 stdin/stdout. It is deliberately a
thin test/embed adapter; a flight integration can bind the same `FlightCore`
through fixed native SpaceWasm imports.

```text
host -> {"op":"init","expected_revision":0,"instructions":"Be terse","tools":[...]}
guest <- {"kind":"ready","revision":1}
host -> {"op":"prompt","expected_revision":1,"text":"Check channel 7"}
guest <- {"kind":"model_request","revision":2,"input":[...]}
host -> {"op":"model_output","expected_revision":2,"items":[...]}
guest <- {"kind":"tool_calls","revision":3,"calls":[...]}
```

Build the guest for `wasm32-wasip1`, lower post-MVP instructions with
`scripts/wasm2spacewasm.sh`, then run it with `spacewasi`. The wrapper enables
the bulk-memory input emitted by current Rust before lowering it; SpaceWasm's
upstream helper otherwise rejects that input on current Binaryen. The
repository-level `scripts/test-spacewasm.sh` performs the complete mocked-host
proof and links the guest with a 64 MiB maximum linear memory. Override that
bound with `NANOCODEX_SPACEWASM_MAX_MEMORY_BYTES` when the embedding has a
different fixed allocation.
