# Tokio audit (2026-10-07)

## Scope and principles

Static review across `crates/`, with a supplementary CLI call-site check. The
[Dial9 article](https://dial9-rs.github.io/blog/principles-for-fast-tokio-applications/)
was read in full, including the exceptions and scheduler appendix. Start from
user latency, distinguish fairness from throughput, amortize shared blocking-pool
coordination, avoid contended synchronous locks, and bound expensive fan-out.
Its isolation/spinning/multiple-runtime suggestions need production scheduling
traces, not blanket application here. Do not replace every mutex or add a yield
at every await. Immediately-ready buffered reads can still monopolize a poll.

[uv PR 21372](https://github.com/astral-sh/uv/pull/21372) batches an entire ZIP
extraction on one blocking task with a bounded download pipe, retaining ownership,
validation and cleanup. We apply that ownership principle to one rollout command
(metadata, serialization, writes, retry/rollback, flush, sync and publication),
not a separate task per filesystem operation. The reported Codex thread-listing
80% improvement is task context, **not a measured Nanocodex result**.

`tokio-inventory.txt` is the regenerated path/line inventory (including test code,
imports and broad loop candidates), reusing the interrupted agent's scan. It is
not a claim that each match is a defect. Reproduce:

```sh
rg -n 'tokio::fs|fs_err::tokio|spawn_blocking|Mutex|RwLock|tokio::spawn|FuturesUnordered|join_all|Semaphore|yield_now|while let|loop \{' crates --glob '*.rs'
```

No `fs_err::tokio` match was found. Unqualified imported locks are included.
Static review did not prove a standard Mutex/RwLock guard survives an await;
lock-held I/O and async-lock serialization remain candidates below.

## Ranked findings / dispositions

1. **Thread/session listing and resume.** `nanocodex-agent/src/rollout/load.rs`
   `list_sessions` already batches synchronous traversal, per-file header/preview
   parsing and sorting; it does **not** perform 1000 `tokio::fs` calls. Active/archive
   preference, malformed-file skipping and optional name index must remain.
   `RolloutConfig::load_session` is also synchronous. CLI `bin/nanocodex/src/main.rs`
   invokes these synchronously inside its async entry point; cold startup/resume
   can occupy a runtime worker. `managed_memory.rs` already moves search/load
   into one blocking segment. Listing measured below: leave algorithm unchanged.
   CLI cold start and full restored-session latency were **not measured**.

2. **Durable turn append / resume replay (implemented).**
   `nanocodex-agent/src/rollout/store/writer.rs` previously used an async File for
   metadata, every JSONL record, flush, sync and rollback. A large history creates
   repeated blocking-pool dispatch. Now its channel actor transfers exclusive
   writer ownership to **one `spawn_blocking` per accepted command**. The segment
   includes all fs work and serialization; it returns ownership before accepting
   the next command. Idle actors do not occupy blocking threads. Channel capacity
   remains eight; shutdown closes admission before flushing. Transaction order,
   flush/sync, rollback/retry, pending-on-failure and release-store publication of
   `committed_bytes` are retained. No extra lock, per-record task or new fan-out.
   Joining errors terminate the actor; queued receipts then close rather than
   falsely reporting persistence. Cancellation cannot abort an already-running
   blocking segment; it owns the file and completes that admitted transaction.
   This does not change the synchronous recorder creation/resume API.

3. **Streaming / subagent fan-out.** `nanocodex-subagents/src/runtime.rs`
   residency/message async locks cover recovery and resource shutdown; they can
   serialize unrelated child traffic. `tools.rs` uses `join_all`; default active
   concurrency (`lib.rs`) is unlimited, though capacity reservations and finite
   user configuration exist. Code Mode `FuturesUnordered` and unbounded update
   channels (`nanocodex-oai-tools/src/code_mode/`) can accumulate work.
   Admission/unknown-outcome semantics make a mechanical Semaphore change unsafe:
   defer until contention/load measurements and transactional tests exist.
   Subagent shutdown already shares a deadline, joins in parallel and aborts
   timed-out handles. Do not claim a discovered deadlock.

4. **Startup.** Code Mode `embedded.rs` uses synchronous readiness `recv` while
   `mod.rs` cold-spawns a host inside shared-host locking. Native Claude image
   ingestion (`nanocodex-claude/src/prompt.rs`, called by async agent paths) does
   bounded metadata/open/read/base64 synchronously. Preserve the frozen request,
   regular-file and size checks if offloading; do not turn admission into a
   cancellable partial mutation. No startup benchmark, no speculative edit.

5. **Hand file operations / retained PTYs.** `nanocodex-oai-tools/src/shell/output.rs`
   runs an EOF read loop on the shared blocking pool; many retained sessions could
   delay fs transactions. Output caps and short buffer locks are already present;
   consider dedicated I/O threads/session limits only with saturation evidence.
   apply_patch and image preparation already batch synchronous work in blocking
   segments. `nanocodex-hand/src/recording.rs` synchronous directory scans are
   candidates for call-site placement review, not per-entry async conversions.

6. **Feature-specific browser I/O.** `nanocodex-browser/src/native/session_trace.rs`
   opens/writes/flushes per event, and encodes snapshots inline. Trace limits and
   optional screenshots/DOM constrain it; buffering needs a visibility contract.
   Profile copy/cookie extraction and heap analysis already offload batches.
   Single image writes and downloads do not justify a workspace-wide fs rewrite.

7. **Mutexes and read-loop fairness.** Steering queues/model-call counters use
   Tokio Mutexes for short bookkeeping in `nanocodex-agent`; control shutdown
   state uses short standard locks. Replacing these has no demonstrated benefit.
   Voice `message_reader.rs` can parse buffered frames without receiving again;
   Code Mode and stream consumers also have immediately-ready loop candidates.
   Existing remote audio/video paths explicitly yield. Adaptive yielding needs
   a concurrent latency workload showing poll starvation: no yield change kept.

## Measurement protocol and results

Baseline source: `368e54bbbce79873cbbc11bd6d19d986be63cba5`; benchmark + baseline
commit: `0160b709c`. Run `benchmarks/tokio-rollout.sh` (or the exact Cargo invocation
in it). The ignored `rollout_perf` test is adjacent to private production writer
code, not a mock filesystem benchmark. macOS 26.3.1(a), Apple M1 Max, Rust 1.97.1,
locked Tokio 1.52.3; unoptimized test profile, two runtime workers, local APFS,
warm OS caches. Five warmups then 101 samples; median is sorted sample 51, p99
is nearest-rank sample 100. Fixture setup/file creation excluded. Append includes
serialization, metadata, durable sync and publication of a fresh 1000-message
commit. After includes the single blocking-task scheduling/join; before called
the old async transaction directly. Channel admission latency is not included.
Listing asserts 1000 returned sessions every iteration. These are local component
latencies, **not release throughput or end-to-end UI claims**. Other agents share
this machine; it was not CPU-isolated, and results do not establish statistical
significance or cold-disk behavior. Raw logs: `baseline.txt`, `after.txt`.

| Component | Before median / p99 | After median / p99 | Median change |
|---|---:|---:|---:|
| List 1000 rollouts (unchanged control) | 40.522 / 43.636 ms | 40.574 / 44.284 ms | +0.1% (noise) |
| Append 1000 history items | 21.844 / 34.771 ms | 13.987 / 15.075 ms | -36.0% |

Append p99 fell 56.6% in this run. Keep only that measured batching change.
No claims for network streams, cold CLI, live Hand traffic, production scheduling
latency, RSS or restored-session speed. A release-profile and loaded-runtime
replication are follow-ups, not inferred results. Avoid extrapolating the uv or
Codex improvement percentages to this repository.

## Validation

See `tests.txt` and `clippy.txt` for actual command output. Existing rollout tests
cover active/archive discovery, compaction, publication, failure retry without
duplicates, and resumed writer repair. `cargo fmt --all` is run; only the touched
crate is tested/linted, not every platform/feature of the workspace.

Final validation: `cargo test --locked -p nanocodex-agent --lib` passes 52 tests
(one manual benchmark ignored), including a new current-thread-runtime regression
for command acknowledgement, committed byte boundary and shutdown rejection of
queued later work. `cargo clippy --locked -p nanocodex-agent --all-targets` succeeds
with two existing `missing_const_for_fn` warnings in `session.rs`.

The full `cargo test --locked -p nanocodex-agent` is **not green**: unit tests pass,
integration tests have 141 passes and four failures. A separate detached baseline
worktree at `0160b709c` reproduces the identical four failures (141 passes), logged
in `baseline-tests.txt`. They assert developer instructions without the existing
runtime-model-identity suffix, causing mock server panics/HTTP fallback errors:

- `model::persistence::serialized_session_and_codex_rollout_share_committed_history`
- `model::persistence::serialized_session_rebinds_deployed_instructions_and_tools`
- `model::transport::websocket::model_is_fixed_at_creation_while_runtime_reasoning_policy_can_change`
- `model::transport::websocket::supported_reasoning_updates_preserve_socket_prefix_and_replay_after_fast_or_reconnect`

These unrelated assertions are not changed or hidden. Doc tests were not reached
in the failing full run. No secrets or credentials were used by these fixtures.
