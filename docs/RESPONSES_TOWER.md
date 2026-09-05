# Responses transports and Tower architecture

Status: implemented.

## Ownership and public composition

`OpenAi::new(auth)` creates the standard client recipe with `gpt-5.6-sol`.
`OpenAi::builder(auth)` exposes the closed Sol/Terra/Luna/Astra model choice plus
transport, storage, history, reasoning, wire namespace, and Tower policy. The
optional wire namespace applies only to API-key HTTPS OpenAI routing gateways;
it changes no provider or model semantics and never expands the typed model
family. `Nanocodex::builder(openai)` then adds agent instructions, tools,
workspace, session identity, and lifecycle policy while keeping driver
mechanics private.

`build()` requires an active Tokio runtime, spawns one stateful driver, and
returns `(Nanocodex, AgentEvents)`. The driver owns mutable conversation,
model, tool-runtime, and Tower service state. Each accepted prompt returns a
`Turn`; `turn.result()` is independent from the optional event stream.

One driver reuses its selected transport policy, server response chain, typed
history, code-mode runtime, shell sessions, and prompt-cache identity across
follow-on turns. A WebSocket policy also reuses its connection. The caller does
not replay earlier results.

The selected Sol, Terra, Luna, or Astra model may be changed only before the
first turn is accepted and is fixed after conversation activity begins. Upstream
Codex permits model changes on later turns at commit
[`acd540f1`](https://github.com/openai/codex/commit/acd540f1581bf30f963fccbcce43ac494102242c):

- [`TurnStartParams::model`](https://github.com/openai/codex/blob/acd540f1581bf30f963fccbcce43ac494102242c/codex-rs/app-server-protocol/src/protocol/v2/turn.rs#L122-L136)
  applies to the current and subsequent turns, exactly like reasoning effort.
- [`ThreadSettingsUpdateParams::model`](https://github.com/openai/codex/blob/acd540f1581bf30f963fccbcce43ac494102242c/codex-rs/app-server-protocol/src/protocol/v2/thread.rs#L236-L251)
  updates subsequent turns without starting a turn.
- On `turn/start`, Codex folds the requested model into thread settings, applies
  those settings while constructing the new turn, and stores the resulting
  session configuration for later turns
  ([request mapping](https://github.com/openai/codex/blob/acd540f1581bf30f963fccbcce43ac494102242c/codex-rs/app-server/src/request_processors/turn_processor.rs#L542-L560),
  [turn-boundary application](https://github.com/openai/codex/blob/acd540f1581bf30f963fccbcce43ac494102242c/codex-rs/core/src/session/handlers.rs#L184-L205),
  [persistent state update](https://github.com/openai/codex/blob/acd540f1581bf30f963fccbcce43ac494102242c/codex-rs/core/src/session/turn_context.rs#L597-L627)).
- Codex's own integration test verifies that a settings-only model update sends
  no model request and that the next turn uses the updated model
  ([test](https://github.com/openai/codex/blob/acd540f1581bf30f963fccbcce43ac494102242c/codex-rs/app-server/tests/suite/v2/thread_settings_update.rs#L32-L92)).

Nanocodex deliberately does not expose that behavior. A model change cannot
continue the prior provider checkpoint safely, so it would invalidate
`previous_response_id` and replay the complete client-owned history. Keeping a
thread on its creation-time model preserves incremental context reuse and
avoids that inefficient replay.

The standard policy is WebSocket plus incremental history and `store: false`
for both API-key and ChatGPT subscription authentication. API-key callers can
opt into durable provider checkpoints with `.store(true)`; ChatGPT
subscription authentication cannot. Selecting HTTPS with storage disabled
automatically selects full replay. WebSocket is the interactive default because
its reused connection has the lowest measured warm first-event latency. Native
callers can select HTTPS when cold start or fresh-fork startup matters more,
but a session and every fork retain the one policy selected at build time. See
[`RESPONSE_TRANSPORT_BENCH.md`](RESPONSE_TRANSPORT_BENCH.md) for the measured
tradeoffs.

`ResponsesClient<S>` is generic over `Service<ResponsesAttempt>`. The
`OpenAi` builder applies caller layers when each independent session service is
constructed:

```rust,ignore
use std::time::Duration;

use nanocodex::{Nanocodex, OpenAi};
use tower::{limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};

let openai = OpenAi::builder(std::env::var("OPENAI_API_KEY")?)
    .layer(TimeoutLayer::new(Duration::from_secs(180)))
    .layer(ConcurrencyLimitLayer::new(1))
    .build()?;

let (agent, events) = Nanocodex::builder(openai)
    .instructions(
        "You are a Rust coding agent. Preserve unrelated work and run relevant tests.",
    )
    .build()?;
```

`OpenAiBuilder::service` replaces the standard stack with a factory for a fully
caller-composed service. Its API documentation contains the complete compiling
`tower::service_fn` adapter shape. Every root, cancellation replacement, child,
and fork receives independent mutable service state. Neither path requires
boxing, a process server, JSONL, or a global client.

Reasoning and fast-mode values configured on `OpenAiBuilder` become defaults
for the agent recipe. Calling the corresponding `NanocodexBuilder` method later
overrides that value for the owned agent.

## Tower operation boundary

One Tower call is one complete logical Responses attempt:

```rust,ignore
Service<ResponsesAttempt,
        Response = ResponsesServiceResponse,
        Error = ResponsesServiceError>
```

The call future receives through `response.completed`. Connect, send, stream,
idle, API, and premature-close failures are therefore visible to timeout,
retry, metrics, tracing, and error-mapping layers. Returning success after only
sending a frame would make those policies incorrect.

`ResponsesAttempt` is an owned replay snapshot. Large history is shared by
`Arc`; cloning an attempt does not deep-clone the conversation. Incremental
history sends only the new delta with `previous_response_id`. Full-replay
history serializes the complete committed conversation. A replacement
ephemeral socket invalidates its connection-local ID and also replays history.

Only completed responses enter history. Failed partial output cannot execute a
tool or be replayed, so retry cannot duplicate a partial side effect.

## Standard resilience

The default stack is one typed retry owner around one configured transport:

```text
ResponsesRetryPolicy
  -> ResponsesService
       -> ResponsesSocket | HTTPS/SSE request
```

Generation and compaction receive at most five attempts. Transient connection,
handshake, send, receive, idle, premature-close, rate-limit, overload, and
server failures may retry. Authentication, malformed protocol, invalid request,
policy, quota, usage-limit, and context failures remain terminal. Server delay
hints override bounded exponential backoff.

Reconnect preserves the stable prompt-cache key and client-owned history,
drops a connection-local `previous_response_id`, and forces full-history
replay. HTTPS with `store: false` always replays because it has no
connection-local checkpoint. Prompt caching is an optimization, not the
history source of truth.

## Caller middleware

| Concern | Placement | Rule |
| --- | --- | --- |
| Whole-call deadline | Outside retry | Bounds stream, retries, and backoff together. |
| Per-attempt deadline | Inside retry | Retry only through deliberate typed classification. |
| Concurrency | Normally limit to one per agent | One response chain is sequential; use separate agents for parallel branches. |
| Load shedding | Outside concurrency limit | Reject rather than create another hidden queue. |
| Rate limiting | Usually outside retry | Decide explicitly whether retries consume budget. |
| Buffering | Avoid by default | The owned driver already has a bounded prompt queue. |
| Tracing and metrics | Around `ResponsesAttempt` | Separate logical calls, attempts, retries, reconnects, and backoff. |
| Circuit breaking | Application layer outside retry | Scope shared outages by endpoint/account. |
| Error mapping | Application boundary | Preserve typed retry classification below it. |

`tower-http::TraceLayer` is not directly applicable because the service carries
`ResponsesAttempt`, not `http::Request`. Use a generic Tower layer and the typed
event stream. The library must not install a tracing subscriber.

## Typed history and allocation policy

Known Responses items use typed enums and retain their API fields, including
output annotations and logprobs. Unknown item kinds remain forward-compatible
at an explicit opaque boundary rather than turning all history into
`serde_json::Value`.

The common path shares complete history and borrows prefix/history/tail slices
during serialization. Repairs, truncation, and compaction allocate only on
their explicit rewrite paths. Buffer pools, SIMD JSON, and small-vector changes
require a representative retained-trace win before entering production.

The 2026-07-18 M1 Max snapshot established the useful orders of magnitude:

| Workload | Result |
| --- | ---: |
| Direct async dispatch | 9.64 ns |
| Generic Tower dispatch | 10.54 ns |
| Concurrency-limit + timeout stack | 76.95 ns |
| 128 KiB serde request encoding | 71.5 us |
| 128 KiB typed history decode | 240 us |
| 128 KiB `Value` history decode | 305 us |
| 128 KiB typed history clone | 30.8 us |
| 128 KiB `Value` history clone | 169.6 us |
| Attempt history `Arc` clone | 10.2 ns |
| 622 KiB raw-payload JSONL decode | 0.67 ms |
| 622 KiB `Value`-payload JSONL decode | 1.70 ms |

Tower overhead is negligible beside serialization and model/network latency.
Typed history materially improves clone cost, and raw retained event payloads
avoid unnecessary DOM parsing. Sonic and simd-json remain benchmark-only
because neither won the complete immutable-input request path.

Run the portable benchmarks with:

```sh
cargo bench -p nanocodex-oai-api --bench tower_responses
```

Add `NANOCODEX_BENCH_EVENTS=/path/to/events.jsonl` to include a retained JSONL
trace without checking private runtime data into the repository.

The live [transport and storage benchmark](RESPONSE_TRANSPORT_BENCH.md) compares
WebSocket and HTTPS/SSE, stored checkpoints and `store: false`, full history
replay, and concurrent historical forks.

## Invariants

- A partial response commits no history and executes no tool.
- A replacement socket omits the dead socket's `previous_response_id` and
  replays full committed history.
- Stable prompt/cache identity and `store: false` survive retry and follow-on
  turns.
- Follow-on prompts reuse one socket and send only their new delta.
- Exactly one terminal event is emitted for every accepted prompt.
- Standard and caller-composed service factories use the same owned driver and
  typed event contract.
