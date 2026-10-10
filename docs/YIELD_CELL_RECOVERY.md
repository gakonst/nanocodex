# Durable Code Mode observation reconciliation

A retained yielded exec receipt can survive replacement of the runtime that owned
its live cell. Previously the next wait only reported a missing cell. This change
makes that wait reconcile durable evidence without evaluating guest source,
dispatching tools, or reconstructing promises.

## Protocol and production wiring

The managed effect journal now provides an optional observation journal. The
runtime resolves the canonical operation/model identity once, registers the
public cell ID before guest execution, and stores each observation before
consuming its in-memory output. Registration and recording use the existing
durable owner generation fence and storage sync acknowledgement.

ManagedAgent constructs the journal in js/managed/src/index.ts and passes it as
codeEffectJournal to agent creation. The existing Node and Claude hosts pass the
same object as effectJournal to createCodeRuntime. No separate feature switch or
production-only alternate implementation was added.

Recovery authenticates through the caller's session and original cell mapping.
It never executes guest source, admits effects, cancels providers, or mutates the
journal. Completed original effect receipts remain historical evidence; pending
intent stays outcome unknown. Missing legacy mappings, unmarked missing observation metadata,
corrupt chunks/checksums and stale ownership fail closed.

A terminal observation proves the recorded script result. Its output and nested
receipts are rendered as historical text; nested_calls and notifications are
always empty. This applies to repeated waits with the same call ID and new wait
IDs. Exact replay of an acknowledged tool call remains the outer durable
CompletedToolCall ledger's responsibility. The observation ledger is deliberately
not a second event replay mechanism.

Unknown nonterminal recovery returns success=false and no cell field, so a past
running observation is never presented as a live executable cell. A terminal
recovery retains its recorded success and cell.running=false. Terminate on a
missing cell performs the same evidence read and does not imply termination.

## Claude durable continuation

Claude journals one cursor per admitted operation. After any continuation
(owner loss, object reconstruction or runtime reopen) the round in flight is
the recovered index, and its Code Mode calls are classified as follows:

- A settled tool receipt replays unchanged.
- An unreceipted exec from a replayed model response is refused with
  "Code Mode admission was lost during recovery". Its guest source may already
  have dispatched effects, and the journal cannot distinguish begun from
  never-begun cells.
- An unreceipted wait from a replayed response runs. wait never evaluates
  source or admits effects: it observes a cell that is still live in this
  runtime, or reconciles a missing cell from the observation journal above
  (CODE_CELL_RECOVERED_EVIDENCE). Its result becomes a durable tool receipt.
- A model response generated fresh after the continuation starts a new round.
  The model receipt is committed before any of its tools dispatch, so none of
  its calls can have run; exec and wait both execute normally.

When a continuation happens without replacing the runtime, the recovered wait
can find the cell still live. An earlier observer that has not yet released
the cell makes the wait fail with "already has an active observer"; output an
interrupted observer consumed is retained only in the journal's latest
observation. Neither case repeats an effect. terminate=true on such a wait
aborts the live cell as requested.

The public HTTP journey covers both owner-loss boundaries with workerd SIGKILL:

```sh
cd js/managed
node --test test/managed-curl-claude-yield-recovery.test.mjs
```

It asserts a replayed wait returns recovered evidence (completed YA receipt,
pending YB) while its unreceipted sibling exec stays refused, that a fresh
exec after an in-flight model request is lost runs once, and that no synthetic
effect is dispatched twice. It is part of npm run test:durability:curl, which
CI runs; evidence is written under output/managed-curl-recovery/claude-yield-*.

## Retention and compatibility

Only the latest observation per cell is retained. Full envelopes are limited to
8 MiB; larger live results still succeed but retain a bounded recovery summary.
The newest retained envelopes are capped at 128 records and 32 MiB aggregate,
with older payloads removed transactionally. Small summaries add bounded metadata
over that payload limit. Original effect receipts and identity tombstones remain
in their existing journals; this is not global garbage collection of all managed
storage. Eviction sacrifices observation availability, never effect identity.
An atomic tombstone records the exact evicted sequence. Recovery with a matching
marker skips the missing observation payload and reads the original effect
journal, explicitly reporting observation_retention=evicted in the textual
evidence. Whole-script outcome stays unknown; completed receipts and pending
intent IDs remain available without new nested events or writes. A newer
observation clears the marker atomically. Missing metadata without a marker,
conflicting retained metadata, or a mismatched sequence still fails closed;
upgrading an older ledger does not retroactively classify missing data as eviction.
Eviction markers, identity tombstones and original effect receipts are not
covered by the observation payload cap.

Wire fields are unchanged: output, success, optional cell, nested_calls, and
notifications. The Rust CodeModeExecution deny_unknown_fields struct accepts
these existing fields; no new machine outcome field was introduced. Claude's
production host consumes the result and exposes an empty nested event list.
Outcome-unknown details continue to be carried in textual evidence.

## Validation

Run the managed SQLite recovery suite:

```sh
cd js/managed
npm run test:code-observations
```

The suite uses native Node SQLite on disk and actual database close/reopen.
It covers replayed yield, completed late receipts, pending external intent,
lost terminal acknowledgement, repeated observer IDs, foreign sessions, stale
owners, corrupt receipts, missing legacy IDs, admission cancellation, oversized
output, wait budgets, count/byte retention, production Claude consumption,
receipt reconciliation after eviction, and fail-closed metadata migration.

Run the related runtime suites from the repository root:

```sh
node --test js/nanocodex/test/code-runtime.test.mjs \
  js/nanocodex/test/code-mode-lifecycle-parity.test.mjs \
  js/nanocodex/test/code-cell-retention.test.mjs \
  js/nanocodex/test/code-mode-preempt.test.mjs
```

These exercise native, QuickJS and worker evaluators, Node host identity
admission, and Node/browser host preemption.

The Node SQLite adapter uses synchronous disk transactions and a no-op
storage.sync. The separate workerd journey uses real Durable Object SQLite and
storage.sync, the shipped managed QuickJS evaluator, and a Rust/WASM Agent:

```sh
cd js/managed
npm run test:code-observations:workerd
```

As with the existing managed process journeys, prepare workspace dependencies,
the managed QuickJS asset, and the SDK pkg-web WASM build first. Each scenario
kills only its own detached Miniflare/workerd process group with SIGKILL, then
opens the same persisted database in a fresh process. It checks retained yielded
receipts, a completed receipt arriving after yield, pending intent, repeated and
terminate waits, foreign sessions, terminal observation acknowledgement loss,
and unchanged durable evaluation/dispatch counters. A real Rust/WASM Agent then
calls wait through its host against that recovered journal; historical receipts
reach the synthetic model without new nested tool events.

The fixture saves the yielded outer envelope in DO storage; it does not exercise
replay through the full ManagedAgent CompletedToolCall ledger. Terminal ACK loss
is injected immediately after the real observation record/storage sync returns.
Only external tool responses and model transport are synthetic. Per-run bundles,
database files and request/event traces are retained under
output/code-observation-workerd/. This local workerd/Rust integration does not
claim a deployed Cloudflare platform restart or production thread recovery.

The standalone workerd command prepares the pinned QuickJS WASM before running.
It is included in test:recovery, but the current CI workflow does not invoke
that package command. Broad JS tests are also paused. CI execution of these
journeys remains pending a workflow change with the required workflow grant;
local passing runs do not establish CI coverage.
