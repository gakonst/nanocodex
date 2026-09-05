# Visualizing Nanocodex traces

Nanocodex emits OpenTelemetry spans independently from its contractual typed
events. The quickest local visualization uses Jaeger: OTLP/HTTP spans enter on
port 4318 and the trace UI is available on port 16686. The checked-in Compose
service is ephemeral, binds only to localhost, and loses its trace data when it
is removed.

Every model request asks for the model's `auto` API-visible reasoning summary. Its
streaming deltas remain typed `reasoning.summary.delta` agent events, while the
completed readable summary and encrypted continuation payload are recorded as
ordered `reasoning` content on the corresponding `model.call` span.

## Quick start

Start Jaeger:

```sh
just otel-up
```

Or start Jaeger and launch the interactive TUI with telemetry in one command:

```sh
just run-otel
```

`just run-otel` exports one compact streaming summary on each `tui.turn`. Use
`just run-otel-detail` when diagnosing a jagged stream; it additionally exports
every correlated API-delta, TUI-application, and presented-frame timing record.
The detailed mode is intentionally opt-in because a long response can produce
thousands of trace records.

`just` loads `OPENAI_API_KEY` from the repository `.env`. Interactive local
logs are written to `.nanocodex/logs/tui.log`; exported traces appear in
Jaeger at <http://localhost:16686> under the `nanocodex` service.

Run one live turn that makes a model call, executes the built-in `exec` tool,
and makes a follow-up model call:

```sh
just otel-demo
```

`OPENAI_API_KEY` must be present in the environment or the repository `.env`.
The demo writes the two independent diagnostic surfaces to:

- `.nanocodex/otel-demo/events.jsonl`: contractual `AgentEvents` encoded by the
  headless process adapter.
- `.nanocodex/otel-demo/tracing.jsonl`: local structured `tracing` output.

Open <http://localhost:16686>, choose the `nanocodex` service, and select **Find
Traces**. Open the newest trace and expand the waterfall. A successful tool turn
has this general shape:

```text
agent.turn
├── model.call
│   └── responses.attempt
│       └── responses.connect
├── tool.call
│   └── tool.execute
└── model.call
    └── responses.attempt
```

To inspect every turn in one conversation, enter `/trace` in the TUI. It opens
Jaeger's search page filtered to the focused main or `/btw` session. The local
Jaeger configuration also turns `session.id` and `parent.session.id` tags into
links to the corresponding session search. Jaeger returns the session as an
ordered search result containing one bounded trace per turn; it does not merge
the turns into one artificial waterfall. Set `NANOCODEX_JAEGER_UI_URL` when the
query UI is not available at the default `http://127.0.0.1:16686`.

