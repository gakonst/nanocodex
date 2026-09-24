# Responses transport and storage benchmark

`response-transport-bench` is a direct live-API experiment for separating the
effects of transport, server storage, incremental response IDs, client-owned
history replay, and historical forks. It is not a second Nanocodex runtime.

## Matrix

The benchmark holds the model, prompt prefix, prompt-cache key policy,
reasoning effort, response validation, and concurrent fork workload constant.
With OpenAI API-key authentication, it runs the seven supported combinations:

| Variant | Transport | `store` | Mainline history | Fresh fork history |
| --- | --- | ---: | --- | --- |
| `ws-store-checkpoint` | WebSocket | `true` | response ID | response ID |
| `ws-store-replay` | WebSocket | `true` | full replay | full replay |
| `ws-ephemeral-connection` | WebSocket | `false` | connection-local response ID | full replay |
| `ws-ephemeral-replay` | WebSocket | `false` | full replay | full replay |
| `https-store-checkpoint` | HTTPS/SSE | `true` | response ID | response ID |
| `https-store-replay` | HTTPS/SSE | `true` | full replay | full replay |
| `https-ephemeral-replay` | HTTPS/SSE | `false` | full replay | full replay |

HTTPS plus `store: false` cannot give a later request or a fresh fork a durable
server checkpoint, so there is no `https-ephemeral-checkpoint` row. The
WebSocket can reuse a response ID while that connection lives even with
`store: false`; a fork opens an independent connection and therefore replays
the complete committed history. This is the important Codex-like hybrid.
The local Codex reference builds ordinary OpenAI requests with `store: false`
in `codex-rs/core/src/client.rs`, sends the complete request through its HTTPS
path, and derives a response-ID plus input-delta request only when its
turn-scoped WebSocket sees a strict extension of the previous input.

Each retained history checkpoint is an immutable linked segment. Cloning a
checkpoint shares its entire prefix, while serialization walks the segments
oldest-first. Fork setup therefore does not deep-clone response items even when
the transport policy later requires a full replay.

## Authentication compatibility

The complete seven-row matrix requires `OPENAI_API_KEY`. ChatGPT subscription
credentials from `~/.codex/auth.json` use the Codex backend endpoints, attach
the ChatGPT account header, and deliberately send `store: false`. Consequently,
the compatible transport policies are:

| Policy | ChatGPT `auth.json` | Current Nanocodex runtime |
| --- | --- | --- |
| WebSocket, connection-local response ID, replay on a fresh fork | yes | yes |
| WebSocket, full replay | yes | yes |
| HTTPS/SSE, full replay | yes | yes |
| Any `store: true` checkpoint policy | no | no |

The benchmark executable itself currently exercises API-key authentication so
that every row can be compared in one run. The compatibility claims above come
from Nanocodex's auth-mode request construction and the reviewed local Codex
HTTPS and WebSocket request paths, not from reusing a ChatGPT access token
against `api.openai.com`.

## Library policy

Transport, storage, and history policy are selected when the agent is built and
are inherited unchanged by every clean child and historical fork:

```rust
use nanocodex::{Nanocodex, OpenAi};
use nanocodex::oai::transport::{ResponsesHistory, ResponsesTransport};

let openai = OpenAi::builder(std::env::var("OPENAI_API_KEY")?)
    .transport(ResponsesTransport::Https)
    .store(false)
    .history(ResponsesHistory::FullReplay)
    .build()?;
let (agent, events) = Nanocodex::builder(openai)
    .instructions(
        "Remember supplied deployment facts and preserve exact identifiers.",
    )
    .build()?;
```

HTTPS with `store: false` automatically selects full client-history replay.
Callers can explicitly select
`ResponsesHistory::{Incremental, FullReplay}` with
`OpenAiBuilder::history` for the other supported combinations. The builder
rejects `store: true` with ChatGPT subscription authentication and incremental
HTTPS history with `store: false`.

The native CLI/TUI fixes transport and storage policy at startup with
`--responses-transport` and `--store-responses`; history replay policy follows
that supported combination automatically.

## Measurements

For each request the JSON report records:

- exact serialized request bytes and encoding time;
- time to first streamed event and time through `response.completed`;
- input, cached-input, cache-write, and output tokens;
- WebSocket setup time where applicable.

It also normalizes cold start-to-first-event and completion, warm reused-client
medians, local fork-snapshot clone time, fresh-fork setup and start timing, and
concurrent mainline-plus-forks wall time. Every assistant reply is checked
against an exact expected token. Stored responses are deleted at the end unless
`FORK_BENCH_RETAIN=1`.

Run the complete default matrix with:

```sh
FORK_BENCH_OUTPUT=.nanocodex/benchmarks/response-transports.json \
  cargo run --release -p nanocodex-examples --bin response-transport-bench
```

Useful controls are:

```text
FORK_BENCH_TURNS
FORK_BENCH_FORK_TURNS             comma-separated, for example 2,4
FORK_BENCH_MAINLINE_CONTINUATIONS
FORK_BENCH_PREFIX_FACTS
FORK_BENCH_REPEATS
FORK_BENCH_VARIANTS               comma-separated names or all
FORK_BENCH_OUTPUT
FORK_BENCH_RETAIN
OPENAI_API_KEY
OPENAI_API_BASE_URL
OPENAI_RESPONSES_WEBSOCKET_URL
```

Repeated runs rotate variant order to reduce a fixed ordering bias.
