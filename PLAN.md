# Nanocodex plan

## Goal

Build a small, high-performance, headless Rust agents SDK for the current best
supported OpenAI coding model. Nanocodex should be pleasant to embed in a CLI,
server, TUI, notebook, test harness, or future language binding without making
any of those application shapes part of the core SDK.

The library owns model execution, conversation history, prompt caching,
Responses WebSocket state, tools, retries, and cancellation. Applications own
their presentation, event selection, tracing subscriber, persistence, and
transport to their users.

## Product contract

- The primary entry point is `Nanocodex::new(auth)` or
  `Nanocodex::builder(auth)`. A string remains the short API-key path; native
  callers may instead pass a managed ChatGPT OAuth authorization.
- `build()` returns `(Nanocodex, AgentEvents)`: a cheap cloneable command handle
  and one optional ordered event receiver.
- `agent.prompt(...)` accepts a user turn and returns an independently awaitable
  `Turn`; `turn.result()` returns its typed `TurnResult`.
- Follow-on prompts automatically reuse the complete retained conversation.
  Callers never pass previous final messages, response IDs, reasoning items, or
  tool results back into the session.
- The default is one fixed model contract with medium thinking, the standard
  instructions, built-in tools, persistent Responses WebSocket, and bounded typed
  retry/reconnect policy.
- `Tools::builder().tool(...)` accepts both `#[tool]` functions and complete
  `Tool` implementations in the same heterogeneous registry.
- `Responses::builder()` lets callers layer or replace the concrete Tower
  service stack without boxing the client.
- The CLI and Harbor InstalledAgent are adapters over this API. JSONL, Python,
  Docker, and Harbor are not required to embed the library.

## Architecture

```text
application
  ├─ Nanocodex handle ── prompt() ──> private owned driver
  └─ AgentEvents <────────────────── typed ordered events
                                      │
                                      ├─ model/session history
                                      ├─ tool runtime + code mode
                                      └─ ResponsesClient<S>
                                           └─ caller Tower layers
                                                └─ retry policy
                                                     └─ persistent WebSocket
```

Crate ownership is fixed:

- `nanocodex-core`: dependency-light prompts, events, model configuration, and
  typed Responses request/event/item data.
- `nanocodex-service`: persistent WebSocket behavior, complete streamed
  attempts, Tower service/client, retry policy, typed errors, and transport
  telemetry.
- `nanocodex-tools`: code mode, local tools, custom-tool registry, process
  lifecycle, and bounded tool output.
- `nanocodex-mcp`: stdio/Streamable HTTP clients, background handshake and tool
  discovery, authenticated transports, BM25 search, and deferred Code Mode
  dispatch.
- `nanocodex`: builders and the owned stateful agent lifecycle.
- `nanocodex-macros`: the `#[tool]` implementation.
- `bin/nanocodex`: the Ratatui daily-driver and headless JSONL adapter.

Lower crates must remain usable without importing higher orchestration crates.
Socket tasks and mutable driver details stay private.

## Foundation: complete

### 1. Repository and crate maintenance

- The workspace is a virtual manifest with the executable under `bin/` and
  focused library crates under `crates/`.
- Tools live in coherent modules under `nanocodex-tools/src/{shell,
  apply_patch,code_mode,...}` rather than one giant application crate.
- Obsolete root `src/`, duplicate CLI library helpers, and unused refactor paths
  are gone.
- Public crate boundaries follow ownership rather than historical file layout.

### 2. Responses WebSocket and Tower service

- One `Service<ResponsesAttempt>` call covers a complete streamed attempt, not
  merely a frame send.
- The standard retry policy classifies typed transient failures, honors server
  delay hints, reconnects, and safely replays committed history.
- `ResponsesClient<S>` stays generic over the caller's concrete service.
- Deferred `.layer(...)` composition and factory-only `.service(|| ...)`
  replacement are public builder paths.
- Large replay history is shared, known API items are typed, unknown items are
  retained only at their genuinely dynamic boundary, and partial failures are
  never committed.

### 3. Owned library API and tools

- A private Tokio task drives sequential turns and owns all mutable state.
- Prompt acceptance and result waiting are separate; no join handle, explicit
  shutdown, result/event join, or caller-managed driver loop leaks into the
  common API.
