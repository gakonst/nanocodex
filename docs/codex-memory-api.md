# Codex memories API adapter

The public tools use pinned Codex `36430b36881cf5c289cb48e671cfc9e8b542ae7b`
namespace `memories` with `list`, `read`, `search`, and `add_ad_hoc_note`.
Native and JavaScript carry identical input and output schemas for those four
tools in their independent Cargo/npm packages; an equality test checks both
consumed copies. No other tool catalog is included.
The native CLI calls `/v1/memories/{method}` through the managed API or its
Connect grant origin; the Connect proxy checks current read/write capabilities
and forwards the unchanged Codex arguments. The managed agent
uses the same file adapter. The old `memory` model tool is no longer registered;
old configuration entries select these compatibility tools plus the managed
[Markdown memory tools](workers-markdown-memory.md), all under `memories__*`.
The same `read` and `search` operations cover canonical Markdown, using Codex
arguments and result shapes. Markdown adds only `write` and `status`; writes need
an operation, path, and content, with storage bookkeeping handled by the host.
An empty tool list remains empty. The four pinned Codex schemas remain unchanged.

The Codex file API covers canonical Markdown and append-only ad-hoc notes.
Direct account sessions have a private root and a read-only `team/` view of
shared memory. Connect sessions can access only their authorized team root.
Model arguments cannot select another user or organization. Every call checks
current capabilities, and subagents cannot append notes.

New ad-hoc notes use atomic create-if-absent storage under
`extensions/ad_hoc/notes/`, preserving their verbatim text and duplicate-filename
behavior. Canonical Markdown is editable through `memories__write` without
changing the four baseline Codex schemas.

The versioned legacy memory store, its CRUD endpoints/SDK methods, and
`legacy/<id>-v<version>.md` projection are retired. Activation drops legacy-only
fact tables and clears fact-bearing prepared snapshots. Canonical Markdown,
append-only notes, conversation history, and background Markdown personalization
are preserved. Previously delivered conversation text cannot be erased.

Validation includes canonical Markdown reads, shared-team visibility, private-note
persistence, duplicate rejection, denied writes, cross-user isolation, input
validation, search/paging, and native/JS schema equality. The managed HTTP tests
load a prebuilt WASM module only to satisfy the worker module dependency; they exercise the new JavaScript adapter and actual SQLite
Durable Objects, not the WASM execution loop. Full semantic equivalence of every
filesystem/search edge case is not established by this focused suite.

Verify the consumed declarations against an external checkout at the pinned revision:

```sh
python3 scripts/codex-parity/memory.py /path/to/codex
```

The checker compiles upstream constructors without copying the full tool catalog.

Run the deterministic contract and behavior evaluation (with workspace dependencies installed):

```sh
bash scripts/codex-parity/memory-eval.sh /path/to/codex-at-pinned-revision
```

This compiles the pinned upstream schema constructors, checks the JavaScript file
adapter, runs the native managed-memory proxy and voice context tests, rebuilds
the browser WASM for voice transport checks, and exercises managed Markdown
memory, prepared text/voice personalization, and independent history projection
in the Worker/SQLite runtime. Regression cases cover Unicode
normalization and ordering, line endings, result projection, and exclusion of
consolidation reports from search. This evaluates tool compatibility and storage
correctness; it does not measure a model's long-term recall quality.

On macOS, also verify the native Apple consumer against a freshly built Rust core:

```sh
pnpm build:voice-core
swift test --package-path apple/NanocodexVoice
```

Voice regressions check personal and shared Markdown before and after the control channel
opens, stop/replacement boundaries, background-only delivery, and large escaped
snapshots. Stalled background fetch/body reads must not delay admission, and
optional notification failures must not fail the live event stream. Recovery must
continue while history projection is unresolved. Live provider tests remain
separate from these deterministic checks.
