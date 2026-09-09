# Durable execution without a turn-length cap

The execution owner retains the active context and current effect batch. The
SQLite head contains references, phase, counters, and a small receipt tail;
immutable records hold payloads. Publishing records and advancing the head is
one transaction, so a crash cannot expose a partial revision.

This replaces compressed whole-state snapshots and whole-turn replay. It does
not require a client retry journal, duplicated request records, or a turn step
limit. Hosts report interrupted execution; Rust resumes its retained position.
Ordinary machine identities survive socket loss so recovery cannot accidentally
select a different execution host. Explicit revocation still removes access.

## Memory and I/O

- Resident memory: current model context + active tool working memory + bounded
  storage buffers. Historical storage grows with work; arbitrary tool allocations
  remain the execution host's responsibility.
- Payload records: at most 256,000 UTF-8 bytes. Context pages: 64 message references.
- Cold hydration: up to 16 requested records in one WASM/SQLite bridge call.
  Cold ownership/status reads load only the execution head.
- Publication: write new messages and changed context pages. Prime the current
  context hash cache on reopen so old messages are not rewritten.
- Consumed steering and settled effect maps are retired. Code-cell origin mappings
  live only until the cell finishes, preserving yielded nested-result identity.
- Managed export: at most 16 records per immutable R2 object. Transfer is resumable
  in SQLite; publish the destination head only after its records have arrived.
  The generic full-archive convenience API materializes all records; use its
  asynchronous page API for large stores.

## Regression evidence

The checked-in gates exercise actual Rust/WASM and deployed Worker behavior:

| Journey | Observed result |
| --- | --- |
| 64-batch WASM turn, lost acknowledgement at batch 31 | Exact continuation; 461-byte active state, 4.1 MB WASM heap |
| Same turn, immutable writes | Largest publication 35,741 bytes for a 512 KiB conversation |
| Paged import into a fresh store and cold continuation | 18 pages; only 7,163 new bytes written |
| Real `nanocodex2`, 64 dependent native effects, steering and Worker redeploy | Exactly one completion per call, no duplicate fixture effects, fresh CLI reopen |
| Real DO, 96 long turns and 432 cancellations | 528 operations; archived old receipt replay, idle cold reopen, tool follow-on; 3,614-byte head |
| Worker SQLite interruption at record 51 of 64 | Old head and records remain intact; retry publishes the complete batch |
| Worker R2 transfer larger than 8 MiB | Bounded source/destination batches resume from durable progress |

Run `node --test js/managed/test/live-binary-durability.test.mjs` with explicit
`NANOCODEX_DURABILITY_TEST_BINARY`, `NANOCODEX_MANAGED_URL`, and
`NANOCODEX_DURABILITY_TEST_API_KEY`. Set
`NANOCODEX_DURABILITY_TEST_REDEPLOY_CONFIG` to a Wrangler configuration to redeploy
at effect 16. The gate uses temporary native receipts, stops its Hand, and deletes
successful fixture agents. Failure leaves the agent and evidence for diagnosis.
The independent `live-durability.test.mjs` gate checks the long-history archive.

These tests do not claim recovery of arbitrary in-memory JavaScript closures or
exactly-once side effects from providers without durable receipt support. See
[DURABILITY.md](DURABILITY.md) for the execution contract and ambiguity rules.

## Evidence from upstream

- Cloudflare issue [1710](https://github.com/cloudflare/agents/issues/1710)
  describes eager full-history hydration bricking long sessions on every wake,
  followed by repeating alarm failures. This is a closely related failure;
  it does not prove that Nanocodex has that exact SDK call path.
- Cloudflare Agents source inspected at
  `0966a0b076cde9b4f04d29c12c469690fd65491f`:
  [SessionsCore](https://github.com/cloudflare/agents/blob/0966a0b076cde9b4f04d29c12c469690fd65491f/packages/agents/src/sessions/core.ts)
  persists messages separately, avoids recursive SQL carrying message bodies,
  and hydrates content in windows. The forward path uses 50 rows / 4 MiB,
  permitting one oversized row. It still materializes path metadata, caps path
  depth at 10,000, and `getHistory` collects all yielded messages. Recent-history
  reads always admit the latest message even when it exceeds the byte budget.
  These are useful techniques, not a proof of constant total memory.
- Pi source inspected at `acaa253cc8e3f159e6100b6f3874861b1f0bfc99`:
  [session manager](https://github.com/earendil-works/pi/blob/acaa253cc8e3f159e6100b6f3874861b1f0bfc99/packages/coding-agent/src/core/session-manager.ts)
  appends transcript entries and represents compaction with references to retained
  entries. Its standard manager retains `fileEntries` and `byId` in memory.
  Incremental file reads do not make the resulting retained entry array bounded.
  [Compaction](https://github.com/earendil-works/pi/blob/acaa253cc8e3f159e6100b6f3874861b1f0bfc99/packages/coding-agent/src/core/compaction/compaction.ts)
  can split a long turn while preserving tool call/result boundaries.
- Installed `agents@0.22.0` does not export `agents/sessions`. Upstream-main
  source and published documentation are not assumed to be a drop-in upgrade.