- Follow-on turns reuse one response chain, WebSocket, cache key, history,
  code-mode runtime, and shell sessions.
- Custom tools use one registry whether defined as a full trait implementation
  or an inline `#[tool]` async function.
- The public examples cover minimal result-only use, event consumption,
  follow-on prompting, and custom tool registration.
- Dynamic providers can start work with the owned driver and expose deferred
  Code Mode tools without inflating the stable prompt prefix.
- MCP servers handshake and list tools concurrently at startup. `tool_search`
  activates matching canonical `mcp__server__tool` names, including an
  immediate dynamic call in the same JavaScript cell. Stdio, Streamable HTTP,
  bearer tokens, custom headers, environment-resolved secrets, filters, and
  bounded startup/tool calls are covered by the public API.

### 4. Embedded consumers: complete

- PyO3 and Node/browser WASM bindings preserve the owned handle/turn/event
  contract without an app server or CLI bridge.
- Top-level Rust, Python, Node.js, and React/Vite examples are real consumers of
  the same session semantics. The browser agent runs in a module Worker and
  leaves its authorized WebSocket boundary to the embedding application.
- Application-defined subagents remain an example-level tool composition, with
  optional host-side event multiplexing rather than a core scheduler.

### 5. Native ChatGPT subscription authentication: complete

- `OpenAiAuth` is one shared capability across the Responses WebSocket,
  standalone search, image generation, child agents, and forks. API-key callers
  remain source-compatible.
- Native ChatGPT OAuth uses authorization-code PKCE with a state-checked
  localhost callback. The CLI owns browser UX and `login`, `status`, and
  `logout`; the library owns token parsing, redacted snapshots, persistence,
  refresh, and recovery.
- The CLI prefers `OPENAI_API_KEY`, including the repository `.env` loaded by
  direct runs. Without a key it falls back to Codex's `$CODEX_HOME/auth.json`
  (normally `~/.codex/auth.json`), so an existing Codex login remains reusable.
  An explicit auth-file path selects ChatGPT authorization even when a key is
  available and remains available for isolated embedders.
- ChatGPT mode selects the Codex backend endpoints and sends the bearer,
  `ChatGPT-Account-ID`, and optional FedRAMP header. Refresh is proactive near
  expiry and reactive once after a 401. Rotating refresh tokens are serialized,
  atomically persisted with owner-only permissions on Unix, and reloaded from disk
  before reuse. A changed account is never adopted by a running agent.
- Browser/WASM remains host-authorized. It does not persist OAuth credentials or
  introduce an SDK-owned relay/app-server boundary.

## Active roadmap

### Phase 1: events and observability (complete)

The ownership and result API now expose stable diagnostics for long-lived
applications without coupling the library to a subscriber or exporter.

Outcomes:

1. Define stable bounded spans for TUI interaction, agent turn, model call,
   Responses attempt, reconnect/backoff, Code Mode cell, and tool execution.
   Keep long-lived sessions as correlation identity rather than open spans.
   Include IDs, lineage/depth, durations, replay mode, error class, token/cache
   usage, structural prompt/tool metadata, and process outcomes as searchable
   attributes. Attach complete prompts and instructions, model input/output
   items, API-visible and encrypted reasoning payloads, Code Mode source, tool
   arguments, tool results, steering, cancellation, and lifecycle data as
   unredacted ordered span events. Carry active caller context with accepted
   prompts so attached child-agent turns render inside their bounded parent
   orchestration while ordinary and detached turns remain roots.
2. Keep subscriber choice outside the library. The CLI may install a sensible
   stderr subscriber, while embedders can install OpenTelemetry, metrics, or
   their own tracing stack.
3. Keep contractual `AgentEvents` distinct from tracing. JSONL remains a lossless
   adapter encoding of typed events.
4. Evaluate event selection against concrete consumers: the CLI/Harbor adapter
   needs every contractual event, while a minimal embedder may need only final
   messages and lifecycle failures. Add public filtering or handlers only if a
   concrete consumer demonstrates that dropping the receiver is insufficient.
5. Add Tower-aware observability around `ResponsesAttempt` so logical model
   calls, attempts, retries, reconnects, stream duration, and backoff are not
   conflated.

