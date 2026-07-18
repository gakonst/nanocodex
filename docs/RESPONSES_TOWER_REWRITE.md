# Responses WebSocket Tower architecture

Status: implemented.

## Public ownership and composition

`Agent::new(api_key)` is the zero-configuration library entry point. It uses the
SDK's fixed model contract, standard system prompt, medium thinking, built-in
tools, persistent WebSocket transport, and bounded Responses retry policy.
`Agent::builder(api_key)` exposes only the useful high-level overrides:
`prompt`, `thinking`, `tools`, `workspace`, `session_id`, and `responses`.

`build()` requires an active Tokio runtime, spawns the stateful driver, and
returns `(Agent, AgentEvents)`: one cheap, cloneable prompt handle and the
single ordered event receiver. The driver is private and remains the sole owner
of mutable model, conversation, tool, and Tower service state. It stops after
every handle is dropped. Each submitted prompt immediately returns a `Turn`;
the driver's bounded command channel queues prompts submitted while a turn is
active. One driver therefore reuses the WebSocket, server response chain, local
typed history, code-mode runtime, and shell sessions across turns.
`prompt().await` waits only for the bounded command channel to accept the work;
the returned `Turn` remains independently awaitable through `turn.result()` and
produces the final typed `TurnResult`.

`AgentEvents` retains typed event kinds and lossless raw payloads for the CLI
adapter. A later events/observability slice will define filtering, callbacks,
and tracing policy without reopening the ownership boundary.

`ResponsesClient<S>` is deliberately generic over
`Service<ResponsesAttempt>`. It owns the caller's concrete service stack and
provides accessors and `map_service` without boxing or imposing a global stack.
`Responses::builder().layer(...)` defers composition until `Agent::build()`,
when the SDK can first construct the correctly configured standard service:

```rust,ignore
use std::time::Duration;
use harness_agent::{Agent, Responses, Thinking, Tools};
use tower::{
    limit::ConcurrencyLimitLayer,
    timeout::TimeoutLayer,
};

let responses = Responses::builder()
    // These layers wrap retry/backoff and each complete streamed attempt.
    .layer(TimeoutLayer::new(Duration::from_secs(180)))
    .layer(ConcurrencyLimitLayer::new(1))
    .build();

let tools = Tools::builder().web_search(false).build();
let (handle, events) = Agent::builder(api_key)
    .prompt("project-specific system prompt")
    .thinking(Thinking::High)
    .tools(tools)
    .session_id("stable-session-id")
    .responses(responses)
    .build()?;
```

`Responses::builder().service(stack)` is the lower-level escape hatch for a
fully caller-composed `Service<ResponsesAttempt>`. `ResponsesClient<S>` remains
generic over the concrete service type, so neither path boxes the stack. There
is no `run_with_responses_client` lifecycle helper and no requirement that
applications adopt a process server, JSON-RPC, or JSONL.

The crate boundaries mirror those ownership rules:

- `harness-core` is dependency-light and owns the shared public data model:
  prompts, event envelopes, model configuration, and the complete typed
  Responses request, server-event, usage, content, tool, and item model.
- `harness-service` owns Responses behavior: the OpenAI WebSocket, stream
  processing, typed transport errors, telemetry, and generic Tower
  client/service/retry API. It depends only on `harness-core` from this
  workspace and is usable by another orchestrator without `harness-agent` or
  the built-in tools.
- `harness-tools` owns tool execution and depends only on `harness-core` from
  the SDK crates.
- `harness-agent` composes core, service, and tools into the queued turn/session
  lifecycle. Its public re-exports keep the common high-level path ergonomic.

Socket pumping and stream state are private service implementation details.
Wire types live in `harness_core::responses`; the public behavioral surface is
expressed in terms of `RequestProfile`, `ResponsesAttemptFactory`,
`ResponsesAttempt`, `ResponsesClient<S>`, typed outputs/errors, and Tower
`Service`. This keeps typed protocol construction/inspection usable without a
socket while preventing higher-level components from depending on the socket
task itself.

## Correct Tower boundary

The Tower operation is one complete logical Responses attempt:

```rust,ignore
Service<ResponsesAttempt,
        Response = ResponsesServiceResponse,
        Error = ResponsesServiceError>
```

