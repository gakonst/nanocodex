# Nanocodex

Nanocodex is a small, library-first Rust agents SDK built around the OpenAI
Responses WebSocket API. It keeps one coding-agent conversation alive in your
process—with typed turns, tools, events, steering, cancellation, queueing, and
fast historical forks—without requiring an app server or durable control plane.

## BLUF: Nanocodex versus Codex

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

### Measured checkpoint performance

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

## The user-facing API

Most applications need only the top-level crate:

```toml
[dependencies]
nanocodex = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The smallest complete program is deliberately ordinary Rust:

```rust
use nanocodex::Nanocodex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let (agent, _) = Nanocodex::new(api_key)?;

    let turn = agent.prompt("Inspect this repository and summarize it.").await?;
    let result = turn.result().await?;
    println!("{}", result.final_message);
    Ok(())
}
```

`Nanocodex::new` installs the standard instructions, medium thinking, built-in
tools, persistent WebSocket, and retry/reconnect policy. Dropping the event
receiver is supported; events then become a no-op.

### Lifecycle and dataflow

`prompt().await` waits only until the driver accepts the command. It returns an
independently awaitable `Turn`; it does not wait for the model to finish.

```text
 NanocodexBuilder
       │ build()
       ▼
 ┌──────────────────────────────── caller ─────────────────────────────────┐
 │                                                                         │
 │  Nanocodex ── prompt(input) ──► Turn                                    │
 │  cheap, cloneable                  │                                     │
 │  command handle                    ├── steer(input)                      │
 │                                    ├── cancel()                          │
 │                                    └── result().await ──► TurnResult     │
 │                                                               │         │
 │  AgentEvents ◄──── optional ordered events ────────────────────┘         │
 └────────│────────────────────────────────────────────────────────────────┘
          │ commands                         ▲ results/events
          ▼                                  │
 ┌──────────────────────── private agent driver ───────────────────────────┐
 │                                                                         │
 │   queued ─────────────► active ─────────────► committed checkpoint       │
 │      │                    │  ▲                         │                  │
 │      │ cancel             │  └─ steer at a safe       ├─ next prompt    │
 │      ▼                    │     model/tool boundary   ├─ fork latest    │
 │  cancelled                ├─ cancel                  └─ fork historical │
 │                           └─ failure                                      │
 │                                                                         │
 │   owns: typed history · response chain · tools · Code Mode runtime       │
 │         shell sessions · Tower service · persistent WebSocket            │
 └───────────────────────────────│─────────────────────────────────────────┘
                                 ▼
                  Responses retry/reconnect policy → OpenAI
```

Those are runtime lifecycle states, not public generic typestates. The public
types instead encode ownership: `Turn` is the capability for unfinished work,
`TurnResult` is an inert completed checkpoint, and `Nanocodex` owns operations
that create another conversation.

| Intent | API | Semantics |
| --- | --- | --- |
| Submit ordinary input | `agent.prompt(input).await` | Adds one FIFO turn and returns its `Turn` after acceptance |
| Steer live work | `turn.steer(input).await` | Adds input to that exact active turn at its next safe boundary |
| Cancel exact work | `turn.cancel().await` | Removes a queued turn or stops an active turn and its subprocesses |
| Split control ownership | `turn.control()` | Creates a cloneable `TurnControl` for another task |
| Await completion | `turn.result().await` | Consumes the turn and returns its typed committed result or error |
| Continue the session | `agent.prompt(input).await` again | Reuses history, tools, socket, cache identity, and response chain |
| Fork latest state | `agent.fork().await` | Creates an independent agent at the latest completed checkpoint |
| Fork older state | `agent.fork_from(&result).await` | Creates an independent agent at that exact retained checkpoint |

Callers never pass transcripts, response IDs, tool outputs, or turn IDs back to
the agent. On a healthy socket the driver sends only the new delta with
`previous_response_id`. After reconnecting it drops that ID and transparently
replays its authoritative typed history.

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

```text
                         fork_from(&turn_2)
                              ┌──► B2 ──► B3
                              │
 root:  turn_1 ──► turn_2 ──► turn_3 ──► turn_4 ──► ...
                                      │
                                      └──► L4 ──► ...
                                          fork() from latest commit
```

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

Forks contain only committed work. Each child gets a new driver, WebSocket,
response chain, service stack, and tool runtime. Immutable typed history and
cache lineage are shared. If a provider checkpoint has expired, Nanocodex
replays committed history once and then returns to incremental requests.

See [`fork_conversations.rs`](examples/fork_conversations.rs) for a complete
ten-checkpoint example with parallel historical forks and a caller-defined
Tower stack.

### Configure only what your application owns

The common paths remain short; factories appear only when lifecycle isolation
requires them.

| Need | Builder API | Why |
| --- | --- | --- |
| Standard agent | `Nanocodex::new(api_key)` | All defaults |
| Session policy | `Nanocodex::builder(api_key)` | Instructions, thinking, workspace, tools, identity, Responses policy |
| Shareable/static tools | `.tools(tools)` | Reuses a completed registry |
| Tools bound to each agent | `.tools_factory(|handle| ...)` | Fresh handlers for every root, fork, and child |
| Wrap standard transport | `Responses::builder().layer(layer)` | Adds timeout, tracing, limits, or other Tower middleware |
| Replace transport stack | `Responses::builder().service(|| stack)` | Fresh mutable service state for every independent lifecycle |

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

`AgentEvents` is an optional ordered stream independent of `TurnResult`. A TUI,
server, notebook, or binding can consume all events, select a subset, or drop
the receiver without changing prompt/result behavior. Libraries emit diagnostic
`tracing` spans but never install a global subscriber.

Lifecycle failures are direct `NanocodexError` variants. Common control flow can
match `TurnCancelled` or `TurnNotSteerable`; transport and API details remain
available through `responses_error()` and the standard `Error::source` chain.

Runnable API tours:

```sh
cargo run -p nanocodex-examples --bin minimal
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
curl -fsSL https://raw.githubusercontent.com/gakonst/nanocodex/master/install | bash
export OPENAI_API_KEY=...
nanocodex
```

The TUI retains one session across prompts. Enter submits, Tab explicitly queues
a follow-up while work is active, and `/cancel` stops the focused turn. After a
completed turn, `/btw <question>` opens a fast latest-checkpoint fork in a
vertical pane while the mainline continues. The headless `nanocodex run`
adapter emits flushed JSONL for scripts and Harbor.

The workspace also contains thin [Python](bindings/python),
[Node](examples/node), and [browser Worker](examples/react-vite) consumers.
Architecture and current work are tracked in [`PLAN.md`](PLAN.md); benchmark
runner research lives in [`docs/HARBOR_RS_LOG.md`](docs/HARBOR_RS_LOG.md).

```sh
just bootstrap      # install pinned host dependencies
just run            # native smoke
just prepare-evals  # build and cache benchmark inputs
just eval           # run the pinned Terminal-Bench suite
just view           # inspect retained Harbor jobs
```
