# Compaction and image recovery parity

Current compaction reference: pinned `openai/codex` commit `36430b36881cf5c289cb48e671cfc9e8b542ae7b`. This document describes the implemented rules and remaining differences; it does not claim complete Codex equivalence.

## Compaction contract

- Developer retention now depends on explicit client provenance, corresponding to Codex `CodexHarnessMetadata.client_authored` (`compact_remote_v2.rs`). Nanocodex records IDs in an internal sidecar at the client-input boundary, carries it through clones, execution continuations, session snapshots, and rollout world-state records, and prunes it after compaction. It never sends the sidecar to the provider. Old snapshots remain readable with an empty sidecar; no text-based or destructive migration guesses old message authorship.
- A client-authored message shaped like an image-resize notice is retained independently from its preceding source. Generated notices remain attached to their source. Generated developer context is not silently reclassified as client input.
- The 90% automatic threshold uses the raw configured window. The tool-output trim budget uses 95% usable capacity (`openai_models.rs`, `turn_context.rs`, `compact_remote_history.rs`). Rewriting already-sent outputs invalidates incremental continuation, so the compact request sends the rewritten full history.
- Local-tail usage treats incoming agent messages as instructions, preserves their preceding local context, and does not count an entire history again when it has no model-generated item. Legacy assistant inter-agent envelopes use the pinned typed-envelope/path validation rules.
- Compaction accepts exactly one `response.output_item.done` summary before completion; a conflicting completion envelope cannot substitute another summary or hide a duplicate.
- Token estimates now follow the model-visible content branches in `context_manager/history.rs`: text bytes instead of transport JSON, tool names/namespaces/arguments, encrypted payload estimates, fixed resized-image costs, decoded original-detail patches capped at 10,000, and decoded audio duration with URL-size fallback. Original-image and audio estimation runs in shared Rust on both native and WASM; no V8 runtime is introduced. These are Codex's approximate token rules, not exact tokenizer counts.

## Limits of equivalence

Nanocodex enables retained-image budgeting and client-developer retention unconditionally; Codex gates these behaviors behind features. Its response-item schema does not yet represent all Codex metadata (for example per-content classifications, file image references, or optional function-output name/namespace fields). Sidecar provenance only exists for newly captured inputs or snapshots that contain it. Provider capabilities, previous-model compaction fallback, hooks, analytics, and rollout schema compatibility are separate surfaces; these changes do not establish equivalence for them. Existing hosted rejected-image repair deliberately persists repaired history, unlike Codex's reconstruction behavior described below.

## Image validation and recovery

The [model egress proxy](../js/egress/src/egress.ts) preserves upstream HTTP status,
recognized error codes/types, bounded structural selectors, and the fixed legacy
image diagnostic without returning arbitrary provider text. Hosted tool outputs
and restored history share MIME/base64 validation, including canonical padding.
Compaction failure receipts retain a backward-compatible image-recovery
discriminator; provider policy stops take precedence.

[Image repair](../crates/nanocodex-oai-api/src/session/state.rs) resets continuation
and advances the history revision so persistence writes repaired history. Call
identities and non-image content survive; external effects are not rerun.

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

## Related runtime boundaries

The CLI preserves its compaction phase across connection updates. An explicitly
rejected stale Hand route is refreshed once with the same effect identity;
ambiguous transport/server failures are not automatically redispatched. These
boundaries are implemented in the
[terminal transcript](../bin/nanocodex/src/nanocodex2/tui/transcript/model.rs) and
[hosted Hand dispatcher](../js/managed/src/account-hosted-tools.ts).
