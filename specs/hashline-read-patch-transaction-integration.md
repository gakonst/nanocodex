# Integrate full Hashline read, patch, and recoverable transactions

- Branch: `feature/hashline_transaction`
- Status: Deterministic native baseline passes; transaction semantics hardening
  is active, and live model, Harbor, and configured evaluation gates await an
  API key
- Owner(s): Nanocodex maintainers and Codex
- Created: 2026-07-20
- Last Updated: 2026-07-20
- Links: [Nanocodex plan](../PLAN.md)

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`,
`Decision Log`, and `Outcomes & Retrospective` current as research,
implementation, validation, and review proceed. When the next milestone is
clear, continue to it and update this spec instead of asking for generic next
steps.

The root `AGENTS.md` is authoritative. In particular, implementation must use
the sibling Codex checkout at `../codex/codex-rs` for source claims and must
follow the active work in `PLAN.md` in order. The user confirmed that checkout
as the canonical local source on 2026-07-20. Milestone 0 must classify its
source behavior before code is copied or adapted.

## Purpose / Big Picture

Nanocodex currently exposes `apply_patch` as its dedicated structured editor.
It parses a freeform patch and applies creates, updates, moves, and deletes with
ordinary filesystem calls. It does not give the model stable line anchors,
bind ordinary edits to a recent read, preview a typed multi-file plan, or retain
recovery evidence after an interrupted multi-file commit.

The intended product is the full useful Hashline workflow, not a transaction-
only port. After this work a native Nanocodex agent can:

1. call `hashline__read` to read a bounded text range carrying compact file and
   line hashes plus an exact-byte SHA-256 digest;
2. call `hashline__find_block` when a block edit needs a reproducible block
   anchor and bounded excerpt;
3. call `hashline__patch` for routine hash-anchored creates, updates, deletes,
   moves, line edits, block edits, dry runs, and sectioned multi-file changes;
4. call `hashline__transaction` to preview, immediately commit, or commit only
   the exact previously previewed create/update/delete/move plan with durable
   rollback and restart recovery on proven native filesystems.

`hashline__find_block` is part of the read/patch workflow rather than a fourth
product area: full block-patch functionality needs a tool that produces the
block anchors consumed by `SWAP.BLK`, `DEL.BLK`, and block insert operations.

Hashline patch and Hashline transaction deliberately have different operational
contracts. Patch is the ergonomic, portable, routine editing surface. It
validates all known file, line, range, and block guards before its first write
and uses guarded per-file mutations, but it does not promise crash recovery or
simultaneous visibility across files. Transaction is the high-integrity batch
surface. It binds exact bytes and filesystem evidence into a deterministic plan,
stages durable before/after evidence, journals progress, and recovers after a
process interruption. Both use the same text model, hashes, path policy, limits,
prepared mutation types, and output conventions; they are not independent
editor implementations.

There are no separate `hashline__write`, `hashline__remove_file`, or
`hashline__rename_file` tools in Nanocodex. Complete-file creation/overwrite,
`REM`, and `MV` are first-class `hashline__patch` operations, while typed create,
delete, and move are first-class transaction mutations. Omitting redundant tool
names keeps the model-visible surface small without omitting those behaviors.

The migration is staged. `apply_patch` remains available while the complete
Hashline family is implemented and compared on the same branch. It is removed
only after read-to-patch anchor round trips, complete patch file-operation
parity, transaction recovery, exact Code Mode schemas, native smoke tests, and
focused model trajectories all pass. The final native product has Hashline as
its only dedicated structured editing family; there is no feature flag or
hidden `apply_patch` fallback.

`exec_command` and the local Node.js Code Mode host remain intentionally able to
write files. “Hashline only” describes the dedicated schema-described editing
surface, not a filesystem security boundary. Constraining shell and Node writes
would require a separate sandbox design and is out of scope.

Success means:

- Code Mode advertises `hashline__read`, `hashline__find_block`,
  `hashline__patch`, and `hashline__transaction` with exact closed schemas, and
  no longer advertises `apply_patch` after the migration gate.
- A bounded read produces line anchors accepted verbatim by patch, a compact
  file guard accepted by section headers, and an `exactDigest` accepted verbatim
  by transaction expected-file inputs.
- Hashline patch supports its complete grammar: single/range swaps and deletes,
  head/tail/before/after inserts, block swaps/deletes/inserts, empty and
  multi-file creation, sectioned existing-file edits, `REM`, `MV` with optional
  line edits, dry run, abort markers, and documented payload forms.
- Stale file, line, range, block, exact-byte, and preview-plan evidence fails
  without beginning a mutation. Hash mismatches tell the model exactly what to
  reread and rebuild.
- Patch validates a complete request before the first write and reports its
  weaker crash boundary honestly. Transaction fault/restart tests demonstrate
  all-before, all-after, or bounded evidence-preserving recovery.
- Read and routine patch work on supported native Nanocodex platforms. Durable
  transaction commits initially support proven Linux ext-family case-sensitive
  filesystems and tmpfs; other transaction capabilities fail closed with a
  typed `Unsupported` result and never fall back to patch.
- Successful writes return refreshed, byte-bounded anchors around changed
  regions. No tool duplicates complete line content in both flat and structured
  output, emits secrets through tracing, or injects full unbounded files into
  model context.
- The work adds no unsafe code, does not loosen `unsafe_code = "forbid"`, and
  does not introduce provider/environment abstractions, an app server, or a
  second agent lifecycle.
- Focused tests, workspace gates, public examples, a native CLI smoke, focused
  Harbor trials, and the configured milestone `just eval` pass with exact JSONL,
  trajectories, verifier output, cost, latency, and tool adoption inspected.

## Progress

- [x] (2026-07-20) Inspect current Nanocodex tool registration, Code Mode schema
  rendering, `apply_patch` behavior, crate ownership, and validation surfaces.
- [x] (2026-07-20) Replace the transaction-only product plan with this full
  Hashline read/patch/transaction contract based on the stated product intent.
- [x] (2026-07-20) Confirm the required sibling
  `../codex/codex-rs` checkout, inspect its divergent relationship to the pinned
  checkpoint, classify the relevant Hashline path history, and record the
  adopted source manifest before implementation.
- [ ] Finish the reconstructed frozen `apply_patch` live baseline. The exact
  pre-Hashline model schema is retained from `9ed472b`, and the pinned
  `terminal-bench/polyglot-c-py` holdout was selected without candidate output,
  but no usable `OPENAI_API_KEY` is available for the historical model run.
- [x] (2026-07-20) Implement shared Hashline hashing, exact observation,
  bounded read, block discovery, patch parsing/application, and deterministic
  tests.
- [x] (2026-07-20) Implement the portable patch executor and expose read,
  block, and patch through Code Mode alongside `apply_patch`.
- [x] (2026-07-20) Implement typed preview/commit/commitPreviewed planning,
  exact-digest and line-anchor validation, bounded journals, all-before restart
  recovery, Linux ext-family/tmpfs negotiation, unsupported-platform failure,
  and expose the transaction tool.
- [x] (2026-07-20) Complete safe descriptor-relative transaction traversal,
  canonical inode-ordered participating-parent coordination, root-identity plan
  binding, casefold-aware capability negotiation, evidence-preserving recovery,
  and subprocess fault injection across every durable create/update/delete/move
  transition. Overlapping cross-process parents conflict while disjoint parents
  proceed independently.
- [ ] Preserve executable and permission metadata across commit, rollback, move,
  and restart recovery.
- [ ] Replace manifest-embedded before/after file bodies with bounded owner-only
  staged and backup artifacts reserved before visible mutation.
- [ ] Add explicit preparation, commit, rollback, recovery-required, cleanup,
  and completion states; perform immediate reverse rollback on live failures
  and distinguish committed recovery from rollback recovery.
- [ ] Isolate malformed or externally disturbed recovery entries, reject
  duplicate transaction reservations, bind every artifact to root and
  transaction identity, and retain bounded terminal evidence through cleanup.
- [ ] Move blocking filesystem work off async executor threads and evaluate
  waiting versus immediate-conflict locks with an embedded consumer.
- [x] (2026-07-20) Remove `apply_patch` after deterministic replacement tests;
  delete its parser, grammar, handler, registrations, and tests.
- [x] (2026-07-20) Run an interactive consumer E2E while implementing and
  validating TUI modifier editing in commit `f6afb79`. Hashline read supplied
  anchors and exact digests; two routine patch attempts failed before writes;
  the equivalent typed transaction committed on its first well-shaped call;
  rustfmt, focused tests, warnings-denied Clippy, and a clean worktree followed.
  A follow-up update to this spec then used a properly formed routine patch for
  both dry-run and commit successfully.
- [ ] Close the interactive usability findings before the milestone gate: emit
  one non-duplicated read representation, preserve complete property guidance
  through the exact model-visible declaration path, add field- and
  dialect-specific bounded diagnostics, and explain `commitPreviewed`
  mutation resubmission explicitly.
- [ ] Add an edit/format/reread regression trajectory proving that external
  formatting makes prior anchors stale, stale mutation fails without a write,
  and a bounded reread supplies fresh evidence for the next edit.
- [ ] Run focused, workspace, example, native smoke, recovery, Harbor, and full
  milestone validation; inspect and record exact retained evidence here.
- [ ] Complete outcomes, adopted source provenance, residual platform risks,
  PR, commit, rollback, and rollout records.

## Surprises & Discoveries

- Observation: the original draft contradicted the intended product. It
  explicitly omitted Hashline read, patch, block discovery, write, remove, and
  rename behavior and made transactions the only model-visible Hashline tool.
  Evidence: Git commit `3f2f58b` and the replaced
  `specs/hashline-transaction-only-integration.md`.

- Observation: Nanocodex currently has three file-mutation paths.
  `apply_patch` is the dedicated structured editor, `exec_command` can run
  arbitrary mutating commands, and Code Mode executes ordinary Node.js with
  filesystem access from the workspace.
  Evidence: `crates/nanocodex-tools/src/runtime.rs`,
  `crates/nanocodex-tools/src/apply_patch/mod.rs`,
  `crates/nanocodex-tools/src/shell/tool.rs`, and
  `crates/nanocodex-tools/src/code_mode/mod.rs`.

- Observation: Nanocodex sends only top-level `exec` and `wait` definitions to
  the Responses API. Every built-in editor is a nested Code Mode tool rendered
  from `ToolDefinition`, and flat registry names such as
  `hashline__transaction` become valid TypeScript properties without adding a
  namespace abstraction.
  Evidence: `crates/nanocodex-tools/src/runtime.rs`,
  `crates/nanocodex-tools/src/code_mode/spec.rs`, and
  `crates/nanocodex-tools/src/code_mode/description.rs`.

- Observation: full block patch behavior needs an anchor producer beyond a
  simple line-range read. A block anchor combines a line anchor with a bounded
  block guard and must round-trip through block discovery before a destructive
  block edit.
  Evidence: canonical sibling-checkout files
  `../codex/codex-rs/core/src/tools/handlers/hashline_block.rs` and
  `hashline_patch_parser.rs`.

- Observation: compact Hashline guards and transaction exact-byte evidence
  serve different purposes. Compact 4-hex line and 8-hex logical file/block
  hashes are efficient editing anchors but are not cryptographic collision
  boundaries. Transactions require a 64-hex SHA-256 digest of exact bytes.
  A read tool that emits only compact hashes would still force the model to use
  shell or Node before every transaction.
  Evidence: canonical sibling-checkout Hashline hash and transaction types;
  the Nanocodex contract therefore adds `exactDigest` to read output.

- Observation: complete patch semantics include file operations. Sectioned
  patch input can create multiple files, remove a file, and move a file while
  also applying line edits, so standalone write/remove/rename tools would be
  redundant in Nanocodex.
  Evidence: canonical sibling-checkout patch parser, section parser, and
  tests; canonical source revalidation remains pending.

- Observation: routine patch and durable transaction cannot honestly share one
  platform promise. A portable patch executor can retain normal native editing
  coverage, while the durable transaction filesystem capability depends on
  stronger descriptor-relative, locking, identity, sync, and recovery
  semantics. The tool descriptions and results must state that distinction.

- Observation: the user confirmed `../codex/codex-rs` as the canonical local
  checkout. Its clean `main` HEAD is
  `eff2c761e2bf3c644730edf795a8055b00818e92`; the historical pinned checkpoint
  is not its ancestor, so provenance review records the divergence explicitly
  instead of describing HEAD as a linear advancement of that checkpoint.

- Observation: canonical behavior was classified by relevant Hashline path
  history because the confirmed checkout diverges from the historical pinned
  checkpoint. Core read/block/patch fixes through `b7426c1b7d` are port
  candidates; transaction planner, exact-edit, journal, recovery, and native
  capability commits from `98ed2bb3fb` through `eb44f13a60` are evaluate/adopt
  evidence; Codex environment/RPC routing is out of scope; standalone Hashline
  write/remove/rename tools are deliberately deferred in favor of patch file
  operations. Evidence: clean canonical `main` HEAD
  `eff2c761e2bf3c644730edf795a8055b00818e92` and path-scoped `git log`.

- Observation: the first Nanocodex transaction candidate intentionally fell
  short of the spec's strongest durability claim. It retained and synced
  bounded exact before evidence and recovered pending journals to all-before,
  but used ambient path APIs and lacked cross-runtime coordination and
  kill-at-every-transition evidence. Commit `0aff4fa` closed those
  descriptor-race, coordination, sync-ordering, and interruption-test gaps.
  The remaining differences are metadata preservation, body-free structural
  manifests backed by staged artifacts, explicit commit/rollback states,
  immediate live rollback, isolated recovery, and async-runtime integration.

- Observation: the interactive consumer trajectory exposed a routine-patch
  adoption failure before any filesystem mutation. An `apply_patch`-shaped
  program first returned `existing-file Hashline patches require a
  [path]#HASH section header`; after that header was added, the next diagnostic
  identified the incompatible sentinel and named the Hashline operations. The
  required top-level `path` plus repeated section path/header also made the
  relationship between the default target and patch sections non-obvious.
  Evidence: the retained 2026-07-20 Codex trajectory producing consumer commit
  `f6afb79`.

