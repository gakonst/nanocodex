# Nanocodex

Nanocodex is a small, headless Rust agents SDK. It is a library first: embed it
in your process, configure the agent and its tools, submit turns through a cheap
handle, and decide whether to consume every event or only typed final results.
There is no required app server, JSON-RPC layer, global runtime, or UI. The CLI
and Harbor integration in this repository are thin adapters over the same
public library API.

The scope is deliberately narrow. Nanocodex currently runs `gpt-5.6-sol` over
the OpenAI Responses WebSocket API, preserves one stateful session across
follow-on prompts, and exposes its transport as a caller-composable Tower
service. Model-generated code mode runs in local Node.js and calls the Rust tool
registry.

## Use the daily-driver CLI

Install the repository binary and launch it from the workspace you want the
agent to edit:

```sh
cargo install --path bin/nanocodex
export OPENAI_API_KEY=...
nanocodex
```

The Ratatui interface keeps one agent and WebSocket alive across follow-on
prompts, streams assistant output, shows tool activity, accepts prompts while a
turn is running, and retains prompt history and scrollback for the session.
Press Enter to submit, Ctrl+J or Shift+Enter for a newline, Up/Down for prompt
history, PageUp/PageDown or the mouse wheel to scroll, Esc to clear the
composer, and Ctrl+C to exit. Use `--cwd`, `--thinking`, `--system-prompt`,
`--web-search`, and `--image-generation` to configure the session; `--prompt`
submits an initial turn immediately.

The headless adapter remains available for scripts and evals. Its stdout is
flushed JSONL only:

```sh
nanocodex run "Inspect this repository and summarize it."
```

The CLI accepts the same MCP providers as the library. For example, a local
stdio server can be exercised across repeated turns on one retained session:

```sh
nanocodex \
  --mcp-stdio workspace=node \
  --mcp-arg workspace=./server.mjs \
  run --repeat 3 "Search the workspace tools and summarize the result."
```

Lifecycle tracing is written to stderr for headless runs and to
`.nanocodex/logs/tui.log` for the TUI. `--log-format json` selects structured
local logs, `RUST_LOG` or `--log-filter` controls filtering, and
`--otel-endpoint http://localhost:4318` exports spans over OTLP/HTTP.
`OTEL_LEVEL` or `--otel-filter` controls export independently from local logs.
Run `just otel-up` followed by `just otel-demo` for a local Jaeger waterfall;
use `just otel-stress` for the deterministic hostile-tool pressure gate. The
complete walkthrough is in [`docs/OBSERVABILITY.md`](docs/OBSERVABILITY.md).

## Use it as a library

Until the crates are published, depend on the repository directly:

```toml
[dependencies]
nanocodex = { git = "https://github.com/gakonst/nanocodex" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The smallest useful program submits one prompt and awaits its typed result. If
you do not need live events, destructure them as `_`; the receiver is dropped
immediately and event production becomes a no-op:

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

`Nanocodex::new` uses the standard prompt, medium thinking, built-in tools,
persistent WebSocket, and retry/reconnect policy. Node.js 12.22 or newer must be
available on `PATH` for model-generated code mode.

### Follow-on prompts and events

`build()` spawns the stateful agent driver and returns `(Nanocodex,
AgentEvents)`. `Nanocodex` is a cheap, cloneable command handle. Calling
`prompt(...)` accepts and queues a turn, then immediately returns a `Turn`; the
agent continues independently until `turn.result()` is awaited.

The session retains the complete typed conversation history. A follow-on prompt
does **not** need the previous `final_message`, transcript, response ID, or tool
results passed back into it. On a healthy socket Nanocodex continues with
`previous_response_id`; after a reconnect it transparently replays its retained
history.

```rust
use nanocodex::{AgentEventKind, Nanocodex};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let (agent, mut events) = Nanocodex::new(api_key)?;

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if event.kind == AgentEventKind::AssistantMessage {
                eprintln!("assistant message emitted");
            }
        }
    });

    let first = agent.prompt("Choose one word for this project.").await?;
    // The caller can do unrelated work while the turn runs.
    let first = first.result().await?;
    println!("first: {}", first.final_message);

    // No first.final_message is passed here. The agent has the first turn.
    let second = agent
        .prompt("Return the word you chose, but in uppercase.")
        .await?;
    println!("second: {}", second.result().await?.final_message);
    Ok(())
}
```

`AgentEvents` is the single ordered event stream for the session and is
independent from turn results. A server, TUI, notebook, or language binding can
translate all events, select a subset, or ignore them without changing prompt
and result handling.

### Define custom tools

The `#[tool]` macro turns a normal async Rust function into a typed tool. It
derives the JSON Schema from the function arguments, decodes calls, awaits the
function, and returns the serialized result through the heterogeneous tool
registry:

