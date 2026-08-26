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

- Stop a workflow as soon as there is sufficient evidence that it is
  pathological, wasteful, destructive, or architecturally wrong. Never let a
  known-bad operation finish merely to obtain its output or preserve momentum.
  Fix the root cause at the highest boundary that owns it, then rerun the
  corrected workflow from that boundary; do not paper over the problem with a
  downstream workaround.
- Build complete vertical slices with a real consumer; do not accumulate
  speculative abstractions.
- Prefer deletion and direct ownership over adapters that merely move data.
  Cleanup must materially reduce production or planning surface.
- Use existing project tooling and patterns. Add a dependency only for a
  concrete need in the current slice.
- Keep commits focused, chronological, and independently understandable. Never
  mix unrelated cleanup into an iteration commit.
- Preserve unrelated user work. Never commit `.env`, caches, retained jobs,
  build output, or another user's untracked files.
- Web interfaces must never show transient loading copy, spinners, skeletons,
  or Suspense placeholders. Preserve the last complete interface when possible;
  otherwise render nothing until the boundary is ready. Show explicit,
  actionable failure states only after an operation actually fails.

## Browser verification

- Local web stacks must remain instance-isolated: each checkout or explicit
  instance owns its Wrangler state, port, and application and playground hosts
  so multiple versions can run concurrently. Do not share a
  live Miniflare state directory between processes.
- Local WebAuthn keeps exact per-instance `*.nanocodex.localhost` browser
  origins but always uses the parent RP ID `nanocodex.localhost`. Never scope a
  passkey to an instance subdomain. Preserve
  the signed public credential handoff that lets an isolated local Worker
  verify the same passkey without sharing private keys or mutable Worker state.
- A local authentication change is incomplete until one real browser creates a
  passkey through the Connect playground and dialog in one instance and reuses
  that exact credential through the complete account-link leg in a second,
  concurrently running instance.
- Every change to a web interface, browser runtime, route, Worker-backed browser
  API, or local web development path must be exercised through the host-managed
  browser against the real running application. Unit tests, source assertions,
  typechecks, builds, curl, and protocol probes are supporting evidence; they
  never substitute for the browser pass.
- Keep a browser verifier running in parallel with web implementation whenever
  practical. After each coherent change, cold-start the owned local stack and
  exercise the affected user journey through visible controls. Inspect page and
  console errors, failed network requests, and WebSocket traffic before
  continuing.
- A web task is not complete and must not be described as working until the
  exact changed flow passes in the browser on its canonical direct URL. For
  navigation or layout changes, test desktop and a representative touch/mobile
  device. For durable or multi-client behavior, use independent tabs or browser
  contexts and verify reload and reconnect behavior.
- Treat a browser-discovered failure as authoritative. Stop downstream
  validation, fix the highest owning boundary, restart from a clean stack when
  required, and rerun the browser journey. Do not dismiss a browser failure
  because lower-level tests pass.
- Authentication and secret-routing browser checks must also prove absence:
  managed and localhost flows must not open or render interactive OAuth or
  device-code UI, and browser network traffic or storage must never receive
  provider credentials.

## Web architecture and performance

- Start from conventional React and Vite ownership. Each HTML entry has exactly
  one `createRoot` and one declarative component tree. Internal surfaces such as
  the artifact runtime are ordinary components or routes in that tree. Do not
  add imperative sub-root mounting, manual mount/unmount helpers, or parallel
  render paths for application features.
- Let Vite own the module graph, dependency optimization, hashing, preload
  generation, tree shaking, and chunk production. Do not add application-level
  module registries, cached `loadX()` wrappers, component `prepareX()` or
  `preloadX()` APIs, manual dynamic-import fan-out, manifest walking, preload
  filtering, or hand-authored chunk groups. If browser evidence requires route
  splitting, use the router or framework's canonical declarative route boundary
  and let Vite compile it; do not build a second loader system around it.
- React owns rendering and lifecycle. Use normal components, hooks, context,
  effects, and transitions at the boundary that owns the state. Do not use
  `Promise.resolve`, microtasks, idle callbacks, timers, or home-grown promise
  caches merely to stage imports, mounting, navigation, or paint order. Every
  asynchronous boundary must correspond to real I/O, a real Worker operation,
  or a framework-owned lifecycle.
- Optimize from measurements in the real browser, not from source-text tests,
  aesthetic bundle scripts, or speculative chunk diagrams. Record cold FCP,
  LCP, blocking time, transferred bytes, and the network waterfall; record
  warm navigation latency, visual stability, and interaction latency across the
  primary routes. Preserve both fast cold start and instant warm navigation
  unless an explicit product decision chooses one tradeoff.
