# Code Mode execution parity

The upstream reference is OpenAI Codex commit
`506a328dab110591d3c1449a15217596e7e9cd61` (2026-09-22).
Nanocodex retains QuickJS in the native Rust and managed Worker runtimes and a
per-cell browser Worker evaluator. Node's evaluator is also covered by the same
JavaScript conformance cases.

## Reuse and compatibility boundary

`scripts/codex-parity/native-behavior.py` compiles and executes the pinned
upstream V8 value/audio helpers, Rust wait parser, and Rust truncation code
**directly from an external clean Codex checkout**. It does not reimplement the
expected results. `native-behavior.json` is the bounded output corpus consumed
by both Rust and JavaScript tests.

```sh
python3 scripts/codex-parity/native-behavior.py /path/to/pinned/codex
# --write regenerates the checked-in fixture; --target-dir can reuse a Cargo cache.
```

Codex's production runtime embeds native V8 and depends on its runtime/protocol
crates. It cannot run as a browser or Cloudflare Worker dependency. We reuse its
actual helper code as the differential oracle, while retaining portable runtime
adapters. The full upstream tree is not vendored. Pin changes should regenerate
and review the oracle rather than silently updating expected outputs.

## Backend tool exposure

Every shipped CLI, SDK and managed session uses Code Mode, including Claude
and Responses (GPT, Codex and gateway) backends. Only `exec`
and `wait` are model-visible; nested discovery tools find deferred capabilities
inside a cell, and workspace and subagent tools use the same nested-call path.
A direct action call is rejected before its handler runs. Restored sessions use
the strict catalog too. Completed receipts survive upgrades, while unresolved
old direct effects retain an explicit warning to reconcile their outcome before
retrying. Shared evaluators schedule each agent session independently so a
parent can wait for a child running its own cell. SDK embedders use
`toolMode: "code-only"` (the default); `code` and `direct` are rejected.
See [tool catalogs by runtime](TOOL_RUNTIMES.md) for the nested capabilities.

## Shared contract

- Tool calls are asynchronous and preserve the registered tool's result or
  rejection. A string rejection must stay a string across the evaluator boundary.
- Finishing, throwing from, or exiting the top-level script ends the cell;
  unresolved promises and timers do not keep it alive. We do not add an implicit
  `Promise.all` or retry side effects when a script fails.
- Completed nested results survive a later script failure. Cancellation cannot
  rewrite an already observed completion.
- Stored values are JSON snapshots; successful completion owns commits rather
  than an interrupted evaluation. Existing tests also cover failed-script write
  commits, concurrent write sets, wait budgets, output accounting and yielding.
- Names exposed by `ALL_TOOLS` reflect the admitted catalog. A standalone host
  tool need not be a callable nested tool. No missing name can expand authority.

## Warm discovery and admission

Every Code Mode cell pins its tool catalog and handlers at admission. Dynamic
providers are enumerated and resolved again for each new admission; a newly
published catalog or replaced handler is visible to the next cell, not to an
already-running cell.

QuickJS transfers the admitted `ALL_TOOLS` catalog as JSON data into each fresh
context instead of compiling schema object literals as guest source. Metadata is
parsed independently per cell: schema keys and descriptions remain data, guest
edits do not persist, and omitted definitions remain undefined. The outer catalog
is frozen as before. This changes neither callable bindings nor authorization,
journal decisions, cancellation or once-only effect receipts.

Factory-owned `toolMapSource` entries can reuse contract normalization within
one router when the complete definition-array JSON is unchanged. The public
array is still read and serialized every time, so array edits and contract
collisions are not hidden. This cache contains schema/handler-map preparation,
not Hand authorization, grants, leases, effect decisions or receipts. Provider
sources are never eligible, including providers that return identical schema
bytes while replacing their handlers.

`js/managed/test/code-mode-warm-latency-journey.test.mjs` measures a running
managed thread after `tool_search`, through actual WASM/QuickJS, SQLite, public
HTTP, reverse Hand WebSocket and native shell calls. It separates first capture,
subsequent-cell capture and already-admitted warm nested awaits. The external
model and authentication seed are synthetic; timings are local rather than WAN.
`js/nanocodex/test/tool-discovery-warm-journey.test.mjs` covers catalog/handler
replacement, mutable factory arrays, pinned cells and collision rollback through
actual QuickJS cells and application-defined disk effects. Catalog CPU gains
must not be presented as established end-to-end or steady nested-call gains.

## Explicit Nanocodex extensions

Codex installs known functions on a plain tools object. Nanocodex adds a guarded
facade: calling an absent string name produces a rejected Promise containing
`TOOL_NOT_AVAILABLE`, the tool name, and catalog/direct-invocation guidance.
This allows `Promise.allSettled([tools.a(), tools.b(), tools.missing()])` to
preserve the first two results. It is a local rejection, not a host dispatch.
The target remains frozen, has no prototype, and enumerates only registered
names. An absent `then` or Symbol property stays absent to avoid accidental
thenable/iteration behavior. Explicitly registered names remain callable.

This factory is shared across evaluator implementations; package-local assets
keep Rust and npm releases self-contained.

Nanocodex also records terminal receipts for every started nested call. When the
cell ends before a result arrives, the receipt says `CODE_MODE_CALL_INTERRUPTED`
and `outcome: "unknown"`. This describes lack of a result, not rollback or proof
that execution stopped. Late settlement cannot mutate that receipt or produce a
second completion. The host remains responsible for resumable process handles.

