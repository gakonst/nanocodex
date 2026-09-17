# Compaction and image recovery parity

Reference: `openai/codex` main at `c775dd3c332de1b69b25a4580f6c5bc44b94e284` (fetched September 17, 2026). This review covers compaction, rejected images, and the recovery paths affected by these fixes; it is not a claim of whole-repository equivalence.

## Verified defects

- The model egress proxy replaced upstream HTTP errors with `502 upstream_rejected`, losing the real status and recovery classification. It now preserves status, recognized error codes/types, bounded structural selectors, and the fixed legacy image diagnostic, without returning arbitrary provider text.
- Hosted tool image preparation was a no-op. Malformed MIME/base64 payloads could enter durable history through raw tool outputs as well as Code Mode. The hosted boundary and restored-history repair now share validation, including canonical base64 padding.
- Compaction converted a typed invalid-image failure into an untyped failure string. Its recorded outcome now carries a backward-compatible recovery discriminator; provider policy stops take precedence.
- Image repair did not advance the history revision. Repairs of already-committed images could therefore disappear on reload. Repair now resets continuation and advances the revision so persistence writes replacement history.

## Upstream comparison

| Behavior | Codex reference | Nanocodex behavior |
| --- | --- | --- |
| Compaction retries and transport fallback | `codex-rs/core/src/compact_remote_v2.rs` and client transport policy | Existing bounded WebSocket attempts followed by HTTPS; terminal failure receipts prevent a new retry cycle during reconstruction. |
| Retained history and resize notices | `codex-rs/core/src/compact_remote_history.rs` | Retains allowed user/developer/agent messages, keeps resize notices with their source, inserts canonical context before the last retained input, and accounts for serialized overhead. |
| Boundary image budget | `codex-rs/core/src/compact_remote_v2_images.rs` | Retains newest content within budget, treats image labels and images atomically, and does not backfill older content after an oversized boundary image. |
| Invalid image response | `codex-rs/core/src/session/turn.rs`, `codex-rs/codex-api/src/api_bridge.rs` | Fails the current turn. HTTP and in-band errors retain image classification; policy errors remain authoritative. |
| Durable poisoned-history recovery | Hosted extension | Replaces rejected image payloads with explanatory text while preserving other content and call identities. The next user turn may proceed; tool effects are not automatically rerun. |

Hosted validation checks the MIME/base64 envelope. It does not decode image pixels or replace the native producer's resizing and format normalization. Syntactically valid but undecodable image bytes still require provider rejection handling; these failures must not poison subsequent turns.

## Regression evidence

The automatic and manual compaction regressions exercise a real PNG tool output, inject provider image rejection during compaction, verify the failed-turn checkpoint retains the tool call/output identity with repaired content, reload the session, and complete a subsequent request and compaction. Additional tests cover malformed stored history, raw hosted tool-output bypass, base64 padding, policy-stop precedence, legacy diagnostic projection, and failure-receipt compatibility.

Related investigation integration also preserves the CLI's compaction phase across connection updates and refreshes an explicitly rejected stale Hand route once while retaining the same effect identity. Ambiguous transport/server failures are not automatically redispatched.

The original session's compaction succeeded at 23:15:03 UTC after earlier masked HTTPS failures. The original upstream rejection body was discarded, so these fixes do not establish that the historical HTTPS failures were caused by an image rejection.
