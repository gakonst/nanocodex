# Does connecting warm the first prompt?

**Not enough today. The connected Durable Object warms personalization and event delivery, but the model runtime and its Responses connection are still started by the first prompt.** A fresh six-agent paired experiment confirms that waiting five seconds after event readiness does not remove those costs.

## Paired production experiment

Three immediate-prompt cases and three five-second-idle cases, alternating order within pairs. New scratch agent and client authority cache each time; same `gpt-6-astra` low/standard settings and exact no-tool `42` prompt. Five seconds is deliberately longer than the typical connection cost seen in the preceding cohort. This tests the deployed behavior; no implementation or setting was changed.

| Pair | Idle after ready | Prompt→first text | Model connection during turn | Admission | Request warmup |
|---|---:|---:|---:|---:|---:|
| 1 | 0 s | 8,907 ms | 4,863 ms | Not captured | None |
| 1 | 5 s | 4,005 ms | 1,128 ms | 579 ms | None |
| 2 | 5 s | 4,546 ms | 1,595 ms | 616 ms | None |
| 2 | 0 s | 4,510 ms | 1,628 ms | 673 ms | None |
| 3 | 0 s | 3,998 ms | 1,186 ms | 640 ms | None |
| 3 | 5 s | 5,043 ms | 2,053 ms | 901 ms | None |

Median prompt→first text: **4,510 ms immediate versus 4,546 ms after five seconds idle**. The first immediate case's 4,863 ms connection outlier is retained. Three pairs do not establish statistical equivalence, but every delayed case still paid a new model connection after the prompt. The evidence does not support a claim that idle waiting currently prewarms it.

All six streams delivered only `ready` before the prompt. All six turns completed with `42`, reported zero warmup duration/usage, and emitted no model-warmup events. Five of six admission traces were captured; the diagnostic tail was restarted too late for the first immediate case, so its admission cell remains missing. Full model telemetry exists for all six. Recorded delayed admissions still show 17 session-state and two ownership reads, the fresh-agent path.

[Individual samples and correlated traces](warmup-measurements.json). All six agents were deleted and subsequently returned 404; [cleanup and deployment receipts](warmup-cleanup.json). Deployments remained unchanged.

## What the source actually does

1. [`Session.#upgrade`](../../../js/managed/src/index.ts#L4260) accepts the event WebSocket, calls `#warmPersonalization`, and sends `ready`. It does not call `#ensureAgent` or establish a Responses connection. A live event stream is therefore not evidence of a prepared model session.
2. [`#startMeasuredManagedTurn`](../../../js/managed/src/index.ts#L6084) calls `#ensureAgent` on first prompt, preparing account context in parallel. [`#ensureAgent`](../../../js/managed/src/index.ts#L6767) refreshes hosted tools/MCP and then constructs the runtime. It performs that refresh even when a runtime already exists unless the specific `reuseReady` option is used.
3. [`Cloudflare Agent`](../../../js/nanocodex/cloudflare/Agent.mjs#L318) enables socket preconnection, but only when this runtime is constructed. Managed configuration sets `waitForPreconnect:false` to keep voice/session control from waiting on the separate Responses connection. That does not make construction happen on event connect.
4. **Socket connection and request-state warmup are separate.** The hosted transport does not opt into `websocketWarmup`. The WASM configuration defaults that flag to false ([configuration](../../../js/nanocodex/src/wasm.rs#L874)). The Rust engine supports warmup, but its current execution hook is inside turn execution ([warmup phase](../../../crates/nanocodex-agent/src/model/run/turn.rs#L567), [guard](../../../crates/nanocodex-agent/src/model/run/lifecycle.rs#L108)). Simply flipping the option would insert a request into the first-turn path; it would not implement advance preparation.
5. Runtime idle shutdown defaults to **30 seconds** ([timeout](../../../js/managed/src/index.ts#L9579)). The [alarm's retention condition](../../../js/managed/src/index.ts#L3839) considers active/recoverable work, activity time and voice session state, not merely an open passive event socket. Any new preparation needs an explicit bounded lifetime, rather than assuming an event socket keeps the runtime ready forever. The deployed override was not independently queried; 30 seconds is the source default.

These findings use Nanocodex source `68a7f4edc` and the measured production deployment documented in the main report.

## OpenAI request-state warmup and Codex

OpenAI documents an optional Responses WebSocket `response.create` request with `generate:false`. It prepares known instructions/tools/messages without producing model output, and its returned response ID can be reused through `previous_response_id`. That is more than merely opening the socket. The documentation does not quantify a guaranteed TTFT improvement or establish GPU/KV-cache behavior. [Official WebSocket documentation](https://developers.openai.com/api/docs/guides/websocket-mode#connect-and-create-responses).

The checked-out Codex implementation starts this work during session startup:

- `codex-rs/core/src/session/session.rs:1816` schedules startup prewarm after initial MCP runtime setup.
- `codex-rs/core/src/session_startup_prewarm.rs:186` schedules a background task; its inner function builds a tool/instruction snapshot without user input and calls `prewarm_websocket`.
- `codex-rs/core/src/client.rs:1912` waits for that non-generating warmup to complete so the subsequent turn can reuse its connection/response state. The code also exposes a distinct connection-only preparation function.

Local Codex checkout is clean at `1427825c4044d48b513c7d4ea32b84e58806a188`. The installed benchmark CLI reports 0.154.0; this source inspection is pinned separately, not asserted to be a binary-identical checkout of that installation.

## Recommendation supported by this pass

Start a **bounded preparation task when an authorized client opens the active conversation**: prepare the runtime/account snapshot and start its model socket before the user submits. Keep it owned by the same session and reuse it only while settings, authority and tool context remain compatible. Passive/background subscriptions should not automatically keep every agent active indefinitely.

Then compare **connection-only preparation** against **connection plus `generate:false` request-state warmup**. Preparation should finish off the prompt path when time permits; a prompt arriving immediately should join the existing work rather than start another preparation. The current first-turn costs identify the target: admission around 0.5–0.9 s and typically 1.1–2.4 s model connection, with larger outliers. These are measured costs, **not an already demonstrated additive saving**. The incremental benefit and resource cost of OpenAI request warmup in Nanocodex remain unmeasured until that separate path is implemented and tested.

For voice, continue opening the realtime media path independently. The real-audio cohort already accepted speech 869 ms before durable admission completed. Responses request warmup should not become another voice-ready prerequisite; `gpt-live-1-codex` call/media readiness is a separate connection path.

No prewarm implementation or deployment was made in this measurement pass.
