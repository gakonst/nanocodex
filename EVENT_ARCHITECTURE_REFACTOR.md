# Event and model-run architecture refactor

## Purpose

This document is a handoff for a small refactor of the current model execution,
event emission, and run-accounting code. The primary goal is to reduce slop and
production LOC while making ownership obvious. It is not a request to reproduce
Codex's full session architecture.

The intended result is:

- one type owns a model run's lifecycle and mutable accounting;
- internal events are typed instead of `kind: &str` plus an arbitrary payload;
- JSONL sequencing and serialization stay at the process boundary;
- `tracing` is reserved for diagnostics and telemetry, not used as the JSONL
  event bus;
- pure wire construction and decoding remain ordinary free functions;
- the refactor is net-negative in production Rust LOC.

## Bottom line

This can reduce LOC, but only if it stays deliberately smaller than Codex.

A literal Codex-style port with an async event channel, event-sink traits,
subscriber layers, `Arc<Mutex<_>>` session state, multiple protocol adapters,
and separate client/session abstractions would add substantial code. None of
that is justified for this harness's current single-request, single-provider
runtime.

The useful ideas to borrow from Codex are ownership boundaries and separation
of concerns, not its amount of infrastructure.

At the time of writing, the directly affected production files contain 1,025
lines:

| File | LOC |
| --- | ---: |
| `src/protocol.rs` | 120 |
| `src/responses.rs` | 150 |
| `src/model/mod.rs` | 222 |
| `src/model/agent.rs` | 356 |
| `src/model/stream.rs` | 177 |

`src/model/wire.rs` is excluded because its 361 lines are mostly API schema and
should not materially change in this refactor.

Acceptance guardrails:

- Do not increase total production LOC in the five files above, adjusted only
  for a clearly justified file move.
- A reasonable target is at most about 925 lines, roughly 100 lines deleted.
- Do not add an event channel, event-sink trait, provider abstraction, custom
  tracing collector, or mutex-backed run statistics.
- Do not change the JSONL contract or remove raw API event preservation to hit
  the LOC target.

If the implementation becomes longer or introduces more concepts than it
removes, stop and simplify it.

## Current problems

The thin public entry point in `src/lib.rs` and the mode dispatcher in
`src/modes/mod.rs` are not the problem. Small free functions are idiomatic when
they are simple entry points or pure transformations.

The problem is the cluster in `src/model/agent.rs`:

- `events`, `config`, and `run_stats` are repeatedly passed through the model
  loop, connection helper, model-call helper, and tool-call helper.
- The socket, task, configuration, event stream, and accounting collectively
  describe one model-run lifecycle, but no type owns that lifecycle.
- `EventWriter::emit(kind, payload)` permits invalid event-name/payload
  combinations and leaks wire serialization into model logic.
- Run accounting is procedural plumbing: callers receive and mutate an
  unrelated `&mut RunStats` instead of operating on an owning object or
  returning an operation result.
- `#[allow(clippy::too_many_arguments)]` on the central model-call path is a
  symptom of the missing owner.
- Event payload definitions are spread across the run, agent, and stream
  modules, with repetitive one-use structs and derives.

`ResponsesSocket::connect` is already correctly associated with a stateful
transport type. The awkward free `agent::connect` helper exists because the
run-level event and timing concerns have no owner.

## What Codex actually does

The reference inspected for this proposal is the local OpenAI Codex checkout at
commit `f90e7deea6`, under `~/github/openai/codex/codex-rs`.

Codex separates three paths that should not be conflated:

```text
                              typed protocol path
model/tools -> Session::send_event(EventMsg) -> async event channel
            -> app-server translation -> surface processor -> JSONL/UI

                              diagnostic path
model/tools -> tracing spans/events -> stderr and OpenTelemetry

                              accounting path
Responses completion -> SessionState/TurnTimingState -> usage/timing snapshot
                     -> typed TokenCount/TurnComplete events
```

### Typed agent events

`Codex` is documented as a queue pair: callers submit operations and receive
typed events. See `codex-rs/core/src/session/mod.rs` around `struct Codex`.

The producer side lives on `Session`, which owns a `Sender<Event>`, mutable
`SessionState`, and session services. Events are `Event { id, msg: EventMsg }`,
where `EventMsg` is a large tagged enum in
`codex-rs/protocol/src/protocol.rs`.

`Session::send_event` is the semantic boundary. It records/persists the event,
updates related session behavior, and sends it through the event channel. A
consumer calls `Codex::next_event`. The app server then maps core events to its
own typed notifications. `codex exec --json` finally maps those notifications
to a smaller `ThreadEvent` enum and serializes one JSON object per line.

Codex therefore has a bespoke domain-event system. It does not use `tracing`
records as its user-visible event protocol.

Relevant reference locations:

- `codex-rs/core/src/session/mod.rs`: `Codex`, `submit`, `next_event`, and
  `Session::send_event`;