- Observation: in the same trajectory, the typed transaction schema was more
  discoverable than the routine patch language. A one-file `replaceLines`
  commit and a later anchored `insertBefore` commit both succeeded on their
  first well-shaped calls and returned old/new exact digests, bounded previews,
  outcome, plan digest, and transaction identity. This is positive transaction
  evidence but negative routine-patch adoption evidence, not a model-evaluation
  gate.
  A subsequent properly formed patch against this spec also passed dry-run and
  committed on its first calls after the grammar was available.

- Observation: the observed read result duplicated every retained line in both
  compact anchored `content` and a structured `lines` array. A requested
  220-line range was bounded to 173 lines with `next_start_line`, but the
  duplicated representation still dominated the tool result. This conflicts
  with the stated no-duplicate-output success criterion above.

- Observation: the orchestrator-visible Hashline declarations in this session
  omitted the per-property path and patch-program descriptions present in
  Nanocodex's `ToolDefinition` schemas. Absolute `path` and `root` inputs
  consequently produced the generic `Hashline paths must be non-empty and
  workspace-relative` diagnostic without identifying the failing field.
  Failures surfaced as untyped `Script error` strings, and one rejected call
  inside `Promise.all` hid successful sibling results. The last behavior
  belongs to the orchestration wrapper rather than the Hashline core.

- Observation: `commitPreviewed` names only `expectedPlanDigest` in its action
  variant while the enclosing transaction request still requires `mutations`.
  The digest safely binds the rebuilt plan, but the short action description
  did not make exact mutation resubmission apparent.

- Observation: running rustfmt after a successful edit correctly changes exact
  bytes, file digest, and affected anchors. A later anchored edit therefore
  requires a reread. This is expected stale-evidence behavior, but it is a
  frequent edit/format/edit path and was not represented in the interactive
  guidance.

- Observation: Rustix exposes the required descriptor-relative Linux calls,
  `openat2` resolution policy, filesystem inspection, inode flags, locking, and
  sync operations through safe APIs. The completed capability therefore keeps
  `unsafe_code = "forbid"` without weakening the selected filesystem contract.
  Evidence: implementation commit `0aff4fa` and the packaged
  `crates/nanocodex-tools/NOTICE`.

- Observation: the local checkout has no repository `.env`, no exported
  `OPENAI_API_KEY`, and no API-key value in the local Codex auth record. Offline
  Harbor setup and all deterministic repository gates work, but a live native
  smoke, historical baseline, focused trials, and `just eval` cannot make an
  authenticated model call until the user supplies that credential.

## Decision Log

- Decision: deliver full Hashline read, block-anchor, patch, and transaction
  behavior as one coherent editing family.
  Rationale: read supplies evidence, patch serves ordinary edits, and
  transaction serves reviewed/recoverable batches. Any one of those in
  isolation leaves a broken model workflow.
  Date/Author: 2026-07-20 / user and Codex