```rust
use nanocodex::{Nanocodex, Tools, tool};

#[tool(description = "Multiplies two signed integers.")]
async fn multiply(left: i64, right: i64) -> Result<i64, &'static str> {
    left.checked_mul(right).ok_or("integer overflow")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let tools = Tools::builder().tool(multiply).build()?;
    let (agent, _) = Nanocodex::builder(api_key).tools(tools).build()?;

    let result = agent
        .prompt("Use the multiply tool to calculate 6 × 7, then return it.")
        .await?
        .result()
        .await?;
    println!("{}", result.final_message);
    Ok(())
}
```

`Tools::builder()` starts with the standard optional web-search and
image-generation integrations enabled. Use `.without_defaults()` to disable
those optional integrations before adding application tools. The core local
coding tools remain available through code mode.

For dynamic state, freeform inputs, multimodal outputs, metadata, or custom
decoding, implement the public `Tool` trait directly and register the value with
the same `.tool(...)` method. Internal and external tools use the same
heterogeneous registry. See
[`custom_tool.rs`](examples/custom_tool.rs) for a runnable
example.

Runnable examples live in the top-level [`examples`](examples) package:

```sh
cargo run -p nanocodex-examples --bin minimal
cargo run -p nanocodex-examples --bin follow-on
cargo run -p nanocodex-examples --bin custom-tool
cargo run -p nanocodex-examples --bin subagents
cargo run -p nanocodex-examples --bin mcp
```

### Add deferred MCP tools

`nanocodex-mcp` implements Streamable HTTP and stdio MCP clients as a dynamic
Code Mode tool provider. Each configured server initializes and runs
`tools/list` concurrently when the owned agent starts. Only the compact
`tool_search` definition is in the initial model prompt; matching tools are
activated on demand and can be called immediately from the same code cell.

```rust
use nanocodex::{Mcp, McpServer, Nanocodex, Tools};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let mcp = Mcp::builder()
    .server(
        "workspace",
        McpServer::http("https://mcp.example.com/mcp")
            .bearer_token_env("WORKSPACE_MCP_TOKEN"),
    )
    .server(
        "local",
        McpServer::stdio("node").args(["./server.mjs"]),
    )
    .build()?;
let tools = Tools::builder().provider(mcp).build()?;
let (agent, _) = Nanocodex::builder(api_key).tools(tools).build()?;

let result = agent
    .prompt("Search the configured MCP tools, use the relevant read-only tool, and summarize.")
    .await?
    .result()
    .await?;
println!("{}", result.final_message);
# Ok(())
# }
```

HTTP authentication can come from a bearer token or arbitrary fixed/environment
headers; secret values are resolved only by the background connection task.
Server/tool filters and startup/tool timeouts are configured per `McpServer`.
See [`mcp.rs`](examples/mcp.rs) for a runnable example.

### Add tracing and OpenTelemetry

Nanocodex libraries emit stable `tracing` spans for sessions, turns, model
calls, Responses attempts and connections, retries, tools, and MCP activity.
They never install a global subscriber, so an embedding application can use
its existing formatting, metrics, or OpenTelemetry stack. Contractual
`AgentEvents` remain separate from diagnostic tracing.