`Service::call` does not return success after sending a WebSocket frame. Its
future owns receive processing through `response.completed`, so retry, timeout,
load shedding, metrics, and error mapping see failures from connect, send,
streaming, idle handling, API errors, and premature close.

`ResponsesAttempt` is an owned replay snapshot. Large histories are shared with
`Arc<Vec<ResponseItem>>`; cloning an attempt is O(1) for history. A healthy
socket sends only the strict delta with `previous_response_id`. Any retry that
replaces the socket invalidates that connection-local ID and serializes the
complete committed history instead.

Only a completed response is committed. Failed partial output cannot execute a
tool or enter logical history, so replay does not duplicate side effects.

## Standard resilience policy

The default stack is:

```text
Tower Retry<ResponsesRetryPolicy>
  -> owned ResponsesService
       -> one persistent ResponsesSocket
```

Generation and compaction get at most five attempts. The typed error classifier
retries transient connection, handshake, send, receive, idle, premature-close,
rate-limit, overload, and server failures when the protocol supplies retry
advice. Authentication, malformed wire data, invalid requests/images, policy,
quota, usage-limit, and context failures remain terminal.

Server delay hints override bounded exponential backoff. Every reconnect keeps
the stable prompt-cache key and opaque turn state, opens a new socket, drops
`previous_response_id`, and forces full-history replay. Warmup is best effort;
generation can fall back to a normal full first request.

Requests continue to use `store: false`. Client-owned typed history is the
source of truth; prompt caching is only an optimization. Stable instructions,
tool definitions, contextual input order, and `prompt_cache_key` are preserved
across attempts and follow-on turns.

## Middleware worth composing

Tower gives embedders useful policy without putting every policy in the core
SDK:

| Concern | Recommended layer or boundary | Important ordering |
| --- | --- | --- |
| Responses retry/reconnect | `ResponsesRetryPolicy` around `ResponsesService` | Keep exactly one logical retry owner. |
| Whole-turn deadline | `TimeoutLayer` outside retry | Bounds connection, stream, retries, and backoff together. |
| Per-attempt deadline | `TimeoutLayer` inside retry | Makes a timed-out attempt eligible for an outer typed retry only if its error is classified deliberately. |
| Concurrency | `ConcurrencyLimitLayer` | Normally `1` per owned WebSocket; use separate agents for true parallel response chains. |
| Load shedding | `LoadShedLayer` outside the limit | Rejects immediately instead of growing hidden latency. The agent command channel already provides a bounded queue. |
| Rate limiting | Tower rate limit outside retry | Decide whether retries consume rate budget; usually they should. |
| Buffering | `Buffer` only for a concrete cross-task need | The agent's bounded command channel already owns prompt queueing and steering order. Avoid a second invisible queue. |
| Logging and tracing | A small `Layer<ResponsesAttempt>` around the stack | Record kind, model call, attempt, replay mode, duration, and error class; never record secrets or full prompt bodies. |
| Metrics | A result/timing layer plus emitted typed events | Count logical calls separately from attempts, retries, reconnects, and backoff. |
| Circuit breaking | Application-owned layer outside retry | Useful for shared upstream outages; scope by endpoint/account rather than by individual socket. |
| Bulkheads | Separate concurrency limits per agent/workload | Prevent one batch or tenant from occupying every connection. |
| Error mapping | `map_err` at the application boundary | Preserve typed retry classification below it. |

HTTP-specific `tower-http::TraceLayer` is not directly suitable because this
service carries `ResponsesAttempt`, not `http::Request`. A small generic Tower
layer or the existing typed event stream is the cleaner observability boundary.

Cancellation is ordinary future cancellation: dropping the driver/turn task
interrupts connection, stream, and backoff futures, while owned shell work keeps
its explicit process-group cleanup rules.

## Typed and allocation-conscious history

Repeated API history uses typed enums rather than `serde_json::Value`. Known
output text retains annotations and logprobs; shell, function, custom-tool,
tool-search, web-search, image-generation, audio, reasoning, and compaction
items preserve their API fields. Unknown item types remain forward-compatible
as `ResponseItem::Other(JsonValue)`, preserving every unknown field for replay
without putting an unstructured `Value` in known history variants.

The common prompt path shares an `Arc<Vec<ResponseItem>>`. Incremental call-ID
sets make the complete call/output-pair check O(1); a repaired copy is allocated
only for an incomplete pair. Tool output truncation and compaction are explicit
rare rewrite paths. Delta serialization borrows prefix/history/tail slices and
does not build a temporary combined `Vec`.