- Decision: expose flat tools named `hashline__read`,
  `hashline__find_block`, `hashline__patch`, and
  `hashline__transaction`.
  Rationale: Nanocodex uses a flat nested-tool registry and Code Mode declaration
  surface. `find_block` is required support for full block-patch semantics.
  Date/Author: 2026-07-20 / Codex

- Decision: do not expose standalone `hashline__write`,
  `hashline__remove_file`, or `hashline__rename_file` tools.
  Rationale: patch already owns complete-file create/overwrite, `REM`, and `MV`,
  and transaction owns typed create/delete/move. This preserves functionality
  without teaching the model overlapping mutation APIs.
  Date/Author: 2026-07-20 / Codex

- Decision: preserve the complete Hashline patch grammar rather than reducing
  patch to `replaceAll`.
  Rationale: compact line and block operations are the ergonomic reason to port
  Hashline read and patch in the first place; transactions remain available for
  explicit typed after-images and high-integrity batches.
  Date/Author: 2026-07-20 / user and Codex

- Decision: add `exactDigest` to read and block outputs while retaining compact
  logical file, line, and block hashes.
  Rationale: patch can copy compact anchors verbatim, and transaction can copy
  exact-byte SHA-256 evidence verbatim, with no shell hash detour.
  Date/Author: 2026-07-20 / Codex

- Decision: share a private exact-text document model, hashing functions, path
  validation, hard limits, prepared mutation types, and preview formatting
  between patch and transaction, but keep separate portable-patch and durable-
  transaction executors.
  Rationale: ordinary patch must remain portable and ergonomic, while durable
  transaction commits need stronger platform semantics. Sharing the pure plan
  avoids divergent edit results without pretending the executors have equal
  crash guarantees.
  Date/Author: 2026-07-20 / Codex

- Decision: compact file and block guards use an 8-hex logical-text hash, line
  guards use a 4-hex line hash, and transaction exact evidence and plan digests
  use 64-character lowercase SHA-256.
  Rationale: this preserves compact Hashline editing while making the
  cryptographic versus non-cryptographic boundaries explicit. Exact algorithm
  and normalization fixtures are frozen before model registration.
  Date/Author: 2026-07-20 / Codex

- Decision: patch validates every target and compiles every after-image before
  the first write. It uses guarded per-file replacement and best-effort rollback
  for an in-process failure, but does not claim durable multi-file atomicity or
  restart recovery.
  Rationale: patch is the common portable path. Callers requiring a preview
  digest and restart convergence must use transactions.
  Date/Author: 2026-07-20 / Codex

- Decision: preserve transaction actions `preview`, `commit`, and
  `commitPreviewed`, including camelCase `expectedPlanDigest` and the existing
  snake_case deserialization compatibility alias if canonical review confirms
  it remains source behavior.
  Rationale: previewed commit is the concurrency-safe review path, while the
  generated schema should remain singular and unambiguous.
  Date/Author: 2026-07-20 / Codex

- Decision: transaction supports create, update, delete, move, `replaceAll`,
  `replaceLines`, `insertBefore`, and `insertAfter`. Patch-only block operations
  compile to exact prepared after-images; the transaction wire schema does not
  duplicate the patch grammar.
  Rationale: each public tool keeps one coherent input style while both share
  identical final byte semantics.
  Date/Author: 2026-07-20 / Codex

- Decision: keep the implementation private under `nanocodex-tools` rather than
  adding a new public crate.
  Rationale: built-in tools belong to `nanocodex-tools`; no current lower-level
  consumer needs a separately versioned Hashline API.
  Date/Author: 2026-07-20 / Codex

- Decision: all model paths are workspace-relative. Transaction may select an
  optional relative `root`; absolute paths, empty components, `..` escapes, and
  symlink escapes fail before observation or mutation.
  Rationale: Nanocodex owns one local workspace and does not need Codex
  environment IDs, remote filesystem RPC, or provider abstractions.
  Date/Author: 2026-07-20 / Codex

- Decision: close the interactive usability findings without adding another
  editor or broadening the public mutation schemas. Read returns compact
  anchored `content` once rather than duplicating it as structured line
  objects; exact model-visible declarations retain path and grammar guidance;
  Hashline owns typed internal path/parser/evidence error classes rendered as
  concise field-specific model diagnostics. Outer orchestration flattening is
  measured separately.
  Rationale: the observed failures were discovery, output, and diagnostic
  failures, not missing editing capability.
  Date/Author: 2026-07-20 / Codex

- Decision: `commitPreviewed` continues to require the exact mutation request
  together with `expectedPlanDigest`, and guidance states that requirement
  directly.
  Rationale: rebuilding and digest-checking the same plan preserves the current
  stateless tool boundary and detects changed files or changed mutations.
  Date/Author: 2026-07-20 / Codex

- Decision: formatting and other external byte changes remain ordinary stale
  evidence. Hashline does not refresh or reinterpret anchors implicitly; the
  failed result identifies stale evidence and the caller rereads before
  rebuilding the edit.
  Rationale: implicit refresh would weaken the observed-state guard that makes
  line edits safe.
  Date/Author: 2026-07-20 / Codex

- Decision: preserve `unsafe_code = "forbid"`. Adapt durable Linux primitives
  through reviewed safe `rustix`/`nix` APIs, and block the transaction milestone
  if required semantics cannot be expressed safely.
  Rationale: weakening a workspace invariant is not an acceptable path to a
  native transaction pass.
  Date/Author: 2026-07-20 / Codex

- Decision: read, block discovery, and portable patch target supported native
  platforms. Durable transactions initially commit only on proven Linux
  ext-family case-sensitive directories and tmpfs; other filesystems/platforms
  return typed `Unsupported` without invoking patch as a fallback.
  Rationale: users retain a structured routine editor cross-platform, while the
  stronger recovery claim remains fail-closed.
  Date/Author: 2026-07-20 / Codex

- Decision: retain transaction recovery state below
  `.nanocodex/hashline-transactions/` inside the selected root and remove empty
  state after successful commit or recovery.
  Rationale: staged files, backups, journals, and guarded renames must remain on
  the same filesystem as the transaction root.
  Date/Author: 2026-07-20 / Codex

- Decision: retain `apply_patch` during implementation, then delete it after the
  complete Hashline replacement gate. Do not ship a feature flag, compatibility
  adapter, or hidden fallback.
  Rationale: an additive branch phase isolates regressions; the final product
  remains narrow and can be rolled back with a cohesive Git revert.
  Date/Author: 2026-07-20 / user and Codex

- Decision: canonical Codex source revalidation uses the user-confirmed sibling
  checkout at `../codex/codex-rs`.
  Rationale: root `AGENTS.md` names the source checkout and parity procedure
  explicitly, and the source-history divergence must remain visible.
  Date/Author: 2026-07-20 / Codex

## Outcomes & Retrospective

- Outcome: the earlier transaction-only draft was replaced with a full Hashline
  integration plan covering anchor-producing reads, block discovery, complete
  patch behavior, durable transactions, staged `apply_patch` removal, and model
  adoption evidence.
  The canonical source review and baseline implementation are complete.
  Remaining: transaction semantics hardening plus live baseline and
  model/evaluation evidence.

- Outcome: the native runtime now advertises the flat `hashline__read`,
  `hashline__find_block`, `hashline__patch`, and `hashline__transaction` family
  with closed schemas and no `apply_patch`. A deterministic Code Mode test
  dispatches read, patch, refreshed reads, a mixed create/update/delete/move
  preview, and commitPreviewed in one cell and verifies final bytes and sidecar
  cleanup. Canonical parser behavior preserves
  BOM, mixed line endings, final-newline shape, compact anchors, block anchors,
  sectioned file operations, and bounded previews.
  Descriptor-relative traversal, disjoint coordination, and a 35-point mixed
  mutation subprocess fault matrix now pass. The next hardening slice preserves
  metadata, separates staged/backup bodies from structural journals, adds
  explicit commit/rollback states with immediate live rollback, isolates
  recovery entries, and removes blocking filesystem work from async executor
  threads. Live baseline, native smoke, Harbor, and configured evaluation gates
  remain blocked on an API key.
  Implementation commit: `15ef6a6` (`feat(tools): integrate native hashline
  editing`) plus `0aff4fa` (`feat(hashline): harden recoverable transactions`).

- Outcome: consumer commit `f6afb79` is the first retained hands-on editing
  trajectory recorded after integration. It completed through read plus typed
  transaction, preserved unrelated content, passed the focused TUI and Clippy
  gates, and left no transaction artifacts. Its two patch calls failed before
  mutation, while the follow-up spec update demonstrated successful read plus
  routine-patch dry-run and commit.

