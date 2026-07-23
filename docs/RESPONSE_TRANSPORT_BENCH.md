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
use nanocodex::{Responses, ResponsesTransport};

let responses = Responses::builder()
    .transport(ResponsesTransport::Https)
    .store(false)
    .build();
let (agent, events) = nanocodex::Nanocodex::builder(auth)
    .responses(responses)
    .build()?;
```

HTTPS with `store: false` automatically selects full client-history replay.
Callers can explicitly select
`ResponsesHistory::{Incremental, FullReplay}` for the other supported
combinations. The builder rejects `store: true` with ChatGPT subscription
authentication and incremental HTTPS history with `store: false`.

The native CLI/TUI fixes the same policy at startup with
`--responses-transport`, `--responses-history`, and `--store-responses`.

## Measurements

For each request the JSON report records:

- exact serialized request bytes and encoding time;
- time to first streamed event and time through `response.completed`;
- input, cached-input, cache-write, and output tokens;
- WebSocket setup time where applicable.

It also records chain wall time, local fork-snapshot clone time, and concurrent
mainline-plus-forks wall time. Every assistant reply is checked against an
exact expected token. Stored responses are deleted at the end unless
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

## July 22, 2026 snapshot

The retained local report is
`.nanocodex/benchmarks/response-transport-repeated.json` and is intentionally
outside Git. The release-build workload used two chain turns, forks from both
turns, one simultaneous mainline continuation, 600 deterministic prefix facts,
and three order-rotated repetitions. The table reports medians across the three
trials; response and first-event medians include every request in that phase.

| Variant | Chain request bytes | Chain response | Chain first event | Mean fork request bytes | Fork response | Mainline + forks wall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `ws-store-checkpoint` | 26,907 | 1,116 ms | 124 ms | 785 | 1,290 ms | 1,759 ms |
| `ws-store-replay` | 52,616 | 1,226 ms | 140 ms | 26,681 | 1,556 ms | 2,179 ms |
| `ws-ephemeral-connection` | 26,933 | 1,089 ms | 122 ms | 26,706 | 2,180 ms | 2,935 ms |
| `ws-ephemeral-replay` | 52,642 | 898 ms | 118 ms | 26,694 | 867 ms | 1,467 ms |
| `https-store-checkpoint` | 26,743 | 1,073 ms | 342 ms | 703 | 1,385 ms | 1,712 ms |
| `https-store-replay` | 52,452 | 936 ms | 350 ms | 26,599 | 1,608 ms | 1,728 ms |
| `https-ephemeral-replay` | 52,478 | 946 ms | 308 ms | 26,612 | 1,387 ms | 1,671 ms |

The stable findings from this small workload are:

1. A stored response checkpoint reduced each fork request from about 26.6 KiB
   to 0.7-0.8 KiB, a roughly 97% reduction. Incremental mainline chaining
   roughly halved the two-turn request total because the initial request still
   carried the complete prefix.
2. `store: false` did not prevent delta requests on the persistent WebSocket,
   but fresh forks had to rebroadcast their complete committed histories.
3. Prompt caching was unchanged across the matrix: every chain reported 8,451
   cached tokens out of 16,933 input tokens (49.9%). Storage and replay policy
   did not improve this workload's cache-token ratio.
4. WebSocket first-event latency was consistently lower: roughly 118-140 ms
   on the chain versus 308-350 ms over HTTPS. A new WebSocket cost roughly
   359-401 ms for the root and 468-499 ms for each fresh fork, so the benefit is
   naturally strongest on a long-lived mainline and weaker for one-shot forks.
5. Local checkpoint cloning took about 0.6-1.2 microseconds. Full-history JSON
   encoding was about 18-20 microseconds at this size. These local costs were
   negligible beside transport and inference, while request volume remained a
   material scaling difference.

End-to-end model completion and concurrent fork latency varied substantially
between repetitions, including multi-second backend outliers. The snapshot is
strong evidence for byte volume, cache behavior, connection setup, and
first-event behavior; it is not enough evidence that `store` itself changes
model generation latency. Larger histories and more repetitions should be used
before making a release-level latency claim.

## Design implication

The TUI should select one transport and storage policy when it creates an agent
session; forks should preserve that policy rather than switching transports.
The persistent WebSocket optimization matters for interactive first-event
latency and cheap mainline deltas. `store: true` matters independently when a
branch must start from a historical checkpoint on a fresh connection: it
avoids replaying that branch's retained history. If storage is unavailable,
including with ChatGPT subscription authentication, the performant fallback is
the current immutable segmented client-owned history, concurrent fork
execution, stable cache key, and full replay on each fresh fork. HTTPS remains
a viable single-session policy and can use stored checkpoints with API-key
authentication when storage is enabled.