Benchmarks remain evidence-driven. On retained Codex- and Harbor-sized
fixtures, serde is the best complete request-encoding path, typed history is
cheaper than `Value`, and Tower dispatch is negligible beside JSON and
network/model latency. Sonic is narrowly faster for some isolated text-delta
decodes but loses on request encoding; simd-json loses on these immutable-input
workloads because it must copy before in-place parsing. Both stay benchmark-only
dependencies unless a representative end-to-end workload reverses that result.

The retained Criterion snapshot from 2026-07-18 on an Apple M1 Max measured:

| workload | result |
| --- | ---: |
| direct async dispatch, 128 KiB prompt | 9.64 ns |
| generic Tower service dispatch | 10.54 ns |
| Tower concurrency-limit + timeout stack | 76.95 ns |
| 128 KiB send-ready request / old send-copy path | 78.0 / 92.8 us |
| 128 KiB request: serde / sonic / simd-json | 71.5 / 95.9 / 104.0 us |
| 16 KiB text-delta decode: serde / sonic / simd-json | 2.86 / 2.66 / 4.58 us |
| 128 KiB history decode: typed / `Value` | 240 / 305 us |
| 128 KiB history encode: typed / `Value` | 84.4 / 83.4 us |
| 128 KiB history deep clone: typed / `Value` | 30.8 / 169.6 us |
| 128 KiB attempt history `Arc` clone | 10.2 ns |
| retained 622 KiB JSONL decode: raw / `Value` payload | 0.67 / 1.70 ms |
| retained 622 KiB JSONL encode: raw / `Value` payload | 0.097 / 0.482 ms |

Other Docker and Harbor work was active during this run, so the absolute
figures are a local snapshot rather than a release threshold; the within-group
comparisons are the useful result. The real JSONL group consumes an existing
retained Harbor stream rather than checking private trace contents into the
repository:

```sh
HARNESS_BENCH_EVENTS=.harness/harbor/jobs/<job>/<trial>/agent/events.jsonl \
  cargo bench -p harness-service --bench tower_responses
```

Without that variable, the portable synthetic groups still run and the
retained-trace group is skipped. Use a quiet target machine before treating a
smaller difference as actionable.

An immediate post-rebase confirmation under variable CPU load measured direct
dispatch at 14.0 ns, the generic Tower service at 15.9 ns, and the
concurrency-limit plus timeout stack at 101 ns. The retained 622 KiB trace
measured raw/`Value` payload decode at 0.706/1.831 ms and encode at 0.117/0.923
ms. The absolute figures moved with the direct control, but the conclusions did
not: generic Tower dispatch remains negligible, while retaining raw event
payloads avoids material DOM parsing and re-encoding cost.

A live repeated-prompt probe used the same 128 KiB prompt three times on one
agent session. The first turn wrote 35,085 cache tokens; turns two and three
each reported 35,085 cached tokens and zero cache writes (99.991% of their
input), while retaining one WebSocket and recording no retry or reconnect.
Full turn times were 1.44, 2.58, and 1.82 seconds, so model/network variance
dominated the local dispatch costs.

Earlier in this worktree, three focused Harbor gates passed with reward 1.0:
`db-wal-recovery` (7/7 checks), `merge-diff-arc-agi-task` (5/5), and
`prove-plus-comm` (4/4). Each used one socket with no retry/reconnect, and their
retained stderr streams were empty. After the final rebase onto `master`, all
82 Rust tests, warnings-denied Clippy and rustdoc, formatting, and the native
Linux artifact build passed. The user waived another Harbor run, so the three
focused results are retained evidence rather than post-rebase eval claims.

## Validation invariants

- A close after partial deltas commits no partial history and executes no tool.
- A replacement socket omits the dead socket's `previous_response_id` and
  replays the full committed history.
- Cache key, stable prefix, `store: false`, and turn state survive retry.
- Follow-on prompts reuse one socket and send only their new user delta.
- Exactly one terminal event is emitted for every accepted prompt.
- Retry/connection events identify attempt, phase, error class, delay, replay
  mode, and connection generation.
- The standard and caller-composed builder paths use the same owned driver and
  typed event contract.