Hands, connector routing, notifications, tool discovery, and these receipts are
host extensions. They must not silently alter the upstream execution contract.

## Regression coverage

JavaScript `code-tools-conformance.test.mjs` and
`code-mode-lifecycle-parity.test.mjs` exercise Node, QuickJS and the browser Worker
adapter (using a real Node Worker transport). Rust tests exercise the embedded
QuickJS evaluator and native tool host. Cases include the screenshot's two
parallel calls plus unavailable third tool, prototype/then handling, input
serialization errors, raw rejection values, root return/throw/exit, late results,
and completion accounting. Existing suites cover cancellation, yielded waits,
storage, multimodal helpers and generated browser bundles.

```sh
node --test js/nanocodex/test/code-{runtime,mode-parity,mode-upstream,tools-conformance,mode-lifecycle-parity}.test.mjs
node --test js/nanocodex/test/{quickjs-evaluator,worker-evaluator,quickjs-bundle,code-mode-browser-bundle,browser-compiler-worker}.test.mjs
cargo test -p nanocodex-oai-tools --lib code_mode
```

These tests establish the covered cases, not complete V8 equivalence. Engine
resource limits, global-object details, native audio decoding, dynamic imports,
and immediate notification injection into an active model turn have separate
compatibility boundaries. Browser Worker transport tests are not a claim of a
live browser deployment test. Calls dispatch concurrently regardless of
parallel-safety metadata. Await dependencies explicitly and handle conflicts
returned by providers.

The portable evaluators still execute source inside an async-function wrapper,
whereas Codex uses a V8 ES module. For example, a top-level `return` is accepted
here and rejected by Codex's module parser. Helper lexical bindings do not imply
that `globalThis.text` or `globalThis.tools` is available. This patch does not
replace the module loader or claim global-object equivalence. Optional console
bridging and host capabilities remain Nanocodex-specific.

## Observer-only instant steering

The observer-preemption contract follows the `execute`/`wait` distinction in
OpenAI Codex `60947e234156ac12bdb7fba2477d3965f166bd34`:
[session protocol](https://github.com/openai/codex/blob/60947e234156ac12bdb7fba2477d3965f166bd34/codex-rs/code-mode-protocol/src/session.rs#L155-L170).
This is **not** full upstream `instant_interrupt` parity.

Steering-triggered observation preemption is opt-in and disabled by default:

- Rust: `Nanocodex::builder(openai).instant_tool_steering(true)`.
- Node/browser SDK: `Agent.create({ ..., instantToolSteering: true })`.
- Durable Cloudflare SDK: `CloudflareAgent.create(owner, { instantToolSteering: true })`.
- Managed SDK/HTTP: create the agent with
  `configuration: { instant_tool_steering: true }`. This is retained in its
  configuration, validated as a boolean at admission, and reapplied on recovery.
  The runtime setting is not a model provider feature flag.

Accepted steering (including new input routed to an active turn) notifies the
agent tool boundary. While the same pinned tool future is in flight, that
boundary asks the runtime to yield current `exec`/`wait` observers. The normal
result includes accumulated output, a live cell ID and its original call
identity. Evaluation, nested tools, promises and effects continue. Later
`wait` observes that same cell; it does not rerun the source. Completed nested
results and metadata are retained through the existing cell observation queue.
New observations do not inherit an earlier signal. Native notifications are
turn-scoped; JS host notifications target active observations in the bound
session/current turn. A withdrawn queued message is never injected. If withdrawal is processed
before the tool boundary sees pending input, no observer wake is requested;
withdrawal after an observer yielded cannot reverse that observation.

Embedders can explicitly call `ToolRuntimeControl::preempt_turn()`. Embedded
hosts implement `CodeModeHost::preempt_turn(session_id)`; the default is a
conservative no-op for existing custom hosts. The shipped Node/browser WASM
bridge forwards it to `nanocodexHost.preemptCodeTurn(sessionId)`. JS hosts also
expose `preemptCode(sessionId, callId)` for an exact foreground observation.
These controls are host APIs, not new model tools or cancellation authority.

A turn can complete while one of its cells is still running (the model answered
after a yield without calling `wait`). The Codex harness then relays that
cell's nested tool starts and completions as they happen, so every started call
still reaches a terminal `tool.result`, possibly after `run.completed`. A later
`wait` takes over the cell and does not repeat updates the relay delivered.
The Claude harness instead drains its cells at every turn end.

`wait(terminate: true)`, cancellation, turn teardown and host shutdown remain
separate terminal controls. Preemption never interrupts an evaluator or an
external tool. In particular, an uncertain write is not rolled back by a yield
and is not permission to retry the write. Cells remain in-memory runtime
resources: this does not make them recoverable after a process/Worker restart.

Model streams still finish at their existing committed-response boundary;
assistant prefixes, continuation IDs and tool history are not rewritten by
this option. A native QuickJS host thread can be busy while its Rust observer
yields. In JS, a non-yielding guest on the host's own event loop prevents any
control delivery; use a separate Worker evaluator for reachable observation
preemption. Killing or interrupting that guest would drop pending promises and
is deliberately not used as a substitute.