Gate:

- Existing result-only and follow-on examples remain unchanged.
- JSONL remains contiguous with one terminal event per accepted prompt.
- Tracing writes no stdout and preserves complete ordered runtime content.
- Parallel Code Mode and attached child-agent work has explicit parentage and
  measurably overlapping exported intervals rather than serialized spans.
- Warnings-denied Clippy, workspace tests, public examples, a native CLI smoke,
  and representative retained-trace benchmarks pass.

The deterministic operations gate covers compact/pretty/JSON formatting,
OTLP/HTTP export and flush, persistent Responses attempt/connect/retry spans,
parallel MCP startup and dispatch, 256 concurrent MCP calls, and an eight-turn
CLI-to-library-to-Code-Mode-to-MCP round trip. The cached MCP BM25 index handles
10,000 repeated searches in roughly 88 ms in the release profile on the
development machine.

### Phase 2: lifecycle control, steering, and branching (implemented)

The local Codex implementation establishes useful behavioral invariants but is
not the implementation template. It accepts steering only for an active regular
turn, preserves FIFO order, and makes queued input model-visible at the next
safe sampling boundary after a complete response/tool-output pair. Its forks
exclude partial work and start an independent thread from committed history.
Nanocodex should adopt those invariants without Codex's shared active-turn
mutexes, watch channels, task-per-turn cancellation, rollout flush/read cycle,
or whole-history clone and truncation.

#### 2.1 One actor for queueing, steering, and cancellation

- Keep `prompt(...)` unchanged: it always enters the bounded FIFO command queue
  and never changes meaning merely because a turn is active.
- Let the private driver continue receiving commands while a model run is
  active. The driver owns a pinned active-turn future and selects between its
  completion and the one command receiver; it does not spawn a task or expose
  shared mutable run state for each command.
- Route explicit steering and cancellation over a private bounded control
  channel owned by the active run. Prefer command receipt in the completion
  race so every accepted control has a deterministic linearization point.
- `turn.steer(...)` and `turn.cancel()` target that exact turn through an opaque
  internal key; ordinary prompting never exposes or requires turn IDs. A
  cloneable `turn.control()` lets result and control ownership live in separate
  tasks. Steering rejects queued and terminal targets. Cancellation removes a
  queued turn or stops the active one, and rejects terminal targets.
- A steering acknowledgement means the input entered the active FIFO, not that
  the model has sampled it. Drain that FIFO only between complete model
  responses: after any tool call and its output have been committed, before the
  next request. Never mutate an in-flight Responses frame or commit a failed
  partial response.
- If steering is pending when a response would otherwise finish, continue the
  same turn and preserve one result and exactly one terminal event for the
  original accepted prompt. Multiple steers remain distinct ordered user
  messages in the next request.
- Cancellation follows the same control path, yields a typed terminal result,
  removes queued work before execution, and terminates subprocess groups and
  descendants for active work. Queue capacities and scheduling policy remain
  private.
- A queued cancellation is acknowledged when the driver replaces that FIFO
  entry with a cancelled tombstone. Its result and terminal event remain at the
  original queue position so the per-agent event stream never interleaves turn
  lifecycles without public turn IDs.

#### 2.2 Persistent committed history

- Replace copy-on-write `Arc<Vec<ResponseItem>>` history with immutable
  per-turn segments. Each committed segment points to its predecessor; the
  active turn alone owns a mutable tail. Committing or snapshotting a turn is
  O(1), and a fork allocates only its new tail instead of cloning the retained
  prefix on first mutation.
- Serialize requests by walking segment references oldest-first plus the active
  tail. Full replay may traverse the prefix but must not clone or flatten its
  `ResponseItem`s. Healthy turns continue to send only their delta.
- Publish an O(1) fork snapshot before each safe sampling step. An active-turn
  snapshot carries the last completed response ID plus its exact client-owned
  delta, including paired tool results and applied steers, while excluding
  partial model output and unmatched tool calls. Terminal checkpoints remain
  the durable result/recovery boundary. Compaction installs a new root for that
  lineage without rewriting roots retained by existing branches.
- Retain an opaque cheap checkpoint in `TurnResult`. Start with
  `agent.fork()` from the latest commit; add `agent.fork_from(&turn_result)` only
  when a consumer demonstrates historical branching.

