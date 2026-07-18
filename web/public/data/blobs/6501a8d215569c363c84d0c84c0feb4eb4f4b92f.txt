# Deferred Responses WebSocket Tower rewrite

Status: deferred. Do not change the runtime retry behavior as part of the current milestones. This note preserves the design constraints and reference findings for a later, dedicated Tower rewrite.

## Decision

The eventual rewrite should use Tower to make retry, backoff, and reconnection policy explicit, but it must operate on a complete logical Responses attempt—not on an individual WebSocket send and not on a stream handle returned before the response finishes.

Until that rewrite is intentionally started:

- Keep the current narrow reconnect behavior in [`src/model/agent.rs`](../src/model/agent.rs).
- Do not add another retry abstraction, HTTP fallback, or provider-generic client layer.
- Do not depend directly on Alloy's transport middleware. Reuse its policy ideas and typed error classification, not its JSON-RPC wire types.

## Protocol state that the rewrite must preserve

The stateful Responses WebSocket protocol is a sequence of `response.create` operations over a long-lived socket. Each operation is complete only when its terminal event arrives, normally `response.completed`; successfully sending the frame is not success.

Harness currently uses three distinct kinds of state:

1. **Connection-local response chain.** On a healthy socket, a generation can send only the strict input delta and set `previous_response_id` to the prior response on that socket.
2. **Opaque turn state.** The `x-codex-turn-state` response header can be carried into a replacement connection.
3. **Client-owned committed history.** This is the authoritative replay source because requests use `store: false`. `prompt_cache_key` is a cache optimization, not recovery state.

After a socket is replaced, the old `previous_response_id` must be treated as invalid. The first generation on the new socket must omit it and replay the full committed history. A stable cache key and any available turn state should still be retained.

A partial stream must not be committed to logical history. In particular, a close after deltas or `response.output_item.done`, but before `response.completed`, is a failed attempt. Harness currently executes tool calls only after completion, which prevents a retry from repeating a tool side effect. Any future change to execute output items earlier must introduce an equally strong commit discipline.

## Current harness behavior

The current implementation deliberately has only small, immediate recovery paths:

- Initial connection/warmup failure gets one fallback connection followed by a full first request.
- A send failure classified as a reconnectable closed WebSocket gets one immediate reconnect. The replacement request omits `previous_response_id` and replays full history.
- Receive, idle, protocol, and mid-stream failures are terminal.
- Compaction uses the same one-send-reconnect behavior.
- [`src/model/stream.rs`](../src/model/stream.rs) accumulates response items locally and returns them only after completion.
- [`src/error.rs`](../src/error.rs) contains only the narrow reconnectable-send classification; it is not a general retry policy.

This is safe enough for the present slice, but it is not the final policy. In particular, it has no backoff, server delay handling, shared attempt budget, or retry of transient receive failures.

## Reference findings

### Alloy

Inspected `~/github/alloy-rs/alloy` at commit `5cabb039`.

Alloy's `crates/transport/src/layers/retry.rs` is a real Tower middleware: `RetryBackoffLayer<P>` wraps a `Service<RequestPacket>`, clones the request for each attempt, applies a bounded retry budget, and delegates error classification and optional server delay extraction to a `RetryPolicy`. Policies compose with `or`.

Useful invariants to carry over:

- Classification is typed and separate from the retry loop.
- The retry budget belongs to the logical request.
- A server-provided backoff hint can override the local delay.
- Policy and mechanism are composable.

Details not worth copying directly:

- The service is coupled to JSON-RPC `RequestPacket`/`ResponsePacket` types.
- Its fallback delay is constant rather than exponential.
- Its compute-unit/concurrency offset solves an Alloy-wide shared-client problem that Harness does not currently have.
- Alloy's pubsub reconnect loop in `crates/pubsub/src/service.rs` is separate from request retry and uses fixed reconnect delays; it does not model Responses state.

### Codex

Inspected `~/github/openai/codex` at commit `f90e7deea6a715bbd153044af6f475eefa749177`.

Relevant files:

- `codex-rs/codex-client/src/retry.rs` defines bounded attempts, exponential backoff from 200 ms, small jitter, and separate switches for transport, 5xx, and 429 failures.
- `codex-rs/core/src/responses_retry.rs` owns the outer retry loop and lets a server-requested delay override local exponential backoff.
- `codex-rs/core/src/session/turn.rs` rebuilds a retry from cloned committed session history and preserves completed items while excluding the incomplete active item.
- `codex-rs/codex-api/src/sse/responses.rs` distinguishes context, quota, usage, policy, invalid-prompt, overloaded, and otherwise retryable response failures.
- `codex-rs/codex-api/src/endpoint/responses_websocket.rs` maps WebSocket, handshake, and API failures, including the retryable WebSocket connection-limit error.

Codex does not wrap its Responses WebSocket lifecycle in Tower; it has a purpose-built connection/session actor and manual retry loop. Its broad `CodexErr::is_retryable` classification should not be copied wholesale: malformed protocol data, authentication failures, and other permanent errors should remain terminal.

Codex can exhaust WebSocket retries, switch to HTTP, and reset its budget. That fallback is outside Harness's deliberately narrower scope.

## Correct Tower boundary

The service operation should represent one complete logical attempt:

```rust,ignore
Service<ResponsesAttempt, Response = CompletedResponse, Error = ResponsesAttemptError>
```

Its future must drive the WebSocket through the terminal response event. This allows Tower retry policy to observe send, receive, idle, premature-close, API, and completion failures.

The tempting alternative is wrong:

```rust,ignore
Service<ResponsesAttempt, Response = ResponseStream>
```

