# Compaction and image recovery parity

Current compaction reference: pinned `openai/codex` commit `36430b36881cf5c289cb48e671cfc9e8b542ae7b`. The earlier image-recovery investigation below used `c775dd3c332de1b69b25a4580f6c5bc44b94e284` and `1427825c40`. This document distinguishes implemented rules from remaining differences; it does not claim complete Codex equivalence.

## Pinned compaction changes

- Developer retention now depends on explicit client provenance, corresponding to Codex `CodexHarnessMetadata.client_authored` (`compact_remote_v2.rs`). Nanocodex records IDs in an internal sidecar at the client-input boundary, carries it through clones, execution continuations, session snapshots, and rollout world-state records, and prunes it after compaction. It never sends the sidecar to the provider. Old snapshots remain readable with an empty sidecar; no text-based or destructive migration guesses old message authorship.
- A client-authored message shaped like an image-resize notice is retained independently from its preceding source. Generated notices remain attached to their source. Generated developer context is not silently reclassified as client input.
- The 90% automatic threshold uses the raw configured window. The tool-output trim budget uses 95% usable capacity (`openai_models.rs`, `turn_context.rs`, `compact_remote_history.rs`). Rewriting already-sent outputs invalidates incremental continuation, so the compact request sends the rewritten full history.
- Local-tail usage treats incoming agent messages as instructions, preserves their preceding local context, and does not count an entire history again when it has no model-generated item. Legacy assistant inter-agent envelopes use the pinned typed-envelope/path validation rules.
- Compaction accepts exactly one `response.output_item.done` summary before completion; a conflicting completion envelope cannot substitute another summary or hide a duplicate.
- Token estimates now follow the model-visible content branches in `context_manager/history.rs`: text bytes instead of transport JSON, tool names/namespaces/arguments, encrypted payload estimates, fixed resized-image costs, decoded original-detail patches capped at 10,000, and decoded audio duration with URL-size fallback. Original-image and audio estimation runs in shared Rust on both native and WASM; no V8 runtime is introduced. These are Codex's approximate token rules, not exact tokenizer counts.

## Limits of equivalence

Nanocodex enables retained-image budgeting and client-developer retention unconditionally; Codex gates these behaviors behind features. Its response-item schema does not yet represent all Codex metadata (for example per-content classifications, file image references, or optional function-output name/namespace fields). Sidecar provenance only exists for newly captured inputs or snapshots that contain it. Provider capabilities, previous-model compaction fallback, hooks, analytics, and rollout schema compatibility are separate surfaces; these changes do not establish equivalence for them. Existing hosted rejected-image repair deliberately persists repaired history, unlike Codex's reconstruction behavior described below.

## Verified defects

- The model egress proxy replaced upstream HTTP errors with `502 upstream_rejected`, losing the real status and recovery classification. It now preserves status, recognized error codes/types, bounded structural selectors, and the fixed legacy image diagnostic, without returning arbitrary provider text.
- Hosted tool image preparation was a no-op. Malformed MIME/base64 payloads could enter durable history through raw tool outputs as well as Code Mode. The hosted boundary and restored-history repair now share validation, including canonical base64 padding.
- Compaction converted a typed invalid-image failure into an untyped failure string. Its recorded outcome now carries a backward-compatible recovery discriminator; provider policy stops take precedence.
- Image repair did not advance the history revision. Repairs of already-committed images could therefore disappear on reload. Repair now resets continuation and advances the revision so persistence writes replacement history.

## Upstream comparison

| Behavior | Codex reference | Nanocodex behavior |
| --- | --- | --- |
| Compaction retries and transport fallback | `codex-rs/core/src/compact_remote_v2.rs` and client transport policy | Existing bounded WebSocket attempts followed by HTTPS; terminal failure receipts prevent a new retry cycle during reconstruction. |
| Retained history and resize notices | `codex-rs/core/src/compact_remote_history.rs` | Retains allowed user/developer/agent messages, keeps resize notices with their source, inserts canonical context before the last retained input, and uses model-visible content accounting from the pinned estimator. |
| Boundary image budget | `codex-rs/core/src/compact_remote_v2_images.rs` | Retains newest content within budget, treats image labels and images atomically, and does not backfill older content after an oversized boundary image. |
| Invalid image response | `codex-rs/core/src/session/turn.rs`, `codex-rs/codex-api/src/api_bridge.rs` | Fails the current turn. HTTP and in-band errors retain image classification; policy errors remain authoritative. |
| Durable poisoned-history recovery | Hosted extension | Replaces rejected image payloads with explanatory text while preserving other content and call identities. The next user turn may proceed; tool effects are not automatically rerun. |

Image preparation was additionally compared with local `openai/codex` at `1427825c40` (`core/src/session/mod.rs` and `core/src/image_preparation.rs`). Codex prepares images both before history insertion and after rollout reconstruction, replacing failed decodes with text. Nanocodex now shares its existing pixel decoder, resizing, and format normalization across native and hosted WASM input/output preparation. JavaScript checks the envelope early; the Rust boundary also decodes syntactically valid payloads before insertion.

Serialized history, execution continuations, and exact checkpoint forks prepare message and tool images before replay, preserving item order, call identities, and requested detail. Replayed tool-effect receipts are prepared before appending their response items without reexecuting or rewriting the recorded effects. Canonical context is prepared too so compaction cannot reintroduce a failed image. Changes reset provider continuation and advance the durable revision. Unlike the referenced Codex reconstruction (which keeps the recorded rollout unchanged), Nanocodex persists repaired history to prevent future reloads from restoring poisoned content. WASM runs decoding inline; native fresh-input preparation uses its blocking pool. Provider rejection recovery remains necessary for provider-specific image constraints.

## Regression evidence

The pinned-reference tests cover the raw/usable budget boundary, client provenance versus generated context, notice-shaped client input, JSON snapshot and rollout recovery, model-visible text/tool/image/audio costs, agent-message usage, and streamed-summary cardinality. The shared crate is compiled for `wasm32-unknown-unknown`; native protocol regressions exercise the same Rust implementation.

The automatic and manual compaction regressions exercise a real PNG tool output, inject provider image rejection during compaction, verify the failed-turn checkpoint retains the tool call/output identity with repaired content, reload the session, and complete a subsequent request and compaction. Additional tests cover malformed stored history, raw hosted tool-output bypass, base64 padding, policy-stop precedence, legacy diagnostic projection, and failure-receipt compatibility.

Related investigation integration also preserves the CLI's compaction phase across connection updates and refreshes an explicitly rejected stale Hand route once while retaining the same effect identity. Ambiguous transport/server failures are not automatically redispatched.

The original session's compaction succeeded at 23:15:03 UTC after earlier masked HTTPS failures. The original upstream rejection body was discarded, so these fixes do not establish that the historical HTTPS failures were caused by an image rejection.
