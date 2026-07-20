# Visualizing Nanocodex traces

Nanocodex emits OpenTelemetry spans independently from its contractual typed
events. The quickest local visualization uses Jaeger: OTLP/HTTP spans enter on
port 4318 and the trace UI is available on port 16686. The checked-in Compose
service is ephemeral, binds only to localhost, and loses its trace data when it
is removed.

## Quick start

Start Jaeger:

```sh
just otel-up
```

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

With MCP configured, `mcp.server_start` and `mcp.tool_call` spans appear around
background discovery and remote dispatch. Useful fields include session and
model-call identity, response replay mode, connection purpose/generation,
attempt count, status, duration, input/output tokens, cached input tokens, and
tool name. Diagnostic spans retain structural metadata such as byte and item
counts, argument kinds and keys, process exit state, and API-visible reasoning
summaries. They never include full prompt bodies, hidden reasoning content,
tool argument values, raw response frames, or the API key.

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
session, the session-persistent Code Mode Node host, shell process lifecycle, MCP stdio transport,
local tracing subscriber, batch OTLP exporter, and Jaeger backend:

```sh
just otel-stress
```

The default workload runs 32 sequential prompts on one session. Every turn
fans out 16 concurrent MCP calls, then mixes in a synthetic MCP error, malformed
patch, non-zero shell, bounded high-volume output, yielded/resumed process, and
unknown JavaScript tool. The gate verifies event counts, exactly one trace root
per accepted prompt, all parent references, expected success/error span volume,
presence of structural model/tool fields and API-visible reasoning summaries,
and absence of prompt, hidden-reasoning, tool-argument, and API-key sentinels
from exported trace data.

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
cargo run --quiet --manifest-path bin/nanocodex/Cargo.toml -- \
  --otel-endpoint http://127.0.0.1:4318 \
  --otel-environment local-demo \
  --log-format json \
  --log-file .nanocodex/otel-demo/tracing.jsonl \
  run --thinking=low "Inspect the repository and summarize it."
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