That service reports success as soon as it returns the stream. Failures encountered while consuming the stream happen outside `Service::call`, so generic Tower retry cannot classify or recover them.

Likewise, reconnect middleware must never blindly replay a serialized incremental `response.create` frame. After reconnection, that frame may contain a `previous_response_id` belonging to the dead socket and only an input delta. Recovery has to re-encode the logical attempt as a full replay.

## Candidate component responsibilities

This is a design direction, not a commitment to exact type names or layering:

- `ResponsesAttempt` is an owned, replayable snapshot: attempt kind (warmup, generation, or compaction), committed history, current input/trigger, stable cache/profile data, and available turn state.
- The inner attempt service owns or borrows the live connection, decides whether its response chain is valid, encodes an incremental request only on that valid connection, and consumes events through completion.
- A connection manager recreates the socket and invalidates all connection-local response-chain state.
- An outer retry policy owns classification, attempt budget, backoff, jitter, and server hints.

Do not lock in a literal `RetryLayer<ReconnectLayer<_>>` stack until the ownership and live-event path are worked through. The important ordering invariant is that retry wraps the logical attempt, while socket recreation remains below it.

Generic Tower adapters are useful primitives but are not a complete implementation:

- Tower `Reconnect` recreates an inner service; it does not know that a Responses retry needs a different request shape.
- Tower `Retry` retries based on the result of `Service::call`; it cannot see later stream errors unless the call future owns stream completion.
- `tokio-tower` multiplexing assumes tagged discrete request/response framing, unlike a Responses event stream whose boundary is a terminal protocol event.

## Error and delay policy requirements

The later policy should start narrow and typed.

Likely retryable:

- Connection close/reset and transient network failures.
- Transient DNS/connect/TLS failures where classification is available.
- Handshake HTTP 5xx.
- Send, receive, and event-idle timeouts.
- Premature close before a terminal Responses event.
- Known transient Responses errors such as server overload or connection limits.
- Rate limiting only when it is transient rather than exhausted quota/usage.

Terminal:

- Invalid configuration or URL.
- Authentication/authorization failures and HTTP 400/401/403.
- Malformed JSON, invalid wire payloads, and unsupported binary frames.
- Context-window, quota, usage-limit, invalid-prompt, and policy failures.
- Cancellation.

Respect `Retry-After` or an API-provided retry delay when present. Otherwise use bounded exponential backoff with jitter. Codex's five stream attempts and 200 ms initial delay are useful reference values, not yet Harness policy.

Every retry caused by a connection failure must invalidate the connection-local response chain and force full-history replay on the replacement socket. Retrying an API failure on a still-healthy socket may be able to retain connection state, but that case must be proven per error rather than assumed.

Warmup needs an explicit decision: either it consumes the same logical attempt budget or it remains best effort and generation owns a fresh budget.

## JSONL, observability, and cancellation

Harness emits exact raw events and user-visible deltas while the response is still in flight. A `Service` future that waits for completion therefore still needs a concrete live observer/writer path. Design that path without introducing a generic event bus, collector trait, or shared mutable run-state abstraction merely for Tower.

At minimum, retry diagnostics should make these facts reconstructable without exposing secrets:

- Logical request and attempt number.
- Phase: connect, handshake, send, receive, idle, API event, or completion.
- Typed error class and selected delay.
- Whether a new socket was opened.
- Whether the attempt used an incremental input or full replay.
- Connection generation and response-chain invalidation.

Raw events and deltas from a failed partial attempt have already crossed the JSONL boundary. They need an attempt identity (or an equivalent derivation rule) so ATIF construction cannot accidentally concatenate failed-attempt text with the successful replay. Logical history must still commit only completed responses.

Cancellation must interrupt backoff, connection establishment, and streaming promptly; terminate any owned work; and still preserve the JSONL contract of exactly one terminal event per accepted request.

## Validation plan for the future rewrite

Use deterministic fault injection for the policy boundary, followed by a real end-to-end Harbor trial:

1. Handshake 5xx retries within budget and then succeeds.
2. Permanent 400/auth failure does not retry.
3. A server delay hint overrides local backoff.
4. Closed-socket send failure opens a new socket, omits `previous_response_id`, and sends full history.
5. Mid-stream close after deltas/output items commits no partial history, executes no tool, and replays from committed history.
6. Compaction retry replays its full history and compaction trigger.
7. Exhaustion emits one terminal failure and accurate attempt/timing statistics.
8. Cancellation during backoff, connect, and stream returns promptly.
9. Opaque turn state is carried to a replacement connection when available.
10. Cache key and cacheable prefix stay stable across attempts.

## Explicit non-goals

- HTTP fallback after WebSocket exhaustion.
- Direct use of `alloy-transport` or its JSON-RPC request types.
- Retrying raw WebSocket frames.
- Provider portability or a generic provider/client hierarchy.
- A process-global concurrency or compute-unit throttle.
- Early execution of partially streamed tool calls.

## External primitive references

- [Tower reconnect module](https://docs.rs/tower/latest/tower/reconnect/index.html)
- [Tower retry module](https://docs.rs/tower/latest/tower/retry/index.html)
- [`tower-resilience-reconnect`](https://docs.rs/tower-resilience-reconnect/latest/src/tower_resilience_reconnect/lib.rs.html)
- [`tokio-tower` multiplex module](https://docs.rs/tokio-tower/latest/tokio_tower/multiplex/index.html)

No existing Tower adapter found during this review implements the stateful OpenAI Responses WebSocket lifecycle through `response.completed`; the protocol-specific attempt and replay semantics remain Harness-owned.
