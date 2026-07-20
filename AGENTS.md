# Development instructions

## Product direction

- Nanocodex is a headless, library-first Rust agents SDK. The public product is
  the embeddable API; the CLI and Harbor adapter are examples and evaluation
  boundaries.
- Keep the scope narrow: one supported OpenAI model family, the Responses
  WebSocket API, one owned agent lifecycle, and caller-defined tools. Do not
  introduce provider/model portability or a generic app-server protocol.
- A normal consumer builds an agent, receives `(Nanocodex, AgentEvents)`, sends
  prompts through the cheap handle, and awaits typed `TurnResult`s. Events are
  optional and independent from results.
- Follow-on prompts reuse the session's retained history automatically. Never
  require callers to pass prior messages, response IDs, or tool results back
  into the agent.
- Builders expose deliberate policy. Queue capacities, socket tasks, mutable
  run state, replay bookkeeping, and similar mechanics stay private.

## Workflow

- Follow the active work in `PLAN.md` in order. Build vertical library slices
  with a real consumer; do not accumulate speculative abstractions.
- Prefer deletion and direct ownership over adapters that merely move data.
  Cleanup should materially reduce production or planning surface.
- Use existing project tooling and patterns. Add a dependency only for a
  concrete need in the current slice.
- Add focused deterministic tests for public contracts and demonstrated
  regressions, not for coverage. Compile public examples as part of validation.
- Use `just run` for a live native smoke. Use focused Harbor trials while
  iterating and the full configured `just eval` only for milestone/release
  gates. Never modify benchmark tasks or verifiers to make Nanocodex pass.
- Inspect the exact JSONL, Harbor result, trajectory, and verifier output for an
  eval claim. Separate cold image/bootstrap time from warm agent work.
- Preserve unrelated work. Never commit `.env`, caches, retained jobs, build
  output, or another user's untracked files.

## Codex reference

- Use the local checkout at `~/github/openai/codex/codex-rs` before making an
  architecture or behavior claim about Codex. Do not browse the web or invoke
  OpenAI documentation tooling unless the user explicitly asks.
- Codex is evidence, not an API requirement. Copy relevant invariants and
  operational behavior while keeping Nanocodex's smaller public surface.
- The reviewed upstream checkpoint is
  `openai/codex@35eaf3ffb0bf2001486c68c47a3d946b34d16634`. A parity review must
  inspect every later commit, classify it as port/evaluate/defer/out-of-scope,
  and cite adopted behavior before advancing the checkpoint.

## Workspace boundaries

- `nanocodex-core` owns dependency-light public data: prompts, events, model
  configuration, and complete typed Responses wire/domain types.
- `nanocodex-service` owns behavior at the API boundary: the persistent
  WebSocket, stream processing, retry policy, telemetry, and generic Tower
  service/client.
- `nanocodex-tools` owns code mode, built-in tools, the heterogeneous registry,
  and the public `Tool` trait.
- `nanocodex-mcp` owns MCP transports, background handshake/discovery,
  authenticated connection inputs, deferred tool search, and remote dispatch.
- `nanocodex` composes those crates into the owned agent lifecycle and exports
  the ergonomic builders and common types.
- `nanocodex-macros` implements `#[tool]`. Keep the executable under
  `bin/nanocodex`; do not move CLI behavior into the library.
- Each lower crate must remain useful without importing the higher orchestration
  crate. Avoid circular concepts and leaky socket/runtime types.

## Runtime invariants

- The private spawned driver is the sole owner of mutable conversation, model,
  tool-runtime, and Tower service state. It runs until all command handles are
  dropped.
- One agent reuses its WebSocket, typed history, code-mode runtime, shell
  sessions, stable cache key, and response chain across sequential turns.
- Agent-relative tools are instantiated per driver with weak self capabilities;
  a fork must never inherit a handler that still targets its parent driver.
- `prompt().await` waits only for command acceptance and returns an independently
  awaitable `Turn`. Prompt queueing order is owned by the driver.
- Client-owned typed history is authoritative. Healthy turns send only the new
  delta with `previous_response_id`; a replacement socket drops that ID and
  replays complete committed history.
- Commit only completed responses. A failed partial response must not execute a
  tool or enter history.
- Preserve stable prompt/cache identity and byte-stable shared prefixes across
  turns, retries, compaction, and reconnects. Stored Responses checkpoints are
  an optional transport optimization for branching; complete client-owned typed
  history remains authoritative and is replayed when a checkpoint is missing.