- `codex-rs/core/src/session/session.rs`: `Session` ownership;
- `codex-rs/protocol/src/protocol.rs`: `Event` and `EventMsg`;
- `codex-rs/app-server/src/request_processors/thread_lifecycle.rs`: consumption
  of `next_event`;
- `codex-rs/app-server/src/bespoke_event_handling.rs`: translation into typed
  server notifications;
- `codex-rs/exec/src/event_processor_with_jsonl_output.rs`: stateful JSONL
  surface adapter;
- `codex-rs/exec/src/exec_events.rs`: the `ThreadEvent` output union.

### Tracing and telemetry

Codex configures `tracing_subscriber` independently of the protocol event path.
The exec surface writes formatted tracing diagnostics to stderr and can attach
OpenTelemetry layers. Model connection and stream operations use spans and
instrumented methods.

Tracing is appropriate for debug messages, internal spans, latency
distributions, and OTEL export. It is not appropriate as this harness's
authoritative JSONL source because tracing records can be filtered, are not the
domain protocol, do not enforce exactly one terminal event, do not naturally
propagate output failures, and do not assign this protocol's request ID and
monotonic sequence.

### Accounting

On a completed Responses request, Codex records exact response usage into
session-owned state and emits a typed token-count snapshot. Timing belongs to a
dedicated `TurnTimingState`; RAII guards classify sampling and tool-blocking
time. `TurnContext` owns the timing state.

Codex uses locks and shared ownership because it supports long-lived sessions,
steering, concurrent work, and multiple frontends. This harness should borrow
the ownership idea without borrowing the concurrency machinery.

### Stateful model transport

Codex has a session-scoped `ModelClient` and a turn-scoped
`ModelClientSession`. WebSocket connection, reconnection, and streaming are
methods on those state owners. Its high-level `run_turn` is still a free async
function, demonstrating that free functions are not themselves the
anti-pattern.

The relevant distinction is:

- behavior that depends on cohesive mutable state belongs on its owning type;
- orchestration entry points and pure transformations may remain free.

### Important difference from this harness

The harness contract requires preserving every exact inbound and outbound API
event. Codex primarily operates on normalized `ResponseEvent` values and emits
optional raw response items/completion information; it is not a template for
removing this harness's raw transport record.

The refactor must retain an explicit raw API event variant carrying the exact
JSON value.

## Proposed narrow design

### 1. Add one model-run lifecycle owner

Introduce one private `ModelRun` (or `AgentRun`) type. It should own or borrow
the cohesive state for one accepted model-mode request:

```rust
struct ModelRun<'a, W> {
    events: &'a mut JsonlEvents<W>,
    task: &'a Task,
    config: &'a ModelConfig,
    stats: RunStats,
}
```

Its impl should contain the operations that currently need the same repeated
arguments:

```rust
impl<W: Write> ModelRun<'_, W> {
    async fn run(&mut self) -> Result<()>;
    async fn connect(&mut self) -> Result<ResponsesSocket>;
    async fn perform_model_call(
        &mut self,
        socket: &mut ResponsesSocket,
        input: &[InputItem],
        previous_response_id: Option<&str>,
    ) -> Result<ModelResponse>;
    async fn execute_function_calls(
        &mut self,
        function_calls: &[FunctionCall],
        call_index: u32,
    ) -> Result<Vec<InputItem>>;
}
```

The exact signatures may differ to satisfy borrowing cleanly. The invariant is
that events, configuration, task context, and stats stop appearing as repeated
parameters. Passing the socket or immutable per-call input remains fine.

Do not introduce a top-level `Harness` object merely to turn the existing
four-line public entry point into a method. That would add ceremony without
creating meaningful ownership.

### 2. Make internal events typed

Replace `emit(kind: &str, payload: P)` at domain call sites with a private
tagged event enum. Prefer struct variants so the current collection of one-use
payload structs and repeated derives can be collapsed rather than supplemented:

```rust
#[derive(Serialize)]
#[serde(tag = "type", content = "payload")]
enum HarnessEvent<'a> {
    #[serde(rename = "run.started")]
    RunStarted {
        mode: &'static str,
        model: &'a str,
        // ...
    },
    #[serde(rename = "api.event")]
    ApiEvent {
        direction: &'static str,
        transport: &'static str,
        event: &'a serde_json::Value,
        // ...
    },
    // ...
}
```

The JSONL writer should accept `HarnessEvent`, wrap it with
`protocol_version`, `request_id`, and `seq`, write one line, and flush. The
writer remains concrete and synchronous for now.

This keeps the valuable part of the existing `EventWriter`: one owner for
sequencing, serialization, flushing, and output-error propagation. It removes
the stringly API and prevents event-name/payload mismatches.

Do not add an `EventSink` trait or async writer task. There is only one JSONL
surface and one provider. A trait or channel would increase LOC and complicate
propagation of writer failures. If later work creates a real second consumer,
that requirement can justify a new abstraction then.