## Context and Orientation

Read the root `AGENTS.md` and current `PLAN.md` before implementation. This spec
does not itself insert Hashline into the active roadmap. Implementation starts
only when the active plan places this slice at the current work position.

`nanocodex-tools` owns Code Mode, built-in tools, the heterogeneous registry,
subprocess lifecycle, and local file operations. `nanocodex` constructs one
private `ToolRuntime` per agent driver. Only top-level `exec` and `wait` are sent
to the Responses API; `exec` describes nested tools and dispatches calls such as
`tools.hashline__read(...)` through `ToolRegistry`.

Current structured editing is implemented by:

- `crates/nanocodex-tools/src/runtime.rs`: registers `ApplyPatchHandler` in
  every native runtime and reserves the built-in name;
- `crates/nanocodex-tools/src/apply_patch/mod.rs`: parses and sequentially
  applies add, update, delete, and move hunks with `std::fs`;
- `crates/nanocodex-tools/src/apply_patch/{parser.rs,seek_sequence.rs,streaming_parser.rs}`
  and `apply_patch.lark`: own the freeform grammar;
- `crates/nanocodex-tools/src/code_mode/description.rs`: renders registered
  tools into model-visible TypeScript declarations;
- `crates/nanocodex-tools/src/code_mode/tests.rs`: proves freeform
  `tools.apply_patch(...)` dispatch and the generated declaration.

The implementation should converge on these private Hashline layers:

1. A dependency-light text layer owns exact line-ending/BOM representation,
   compact and exact hashes, bounded excerpts, block resolution, patch parsing,
   edit compilation, and prepared before/after mutations.
2. A portable patch executor owns workspace path resolution, complete
   prevalidation, parent creation for patch creates, guarded per-file writes,
   move/delete behavior, post-write verification, and in-process rollback.
3. A transaction engine owns hard limits, exact observation, deterministic plan
   digests, previews, journals, execution state, rollback, and recovery.
4. A durable native capability owns descriptor-relative traversal, filesystem
   support negotiation, locking, staging, guarded mutation, sync ordering, and
   recovery storage.
5. Thin `Tool` handlers own closed model schemas, argument translation, blocking
   task boundaries, bounded structured outputs, and typed model-facing errors.

Canonical source candidates in the sibling checkout include
`codex-rs/core/src/tools/handlers/hashline*.rs`,
`codex-rs/hashline-transaction/src/`, and
`codex-rs/exec-server/src/hashline_transaction_fs*.rs`.

Terms used here:

- A logical file hash is a compact, non-cryptographic guard over BOM-stripped,
  newline-normalized text. Its final algorithm is frozen by canonical source
  review and deterministic fixtures before registration.
- A line hash is the lowercase four-hex Hashline anchor over one normalized
  logical line.
- A block hash is the lowercase eight-hex guard over the selected normalized
  block. It is coupled with a line anchor so the selected heuristic span can be
  reproduced and rejected when stale.
- An exact digest is the lowercase 64-character SHA-256 of exact file bytes,
  including BOM, line-ending spelling, and final-newline state.
- A prepared mutation is an immutable create/update/delete/move carrying
  canonical paths, observed before evidence, exact after bytes, and summaries.
- Validation atomicity means all request evidence and after-images are valid
  before the first user-file write.
- Patch recoverability means only best-effort rollback in the live process. It
  does not survive process death and does not imply simultaneous multi-path
  visibility.
- Transaction recoverability means interruption converges to all-before,
  all-after, or a durable non-destructive evidence-preserving state. It still
  does not mean database isolation or simultaneous visibility to unrelated
  readers.

## Plan of Work

### Milestone 0: Freeze canonical source, baseline, and holdout

Scope: make source provenance and current model behavior reproducible before any
model-visible or production code changes.

Work:

- Use the user-confirmed `../codex/codex-rs` checkout. Do not clone, fetch, or
  substitute another checkout without user direction.
- Record its branch, exact HEAD, cleanliness, and the pinned root-AGENTS
  checkpoint. Inspect every later commit required by the parity procedure and
  classify it as port, evaluate, defer, or out-of-scope before citing adopted
  behavior or advancing any checkpoint.
- Inspect and record the exact source files, commits, tests, hash algorithms,
  schemas, limits, parser forms, patch behavior, transaction behavior, and
  platform capability selected for this integration. Resolve any discrepancy
  between this product contract and source behavior in the Decision Log.
- Record current Nanocodex `ToolRuntime::model_specs()` and the exact generated
  `exec` description proving `apply_patch` is present and Hashline is absent.
- Run one frozen current-model baseline on
  `terminal-bench/large-scale-text-editing` and inspect whether it edits through
  `apply_patch`, shell, or Node. Record job path, model/provider/settings,
  reward, verifier output, tool calls, cost, latency, and cache use.
- Select and record one existing multi-file Harbor holdout before candidate
  runs. Do not tune from its candidate output.

Acceptance:

- This spec contains a complete source manifest and relevant Hashline commit
  classification based on the user-confirmed sibling checkout, including its
  divergence from the historical pinned checkpoint.
- The baseline job and holdout ID are retained and named here.
- No production source, dependency, lockfile, model schema, task, or verifier is
  changed in this milestone.

### Milestone 1: Implement the shared text model, read, and block discovery

Scope: deliver the first vertical Hashline slice: exact text observation and
bounded anchor production through a real `ToolRuntime`, while retaining
`apply_patch` as the only mutation tool.

Files and interfaces:

- `Cargo.toml`, `Cargo.lock`, and `crates/nanocodex-tools/Cargo.toml`: add only
  dependencies proven by the frozen source contract, expected to include
  `xxhash-rust` plus existing `sha2`.
- `crates/nanocodex-tools/src/hashline/mod.rs`: private subsystem entry point.
- `crates/nanocodex-tools/src/hashline/{document,hash,format,block,path,limits}.rs`:
  exact representation, hashes, bounded excerpts, block spans, path policy, and
  trusted limits.
- `crates/nanocodex-tools/src/hashline/{read,find_block}.rs`: `Tool` handlers and
  closed schemas.
- focused sibling tests and Code Mode declaration/dispatch tests.

Read contract:

    {
      "path": "relative/path",
      "start_line": 1,
      "end_line": 200,
      "max_lines": 200
    }

Only `path` is required. Lines are one-indexed and inclusive. Defaults and hard
caps are implementation-owned policy, not builder knobs. The result includes:

    {
      "path": "relative/path",
      "hash": "8-hex-logical-file-guard",
      "exactDigest": "64-hex-exact-byte-sha256",
      "header": "[relative/path]#1234abcd",
      "start_line": 1,
      "end_line": 200,
      "total_lines": 300,
      "truncated": true,
      "next_start_line": 201,
      "content": "1:1a2b|first line\n...",
      "lines": [{"n": 1, "hash": "1a2b"}]
    }

`content` is the sole carrier of line text. Structured rows carry anchor and
truncation metadata only. Empty files return null range endpoints, empty content,
and no line rows. A line that exceeds the serialized budget is truncated at a
UTF-8 boundary and marked without changing the hash of its complete logical
text. Reads reject missing files, directories, devices, binary/NUL content,
invalid UTF-8, paths outside the workspace, and unbounded requests.

Block discovery accepts a path, a recent line/block anchor, and an optional
`max_lines`. It resolves the documented language/indentation heuristic,
returns the selected start/end, language guess, compact file and block guards,
exact digest, combined `line:hash@blockhash` anchor, and one bounded anchored
excerpt. Bare line numbers are rejected; ambiguous bare line hashes report all
candidate line numbers without content.

Hashing and formatting freeze behavior for empty files, BOM, LF/CRLF/CR, mixed
endings, final newline, Unicode, long lines, and range boundaries. Display is
LF-normalized, while exact bytes remain authoritative and untouched content
retains its original representation during edits.

Acceptance:

- Focused fixtures freeze compact file/line/block hashes and exact SHA-256 for
  all representation edge cases.
- Read output is accepted verbatim by patch parser fixtures and its
  `exactDigest` is accepted verbatim by transaction expected-file fixtures.
- Read and block outputs remain within 24 KiB serialized excerpt budgets and do
  not duplicate text in structured rows.
- Block anchor output round-trips and rejects stale line or block evidence.
- Code Mode advertises exact declarations for `hashline__read` and
  `hashline__find_block`, dispatches each once, and still advertises the
  unchanged `apply_patch` tool.
- Tracing contains only operation type, counts, byte counts, duration, and
  digest-safe status; no path, content, arguments, or exact digest is logged.

