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
cargo test -p nanocodex-tools --lib code_mode
```

These tests establish the covered cases, not complete V8 equivalence. Engine
resource limits, global-object details, native audio decoding, dynamic imports,
and immediate notification injection into an active model turn have separate
compatibility boundaries. Browser Worker transport tests are not a claim of a
live browser deployment test. Hand executor capacity/admission is a separate
host scheduling issue; this change does not relax parallel-safety restrictions.

The portable evaluators still execute source inside an async-function wrapper,
whereas Codex uses a V8 ES module. For example, a top-level `return` is accepted
here and rejected by Codex's module parser. Helper lexical bindings do not imply
that `globalThis.text` or `globalThis.tools` is available. This patch does not
replace the module loader or claim global-object equivalence. Optional console
bridging and host capabilities remain Nanocodex-specific.