The optional `nanocodex-observability` crate provides the same compact stderr,
JSON/file, and OTLP/HTTP setup used by the CLI:

```toml
[dependencies]
nanocodex-observability = { git = "https://github.com/gakonst/nanocodex" }
```

```rust
use nanocodex_observability::{LogFormat, ObservabilityBuilder};

# fn install() -> Result<(), Box<dyn std::error::Error>> {
let _guard = ObservabilityBuilder::new("my-agent", env!("CARGO_PKG_VERSION"))
    .filter("warn,nanocodex=info,nanocodex_service=info,nanocodex_mcp=info")
    .otel_filter("warn,nanocodex=info,nanocodex_service=info,nanocodex_mcp=info")
    .format(LogFormat::Json)
    .otlp_endpoint("http://localhost:4318")
    .install()?;
# Ok(())
# }
```

Keep the returned guard alive for the application lifetime so non-blocking
formatting and batched trace export are flushed during shutdown. Spans include
IDs, attempt/replay state, durations, status, token/cache usage, structural
prompt/tool metadata, process outcomes, and API-visible reasoning summaries.
Full prompts, Code Mode source, tool argument values, hidden reasoning, and API
keys are never attached.

### Embed from Python, Node.js, or a browser Worker

The language bindings preserve the same owned session rather than wrapping the
CLI or starting an app server:

```python
from nanocodex import Nanocodex

agent, events = Nanocodex(api_key, thinking="low")
first = agent.prompt("Choose one word for this project.")
print(first.result())
second = agent.prompt("Return that word in uppercase.")
print(second.result())  # no previous result or transcript is passed back
```

The PyO3 extension owns a native Tokio runtime and exposes `Nanocodex`, `Turn`,
and the ordered event receiver directly. See
[`bindings/python`](bindings/python) for build instructions and the top-level
[`examples/python`](examples/python) programs.

Node.js and web consumers use one shared Rust/WASM artifact. Node supplies a
header-capable WebSocket and can define async JavaScript tools; a browser Worker
supplies its own authenticated WebSocket boundary and browser-native tools:

```js
const turn = agent.prompt("Use multiply to calculate 6 × 7.");
console.log(await turn.result());
const followOn = agent.prompt("Add one to that result.");
console.log(await followOn.result());
```

See the top-level [`examples/node`](examples/node) and
[`examples/react-vite`](examples/react-vite) consumers. The React example runs
the persistent Rust/WASM agent in a real module Worker, displays the ordered
event stream, and registers a browser-native custom tool. Browser WebSockets
cannot set the Responses authorization upgrade header, so Nanocodex does not
pretend direct browser authentication works and does not ship a relay; the
embedding application supplies an already-authorized endpoint or custom
`createWebSocket` implementation.

[`subagents.rs`](examples/subagents.rs) shows that delegation does not require a
multi-agent subsystem in the library. Its application-defined `spawn_agent`
tool builds an independent `Nanocodex` for each task; the parent can invoke
several of them concurrently from code mode with `Promise.all`. The example
keeps delegation one level deep by leaving `spawn_agent` out of each child's
tool registry. It also routes every parent and child `AgentEvent` through one
host-owned writer, producing a unified JSONL stream with a global `stream_seq`
and a tagged `source` while retaining each event's session-local `request_id`
and `seq`.

### Configure the agent and Tower stack

`Nanocodex::builder(api_key)` exposes deliberate overrides for the system
prompt, thinking level, tools, workspace, stable session ID, and Responses
stack. `.prompt(...)` on the builder replaces the system/developer prompt;
`.prompt(...)` on the built handle submits a user turn.

Add `tower = { version = "0.5", features = ["limit", "timeout"] }` when
composing the middleware used below.