#### 2.3 Independent branch execution

- `fork()` returns a normal `(Nanocodex, AgentEvents)` pair with its own driver,
  Tower service, WebSocket, response chain, and `ToolRuntime`. Immutable tool
  definitions, stateless handlers, and MCP provider clients may be shared;
  shell sessions and Code Mode state may not. Agent-relative handlers use a
  per-driver tools factory and receive a weak `AgentHandle` bound to the
  invoking driver. The handle creates either a clean child with builder-owned
  private configuration or a contextual fork, so callers never pass API keys
  into tools and recursive delegation cannot accidentally target the root.
- Never clone the current standard `ResponsesService` for a branch because its
  clone shares connection state. Retain a factory that can build a fresh
  service stack. Standard services, deferred cloneable layers, and arbitrary
  replacement services all provide factories that recreate independent stacks.
- Give every branch a unique session/request identity while preserving a shared
  lineage cache key and byte-stable prompt prefix. Decouple those concepts in
  `RequestProfile` before exposing forks.
- Store completed Responses so a child can begin on a fresh connection from its
  parent's opaque `previous_response_id` without uploading the shared prefix.
  Keep complete client-owned typed history as the durable source of truth. If
  the server checkpoint is absent or expired, retry once without the ID and
  replay the exact byte-stable prefix plus committed history. Later child turns
  use their own delta and response chain. Concurrent branches must actually
  overlap on distinct connections rather than serialize through a shared
  connection mutex.

#### Delivery order

1. [x] Refactor the driver to receive commands during an active turn without
   changing the public prompt API; prove existing queue order first.
2. [x] Add explicit steering at safe response boundaries, then cancellation through
   the same owned control path.
3. [x] Introduce segmented history and retained committed checkpoints, with memory
   and replay benchmarks before adding the public fork operation.
4. [x] Add fresh service-stack factories, separate lineage/cache identity from
   session identity, and expose latest-safe-boundary `fork()`.
5. [x] Promote historical `fork_from(...)` with the public ledger consumer. Keep
   Code Mode child orchestration application-owned, and add one thin Ratatui
   `/btw` consumer over latest-checkpoint forks. Do not add an app-server
   protocol, persistence journal, or generic core scheduler in this phase.

Gate:

- Deterministic tests cover queued prompt order; steer FIFO and safe-boundary
  placement; no-active, stale, and terminal-race rejection; cancellation and
  descendant cleanup; and exactly one terminal result/event per accepted turn.
- Fork tests cover active-turn exclusion, historical checkpoint isolation, and
  compaction isolation. Per-driver tool tests prove recursive parentage and that
  weak fork handles do not retain stopped drivers. Branching a large retained
  trace shares segment pointers, performs no `ResponseItem` clone/flatten, and
  grows memory with new branch tails rather than branch count times retained
  history.
- Captured requests prove that a healthy child sends only its delta with the
  lineage cache key and parent response ID. Checkpoint-miss tests prove a single
  fallback replay of the exact byte-stable prefix and committed history before
  returning to deltas. Concurrent branch tests prove distinct connections and
  overlapping execution.
- Reconnect replay, cache-prefix invariants, the default one-prompt program, and
  all existing result/event consumers remain unchanged.
- The Code Mode example contrasts an independent child with a checkpoint fork
  inside one parent turn. The Ratatui `/btw` path keeps main and side events,
  queues, transcripts, and mutable runtimes isolated while allowing focus to
  switch without interrupting either turn.

### Phase 3: bindings and richer consumers (complete foundation)

The Ratatui client, PyO3 extension, and Node/browser WASM packages are promoted
embedded consumers of the same handle/turn/event contract:

- PyO3 owns one native Tokio runtime per constructed agent and releases the GIL
  while waiting for turn results or events.
- Node and web use one shared Rust/WASM model, history, cache, protocol, and
  Tower implementation. JavaScript owns only WebSocket/code-mode host
  capabilities and application-defined tools.
- Browser credentials/endpoints remain application policy. The SDK does not
  introduce an app server, relay, daemon, or JSON-RPC boundary.
