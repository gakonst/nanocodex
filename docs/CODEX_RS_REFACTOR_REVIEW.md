# Recent codex-rs changes relevant to Nanocodex

Reviewed 11 September 2026. Fetched `openai/codex` main at
`02a8f038b87ad34d4a1dc5058eda26972ed7aa6c`; surveyed changes since 28 August and
inspected selected commit descriptions and implementation diffs. Nanocodex base
is PR #307 at `55d9b53a`. Upstream checkout was read-only apart from fetching Git
objects. These public CLI/app-server changes do not establish what code runs in
the hosted Agents API.

## Highest-value lessons

| Landed upstream | Lesson | Nanocodex assessment and suggested action |
| --- | --- | --- |
| [Captured settings for context, #44200](https://github.com/openai/codex/commit/b4507997e0bbbaab718bd5e30df1ea3d8ce85893) and [tools, #44242](https://github.com/openai/codex/commit/205f3671e14e8306919501cdef36ca2e5d360f5a), Sep 9 | Bind model capabilities, truncation and telemetry to the step that issued work, including delayed tool completions. | Our execution continuation already retains model/effort/tier settings. Audit asynchronous tools and mid-turn changes against that retained snapshot; do not create another independent settings store. Test model A issuing a delayed tool, switching to B, then A's completion arriving. |
| [Root turn attribution, #44611](https://github.com/openai/codex/commit/196964ef10db326047c3e71fc568693cbd7c58a8), Sep 10 | Persist root-turn lineage when work starts and preserve it through history. | `#recordAgentEvent` currently wraps child events with the current `#eventTurnId`. A child finishing after the root advances can therefore need stronger attribution than arrival-time association. Prioritize an originating-root-turn field and a late-child regression case before presenting child costs as authoritative per-turn accounting. This is a source-level risk, not a reproduced defect. |
| [Raw response usage, #41980](https://github.com/openai/codex/commit/e017e93aceafb2fe04bed1c926e448a5fb4f913d), Sep 1 | Keep complete upstream usage alongside normalized amounts; update generated API types together. | We retain raw wire events, but model-completion usage passes through Rust's fixed `Usage` fields. The new request projection inherits that normalization. Retain additive raw usage on the operational projection so future provider fields are not silently discarded. Keep our existing cache-write counts and cost status. |
| [Total exec request duration, #44207](https://github.com/openai/codex/commit/4f2449b4b21988d5015ce6edf755fbd6a37a4908), Sep 9 | Measure receipt-to-response duration, including admission/queueing, separately from dispatch duration. | Keep queue/admission, connection, provider first event, visible first text and completion as distinct measurements. The new benchmark records client clocks and Nano connection/model telemetry. Extend the managed inspector with these distinct phases instead of labeling a model span as total task latency. |
| [WebSocket ownership reset, #44489](https://github.com/openai/codex/commit/537278c65f6b405635de76e91a990105a629110b), Sep 10; [model-cache identity, #43906](https://github.com/openai/codex/commit/f046cf35df), Sep 8 | Cached transport and model metadata need explicit provider/account ownership. | Our new multiplex helper accepts a caller-owned socket and has no credential lifecycle. Keep it scoped to one owner; destroy/recreate the whole pool on identity changes. Review transport, context-cache and credential generations as one lifecycle. This is a defensive design requirement, not a finding of a reproduced cross-account issue in Nanocodex. |

## Refactor direction

Their extractions are useful because they move ownership and lifecycle boundaries,
not merely because they reduce file length. [#42102](https://github.com/openai/codex/commit/9969043b95339ec008b2f2f317f664d763fa4825)
extracts a trace transport that owns listeners, startup errors and shutdown. Their
[repository guidance](https://github.com/openai/codex/blob/02a8f038b87ad34d4a1dc5058eda26972ed7aa6c/AGENTS.md)
explicitly discourages adding new concepts to an already-large core crate and
asks for focused modules with nearby tests.

Nanocodex already separates its Rust agent, provider API, tools and observability
crates. Preserve that structure. The more immediate concentration is the managed
Worker: `js/managed/src/index.ts` is about 10.7k lines. The configuration and
session-operations helpers in #307 are a start. Extract coherent owned components
for environment preparation, settings application, webhook delivery and turn
projection, with explicit transaction/cancellation boundaries. Avoid an unrelated
repository-wide rewrite or separate objects that lose the shared SQLite transaction.

## Other changes worth learning from

- [Streamed compaction consolidation, #44255](https://github.com/openai/codex/commit/3dc1e2a58406dc69db5812539adfee7d89fa9ef7),
  Sep 9: retired a legacy implementation and its feature toggle. Nanocodex already
  uses `ResponseItem::compaction_trigger()` and streamed response handling. Verify
  cancellation, usage and retained context through this existing path; no migration
  is suggested merely because upstream consolidated theirs.
- [Environment ready snapshots, #42403](https://github.com/openai/codex/commit/1281778e3273ab8e28c700ba84f5f12115e0ddc0),
  Sep 3, and [startup errors, #44277](https://github.com/openai/codex/commit/9caddc5cf5bf4df5f114498e23bece90eaedb37b),
  Sep 9: distinguish no readiness report from a valid empty report and preserve
  useful bounded failure reasons. Our setup state already exposes failed/ready and
  avoids uncertain command replay. Native-hand readiness should additionally carry
  stable backend/build identity and an immutable accepted capability snapshot.
  Keep error text bounded and separate it from authorization or instructions.
- [Ignored-setting diagnostics, #44691](https://github.com/openai/codex/commit/e53c444964b02aef5d72dfde0a31470f43456f3e),
  Sep 11: report unknown field names and sources without dumping values. Our strict
  schemas already reject unknown fields, but some catalog errors are generic.
  Return safe field-path diagnostics and keep one shared settings schema across
  saved definitions and session creation.

## Concrete priority

1. Stable originating-root attribution and typed/raw usage in the new inspector.
2. One captured settings contract across model context, tool execution and telemetry.
3. Explicit cache/connection ownership before expanding pooling beyond caller-owned sockets.
4. End-to-end phase timings, then focused extraction from the managed Worker.
5. Native environment readiness/diagnostics as native setup grows.

This review recommends follow-up work. It does not change the execution engine,
credential ownership or deployed service. The accompanying live benchmark tests
an arithmetic prompt, not these concurrency or recovery cases.

The benchmark measures visible text from `assistant.delta.text` or the provider's
`output_text.delta`. A first protocol event or a model's first output item can be
metadata or reasoning; neither should be labeled visible TTFT.

The subsequent [deployed Cloudflare study](../output/cloudflare-agents-2026-09-11/README.md)
extends this to extraction, dependency scheduling, long JSON output and repeated
sessions. It adds startup phase evidence and identified a stale-owner heartbeat
problem during a platform instance replacement; the PR now checks durable progress
before heartbeats so the SDK can reconnect when the old storage connection fails.

Local source references: [execution retention](../crates/nanocodex-agent/src/model/run/continuation.rs),
[usage schema](../crates/nanocodex-oai-api/src/responses/event.rs),
[managed attribution](../js/managed/src/index.ts),
[operational projections](../js/managed/src/session-operations.ts),
[compaction](../crates/nanocodex-oai-api/src/session/compaction.rs),
[pool lifetime](../js/nanocodex/runtime/response-lanes.mjs).