- Keep the canonical development server alive during normal iteration. Use HMR
  or reload the page and preserve the browser session; restart the stack only
  when configuration or corrupt external state actually requires it. A clean
  browser navigation is not a reason to kill the services.
- Prefer behavioral browser and component tests over regular expressions that
  pin incidental implementation text. Static architecture tests may enforce a
  small public invariant such as one React root or the absence of a forbidden
  loader layer, but they must not force bespoke orchestration to survive.

## Frontier eval iteration

- Optimize for wall-clock time from an idea to evidence from the real benchmark
  host. Local compilation ceremony, compatibility work, speculative tests, and
  preserving replaceable experimental processes are subordinate to that loop.
- Run benchmarks on `ubuntu@dev-georgios`. The canonical state directory is
  `/mnt/nanocodex-evals/evals` and the canonical ledger is
  `/mnt/nanocodex-evals/evals/state.sqlite3`. Imports, new worksets, resumed
  runs, coordinator/API reads, and the eval dashboard use that ledger unless
  the user explicitly requests an isolated scratch run.
- Deploy a coherent slice immediately and exercise it there. Start from fresh
  `origin/master` plus the focused change being tested; if GitHub or DNS is
  unavailable on the host, transfer the exact local source instead of waiting
  or using an old deployment.
- Replacement is component-scoped, not preservation-oriented. Controller/UI
  work replaces the controller/UI and leaves workers and coordinator alone;
  coordinator work replaces the coordinator; worker/runtime or schema work may
  stop the controller and all workers for that benchmark before restarting the
  whole scoped run. Never disturb unrelated profiles or services. When the user
  asks to replace a scoped component on the box, replace it instead of trying
  to preserve that component's current process.
- Do not run `cargo test`, broad `cargo check`, Clippy, or full-workspace builds
  during the active edit loop. Make the complete focused change, format it, use
  cheap consumer typechecks when useful, then build once for deployment on
  `dev-georgios`. Run a focused Rust test only for a demonstrated regression or
  when the user explicitly asks. Reserve broad validation for an explicit
  milestone, release gate, or final handoff where its signal justifies the
  compile time.
- Never test neural scheduling policy by asserting prompt text. Build, deploy,
  and exercise orchestration changes against the real coordinator and host.
  Record worker/VM correspondence, task deltas, completions per unit time,
  memory, swap, load, pressure, infrastructure retries, and OOMs.
- High utilization is the goal, not a failure. Judge saturation by productive
  throughput, stale claims, infrastructure retries, OOM behavior, and recovery;
  do not label a host unhealthy merely because CPU, RAM, swap, load, or pressure
  is high. During a normal saturation measurement, never manually shed workers:
  the OS and controller own exhaustion behavior. Scoped deployment and schema
  resets are the explicit exception.
- Treat live waves as telemetry, not blocking work. Continue inspecting real
  evidence, fixing known failures, and preparing the next deployment while a
  wave runs. Wait only when a concurrent mutation would invalidate a specific
  measurement needed for the next decision.
- Treat obsolete services, systemd drop-ins, scratch directories, deployments,
  and other stale host residue as operator cleanup. Inspect their exact scope
  and remove them directly on `dev-georgios`; do not infer a product feature,
  compatibility path, migration, or automatic cleanup requirement merely
  because old operational state exists.
- Use `just run` for a live native smoke, focused trials while iterating, and the
  full configured eval only for milestone or release gates. Never modify a
  benchmark task or verifier to make Nanocodex pass. Inspect exact JSONL,
  trajectories, verifier output, and retained evidence for concrete claims.

## Experimental eval state

- Eval ledgers, coordinator state, retained artifacts, and their schemas are
  mutable development state, not compatibility boundaries.
- Keep SQLite `user_version = 1`; it is a current-format marker, not migration
  history. On every schema change, stop the scoped run, directly mutate the
  canonical database in place to the one new layout, update the single current
  schema definition, and restart. Preserve completed rows only when the direct
  transformation is useful and obvious; otherwise recreate or reseed them.
- Never add old-schema readers, migration ladders, version-specific branches,
  dual writes, fallback runtimes, or compatibility shims unless the user
  explicitly asks. Never return to an older binary because current code rejects
  experimental state.
- Do not make backups or pause iteration to preserve experimental state unless
  the user explicitly requests one. Once the canonical database is migrated,
  delete obsolete schema and migration code immediately.

## Codex reference

- Use the local checkout at `~/github/openai/codex/codex-rs` before making an
  architecture or behavior claim about Codex. Do not browse the web or invoke
  OpenAI documentation tooling unless the user explicitly asks.
- Codex is evidence, not an API requirement. Copy relevant invariants and
  operational behavior while keeping Nanocodex's smaller public surface.
