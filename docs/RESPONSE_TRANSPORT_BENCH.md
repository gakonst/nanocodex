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

## July 23, 2026 ten-turn rerun

The retained raw reports are
`.nanocodex/benchmarks/response-transport-10turn-rerun.json` and
`.nanocodex/benchmarks/response-transport-10turn-ws-store-replay-replacement.json`;
they are intentionally outside Git. The release-build workload used ten chain
turns, historical forks from turns 3, 6, and 9, one simultaneous mainline
continuation, 600 deterministic prefix facts, and three successful samples per
variant. Variant order rotated between repetitions. One `ws-store-replay`
sample was rejected after a transient peer TLS EOF and replaced with a clean
targeted sample; it is not included in the medians.

Cold timing starts before fresh transport setup and includes the first request.
For WebSocket this includes the handshake; for HTTPS the first request includes
connection establishment. Warm medians cover chain turns 2 through 10 on the
reused WebSocket or pooled HTTPS client. Compilation and process startup are
excluded.

| Variant | Cold first event | Cold complete | Warm first event | Warm complete | 10-turn request bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| `ws-store-checkpoint` | 679 ms | 1,847 ms | 80 ms | 1,047 ms | 32,974 |
| `ws-store-replay` | 657 ms | 2,202 ms | 151 ms | 1,243 ms | 277,051 |
| `ws-ephemeral-connection` | 587 ms | 1,380 ms | 99 ms | 955 ms | 33,104 |
| `ws-ephemeral-replay` | 610 ms | 1,564 ms | 97 ms | 920 ms | 277,181 |
| `https-store-checkpoint` | 400 ms | 1,211 ms | 369 ms | 1,241 ms | 32,154 |
| `https-store-replay` | 374 ms | 1,944 ms | 361 ms | 1,315 ms | 276,231 |
| `https-ephemeral-replay` | 405 ms | 1,260 ms | 277 ms | 1,051 ms | 276,361 |

Fork snapshot cloning is the local cost before transport work. Fork
start-to-first and start-to-complete include each branch's fresh transport
setup and request. HTTPS setup rounds to zero because cloning the pooled client
is local; any new network connection remains inside start-to-first. Race wall
is the concurrent completion time for one mainline request and all three
forks.

| Variant | Clone per fork | Fork setup | Fork first event | Fork complete | Three-fork bytes | Mainline + forks wall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `ws-store-checkpoint` | 0.4 µs | 910 ms | 1,069 ms | 2,281 ms | 2,346 | 2,489 ms |
| `ws-store-replay` | 0.4 µs | 971 ms | 1,135 ms | 2,040 ms | 84,759 | 2,217 ms |
| `ws-ephemeral-connection` | 0.4 µs | 912 ms | 1,012 ms | 1,800 ms | 84,834 | 3,673 ms |
| `ws-ephemeral-replay` | 0.4 µs | 832 ms | 999 ms | 2,047 ms | 84,798 | 2,376 ms |
| `https-store-checkpoint` | 0.3 µs | <0.1 ms | 392 ms | 1,713 ms | 2,100 | 1,830 ms |
| `https-store-replay` | 0.3 µs | <0.1 ms | 436 ms | 1,129 ms | 84,513 | 1,380 ms |
| `https-ephemeral-replay` | 0.3 µs | <0.1 ms | 373 ms | 1,372 ms | 84,552 | 3,427 ms |

Every policy reported the same median model usage for the complete 14-request
workload: 120,201 input tokens, including 111,363 cache reads and 8,796 cache
writes, plus 104 output tokens. Cache reads were therefore 92.6% of input.
At the published standard GPT-5.6 Sol rates of $5 per million uncached input
tokens and $30 per million output tokens, with cache reads at a 90% discount
and cache writes at 1.25 times uncached input, the estimated cost is $0.114 per
variant workload. The 21 successful samples cost an estimated $2.39, excluding
the rejected partial request. Account-specific or priority pricing may differ.
See [OpenAI's GPT-5.6 pricing](https://openai.com/index/gpt-5-6/#availability-and-pricing).

The stable findings are:

1. A stored response checkpoint reduced three historical-fork requests from
   about 84.5 KiB to 2.1-2.3 KiB, a roughly 97% reduction. Incremental chaining
   reduced ten-turn mainline request volume by about 88%.
2. `store: false` still permits connection-local delta requests on a persistent
   WebSocket, but a fresh fork must replay complete committed history. HTTPS
   with `store: false` must replay on every request.
3. Transport, storage, and replay policy did not change model token usage,
   cache reads, cache writes, or estimated API cost in this workload. Their
   measurable effect was client wire volume and latency shape.
4. Warm WebSocket first-event latency was 80-151 ms, materially lower than
   HTTPS at 277-369 ms. Cold WebSocket first-event latency was slower because
   its explicit handshake brought the total to 587-679 ms, versus 374-405 ms
   for the first HTTPS event.
5. Creating the local immutable fork snapshot remained effectively free at
   0.3-0.4 microseconds per fork. A fresh WebSocket fork was dominated by an
   0.8-1.0 second handshake; HTTPS forks reached their first event in
   373-436 ms.

End-to-end completion and concurrent race latency varied substantially between
repetitions, including multi-second backend outliers. The rerun is strong
evidence for byte volume, cache behavior, connection setup, and first-event
behavior; it is not evidence that `store` itself changes model generation
speed.

## Design implication

The TUI should select one transport and storage policy when it creates an agent
session; forks should preserve that policy rather than switching transports.
The persistent WebSocket is therefore the default for both authorization modes:
it optimizes the common long-lived interactive mainline with the lowest warm
first-event latency and cheap incremental turns. HTTPS is the explicit
alternative for cold, one-shot, or fork-heavy sessions where avoiding a fresh
WebSocket handshake matters more than warm mainline latency.

`store: true` matters independently when a branch must start from a historical
checkpoint on a fresh connection: it avoids replaying that branch's retained
history. If storage is unavailable, including with ChatGPT subscription
authentication, the performant fallback is the current immutable segmented
client-owned history, concurrent fork execution, stable cache key, and full
replay on each fresh fork. HTTPS can use stored checkpoints with API-key
authentication when storage is enabled.
