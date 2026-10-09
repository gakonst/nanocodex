# Observability

Nanocodex emits structured tracing events and bounded OpenTelemetry spans.
Use any OTLP/HTTP collector with the local CLI or configure the
`nanocodex-observability` library in an embedding application.

## Local CLI

```sh
nanocodex --local run \
  --otel-endpoint http://127.0.0.1:4318 \
  --otel-environment local-demo \
  --log-format json \
  --log-file .nanocodex/otel-demo/tracing.jsonl \
  --thinking=low "Inspect the repository and summarize it."
```

`--otel-endpoint` takes a collector base URL; Nanocodex appends `/v1/traces`
unless it is already present. `--log-filter` (or `RUST_LOG`) controls local
tracing, and `--otel-filter` (or `OTEL_LEVEL`) independently filters exported
spans. Interactive local sessions default to persistent logs beneath
`$XDG_STATE_HOME/nanocodex/logs`, or `~/.local/state/nanocodex/logs`.

The CLI does not install a collector or launch a tracing UI. Configure
retention and access in the collector you operate.

## Trace structure

Each agent turn is a bounded unit of work. Model and tool calls are children
of that turn; Code Mode fan-out and attached child agents can overlap.
Follow-up work outside an active orchestration starts another bounded root.
Correlate these roots using `session.id`, `parent.session.id`,
`session.lineage_id`, `agent.origin`, and `agent.depth`.

MCP discovery and calls have `mcp.server_start` and `mcp.tool_call` spans.
Other diagnostic fields include model and response identity, connection
generation, retry count, status, duration, token usage, tool name, payload size,
and process exit state. Local TUI stream timing uses the
`nanocodex_stream_timing` target. Enable its `trace` level only when
investigating individual event delivery and rendering.

Captured model and tool content may contain private conversation data. The
exporter does not decrypt encrypted reasoning or reconstruct content absent
from the provider response. Protect exported data and set appropriate
retention.

## Embedded applications and shutdown

Keep the returned `ObservabilityGuard` alive while the application runs.
Dropping or explicitly shutting down the guard flushes batched exports.
Multithreaded Tokio applications use the asynchronous batch processor;
current-thread runtimes and non-Tokio applications use a dedicated-thread
blocking fallback to avoid shutdown deadlocks.

When spans do not arrive, check the configured collector endpoint and exporter
logs. Export is batched, so inspect the collector after a turn finishes and the
application flushes its guard.