- The reviewed upstream checkpoint is
  `openai/codex@7ada37a15e1f6aa84f83b4b9410f9d29e66fefe4`. A parity review must
  inspect every later commit, classify it as port/evaluate/defer/out-of-scope,
  and cite adopted behavior before advancing the checkpoint.

## Workspace boundaries

- `nanocodex-oai-api` owns the complete OpenAI boundary: dependency-light
  prompts/events/wire types, the managed context state machine, persistent
  transports, typed retry policy, telemetry, and generic Tower client.
- `nanocodex-tools` owns code mode, built-in tools, the heterogeneous registry,
  MCP transports and discovery, deferred tool search, and remote dispatch. MCP
  is always available on native targets.
- `nanocodex-agent` owns the private driver, lifecycle policy, branching,
  snapshots, Codex rollouts, and ergonomic agent builders.
- `nanocodex-subagents` is an optional extension above `nanocodex-agent`. It
  owns task-tree lifecycle, structured child results, directed communication,
  and the canonical subagent tools; `nanocodex-agent` must not depend on it.
- `nanocodex` is an Alloy-style facade containing reexports, named component
  modules, and a small prelude. It contains no runtime implementation.
- Keep facade imports canonical: common types may appear at the crate root and
  detailed APIs under their owning `agent`, `oai`, or `tools` module. Do not add
  sibling convenience reexports.
- `nanocodex-tools/macros` contains the `nanocodex-tools-macros` package that
  implements `#[tool]`. Keep the executable under `bin/nanocodex`; do not move
  CLI behavior into the library.
- The unpublished experimental `nanocodex-egress` crate owns the authenticated
  loopback HTTP(S) proxy, ephemeral CA, bounded forwarding, and ordered outbound
  layer seam. Provider and payment behavior stays in the consuming application.
- Tempo payment policy and `NanoUSD` support stay under `bin/`; public
  `nanocodex-*` library crates must not depend on them.
- The unpublished experimental `nanocodex-vm` crate owns the complete VM
  boundary: the audited libkrun interface, VM/process configuration, gvproxy
  and provider-neutral egress, OCI/Dockerfile image preparation, and retained
  host/guest workspace tools. Its guest reuses the canonical local
  workspace-tool contracts rather than introducing a second tool runtime.
- Each lower crate must remain useful without importing the higher orchestration
  crate. Avoid circular concepts and leaky socket/runtime types.
- `scripts/check-crate-boundaries.sh` is the executable dependency policy.
  Update its snapshot only for a deliberate architecture change.

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
- Turn cancellation stops turn-owned Code Mode work, invalidates Code Mode
  admissions already waiting at its boundary, and preserves shell processes
  already retained by the runtime. Explicit shell termination and runtime
  shutdown must terminate subprocess groups and descendants. Runtime shutdown
  holds Code Mode admission closed while draining shells.

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
- Tracing is a full-fidelity record of all data observed by the agent lifecycle.
  Preserve complete prompts and instructions, model requests and responses,
  API-visible reasoning content and summaries, opaque encrypted reasoning
  payloads, tool arguments and results, steering, cancellations, and lifecycle
  events in their original order. Do not redact, filter, truncate, or omit
  observed values based on their content or sensitivity.
- Put large ordered content in span events rather than searchable span
  attributes. Keep attributes structural: identity, lineage, ordering, sizes,
  status, timing, token usage, cache behavior, and routing metadata.
- Follow init4-style span hygiene: a root span represents one bounded unit of
  work, not a long-lived driver or session. Correlate sequential turn roots with
  session and lineage attributes. Propagate explicit parents with the work sent
  across channels, instrument futures before spawning them, and let concurrent
  child work appear as overlapping sibling branches.
- Telemetry must observe the normal runtime data path rather than performing
  additional configuration or environment reads solely to manufacture trace
  content. Operators must treat the trace backend as a complete copy of agent
  conversations and tool activity and apply matching access and retention.

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
- At an explicit final handoff or release gate, run the smallest relevant set of
  rustfmt, warnings-denied Clippy, focused tests, and public-example checks.
  Benchmark performance claims on representative retained traces, not synthetic
  microbenchmarks alone.

## TUI performance

- Develop the Ratatui consumer against replayed, representative workloads, not
  visual intuition alone. Treat retained Codex rollout traces and the longest
  available Amp thread exports as the primary corpus. Codex traces provide
  event ordering, streaming bursts, tool/reasoning interleaving, and timing;
  Amp threads provide mature interactive transcript shapes, long messages, and
  long-session behavior. Discover candidates with `amp threads list
  --include-archived --json` and read selected payloads with `amp threads export
  <thread-id>`.
- Keep the retained trace corpus outside Git. Commit only deterministic derived
  fixtures or structural workload summaries that are explicitly intended to be
  source-controlled test data.
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