### Milestone 2: Implement complete patch parsing and portable execution

Scope: implement the full Hashline patch language and register a real routine
editing tool alongside `apply_patch`.

Files and interfaces:

- `crates/nanocodex-tools/src/hashline/patch/{parser,sections,lines,preview}.rs`:
  pure grammar, section parsing, ordered edit compilation, and bounded previews.
- `crates/nanocodex-tools/src/hashline/patch/{prepared,executor,rollback}.rs`:
  prepared mutation types and portable filesystem application.
- `crates/nanocodex-tools/src/hashline/patch/tool.rs`: function `Tool` named
  `hashline__patch`.
- parser, representation, path, race, rollback, direct-tool, and Code Mode tests.

The model input is a closed function object:

    {
      "path": "default/or/single/path",
      "patch": "[path]#1234abcd\nSWAP 12:1a2b:\n+replacement",
      "dry_run": false,
      "create": false
    }

`path` and `patch` are required; booleans default false. Preserve and test the
complete grammar selected in Milestone 0, including:

- `SWAP` and `DEL` for one line and inclusive anchored ranges;
- `INS.PRE`, `INS.POST`, `INS.HEAD`, and `INS.TAIL`;
- `SWAP.BLK`, `DEL.BLK`, `INS.BLK.PRE`, `INS.BLK.POST`, and the documented
  `INS.BLK` alias;
- required `[path]#HASH` sections for existing files and `[path]` sections with
  `create=true` for missing files;
- empty and multi-file creation;
- sectioned `REM` and `MV <destination>`, with `MV` optionally combined with line
  edits and `REM` required to stand alone;
- README-style `+` payload rows, compact `|text` forms where documented, bare
  payload rows, pasted/decorated read rows, literal `+`/`-` escapes, recoverable
  bracketed path noise, and `*** Abort` suppression;
- deterministic rejection of malformed, ambiguous, mixed-mode, duplicate,
  overlapping, out-of-order, no-op, or limit-exceeding input.

For every existing target, validate the compact file guard and every used line,
range interior, and block guard against one retained exact observation. Parse
and compile every section into immutable exact before/after bytes before the
first write. Preserve untouched BOM, per-line endings, local insertion ending,
and final-newline state. A compact hash collision remains a documented residual
risk; patch does not claim cryptographic stale-read protection.

The portable executor rejects root escapes and unsafe target types, creates
missing parent directories only for explicit creates, stages regular-file
after-images, revalidates observed files immediately before each guarded
mutation, verifies after bytes, and tracks owned artifacts for cleanup or
in-process rollback. Multi-file writes are not simultaneously visible and a
process death may leave a mixed state; output and documentation must say so.
No durable transaction journal is created by patch.

`dry_run=true` performs all observation, parsing, validation, after-image
compilation, conflict detection, and bounded preview generation without creating
directories, temporary files, or user-file changes. Patch previews and changed-
region excerpts are capped at 4 KiB per file, and the total per-file detail array
is capped at 24 KiB with `total_files` and `files_truncated` metadata.

Successful commit output identifies create/update/delete/move outcomes and
returns refreshed compact hashes, exact digests, and bounded changed-region
anchors for files that remain. A failed multi-file execution reports whether no
write began, rollback restored all before-images, or partial mutation may remain;
it never flattens those states into generic success.

Acceptance:

- Pure tests cover every grammar form above, malformed input, hash collisions
  through injected hash fixtures, stale guards, mixed EOL/BOM/EOF behavior,
  noisy pasted payloads, overlapping edits, and output limits.
- Direct tests cover create/update/delete/move, multi-file mixtures, missing
  parent creation, destination conflicts, symlinks, hard links, parent swaps,
  permission failures, post-write mismatch, rollback, cleanup, and no-op cases.
- Dry run leaves a byte-for-byte and directory-entry-identical workspace.
- A successful `hashline__read` result feeds a successful
  `hashline__patch`; changing any guarded source content between those calls
  produces a bounded stale error and no write.
- Code Mode advertises and dispatches `hashline__patch` with structured JSON
  input/output while the old freeform `apply_patch` declaration and tests remain
  unchanged for A/B diagnosis.
- Routine patch works on every supported native CI target. Platform-specific
  guarantees and failures are tested rather than silently skipped.

### Milestone 3: Import the pure transaction engine

Scope: adapt the dependency-light transaction planner, edit compiler, preview,
journal, executor state machine, rollback, and recovery logic without registering
the model tool or changing patch execution.

Files and interfaces:

- `crates/nanocodex-tools/src/hashline/transaction/mod.rs`: private subsystem.
- `crates/nanocodex-tools/src/hashline/transaction/engine/*.rs`: request,
  observation, planner, edits, preview, prepared plan, journal, execution,
  rollback, recovery, limits, and typed errors.
- complete focused deterministic tests and source provenance notice chosen by
  the repository's packaging/license practice.

Preserve typed mutations, hard limits, exact-byte SHA-256 evidence, compact line
anchors, canonical path conflicts, deterministic plan digests, bounded preview
serialization, journal state transitions, rollback, and recovery. Replace Codex
environment/URI/RPC concepts with the existing Nanocodex workspace and private
root/path types. Share the exact document and prepared mutation logic from
Milestones 1-2; do not copy a second EOL/parser/hash implementation.

The public wire contract is:

    {
      "action": {"type": "preview"}
        | {"type": "commit"}
        | {"type": "commitPreviewed", "expectedPlanDigest": "<sha256>"},
      "root": "optional/relative/root",
      "mutations": [create | update | delete | move]
    }

Existing files require `{"exactDigest":"<exact-byte-sha256>"}`. Updates and
moves accept ordered `replaceAll`, `replaceLines`, `insertBefore`, and
`insertAfter` edits. Line anchors are structured one-indexed line/four-hex-hash
pairs. Content strings are exact UTF-8 and are never newline-normalized.
Transaction limits are internal and cannot be supplied by the model.

Acceptance:

- Ported planner, edit, preview, journal, executor, rollback, and recovery tests
  pass under `nanocodex-tools`.
- Repeating an identical fixture yields the same lowercase 64-hex plan digest;
  changing root identity, path, action, mutation, exact bytes, metadata evidence,
  or after-image changes it.
- Patch and transaction edit fixtures produce identical final bytes for their
  overlapping line operations.
- Limits cover mutation count, edit count, edit lines, path bytes, per-file and
  total bytes, preview bytes, response bytes, and journal bytes before allocation
  or mutation.
- `rg '\bunsafe\b|libc::' crates/nanocodex-tools/src/hashline` finds no
  implementation use.
- Model-visible registrations remain read/find-block/patch/apply-patch only.

### Milestone 4: Implement the unsafe-free durable filesystem capability

Scope: make transaction preview, execution, rollback, and restart recovery
operate on the Nanocodex workspace with proven native semantics.

Files and interfaces:

- `crates/nanocodex-tools/src/hashline/transaction/fs.rs`: capability selection
  and complete fail-closed unsupported implementation.
- `crates/nanocodex-tools/src/hashline/transaction/fs_linux/*.rs`: root/path
  handles, semantics, evidence, coordination, storage, guarded mutation, sync,
  rollback, and recovery.
- native, unsupported-platform, race, fault-injection, and subprocess restart
  tests.

Use safe owned-descriptor APIs throughout. Never canonicalize a model path and
later reopen it for mutation by ambient string. Reject empty or absolute paths,
`..`, symlinks, directories, devices, hard links, duplicate source/destination
keys, unstable metadata, invalid UTF-8 contents, and unproven lookup semantics.
Transaction parent directories must already exist; directory creation is patch
functionality and is not part of the durable mutation schema.

Store reservations, staged after-images, backups, and versioned bounded journals
below `.nanocodex/hashline-transactions/` on the transaction root filesystem.
Lock participating parent directories in canonical order. Revalidate root-to-
parent edges, file identity, metadata, link count, and exact bytes while holding
the lease. Sync staged files, backups, journals, storage directories, every
changed parent, and terminal state in the reviewed order. Scan and recover
bounded pending journals before accepting a new transaction for the root.

Missing safe wrappers, unsupported filesystem type/flags, failed durability
primitives, or unproven case-sensitive lookup return typed `Unsupported`. Do not
add local unsafe, weaken capability negotiation for the development host, or
fall back to portable patch execution.

Acceptance:

- Linux tests prove ext-family case-sensitive and tmpfs acceptance and
  deterministic rejection of casefolded or unrecognized filesystems.
