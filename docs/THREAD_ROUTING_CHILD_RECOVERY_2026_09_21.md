# Structured child results and managed idle recovery

Follow-up to the failures recorded in `THREAD_ROUTING_CLI_VERIFICATION_2026_09_21.md` for PR #436. This report preserves those earlier failed attempts as historical evidence.

## Result handling

The child output contract remains authoritative. A directly valid result is stored unchanged. If a result is a string that does not satisfy the contract, the runtime may decode one JSON object or array, bounded to 1 MiB, and accept it only if the decoded value satisfies the exact same schema. The tool receipt reports `decoded_json_text: true`. Legitimate string results remain strings; scalar coercion, recursive decoding, invalid fields and extra properties are rejected.

## Graceful idle recovery

Before graceful Cloudflare runtime shutdown, child actors stop active work and capture safe conversation boundaries. A versioned checkpoint retains child identity/topology, model/thinking, output contract, turn token, last result, host-context reference and the next child ID. The Cloudflare owner writes bounded checkpoint chunks atomically while holding its current session generation. A replacement runtime rebuilds child drivers and validates their identity and host context against retained bindings. It retains the existing provider route without calling the chooser again.

Explicit close still releases the child's descriptor and route. Closed children are excluded from the next checkpoint, and the ID high-water mark prevents reuse. Failed reconstruction preserves stored state and releases only the failed host's in-memory registrations; rollback restores a live predecessor's registrations. Superseded runtimes cannot overwrite current checkpoints.

This implements graceful managed idle shutdown/reconstruction. It does not claim arbitrary process-crash recovery, persistence of every in-flight mailbox message, or recovery of child history already deleted by older code.

Each restored child acquires its own fenced execution owner and must exactly match its retained session snapshot before executing. A child reopened without another turn keeps the original committed snapshot, including its canonical-context identity. Delegation can update the current task without changing the immutable spawning descriptor used for hosted authorization.

## Validation

- Rust: 177 tests passed (63 subagent, 31 durability unit, 43 durability agent integration, 40 durability store integration), plus one doctest. One existing documentation example remains ignored. Coverage includes encoded result validation, nested/evicted reconstruction, exact retained snapshots and independent child execution owners, history/schema/model/token/host context, interrupted work and closed-child omission.
- Optimized Rust/WASM and JavaScript lifecycle/host/runtime suite: 114 passed. The delegated child survives two shutdown/recreate cycles while hosted tools retain its original authorization descriptor.
- Tools package suite: 38 passed.
- Hosted admission and authorization suite: 13 passed.
- Routing suite: 96 passed.
- JavaScript package and managed TypeScript checks passed.
- Focused Clippy passed with the repository's existing `missing_const_for_fn` allowance.
- Actual native `nanocodex`: repair passed in 14.356 s (seven model requests); a second CLI process recalled the prior receipt and original value in 2.559 s (one request). One Jev decision, eight live GLM requests; route unchanged. The model corrected a rejected whitespace patch and an unsupported macOS `cat -A` invocation. All eight inference requests succeeded; Wrangler separately emitted internal-error diagnostics. The final file and history assertions passed.

The native harness composes the PR's Jev route externally and passes the pin to the real binary; the CLI has no standalone routing admission flag. It uses real Code Mode tools and SQLite history, with synthetic task files.

## Actual managed nanocodex2

Three separate CLI processes ran against the final locally bundled PR Worker, real Rust/WASM, SQLite Durable Objects and Just Bash. Jev and GLM inference were live; account authentication, ancillary account egress and cleanup of never-allocated containers were synthetic fixtures. Eligible models were restricted to GLM low/medium. No production deployment or installed executable was changed.

| Journey | Result | CLI elapsed | GLM requests |
| --- | --- | ---: | ---: |
| Spawn one child, read a synthetic file, submit and wait for an object | pass | 11.392 s | 6 |
| After real idle shutdown, delegate to the same child and recall its value only from history | pass | 8.931 s | 5 |
| After a second real idle shutdown, close the same child | pass | 2.904 s | 2 |

Both child submissions arrived as JSON-encoded strings. The runtime decoded them once, validated the exact object schema, reported `decoded_json_text: true`, and returned typed objects through `wait_agent`. Child turn tokens advanced from 1 to 2. The continuation neither reread the file nor received the remembered value in its delegation message.

Both idle boundaries were verified using fresh `idle_shutdown` log entries, an unloaded runtime, zero active turns and no connected CLI sockets. Root and child routes, session identity and authorization remained unchanged. The checkpoint after the second idle retained the delegated task and second result. Closing removed the child's route, authorization and descriptor; public deletion returned HTTP 204 and removed the root route, turns and checkpoints. No delegated-binding conflict, failed tool call or failed inference request was observed. Initial local Hand attachment can receive a startup 503 while routing awaits the opening prompt; the existing client retry recovers.

The final managed run used two Jev calls (root and child) and 13 GLM requests. Neither restoration called Jev. The current fix pass totals four Jev / 26 GLM requests across two managed attempts, plus one Jev / eight GLM requests for native testing. Earlier historical attempts remain separate in the prior report. The final confirmation used a cumulative managed harness cap of five Jev / 30 GLM requests, raised from 16 GLM after the first attempt exposed the delegated-authorization bug.

## Preserved intermediate failures and limits

The first managed fix attempt completed all three model turns, but its logs exposed a conflict when a delegated task replaced the spawning descriptor. The implementation now preserves that descriptor for authorization, and the final test adds a second idle boundary to exercise reconstruction after delegation. That first attempt also returned deletion HTTP 503 because the local fixture omitted its cleanup-only sandbox namespace. The fixture was completed and separately passed create/delete without inference before the final run. The verifier initially compared the public agent ID with the distinct runtime-root session ID; that assertion was corrected to cross-check the authoritative route and authorization records. Original reports and logs were preserved.

These checks establish the bounded native and managed CLI journeys above. They do not establish held-out chooser quality, general GLM reliability, authenticated gateway generation, arbitrary crash recovery or production rollout readiness. The previously recorded low-confidence preference-alignment failure remains a calibration limitation; no passing claim replaces it.
