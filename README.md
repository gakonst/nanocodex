<div align="center">

<h1>Nanocodex</h1>

<p><strong>Blazing-fast, minimal, library-first reimplementation of Codex.</strong></p>

[![CI](https://img.shields.io/github/actions/workflow/status/gakonst/nanocodex/ci.yml?branch=master)][ci]
[![Crates.io](https://img.shields.io/crates/v/nanocodex.svg)][crates]
[![Docs.rs](https://img.shields.io/docsrs/nanocodex)][docs]
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)][license]

**[Install](#installation)** | **[Thesis](#model-and-harness-co-design)** | **[Why Code Mode?](#why-code-mode)** | **[API](#api)** | **[Examples](examples)** | **[Benchmarks](#how-fast)**

[ci]: https://github.com/gakonst/nanocodex/actions/workflows/ci.yml
[crates]: https://crates.io/crates/nanocodex
[docs]: https://docs.rs/nanocodex
[license]: LICENSE-MIT

</div>

---

Nanocodex is a Code Mode-first Rust agents SDK. It provides typed turns, tools,
events, steering, cancellation, queueing, and fast historical forks over the
OpenAI Responses WebSocket API. It keeps the complete coding-agent conversation
inside your process without requiring an app server or durable control plane.

## Installation

Install the daily-driver CLI on macOS or Linux:

```sh
curl -fsSL https://nanocodex.paradigm.xyz | bash
```

The installer tracks stable releases. Switch an installed CLI to the rolling
nightly channel with:

```sh
nanocodex update --nightly
```

Multi-architecture Linux images are published to GHCR as
`ghcr.io/gakonst/nanocodex:latest` and `ghcr.io/gakonst/nanocodex:nightly`.
Immutable version, commit, and `nightly-<commit>` tags are also available.

Add the library to a Rust project:

```sh
cargo add nanocodex
```

Or add it directly to `Cargo.toml`:

```toml
[dependencies]
nanocodex = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Node.js 12.22 or newer must be available on `PATH` for Code Mode.

## Model and harness co-design

Nanocodex starts from a simple thesis: **the model and its harness are one
system**. A coding model is not an interchangeable completion engine floating
above an arbitrary tool loop. Its effective capabilities depend on the exact
instructions, tool contracts, response shapes, history ordering, continuation
semantics, cache identity, streaming behavior, and failure recovery that
surround it. Change the harness and you change the agent.

That is why Nanocodex begins with how Codex actually works. We inspect the
concrete Codex implementation, identify the model-facing invariants and
operational behavior it relies on, and adapt them rather than designing a
provider-neutral agent abstraction from first principles. Typed Responses
items, stable prompt prefixes, incremental conversation continuation, complete
history replay, tool-call ordering, WebSocket lifecycle, cancellation, and
subprocess cleanup are parts of the behavioral contract, not incidental
plumbing.

The adaptation is as important as the fidelity. Nanocodex keeps the pieces that
shape model behavior, then recasts them for a headless, library-first product:
one owned driver instead of an app server, typed results and optional events
instead of a durable rollout control plane, caller-defined tools instead of a
product-wide integration catalog, and generated Code Mode orchestration instead
of a generic workflow or multi-agent scheduler. Codex is the evidence and the
behavioral reference; Nanocodex chooses the smaller API boundary.

This also means agent quality and performance belong to the **model–harness
pair**. We evaluate the complete path—prompt, tools, transport, caching,
execution, and recovery—because a model-only comparison would miss the system
we are actually building.

## Why Code Mode?

Most tool-calling agents expose a flat catalog and return to the model between
operations. Nanocodex instead presents caller-defined Rust tools and MCP tools
as typed JavaScript functions behind one Code Mode entrypoint. The model can
write a small program that uses loops, conditions, data transformations, and
`Promise.all`, so related tool work can be composed inside one cell without a
model round trip between every operation.

This keeps the model-facing surface small while the application keeps normal
Rust ownership. Tool implementations, credentials, retry policy, and mutable
state stay outside the generated program. Code Mode runs cells on a prewarmed,
session-persistent Node host and gives the model explicit controls for yielding,
resuming long work, and bounding returned output.

Subagents make that composition especially useful. An application can expose
`AgentHandle::spawn()` as a clean-room worker, `AgentHandle::fork()` as a worker
with the latest safe conversation context, and another tool for follow-up turns
on a retained child. Code Mode can then generate the orchestration topology for
the task instead of selecting a hard-coded workflow DAG:

```js
const [independent, contextual] = await Promise.all([
  tools.spawn_agent({
    role: "reviewer",
    task: "Find assumptions the parent may have missed."
  }),
  tools.fork_agent({
    role: "investigator",
    task: "Trace the suspected regression using our existing context."
  })
]);

const followUp = await tools.prompt_agent({
  agent_id: independent.agent_id,
  task: `Challenge this conclusion:\n\n${contextual.report}`
});

text({ independent, contextual, followUp });
```

That allows dynamic fan-out, fan-in, independent checks, contextual branches,
and targeted follow-ups while Nanocodex core remains one owned agent lifecycle
rather than a generic multi-agent scheduler. The bundled CLI exposes this
example surface with `nanocodex --subagents true`; library consumers define the
tools and policy themselves. See [`subagents.rs`](examples/subagents.rs) for the
complete implementation.

## API

```rust
let (agent, _events) = Nanocodex::new(api_key)?;

// Accepted now.
let turn = agent.prompt("Inspect this repository.").await?;
// Optional cloneable control.
let _control = turn.control();
// Steer the same active turn.
turn.steer("Focus on the failing tests.").await?;
// Completed result and checkpoint.
let checkpoint = turn.result().await?;

let _follow_on = agent.prompt("Now propose a fix.").await?.result().await?;

let turn = agent.prompt("Run a long investigation.").await?;
// Cancel queued or active work.
turn.cancel().await?;
// Returns Err(TurnCancelled).
let _cancelled = turn.result().await;

// Fork from the latest safe model/tool boundary.
let (latest, _events) = agent.fork().await?;
// Fork from the exact older state.
let (historical, _events) = agent.fork_from(&checkpoint).await?;
```

`prompt().await` means accepted, not completed. The agent retains conversation
history, tools, cache identity, response chain, and its WebSocket automatically.

### Lifecycle and dataflow

```text
NanocodexBuilder
       │
       ▼
 private agent driver ───────────────► AgentEvents
       ▲                                  side channel
       │
 Nanocodex
 cloneable conversation handle
       │
       ├── prompt(...) ──► Turn
       │                    ├── steer(...)
       │                    ├── cancel(...)
       │                    ├── control() ──► cloneable TurnControl
       │                    └── result() ──► TurnResult
       │                                         │
       ├── fork()                                │ checkpoint
       └── fork_from(&TurnResult) ◄──────────────┘

AgentHandle, supplied to tools_factory(...)
       ├── spawn()  clean child
       └── fork()   child from latest safe model/tool boundary
```

That is the complete ownership model. See the runnable
[`lifecycle.rs`](examples/lifecycle.rs) for all of it in one file.

### Authentication

Native applications can use either an `OpenAI` API key or a `ChatGPT`
subscription. Existing API-key construction remains unchanged:

```rust
let (agent, events) = Nanocodex::new(api_key)?;
```

For a `ChatGPT` subscription, the bundled CLI performs an authorization-code
OAuth login with PKCE and reuses Codex's credential file at
`$CODEX_HOME/auth.json`, or `~/.codex/auth.json` by default. If Codex is already
logged in, no separate Nanocodex login is required:

```sh
nanocodex auth login
nanocodex auth status
nanocodex
```

Plain `nanocodex` and `nanocodex run` prefer `OPENAI_API_KEY`; direct binary runs
load it from the nearest `.env` automatically. An explicit `--api-key` overrides
both automatic sources. Without an API key, the CLI falls back to the stored
subscription session. To select `ChatGPT` explicitly while a key is available,
pass `NANOCODEX_AUTH_FILE` or `--auth-file`. `nanocodex auth logout` removes the
shared file and therefore logs both Codex and Nanocodex out.

Library consumers own their login UX and can reuse the same managed session:

```rust
use nanocodex::{Nanocodex, load_chatgpt_auth};

let auth = load_chatgpt_auth("/path/to/.codex/auth.json")?;
let (agent, events) = Nanocodex::new(auth)?;
```

The shared authorization handle selects the matching Responses endpoints,
attaches the account and FedRAMP routing headers, refreshes shortly before JWT
expiry, reloads credentials rotated by another process, and retries one rejected
request after a serialized refresh. Forks and built-in HTTP tools share that
same rotating session. Browser/WASM embeddings continue to receive an
already-authorized WebSocket from the host and never own refresh tokens.

## Lifecycle details

`Nanocodex::new` installs the standard instructions, medium thinking, built-in
tools, persistent WebSocket, and retry/reconnect policy. Dropping the event
receiver is supported; events then become a no-op.

Callers never pass transcripts, response IDs, tool outputs, or turn IDs back to
the agent. On a healthy socket the driver sends only the new delta with
`previous_response_id`. After reconnecting it drops that ID and transparently
replays its authoritative typed history.

The Responses path encodes each wire request once and rejects common
non-metadata frames before attempting metadata decoding. Every streamed attempt
records request and response bytes, encode/send and socket-wait time, parsing,
public-event emission, typed decoding, and time to first event/output as
structural tracing fields.

### Queue, steer, cancel

Every `prompt` is an ordinary queued turn. Steering is explicit because it has
different semantics: it joins one already-active turn and is sampled only
between complete model responses and tool outputs. It does not create a second
turn or terminal event.

Cancellation targets the same opaque unfinished turn. Cancelling queued work
removes it before it reaches the model. Cancelling active work waits for model
work, Code Mode cells, and shell process groups to stop, then resolves the turn
as `NanocodexError::TurnCancelled`. Partial model or tool work is never
committed; surviving queued prompts resume from the last completed checkpoint.

Call methods directly when one task owns the turn:

```rust
let turn = agent.prompt("Investigate the failing tests.").await?;
turn.steer("Prioritize deterministic failures.").await?;
let result = turn.result().await?;
```

Use `TurnControl` only when result and control ownership need to split:

```rust
use nanocodex::NanocodexError;

let turn = agent.prompt("Run a long investigation.").await?;
let control = turn.control();
let result_task = tokio::spawn(async move { turn.result().await });

control.steer("Check the integration tests first.").await?;
control.cancel().await?;
assert!(matches!(result_task.await?, Err(NanocodexError::TurnCancelled)));
```

### Continue and fork conversations

Follow-on prompts reuse retained context automatically:

```rust
let first = agent
    .prompt("Choose one word for this project.")
    .await?
    .result()
    .await?;

let second = agent
    .prompt("Return the word you chose in uppercase.")
    .await?
    .result()
    .await?;
```

Each completed result is also an opaque historical checkpoint. The mainline can
keep advancing while multiple branches start from different points:

```rust
let turn_2 = agent
    .prompt("Record design decision A.")
    .await?
    .result()
    .await?;

agent
    .prompt("Record later decision B.")
    .await?
    .result()
    .await?;

// The mainline may continue while both new agents are being constructed.
let mainline = agent.prompt("Continue the primary analysis.").await?;
let ((historical, _), (latest, _)) = tokio::try_join!(
    agent.fork_from(&turn_2),
    agent.fork(),
)?;

let historical_turn = historical.prompt("Explore an alternative to A.").await?;
let latest_turn = latest.prompt("Challenge our newest assumptions.").await?;
let (mainline, historical, latest) = tokio::try_join!(
    mainline.result(),
    historical_turn.result(),
    latest_turn.result(),
)?;
```

#### Why checkpoint forks are efficient

Every Nanocodex `response.create` request sets `store: true`. Once a response
completes, the API can retain it as a checkpoint; Nanocodex keeps its response
ID private inside the completed `TurnResult`. The next healthy model call sends
that ID as `previous_response_id` plus only the new delta—the user message,
steer, or tool output added since the stored response—not the transcript again.

The same mechanism makes a historical fork cheap:

```text
prewarm       input: stable instructions/tools, store: true   → prefix response
root turn A   previous_response_id: prefix, input: A delta    → response A
root turn B   previous_response_id: A, input: B delta         → response B
fork from A   previous_response_id: A, input: branch delta    → branch response
```

The root remains attached to response B and continues independently. The fork
gets its own driver, WebSocket, response chain, service stack, and tool runtime,
but its first request references response A and uploads only the branch delta.
Locally, immutable typed-history segments and stable cache lineage are shared,
so constructing the branch does not copy the retained conversation either.
The API still evaluates the complete logical context—and token usage reflects
that context—even though the request payload carries only the delta.

Stored responses are an optimization, not the source of truth. Nanocodex keeps
complete client-owned typed history for every checkpoint. If the provider no
longer has a response ID, the retry drops `previous_response_id`, replays that
committed history once, and then resumes delta-only requests from the new stored
response. Partial or failed responses are never committed or used as fork
points.

In the retained checkpoint benchmark, three concurrent branch requests sent
2,175 bytes instead of an equivalent 84,612-byte full replay—a 97.4% payload
reduction. See [`benchmarks/fork_results.md`](benchmarks/fork_results.md) for the
live methodology, cache observations, and raw trials.

See [`fork_conversations.rs`](examples/fork_conversations.rs) for a complete
ten-checkpoint example with parallel historical forks and a caller-defined
Tower stack.

### Configure only what your application owns

The common paths remain short; factories appear only when lifecycle isolation
requires them.

```rust
use std::time::Duration;

use nanocodex::{Nanocodex, Responses, Thinking};
use tower::timeout::TimeoutLayer;

let responses = Responses::builder()
    .layer(TimeoutLayer::new(Duration::from_secs(120)))
    .build();

let (agent, events) = Nanocodex::builder(api_key)
    .instructions("You are a concise repository maintenance agent.")
    .thinking(Thinking::Medium)
    .workspace("/work/project")
    .tools(tools)
    .responses(responses)
    .build()?;
```

Use `tools_factory` when a tool must spawn or fork the agent that invoked it.
The factory receives a weak `AgentHandle`, not credentials:

```rust
let (agent, events) = Nanocodex::builder(api_key)
    .tools_factory(|handle| build_agent_tools(handle))
    .build()?;
```

`handle.spawn()` creates a clean child; `handle.fork()` creates a contextual
child from the invoking agent. Both privately reuse the builder's credentials
and policy. The application can expose these as Code Mode tools and let the
model generate loops, fan-out, follow-up prompts, and synthesis without encoding
a DAG in the SDK. See [`subagents.rs`](examples/subagents.rs) for that complete
orchestration pattern.

One Tower call is one complete streamed Responses attempt. Nanocodex owns retry
and reconnect policy; caller middleware can own deadlines, load shedding,
tracing, metrics, and circuit breaking without creating a second retry loop.
See [`docs/RESPONSES_TOWER.md`](docs/RESPONSES_TOWER.md) for the boundary and
ordering rules.

### Tools, MCP, events, and errors

`#[tool]` turns an async Rust function into a typed tool and derives its input
schema. `Tools::builder()` accepts generated or manual `Tool` implementations;
`Mcp::builder()` adds deferred Streamable HTTP or stdio MCP providers. The model
normally sees only Code Mode and its wait operation, then composes nested tools
with generated JavaScript, including loops, conditionals, and `Promise.all`.

Code Mode prewarms one persistent Node host alongside the first model call and
reuses it for the session. Cells receive one shared owned history snapshot;
resumed waits do not copy history they cannot read. A nested shell request can
extend the default outer-cell yield deadline while an explicit `@exec` deadline
still wins. Live shell session IDs remain visible for later `write_stdin`
calls, and stdout/stderr drains share one bounded completion deadline.

`AgentEvents` is an optional ordered stream independent of `TurnResult`. A TUI,
server, notebook, or binding can consume all events, select a subset, or drop
the receiver without changing prompt/result behavior. Libraries emit diagnostic
`tracing` spans but never install a global subscriber. Nested tools that finish
after a yielded cell retain their original Code Mode and model-call lineage, so
the public event stream and trace hierarchy agree.

Lifecycle failures are direct `NanocodexError` variants. Common control flow can
match `TurnCancelled` or `TurnNotSteerable`; transport and API details remain
available through `responses_error()` and the standard `Error::source` chain.

Runnable API tours:

```sh
cargo run -p nanocodex-examples --bin minimal
cargo run -p nanocodex-examples --bin lifecycle
cargo run -p nanocodex-examples --bin follow-on
cargo run -p nanocodex-examples --bin custom-tool
cargo run -p nanocodex-examples --bin mcp
cargo run -p nanocodex-examples --bin fork-conversations
cargo run -p nanocodex-examples --bin subagents
```

## CLI and repository

Install the daily-driver CLI and start it in the workspace the agent should
edit:

```sh
curl -fsSL https://nanocodex.paradigm.xyz | bash

# OPENAI_API_KEY is loaded from the nearest .env by default.
nanocodex

# Or explicitly use the same subscription store Codex uses.
nanocodex auth login
nanocodex --auth-file "${CODEX_HOME:-$HOME/.codex}/auth.json"
```

The TUI retains one session across prompts. Enter submits, Tab explicitly queues
a follow-up while work is active, and `/cancel` stops the focused turn. At any
safe model/tool boundary, `/btw <question>` opens a fast fork in a vertical pane
while the mainline continues. The fork inherits the last completed response ID
plus complete tool results and applied steers after that response; partial model
output and unmatched tool calls remain excluded. With local telemetry running,
`/trace` opens Jaeger filtered to every turn in the focused main or `/btw`
session. The complete keybinding reference, retained Amp and Codex research, and
prioritized Ratatui backlog live in
[`docs/TUI_NOTES.md`](docs/TUI_NOTES.md). The headless `nanocodex run` adapter
emits flushed JSONL for scripts and Harbor.

`just run-otel` exports compact per-turn streaming summaries alongside the full
agent trace. When diagnosing a jagged stream, opt into the individual correlated
API-delta, TUI-application, and presented-frame records:

```sh
just run-otel-detail
```

The TUI log at `.nanocodex/logs/tui.log` correlates each request and event across
socket receipt, agent emission, TUI receipt, state application, frame
coalescing, Ratatui changed cells, terminal output bytes, and final flush.
Detailed mode is intentionally opt-in because long responses can create
thousands of records. Use `just bench-stream` for the focused event-delivery,
transcript-update, and steady-frame regression gate. See
[`docs/OBSERVABILITY.md`](docs/OBSERVABILITY.md) for the full trace contract.

The workspace also contains thin [Python](bindings/python),
[Node](examples/node), and [browser Worker](examples/react-vite) consumers.
Architecture and current work are tracked in [`PLAN.md`](PLAN.md); benchmark
runner research lives in [`docs/HARBOR_RS_LOG.md`](docs/HARBOR_RS_LOG.md).

```sh
# Install pinned host dependencies.
just bootstrap
# Run the native smoke test.
just run
# Build and cache benchmark inputs.
just prepare-evals
# Run the pinned Terminal-Bench suite.
just eval
# Inspect retained Harbor jobs.
just view
```

## Nanocodex versus Codex

Use Nanocodex when the agent is a component of your Rust application. Use Codex
when you want the complete product: durable threads, approval UX, broad built-in
integrations, managed subagents, and a mature TUI and IDE ecosystem.

| | Nanocodex | Codex |
| --- | --- | --- |
| Product boundary | Rust library in your process | Application and durable agent runtime |
| State | One owned in-memory session | Persisted threads and rollouts |
| Follow-on turns | New input delta on one persistent WebSocket | Full Codex session lifecycle |
| Historical forks | Exact completed checkpoint; parent keeps running | Durable thread reconstruction |
| Tools | Code Mode over Rust tools and MCP | Broad built-in tool and integration surface |
| Middleware | Your concrete Tower stack | Codex-owned runtime policy |
| Results and events | Typed `TurnResult` plus optional ordered `AgentEvents` | Product-wide rollout/event lifecycle |
| Orchestration | Model composes application-defined agent tools | Managed agents, task identities, mailboxes, and budgets |

The smaller boundary is the feature. A caller builds an agent, receives
`(Nanocodex, AgentEvents)`, sends prompts through a cheap cloneable handle, and
awaits independently owned `TurnResult`s. The CLI, Harbor adapter, Python
binding, and Rust/WASM binding all consume that same API.

### How Fast?

Focused local profiling also covers the ordinary streaming and tool path. On an
M1 Max, prewarming the retained Node host reduced first-cell host latency from
301 ms to 1.7–2.7 ms and complete top-level Code Mode time from 312 ms to 11–13
ms across three trials. On a retained 41-task workload, model generation and
caller-requested subprocesses accounted for 99.864% of summed run time; the
unattributed local remainder was 0.136%. These are diagnostic measurements, not
model-service speed guarantees. See
[`single_prompt_profile_2026-07-20.md`](benchmarks/single_prompt_profile_2026-07-20.md)
and
[`long_prompt_profile_2026-07-20.md`](benchmarks/long_prompt_profile_2026-07-20.md)
for methodology and reproduction commands.

Our live checkpoint benchmark uses `gpt-5.6-sol`, a deterministic 600-fact
prefix, ten sequential turns, and concurrent historical forks. Three runs on
2026-07-20 compared Nanocodex `210ac85` with stock Codex CLI
`0.145.0-alpha.18`:

| Measurement | Nanocodex | Stock Codex | Difference |
| --- | ---: | ---: | ---: |
| Ten sequential turns, median total | 14.78 s | 24.99 s | **1.69x faster** |
| Warm turn p50, turns 3–10 | 1.304 s | 1.532 s | **1.18x faster** |
| Historical fork to first answer, p50 | 1.570 s | 6.530 s | **4.16x faster** |
| Historical fork model time, p50 | 1.291 s | 5.862 s | **4.54x faster** |

**Takeaway: Nanocodex was 1.69x faster across ten turns and 4.16x faster to
the first historical-fork answer in this checkpoint benchmark.**

A Nanocodex fork sent about 725 bytes of new request data from its stored
checkpoint. Replaying the same history would send 27–29 KB: a 97.4% reduction.
On a separate 41-task coding gate, Nanocodex completed 38 tasks with 92.23% of
input tokens cached, zero Responses retries, and zero WebSocket reconnects.

These are checkpoint-path measurements, not a normalized full-agent quality
comparison. The Nanocodex arm used a minimal benchmark developer message and no
production tool definitions; the Codex arm ran the complete stock app-server
agent. See [`benchmarks/fork_results.md`](benchmarks/fork_results.md) for the
methodology, cache observations, raw trials, and reproduction commands.

### The tradeoff

Nanocodex currently supports one model family (`gpt-5.6-sol`), one Responses
WebSocket transport, and caller-defined tools. Sessions and branches live only
as long as your process. Your application owns sandboxing, permissions,
durability, and recursive cancellation policy for application-defined child
agents. Code Mode requires Node.js 12.22 or newer on `PATH`.

That is substantially less product than Codex. It is also much less machinery
between your code and an agent turn.