- The Ratatui client may present one application-owned ephemeral `/btw` branch
  as a side-by-side pane. It consumes the same opaque fork API and does not add
  transport IDs, branch scheduling, or UI state to the library contract.

The deterministic binding gate covers construction/error translation, one
persistent Node WebSocket across follow-on turns, incremental response IDs,
stable cache/session headers, custom JavaScript tools, unified events, and the
browser host contract. Native cancellation remains owned by the Phase 2 turn
lifecycle rather than a binding-specific alternate runtime.

## Performance policy

- Optimize representative retained API/JSONL traces and real turns, not type
  aesthetics or isolated parser throughput.
- Preserve the stable prompt prefix, lineage cache key, stored-checkpoint
  fallback contract, and
  incremental `previous_response_id` path. Prompt caching is a primary runtime
  invariant.
- Known history remains typed. `RawValue` is appropriate for intentionally
  opaque retained payloads; `Value` belongs only at dynamic JSON/tool
  boundaries.
- Share immutable history and preallocate only where measured cardinality makes
  it useful. Do not add `SmallVec`, buffer pools, SIMD JSON, or custom allocators
  without a before/after retained-trace benchmark.
- Generic Tower dispatch is already negligible beside JSON, network, and model
  latency. Middleware should be chosen for correctness and operability first.
- Keep subprocess output bounded during production and preserve explicit
  process-group cancellation.

The detailed implemented transport invariants and current microbenchmarks live
in [`docs/RESPONSES_TOWER.md`](docs/RESPONSES_TOWER.md).

## Validation

For ordinary library changes:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo check --workspace --all-targets
```

Run `just run` when the public agent path changes. Use two or three focused,
fast Harbor tasks for model/tool behavior changes. Run the complete configured
`just eval` for a milestone, release, or cross-cutting lifecycle/transport
rewrite.

Latest full gate: the current worktree on `master@f466fb3`, 41 Terminal-Bench
tasks, 38/41 reward in 20 minutes 58 seconds. All 93,248 JSONL events parsed and
all 41 streams had contiguous sequence numbers, one stable request ID, and one
terminal event: 40 `run.completed` and one typed `run.failed`. Across 503 model
calls and 892 tool calls there were 41 initial connections, zero Responses
retries, and zero WebSocket reconnects. The run used 8,359,123 input tokens,
7,709,348 cached input tokens (92.23%), and 114,675 output tokens.

The three verifier misses were model/task outcomes rather than transport
failures: an invalid synonym substitution, forbidden extra build/cache files,
and a C extension below the required speedup. An isolated rerun confirmed the
speedup miss. The one Harbor-classified error was an upstream `cyber_policy`
rejection after the task had already produced a verifier-passing artifact; the
same pinned task then completed and passed in isolation. The retained full job
is `.nanocodex/harbor/jobs/2026-07-19__10-00-16-eval-35805`; focused records are
`2026-07-19__10-22-04-portfolio-optimization-48842` and
`2026-07-19__10-24-43-model-extraction-relu-logits-50144` under the same jobs
directory.

Harbor results and ATIF are the eval record. Do not copy another append-only
experiment diary into this plan; use Git history and retained job paths for
past investigations.

## Codex parity checkpoint

The local upstream review is complete through
`openai/codex@8431dc590a5bba9a1185d5579a5aabfbc469e50b`. Nanocodex adopted the
272,000-token Sol context window and 244,800-token automatic compaction
threshold, the generated-image no-duplicate-render hint from `7e51abbbd1`, and
terminal invalid-tool-image handling from `8431dc590a`. Audio forwarding remains
deferred until the supported model advertises audio input. Review and classify
every later upstream commit before advancing this checkpoint.

## Deferred and out of scope

- Provider/model abstraction and backwards compatibility.
- A Nanocodex-owned app server, JSON-RPC protocol, or daemon.
- Additional language bindings without a concrete embedded consumer.
- Browser/computer-use runtimes until a deterministic eval and consumer justify
  the capability.
- Skills/plugins, approval machinery, alternate runtime modes, or duplicate
  shell implementations.
- JJ provenance, graders, human-review state, durable replay journals, and local
  multi-agent scheduling until promoted by a concrete product slice.
- Broad event buses, collector traits, shared mutable run state, and generic
  provider/client layers without a current consumer.