- Fault injection at every durable transition yields all-before, all-after, or
  a valid evidence-preserving recovery state; no test accepts an unjournaled
  mixed outcome.
- A subprocess test kills execution after every durable transition, starts a
  fresh runtime, invokes recovery, and proves convergence and cleanup.
- Overlapping cross-runtime transactions serialize or return typed conflict;
  disjoint transactions do not require shared mutable global state.
- Successful and recovered operations remove owned staging, backup,
  reservation, journal, and empty sidecar directories.
- Non-Linux compile checks instantiate complete capability traits and return
  `Unsupported` without conditional test holes.
- Workspace Clippy with warnings denied still enforces
  `unsafe_code = "forbid"`.

### Milestone 5: Expose transactions and prove the complete family

Scope: register `hashline__transaction`, prove all Hashline tools compose, and
retain `apply_patch` only for the final A/B evaluation window.

Files and interfaces:

- `crates/nanocodex-tools/src/hashline/transaction/{schema,tool}.rs`: closed
  function schema and `Tool` handler.
- `crates/nanocodex-tools/src/runtime.rs`: register the transaction handler and
  reserve every Hashline built-in name.
- transaction tool, Code Mode, output-bound, event, and tracing tests.

The handler decodes the closed schema, resolves the relative root, runs blocking
filesystem work off the async runtime, performs bounded recovery before new
planning/commit, preserves typed conflict/unsupported/recovery failures, and
caps model-visible transaction output at 8 KiB by truncating preview content
before structural evidence.

Tool guidance must connect the family:

- read before patch; copy anchors, never invent hashes, and reread after stale
  evidence;
- use patch for routine edits and transaction for reviewed/high-risk batches;
- copy `exactDigest` from read into transaction expected files;
- prefer transaction preview plus `commitPreviewed` when a plan is reviewed,
  resupplying the exact mutations with the returned `expectedPlanDigest`;
- use `replaceAll` when exact bytes are known but no suitable line anchors are
  available;
- do not claim patch crash recovery, transaction database isolation, or a shell
  write security boundary.

Acceptance:

- Exact `ToolRuntime::model_specs()` input contains one declaration each for
  read, find-block, patch, transaction, and temporary `apply_patch`.
- One Code Mode cell reads anchors, dry-runs and commits a patch, rereads exact
  evidence, previews a mixed transaction, and commits the returned plan digest.
  Each nested invocation is recorded once and every returned value is bounded
  structured JSON.
- Read results contain one compact anchored content representation plus
  structural metadata; they do not duplicate every line as structured objects.
- Exact Code Mode declarations retain workspace-relative path rules, a complete
  routine-patch example, and the exact `commitPreviewed` resubmission contract.
- Wrong-dialect patch sentinels, invalid `path` versus `root`, stale anchors,
  and stale exact digests have distinct bounded diagnostics naming the failing
  field or program line; tests assert no write.
- `expectedPlanDigest` and any canonically verified compatibility alias decode,
  while generated schema advertises only camelCase.
- Invalid JSON, unknown fields, invalid digest widths, stale evidence,
  conflicting paths, unsupported roots/filesystems, malformed recovery state,
  and output overflow return bounded failed results with no hidden partial
  mutation.
- Transaction tracing contains structural counts, durations, outcome category,
  and recovery-required status only. Patch/read content, paths, arguments,
  compact hashes, exact digests, plan digests, and journals are not logged.

### Milestone 6: Remove `apply_patch` and prove Hashline-only behavior

Scope: make the full Hashline family the only dedicated structured editing
surface, delete obsolete patch code, and exercise real consumers.

Files and interfaces:

- `crates/nanocodex-tools/src/runtime.rs`: remove `ApplyPatchHandler` and the
  reserved `apply_patch` name; retain all Hashline handlers by default.
- `crates/nanocodex-tools/src/lib.rs`: remove the `apply_patch` module.
- `crates/nanocodex-tools/src/apply_patch/`: delete handler, parser, grammar,
  seek helper, streaming parser, and tests.
- Code Mode, binary observability stress, CLI, and example assertions: replace
  old apply-patch assumptions with exact Hashline behavior.

Run a disposable native CLI smoke that:

1. reads a file, applies a hash-anchored routine patch, and rereads refreshed
   anchors;
2. creates, updates, moves, and deletes files through one previewed transaction;
3. verifies exact bytes and Git diff;
4. leaves no transaction sidecar, temporary file, backup, or empty created
   parent directory after cleanup;
5. emits contractual JSONL on stdout and diagnostics only on stderr.

Run frozen focused Harbor trials on:

- `terminal-bench/large-scale-text-editing` for sustained reads and edits;
- `terminal-bench/fix-git` for repository-aware mutation;
- `terminal-bench/polyglot-rust-c` for multi-file source/build changes;
- the holdout selected in Milestone 0.

Inspect agent JSONL, exact input/output streams, ATIF trajectories, Harbor
results, verifier output, timing, token/cache use, and every mutation path.
Record read/patch/transaction/shell/Node adoption separately from reward. A
passing task with shell writes is product evidence but not Hashline adoption;
at least one successful routine trajectory must use read plus patch and at least
one successful representative trajectory must use transaction end to end.

Acceptance:

- `rg -n 'apply_patch' crates/nanocodex-tools/src` returns no production
  surface; intentional historical/spec references are reviewed separately.
- Exact model input advertises the four Hashline tools and no `apply_patch`.
- Patch retains create/update/delete/move, empty/multi-file creation, block, and
  stale-anchor coverage after all old patch code is deleted.
- Native smoke and all four retained Harbor jobs have inspected evidence; at
  least one successful read/patch trajectory and one successful transaction
  trajectory are retained.
- A retained edit/format/edit trajectory proves stale rejection after formatting,
  bounded reread, successful rebuilt mutation, and no unrelated byte changes.
- No trajectory can call the removed tool, and benchmark tasks/verifiers remain
  byte-for-byte unchanged.

### Milestone 7: Complete release proof and documentation

Scope: run the full repository gate, inspect artifacts, and leave a reviewable
provenance, rollout, and rollback record.

Update this spec, `PLAN.md` at the active-roadmap position, applicable crate/root
changelogs, and the chosen source notice only when implementation and gates are
complete. Do not create a second experiment diary; Harbor records and Git
history remain authoritative.

Acceptance:

- `just check` or its exact constituent format, warnings-denied Clippy, workspace
  test, and all-target check commands pass without skipped Hashline tests.
- Public examples compile and the standard native `just run` smoke succeeds.
- The full configured `just eval` finishes with exact retained job path and
  inspected per-task results. Failures and behavior deltas are classified, not
  hidden or retried into a claim.
- Packaging checks include every required source and notice file when packaging
  is part of the release gate.
- Final `git diff --check`, diff/stat review, and `git status --short` contain
  only intentional source, test, spec, notice, changelog, and lockfile changes.
- Final outcomes report correctness, tool adoption, token/cache cost, latency,
  platform coverage, unresolved recovery evidence, source classifications,
  commits, PR, and rollback readiness separately.

## Interfaces and Dependencies

Local interfaces:

- `hashline::Document`:
  - Inputs: bounded exact UTF-8 bytes.
  - Outputs: exact source-line records, BOM/final-newline state, local line-ending
    fallback, normalized logical view, compact hashes, and exact digest.
  - Failures: binary/invalid UTF-8, file/line limits, or representation overflow.

- `hashline::ReadRequest` / `ReadResult`:
  - Inputs: workspace-relative path and optional inclusive range/line cap.
  - Outputs: bounded anchored text, compact file/line guards, exact digest,
    truncation metadata, and next range.
  - Failures: path, type, range, encoding, observation, or output-limit error.

- `hashline::BlockRequest` / `BlockResult`:
  - Inputs: path, line/block anchor, and optional excerpt cap.
  - Outputs: selected span, language guess, compact block anchor, exact digest,
    and bounded anchored excerpt.
  - Failures: stale/ambiguous anchor, unsupported/unknown span, path, or limit.

- `hashline::PatchRequest` / `PreparedPatch`:
  - Inputs: default path, complete patch string, `create`, `dry_run`, and trusted
    internal limits.
  - Outputs: fully validated ordered prepared mutations, warnings, previews, and
    before/after evidence.
  - Failures: parse, stale guard, conflicting path, invalid edit, no-op, type,
    representation, path containment, or limit.

- `hashline::PortablePatchExecutor`:
  - Inputs: complete prepared patch and workspace root.
  - Outputs: per-file receipts, refreshed evidence, cleanup, or explicit rollback
    state.
  - Failures: pre-write conflict, platform filesystem error, rollback-complete,
    or partial-mutation-possible. No restart recovery claim.