The structure follows init4tech's
[`teaching-tracing` guidance](https://github.com/init4tech/teaching-tracing): a
root is one bounded unit of work, futures are instrumented before they are
spawned, and long-running task/session lifetimes are not held open as spans.
When a caller has no active span, `agent.turn` is the root. The TUI wraps each
interaction in a bounded `tui.turn`, so its agent work is directly expandable
in the same trace:

```text
tui.turn
├── agent.turn
│   ├── model.call
│   ├── tool.call
│   └── model.call
└── event: tui.stream.completed
```

`responses.attempt` measures socket-pump queue time, JSON parsing/decoding and
event-emission time, display-delta bytes, inter-delta gaps, and 50/100/250 ms
stall counts. The TUI summary continues the same event timestamps through agent
delivery, state application, frame coalescing, Ratatui changed cells, terminal
output bytes, and source-to-terminal-flush latency. Main and `/btw` turns keep
independent aggregates even when both panes are presented by one terminal
frame. Select `nanocodex_stream_timing=trace` (or use
`just run-otel-detail`) for the individual correlated records.

Code Mode promise fan-out and attached child agents appear as overlapping
sibling branches under the active cell/tool operation. A child or follow-up
prompt made outside an active orchestration is instead a new bounded root and
remains correlatable through `session.id`, `parent.session.id`,
`session.lineage_id`, `agent.origin`, and `agent.depth`.

With MCP configured, `mcp.server_start` and `mcp.tool_call` spans appear around
background discovery and remote dispatch. Useful fields include session and
model-call identity, response replay mode, connection purpose/generation,
attempt count, status, duration, input/output tokens, cached input tokens, and
tool name. `tui.view_state` traces record whether the TUI is main-only or split,
which pane is focused, the local BTW ID, and the forked session ID once it is
available. `tui.main.session_id` and `tui.active.session_id` correlate every
view transition with the controlled agent, while each bounded `tui.turn` trace
contains its `agent.turn` directly. Diagnostic spans retain structural metadata such as byte and item
counts, argument kinds and keys, and process exit state as searchable tags.
Their ordered Logs/Events contain the complete available conversation: prompts,
model input and output items, readable reasoning content and summaries, opaque
encrypted reasoning payloads, tool arguments, and tool outputs. Nanocodex does
not decrypt encrypted reasoning or reconstruct reasoning absent from the API.
It does not attach API-key configuration or read `.env` for telemetry, but
captured conversation and tool content is intentionally unredacted; secure and
expire Jaeger data accordingly.

Each turn is its own root unit of work so a long-lived embedded agent does not
produce one unbounded trace. Sequential turns remain searchable as a session
through their shared `session.id` attribute. Local logging and OTLP export have
independent filters: use `RUST_LOG`/`--log-filter` for the formatter and
`OTEL_LEVEL`/`--otel-filter` for exported spans.

Stop and remove the ephemeral backend when finished:

```sh
just otel-down
```

## Stress the complete path

The manual stress gate uses a deterministic local Responses WebSocket rather
than spending model tokens. It still drives the real CLI, retained Nanocodex
session, the session-persistent Code Mode Node host, shell process lifecycle,
MCP stdio transport, local tracing subscriber, batch OTLP exporter, and Jaeger
backend:

```sh
just otel-stress
```

The default workload runs 32 sequential prompts on one session. Every turn
first exercises the provider-native MCP `tool_search_call`/`tool_search_output`
pair, then fans out 16 concurrent calls to the discovered read-only tool. It
also mixes in a synthetic MCP error, malformed patch, non-zero shell, bounded
high-volume output, yielded/resumed process, and unknown JavaScript tool. The
gate verifies event counts, exactly one trace root per accepted prompt, all
parent references, expected success/error span volume, shared cell-actor
parentage and complete interval overlap for the delayed `Promise.all` fan-out,
model and terminal latency, exact token/cache fields, automatic
the selected GPT-5.6 model's cost, structural model/tool fields, API-visible reasoning
summaries, prompt/readable/encrypted reasoning/tool-argument sentinels in
ordered span events, and absence of the separately configured API key from
exported trace data.

The PR #50 milestone run on 2026-07-28 emitted 2,215 contractual events and
retained 32 validated turn roots. The identical 32-turn/16-way workload took
34.420 seconds without OTLP and 34.504 seconds with OTLP; exporter and Jaeger
validation then took 399 milliseconds. The observed workload delta was 84
milliseconds (0.24%); the deliberate one-second yielded subprocess in every
turn remained the dominant measured harness cost.

Scale it up without changing code:

```sh
just otel-stress 100 128
```

That maximum profile drives 100 retained turns and 12,800 successful concurrent
MCP dispatches plus the failure and process cases. The test is ignored during
the normal workspace suite because it requires the local Jaeger service and is
intentionally expensive. Its Responses side remains deterministic so failures
indicate library, tool, exporter, or topology regressions rather than model
sampling variance.

Run the focused attached-subagent topology gate separately:

```sh
just otel-subagent-stress
```

It drives two child agents concurrently from one Code Mode `Promise.all`, then
queries Jaeger to verify that both child `agent.turn` spans share the parent
trace, sit below their corresponding `spawn_agent` tool spans, retain their
independent session identities, and overlap in exported wall-clock intervals.

To measure the cost of span collection/export against the identical workload,
run the no-OTLP twin with the same arguments:

```sh
just otel-stress-baseline 100 128
```

Compare its reported `workload_elapsed` with `just otel-stress 100 128`. This
keeps model responses, tool inputs, process work, and event validation
identical; only the OTLP layer and Jaeger export are removed. The separate
`validation_elapsed` covers querying and checking Jaeger, whose cost grows with
the in-memory demo backend's retained trace volume and is not agent runtime.

## Run a different prompt

The CLI accepts the same exporter configuration directly:

```sh
cargo run --quiet -p nanocodex-bin --bin nanocodex -- \
  run \
  --otel-endpoint http://127.0.0.1:4318 \
  --otel-environment local-demo \
  --log-format json \
  --log-file .nanocodex/otel-demo/tracing.jsonl \
  --thinking=low "Inspect the repository and summarize it."
```

`--otel-endpoint` is a collector base URL. Nanocodex appends `/v1/traces`
unless it is already present. Keep the returned `ObservabilityGuard` alive when
using `nanocodex-observability` as a library; dropping or explicitly shutting
down the guard flushes its batched exporter.

## Troubleshooting

Check that the backend is running and the two local ports are reachable:

```sh
docker compose -f docker-compose.otel.yml ps
curl --fail http://127.0.0.1:16686/
```

If the service does not appear immediately, wait a moment and refresh after the
CLI exits. Export is batched and is flushed during observability shutdown. On a
multithreaded Tokio runtime, OTLP/HTTP export uses Tokio's asynchronous batch
processor; current-thread runtimes and applications without Tokio use the
dedicated-thread blocking fallback so synchronous shutdown cannot deadlock the
runtime. Use `docker compose -f docker-compose.otel.yml logs jaeger` to inspect
collector errors.