- Cancellation and process cleanup are explicit. Timeout or cancellation must
  terminate subprocess groups and descendants.

## Tower boundary

- One Tower call is one complete streamed Responses attempt, through
  `response.completed` or a typed failure. Do not return success after merely
  sending the WebSocket frame.
- `ResponsesClient<S>` remains generic over the caller's concrete
  `Service<ResponsesAttempt>`; do not box or globalize the service stack.
- The SDK owns one typed retry/reconnect policy. Caller middleware may wrap it
  with deadlines, concurrency, load shedding, tracing, metrics, circuit
  breaking, or error mapping without becoming a second retry owner.
- An attempt is replayable owned state. Large history remains shared; retrying
  must not duplicate side effects.

## Events and observability

- Typed events are a public library stream. JSONL is only the process adapter's
  encoding of that stream, not the internal transport.
- Tracing is diagnostic and belongs on stderr or in the embedding application's
  subscriber. It must never replace contractual events.
- Do not add a generic event bus, shared mutable collector state, or callback
  framework without a concrete library consumer and an explicit lifecycle.
- Never emit secrets, `.env` contents, hidden chain of thought, or full prompt
  bodies into logs or tracing. Retain only API-visible reasoning summaries.

## JSONL adapter contract

- Stdout is flushed JSONL only; diagnostics go to stderr.
- Every event contains protocol version, stable request/session ID, monotonic
  sequence, type, and object payload.
- Emit exactly one terminal event for every accepted prompt and preserve exact
  input/output streams before deriving ATIF.
- Harbor owns task containers, verification, and retained eval records. Python
  may install/run the binary and derive ATIF, but model decisions, API calls,
  tools, and mutations stay in Rust.

## Rust practices

- Follow Alloy-style Rust: small typed components, explicit ownership, and
  builder APIs for policy.
- Put stateful async lifecycle operations on owning structs. Reserve free
  functions for stateless transformations.
- Keep repeated wire shapes typed. Use `RawValue` for intentionally retained
  opaque payloads and `Value` only at genuinely dynamic boundaries; do not turn
  known history into a DOM for convenience.
- Prefer moving owned protocol/tool values over cloning them to satisfy a
  borrowed interface. Keep hot-path allocations and subprocess output bounded
  while data is produced.
- Return errors with context. Avoid `unwrap`, `expect`, and silent fallback in
  runtime paths. Use focused typed errors where callers distinguish policy or
  retry classes; keep `eyre` at application boundaries.
- Before handoff run rustfmt, Clippy with warnings denied, relevant tests, and
  public-example checks. Benchmark performance claims on representative retained
  traces, not synthetic microbenchmarks alone.

## TUI performance

- Develop the Ratatui consumer against replayed, representative workloads, not
  visual intuition alone. Treat retained Codex rollout traces and the longest
  available Amp thread exports as the primary corpus. Codex traces provide
  event ordering, streaming bursts, tool/reasoning interleaving, and timing;
  Amp threads provide mature interactive transcript shapes, long messages, and
  long-session behavior. Discover candidates with `amp threads list
  --include-archived --json` and read selected payloads with `amp threads export
  <thread-id>`.
- Derive deterministic sanitized fixtures or structural workload summaries from
  those traces. Never commit raw Codex traces, full prompts, tool arguments,
  secrets, or user content to the repository.
- Give every TUI phase a measured baseline and an explicit regression gate for
  the costs it changes: state-update throughput, frame construction and layout,
  rendered frame count, changed-cell/output volume, allocations or retained
  memory, input-to-frame latency, and resize behavior as applicable.
- Use focused synthetic cases only to isolate a demonstrated cost or correctness
  boundary. Validate claimed wins by replaying representative trace-derived
  sessions at multiple terminal sizes, including streaming and long-history
  tails.

## Current non-goals

- No app server, JSON-RPC daemon, provider abstraction, approval subsystem,
  compatibility layer, skills/plugins framework, or alternate runtime mode.
- Keep the promoted Ratatui, PyO3, and Node/browser WASM consumers as thin
  adapters over the owned session API; they must consume, not reshape, the
  library contract. Do not add browser/computer use, JJ review provenance,
  graders, or a generic local multi-agent scheduler. Application-owned Code
  Mode child tools and the Ratatui `/btw` fork remain thin consumers of the
  owned session API rather than core scheduling concepts.
- Do not expose raw transport response IDs or internal turn IDs. Branching may
  be exposed through opaque checkpoints on completed typed turn results only
  after the behavior is implemented end to end.