- `hashline::transaction::TransactionRequest`:
  - Inputs: root identity, action, explicit mutation list, and trusted limits.
  - Outputs: immutable plan, exact before/after evidence, summary, and
    deterministic SHA-256 plan digest.
  - Failures: invalid request, unsupported capability, conflict/staleness, limit,
    filesystem, execution, rollback, or recovery.

- `hashline::transaction::PlanningFileSystem` and
  `TransactionFileSystem`:
  - Inputs: native root, model-relative paths, observations, prepared plans, and
    guarded storage/mutation/recovery operations.
  - Outputs: canonical path keys, owned handles, metadata/identity evidence,
    staged files, backups, journal generations, receipts, and cleanup.
  - Failures: fail-closed path/platform semantics, conflict, capacity, sync,
    recovery-required, or unsupported capability.

- Hashline `Tool` handlers:
  - Inputs: function JSON matching closed schemas.
  - Outputs: `ToolExecution::from_json` with bounded structured value and exact
    success status for Code Mode, model output, and typed events.
  - Failures: bounded model-recoverable categories; no panic, `unwrap`, hidden
    partial state, raw descriptor, or internal journal payload.

- `ToolRuntime`:
  - Inputs: agent workspace at construction.
  - Outputs: default Code Mode registry containing the full Hashline family.
  - Failures: duplicate built-in names rejected deterministically; transaction
    unsupported behavior reported on invocation, not hidden by schema omission.

Expected external dependencies, pending canonical review:

- Canonical local Codex source for behavior and tests; source checkpoint,
  adopted commits, license paths, and classifications must be filled in during
  Milestone 0.
- `xxhash-rust` for frozen compact line/file/block guards, with only required
  features enabled.
- Existing `sha2` for exact bytes and plan SHA-256.
- Existing or directly pinned `rustix`/`nix` safe wrappers for durable Linux
  descriptor-relative operations. Add only features required by reviewed calls.
- Existing serialization/schema/runtime dependencies. Do not add regex,
  parser generators, mmap, tree-sitter, async filesystem crates, or general
  transaction dependencies without a demonstrated current-slice need.

## Concrete Steps

Update exact commands if implementation reveals more precise focused targets.
Run from `/home/ericjuta/.openclaw/workspace/repos/nanocodex`:

    git status --short --branch
    git -C ../codex/codex-rs rev-parse HEAD
    git -C ../codex/codex-rs log --oneline \
      35eaf3ffb0bf2001486c68c47a3d946b34d16634d..HEAD

    cargo test -p nanocodex-tools hashline::hash
    cargo test -p nanocodex-tools hashline::read
    cargo test -p nanocodex-tools hashline::block
    cargo test -p nanocodex-tools hashline::patch
    cargo test -p nanocodex-tools hashline::transaction::engine
    cargo test -p nanocodex-tools hashline::transaction::fs
    cargo test -p nanocodex-tools hashline::transaction::tool
    cargo test -p nanocodex-tools code_mode

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo check --workspace --all-targets

    just run
    just eval-task task=terminal-bench/large-scale-text-editing effort=low
    just eval-task task=terminal-bench/fix-git effort=low
    just eval-task task=terminal-bench/polyglot-rust-c effort=low
    just eval

    rg -n '\bunsafe\b|libc::' crates/nanocodex-tools/src/hashline
    rg -n 'apply_patch' crates/nanocodex-tools/src
    git diff --check
    git diff --stat
    git status --short

Expected implementation evidence, to be replaced with exact outputs:

    hashline__read: compact anchors and exactDigest round-trip
    hashline__patch dry_run: complete preview, no filesystem changes
    hashline__patch commit: refreshed bounded anchors
    hashline__transaction preview: planDigest=<64 lowercase hex>, no writes
    hashline__transaction commitPreviewed: outcome=committed, sidecar removed
    unsupported: durable Hashline transactions require proven Linux filesystem semantics
    Harbor evidence is not available in this draft

## Validation and Acceptance

Automated validation:

- Hash/read tests cover algorithm casing/width, BOM, LF/CRLF/CR/mixed endings,
  final newline, empty text, Unicode, binary/invalid UTF-8, long lines, exact
  digest, ranges, truncation, and formatter/parser round trips.
- Block tests cover supported language heuristics, indentation fallback,
  ambiguous line hash, complete combined-anchor round trip, stale line/block
  guard, bounded excerpt, and heuristic span visibility.
- Patch parser/apply tests cover every operation and payload form, single and
  sectioned files, empty/multi create, remove, move-plus-edit, abort, malformed
  syntax, stale file/line/interior/block evidence, overlapping edits, no-op,
  mixed representation, path conflicts, and all limits.
- Portable patch executor tests cover complete prevalidation, missing parents,
  symlinks/hard links/devices, parent replacement, permission failure,
  revalidation race, staging, guarded mutation, verification, in-process
  rollback, partial-state reporting, artifact cleanup, and cross-platform gates.
- Transaction engine/native tests cover deterministic digests, mixed
  create/update/delete/move, exact mismatch, all edit variants, conflict paths,
  journals, fault injection, rollback, restart recovery, cross-process locks,
  sync ordering, unsupported platforms/filesystems, and cleanup.
- Tool tests cover exact schemas and TypeScript declarations, structured Code
  Mode values, one nested call per invocation, malformed JSON, unknown fields,
  digest aliases, output bounds, event status, cancellation, and tracing
  redaction.
- Workspace tests/examples prove shell sessions, custom tools, MCP, web/image
  tools, event ordering, reconnect/replay, compaction, forks, and language
  bindings remain unchanged.

Manual/runtime validation:

1. In a disposable Git workspace, read a mixed-EOL file, copy anchors into a
   patch, dry-run, commit, and verify only intended exact bytes changed.
2. Reread, externally change one guarded line, replay the patch, and observe a
   stale error with no additional change.
3. Discover a block, patch it by the combined block anchor, then prove stale
   block selection is rejected after an interior change.
4. Exercise one sectioned patch containing create/update/move/delete and inspect
   the exact Git diff and bounded refreshed output.
5. Copy `exactDigest` from read into a mixed transaction, preview it, externally
   change one input, and prove `commitPreviewed` rejects the changed plan.
6. Preview again, commit, verify exact bytes, and prove all transaction artifacts
   are gone.
7. Terminate the deterministic transaction harness at each durable transition,
   start a fresh runtime, trigger recovery, and prove declared convergence.

Model-behavior evaluation:

- Representative tasks are the three named Harbor tasks with frozen task,
  model, effort, provider, and verifier settings.
- Edge cases include stale read evidence, stale preview digest, multi-file patch,
  mixed transaction, block edit, large escaped preview, and cancellation after a
  durable journal transition.
- Adversarial cases include symlink/hard-link swaps, parent replacement,
  duplicate destinations, malformed sections/journals, casefolded/unproven
  filesystems, and concurrent external modification.
- The holdout is selected and recorded before candidate runs and is not named in
  tool descriptions or focused examples.

Acceptance rubric:

- Correctness: no accepted request violates its documented guards; patch reports
  any residual partial state and transaction never leaves an unjournaled mixed
  result.
- Workflow: read anchors feed patch directly, block anchors round-trip, and read
  exact digests feed transaction directly.
- Recovery: every injected durable interruption reaches all-before, all-after,
  or a typed evidence-preserving state on a fresh runtime.
- Behavior: exact model input contains only the Hashline structured editing
  family; retained successful trajectories demonstrate both read/patch and
  transaction adoption.
- Regression: focused and full repository gates pass; reward, cost, cache, and
  latency deltas are reported without changing tasks/verifiers.
- Safety: no new unsafe, secret/content tracing, root escape, silent fallback,
  unsupported-filesystem transaction mutation, or unbounded model output.
- Operations: successful patch and transaction calls clean owned artifacts;
  unresolved transaction evidence remains bounded and diagnosable.

Regression checks:

- Sequential turns retain one tool runtime, shell sessions, Code Mode host,
  prompt/cache identity, response chain, and event behavior.
- Hashline definitions remain byte-stable across turns and do not disturb the
  stable shared prompt prefix after registration.
- Cancellation still terminates subprocess descendants. Patch reports the
  observed commit state; transaction recovery ownership survives a dropped
  response future or process death.
- Caller-defined tools and dynamic MCP providers compose with built-ins and
  reject duplicate Hashline names deterministically.
- WASM continues excluding native tools without importing Linux-only types.
- CLI stdout remains flushed contractual JSONL and diagnostics remain on stderr.

## Idempotence and Recovery