```rust
use std::time::Duration;

use nanocodex::{AgentEvents, Nanocodex, Responses, Thinking};
use tower::{limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};

fn build_agent(api_key: String) -> nanocodex::Result<(Nanocodex, AgentEvents)> {
    let responses = Responses::builder()
        .layer(TimeoutLayer::new(Duration::from_secs(120)))
        .layer(ConcurrencyLimitLayer::new(1))
        .build();

    Nanocodex::builder(api_key)
        .prompt("You are a concise repository maintenance agent.")
        .thinking(Thinking::Medium)
        .workspace("/work/project")
        .responses(responses)
        .build()
}
```

Tower layers are deferred until the standard persistent-WebSocket service is
created. Callers can add deadlines, concurrency limits, load shedding, tracing,
metrics, circuit breaking, or error mapping without boxing the client or
rebuilding agent orchestration. `Responses::builder().service(stack)` replaces
the standard service with any caller-composed
`tower::Service<ResponsesAttempt>`.

See [`docs/RESPONSES_TOWER.md`](docs/RESPONSES_TOWER.md) for the implemented
operation boundary, layer ordering, retry safety, and benchmark evidence.

### Crate boundaries

The workspace exposes five independently useful library layers, following the
same boundary style as `alloy-core` and Alloy's ergonomic top-level crate:

- `nanocodex-core`: dependency-light prompts, events, model configuration, and
  complete typed Responses wire/domain types.
- `nanocodex-service`: persistent WebSocket transport, stream processing,
  typed errors, Tower service/client, retry middleware, and telemetry.
- `nanocodex-tools`: built-in tools, code mode, heterogeneous tool registry,
  and the public tool trait.
- `nanocodex-mcp`: background MCP transports, discovery catalog, BM25
  `tool_search`, authentication inputs, and deferred Code Mode dispatch.
- `nanocodex`: owned agent lifecycle, builders, and ergonomic re-exports.

`nanocodex-macros` implements `#[tool]`. The `nanocodex-bin` package under
`bin/nanocodex` is an example CLI adapter, not the SDK boundary.
The PyO3 and Rust/WASM packages under `bindings/` are likewise thin embedded
adapters over the owned session and typed event contract.

## Develop this repository

```sh
just bootstrap      # install pinned host dependencies once
just run            # native low-effort smoke; requires local Node.js
just prepare-evals  # build/cache tasks and the shared verifier toolbox
just eval           # fresh full model-driven Terminal-Bench suite
just eval-hosted    # same pinned suite in hosted Daytona sandboxes
just view           # inspect retained Harbor jobs
```

The native CLI defaults to the interactive Ratatui client. Its `run` subcommand
accepts one positional prompt and streams flushed JSONL to stdout for Harbor and
other process integrations. Neither adapter is required by the library.

Harbor builds a static Linux binary, installs it in an unchanged task container,
and derives ATIF from the retained JSONL. Python owns upload/process lifecycle
only; model decisions, API calls, tools, and mutations remain in Rust.

```text
native BuildKit compile -> static Linux binary
                       -> Harbor task container
                       -> /installed-agent/nanocodex
                       -> Rust executes tools in /app
                       -> Harbor verifier
```

Local artifacts use Cargo's `dev` profile. Set
`NANOCODEX_BUILD_PROFILE=profiling` for an optimized build with debug symbols.
The pinned eval selection lives in
[`evals/terminal-bench-2.yaml`](evals/terminal-bench-2.yaml), not the Justfile.

Hosted evals use Harbor's Daytona environment and a separate AMD64 artifact:

```sh
just eval-task-hosted terminal-bench/fix-git
just eval-hosted
```

Retained jobs live under `.nanocodex/harbor/jobs`; `just view` opens them. The
latest full 41-task gate scored 38/41 with zero Responses retries or WebSocket
reconnects and 92.23% cached input. One task hit a transient upstream policy
rejection after producing a verifier-passing artifact and passed an isolated
rerun. Current architecture, validation policy, failure classifications, and
ordered future work live in [`PLAN.md`](PLAN.md).