If one large event enum becomes less readable than the deleted payload structs,
keep a small number of named payload types. Typed correctness and net
simplicity matter more than forcing everything into one syntactic form.

### 3. Let the run own accounting

Keep `RunStats` as a plain value owned by `ModelRun`. Model-run methods update
`self.stats`; callers should not pass `&mut RunStats` separately.

Parallel tool futures should return a complete outcome containing the result
and elapsed duration. The owning run reduces those outcomes into its stats and
emits corresponding result events after completion. No shared mutex or atomic
accounting is needed.

For example:

```rust
struct ToolExecution {
    outcome: ToolOutcome,
    duration_ns: u64,
}
```

Do not introduce generic timing guards solely to eliminate two calls to
`Instant::now()`. A dedicated RAII timing type is only worthwhile once it
replaces enough duplicated error/success accounting to be net-negative.

### 4. Keep transport responsibilities narrow

`ResponsesSocket` should continue to own WebSocket framing, ping/pong handling,
timeouts, JSON decoding, and connection metadata.

Run-semantic events such as `model.connection.started` and aggregate run stats
belong to `ModelRun`, not `ResponsesSocket`. The socket should not know about
the harness JSONL protocol.

The response accumulator may remain a small private struct. Move its receive
loop into an impl only if doing so removes parameter plumbing or clarifies a
real state boundary. Moving the same code between files without deleting
concepts is not a goal.

### 5. Add tracing only where it deletes or replaces diagnostics

Do not make adding `tracing` a prerequisite for this refactor. The current
runtime has little diagnostic logging, and adding dependencies, subscriber
initialization, spans, and a custom collector would increase LOC.

If tracing is introduced, use a minimal stderr subscriber initialized in the
binary and add spans only around operations that benefit from diagnostics,
such as connection, one model request, response receiving, and a tool batch.
JSONL protocol events and terminal metrics must remain explicit and typed.

Never derive authoritative run stats by scraping span fields or reacting to
span-close callbacks. Those stats are part of the persisted eval record and
should remain deterministic domain data.

## What should remain free functions

Do not turn every function into a method. Keep free functions where no
meaningful owner exists:

- the public `harness::run` process entry point;
- the small mode dispatcher;
- pure request/wire constructors in `model/wire.rs`;
- pure event decoding and validation;
- small time conversion helpers if they are shared;
- stateless tool argument parsing or error construction.

The target is coherent ownership, not object-oriented styling.

## Suggested implementation order

1. Capture a known-good model-mode JSONL stream and Harbor result for
   comparison.
2. Introduce `ModelRun` and move `run`, connection instrumentation,
   model-call accounting, and tool-batch accounting behind its impl.
3. Remove `&mut RunStats` and repeated event/config/task parameters from the
   internal call graph.
4. Introduce the typed `HarnessEvent` boundary and collapse one-use payload
   structs where that is a net LOC reduction.
5. Keep `JsonlEvents`/`EventWriter` as the only place that assigns sequence
   numbers and writes/flushed envelopes.
6. Run rustfmt and Clippy with warnings denied.
7. Run `just run` and inspect stdout/stderr separation and the full JSONL
   sequence.
8. Run a real `just eval`; inspect raw events, ATIF, Harbor result, trajectory,
   verifier output, usage totals, and timing metadata.
9. Compare production LOC and concept count against the baseline above. If it
   is not net-negative, simplify before handoff.

## Behavioral invariants

The refactor must preserve all of the following:

- stdout is flushed JSONL only;
- diagnostics go to stderr;
- every output event has protocol version, request ID, and monotonic sequence;
- every accepted request emits exactly one terminal event;
- exact inbound and outbound Responses API events remain in the stream;
- API secrets and `.env` contents are never emitted;
- tool caller linkage and structured tool output remain intact;
- independent nested tool calls still execute concurrently;
- model, effort, cache/token usage, response ID, latency, tool counts, retries,
  and compaction-related metadata remain trustworthy;
- Python remains only the Harbor lifecycle/ATIF adapter;
- native `just run` remains independent of Harbor and Docker.

## Review checklist

Reject the refactor if any of these are true:

- it replaces typed protocol events with tracing records;
- it introduces an event bus or abstraction with only one implementation;
- it adds `Arc`, `Mutex`, atomics, or async channels solely for run stats;
- it retains the same repeated parameters but merely moves functions into an
  impl block;
- it weakens exact raw API event preservation;
- it makes terminal-event emission less explicit;
- it increases production LOC without removing a comparably larger source of
  complexity;
- it copies Codex abstractions that solve long-lived-session or multi-frontend
  requirements this harness does not have.

Accept it when the model run has one obvious owner, event serialization has one
obvious boundary, stats have one obvious owner, pure helpers remain simple, the
eval behavior is unchanged, and the implementation is materially smaller.