- Read and block discovery are observation-only and safe to repeat. Identical
  exact bytes and request bounds yield identical evidence.
- Patch dry run is observation-only. A committed patch generally fails when
  repeated because its compact file/line/block preconditions no longer match.
- Patch takes complete before-images for its live execution and may roll back an
  in-process failure. Do not claim that restart replays or repairs a patch. If a
  process dies during patch, inspect user files and Git state before removing
  verified owned temporary artifacts.
- Transaction preview is observation-only. Identical root and file evidence plus
  request yield the same plan digest.
- Repeating immediate transaction commit after success normally fails original
  preconditions rather than applying twice.
- `commitPreviewed` after any relevant change fails the plan digest and performs
  no user-file mutation.
- Before the first durable journal, remove only verified unreferenced transaction
  staging. After journaling, retain evidence and invoke bounded recovery through
  a fresh runtime.
- Recovery is idempotent: repeated invocation resumes from the latest valid
  journal generation and never trusts an unvalidated artifact name or path.
- Manual cleanup must first prove all user files match a declared terminal state.
  Never instruct operators to delete a live
  `.nanocodex/hashline-transactions/` directory in place.
- Before reverting a deployed transaction-capable binary, recover all pending
  journals or retain that binary as a recovery utility. The old binary cannot
  interpret new state.

## Rollout and Operations

- Feature flags/config/env vars: none in the final product. Implementation uses
  a branch-local additive phase; release atomically removes model-visible
  `apply_patch` after gates.
- Migration/backfill: none for normal user data. Existing workspaces have no
  Hashline state. Never deploy an older binary over unresolved new journals.
- Platform rollout: routine read/block/patch on supported native targets;
  durable transaction commits first on proven Linux filesystems. Unsupported
  transaction calls retain the same schema and return typed failure.
- Monitoring: spans for read, block resolution, patch parse/plan/apply/rollback,
  transaction plan/recovery/stage/commit/rollback/cleanup, and total duration.
  Record counts, byte counts, outcome/error category, recovery-required state,
  and durations only—never paths, contents, arguments, compact/exact/plan
  digests, or journal payloads.
- Healthy values: zero unresolved transaction recovery after burn-in, zero
  successful calls with retained owned artifacts, bounded outputs, low stale-
  retry loops, and no mixed-state invariant failures.
- PR workflow: milestone-sized commits, current living spec, complete diff and
  source-provenance review, focused gates per commit, then one reviewable PR with
  explicit rollback notes. Do not push, merge, or deploy without user direction.

## Risks and Open Questions

- Risk: the canonical sibling checkout diverges from the historical pinned
  checkpoint, so a linear `checkpoint..HEAD` parity classification is not
  meaningful.
  Mitigation: record the divergence and classify the checkout's relevant
  Hashline history directly.

- Risk: compact line/file/block guards can collide and are not adversarial stale-
  write protection.
  Mitigation: describe them as compact trusted-workspace anchors, validate all
  available guards, revalidate immediately before patch writes, emit exact
  digests for transaction use, and reserve high-integrity claims for transaction.

- Risk: heuristic block selection may choose a broader or narrower span than the
  model expects.
  Mitigation: return selected span, language, excerpt, and combined block anchor;
  require that anchor for block mutation and reject stale selection.

- Risk: the complete patch grammar is large and can inflate the stable tool
  definition or confuse the model.
  Mitigation: keep descriptions operational, freeze exact schema bytes, use
  examples sparingly, test every parser form directly, and compare tool adoption,
  tokens, cache hit rate, and stale retries against baseline.

- Risk: portable multi-file patch cannot promise restart recovery.
  Mitigation: prevalidate all sections, stage and guard each write, provide
  explicit live rollback/partial-state outcomes, and direct high-risk batches to
  previewed transactions.

- Risk: safe wrappers may not reproduce every required durable Linux primitive.
  Mitigation: map every canonical source operation to a reviewed safe API, port
  fault/race tests, and block support rather than add unsafe or path-based races.

- Risk: removing `apply_patch` may regress uncommon parser tolerance or file
  creation behavior.
  Mitigation: retain it through the additive A/B milestones, port full Hashline
  payload/file-operation coverage, test missing parents and empty files, inspect
  trajectories, and delete only after exact replacement gates.

- Risk: durable transaction platform/filesystem support remains narrower than
  routine patch support.
  Mitigation: expose the distinction directly and never route a transaction to
  patch silently. Cross-platform durable adapters are separate future slices.

- Risk: the model may keep editing through shell or Node.
  Mitigation: connect read/patch/transaction guidance, inspect trajectories, and
  report adoption separately from task pass rate. Enforcement remains out of
  scope.

- Risk: large replace-all or patch bodies increase tokens and can pressure tool
  input/output limits.
  Mitigation: prefer anchored edits, keep definitions stable, bound requests and
  outputs before allocation, truncate previews before evidence, and report
  measured token/cache/latency effects.

- Risk: transaction recovery artifacts can appear in repository status after a
  crash.
  Mitigation: keep artifacts bounded and clearly named, clean empty sidecars,
  document diagnosis, and never modify external `.gitignore` automatically.

- Resolved: the frozen holdout is pinned `terminal-bench/polyglot-c-py`, selected
  from configured task metadata without inspecting a candidate trajectory.

- Resolved: adapted canonical behavior is attributed in
  `crates/nanocodex-tools/NOTICE`, which is present in the generated crate
  archive. The implementation remains under the workspace's Apache-2.0 option.

## Artifacts and Notes

Initial planning state:

    Nanocodex branch: feature/hashline_transaction
    Nanocodex HEAD before rewrite: 9ed472b
    Superseded transaction-only plan: 3f2f58b
    Required Codex checkout: ../codex/codex-rs
    Required Codex checkout state: user-confirmed canonical sibling checkout
    Canonical checkout HEAD: eff2c761e2bf3c644730edf795a8055b00818e92 (clean main)
    Pinned-checkpoint relationship: divergent; pinned commit is not an ancestor
    Baseline model schema: .nanocodex/evidence/hashline/baseline-9ed472b-model-specs.json
    Baseline schema result: apply_patch present; Hashline absent
    Baseline live model run: blocked; OPENAI_API_KEY unavailable
    Holdout: terminal-bench/polyglot-c-py at the configured pinned dataset digest
    Nanocodex unsafe lint: unsafe_code = "forbid"
    Native implementation commit: 15ef6a6
    Durable filesystem commit: 0aff4fa
    Deterministic Hashline tests: 12 direct + 1 Code Mode family round trip passed
    Durable fault matrix: 35 subprocess interruption points passed
    nanocodex-tools tests: 65 passed, 2 ignored subprocess helpers
    Workspace tests: 167 passed, 5 ignored manual/isolated/helper tests
    Harbor adapter tests: 34 passed
    Workspace all-target Clippy: passed with warnings denied
    Public examples: compiled as workspace test targets
    Hashline unsafe/libc scan: no matches
    just check: passed
    just release-check 0.1.0: passed, including packaged docs
    nanocodex-tools archive: NOTICE and transaction_fs.rs present

Do not replace missing source/eval evidence with estimates. Update this section
with exact source HEAD/classification, deterministic test counts, retained job
paths, rewards, tool calls, cost, latency, cache use, and verifier observations
as each milestone completes.

## Revision Notes

- 2026-07-20: Replaced the transaction-only draft with the intended full
  Hashline read, block-anchor, patch, and recoverable transaction integration
  plan. Added direct read-to-patch and read-to-transaction evidence flow,
  portable patch versus durable transaction guarantees, staged `apply_patch`
  removal, canonical source revalidation, and separate adoption gates.
- 2026-07-20: Recorded native implementation commit `15ef6a6`, deterministic
  Code Mode and workspace validation, the user-confirmed `../codex/codex-rs`
  source path, and the remaining descriptor-relative durability, exhaustive
  fault-injection, and live evaluation gates.
- 2026-07-20: Recorded durable filesystem commit `0aff4fa`, safe
  descriptor-relative traversal, disjoint parent leases, root identity binding,
  mixed-mutation subprocess fault coverage, packaged attribution, reconstructed
  baseline schema, selected holdout, full offline repository/release gates, and
  the missing API-key blocker for all remaining live evidence.
- 2026-07-20: Compared the compact transaction baseline with local Codex
  `eff2c761e2` and made the remaining semantics hardening explicit: metadata
  preservation, staged/backup storage, durable transaction states, immediate
  rollback, isolated recovery, reservations, and async-runtime integration.
  This behavioral reference does not advance the formal Codex parity checkpoint.
