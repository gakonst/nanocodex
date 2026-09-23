# Inference critical path — 2026-09-22

Base: `45ccf07`. Changes are limited to admission storage, passive generation telemetry, JSON stream consumption, and idle discovery reuse. Provider/model selection is unchanged.

A session's first generation now commits its route pin and incremented request counter in one awaited storage write. Previously those required two writes before provider dispatch. The terminal counter write remains awaited. Tests block the admission write and prove generation cannot start early; a rejected write prevents generation and leaves the persisted session unpinned. Existing tests cover ownership, deletion, restart, pin stability, sanitized errors, timeout, and cancellation.

Managed idle shutdown still retires the runtime and model socket on the existing 30-second schedule. It now retains account catalog, prepared account information, and hosted-tool discovery until their existing 120-second deadlines. Repeated idle shutdown does not extend those deadlines. Other shutdown paths still invalidate discovery. Authorization keys and per-request capability projections remain enforced.

The three Durable Object/WASM lifecycle tests demonstrate a completed turn before and after idle, socket close/reopen, no repeated catalog/vault/hand discovery within the original TTL, refresh after expiry, and stale-epoch rejection. Their existing successful run is recorded in `output/inference-idle-runtime.log` (3/3; exit 0) and `output/inference-idle-typecheck.log` (exit 0).

Streaming validation is described in [streaming.md](streaming.md). This is structural and deterministic runtime evidence, not a live latency benchmark. No deployment or live provider requests were performed.
