# Nanocodex Agent

The owned lifecycle for one headless `OpenAI` coding agent.

`nanocodex-agent` composes the Tower-native Responses state machine from
`nanocodex-oai-api` with the runtime from `nanocodex-tools`. A normal consumer
builds one agent, receives a cheap cloneable [`Nanocodex`] handle and an
independent [`AgentEvents`] stream, then submits ordered prompts.

## Quick start

```rust,no_run
use nanocodex_agent::{Nanocodex, OpenAi};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Nanocodex::builder(openai)
    .instructions(
        "You are a Rust coding agent. Preserve unrelated work and run relevant tests.",
    )
    .workspace(std::env::current_dir()?)
    .build()?;

let result = agent
    .prompt("Explain the cause of the failing parser test.")
    .await?
    .await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

The first `await` means the private driver accepted and ordered the prompt.
[`Turn`] is both a per-turn event stream and a future for [`TurnResult`].
Awaiting the turn waits only for its result; event consumption is independent.

The private driver is the sole owner of mutable conversation, transport, tool,
and process state. Cloning [`Nanocodex`] only clones its command capability;
[`Nanocodex::spawn`] creates a clean sibling and [`Nanocodex::fork`] creates an
independent branch from committed history.

## Remote tool environments

When tools execute in a VM or remote workspace, provide one coherent snapshot
of the facts described to the model. This prevents host time and `AGENTS.md`
discovery from being mixed with a different tool filesystem:

```rust,no_run
use nanocodex_agent::{ExecutionEnvironment, Nanocodex, OpenAi};

# fn build(openai: OpenAi) -> Result<(), Box<dyn std::error::Error>> {
let environment = ExecutionEnvironment::new("2026-07-29", "Etc/UTC")
    .project_instructions("Preserve generated files under build/.");
let (_agent, _events) = Nanocodex::builder(openai)
    .execution_environment(environment)
    .build()?;
# Ok(())
# }
```

Omit [`ExecutionEnvironment::project_instructions`] when the remote workspace
has no project instructions. Without an execution environment, native agents
continue discovering date, timezone, and project instructions from the local
embedding host.

## Typed events

[`AgentEvents`] is optional and independent from turn results. Its raw
JSONL-compatible envelope remains lossless, while
[`AgentEvent::data`](nanocodex_agent::events::AgentEvent::data) provides a
normalized domain view:

```rust,no_run
use futures_util::StreamExt;
use nanocodex_agent::{
    Nanocodex, OpenAi,
    events::{AgentEventData, AssistantEvent},
};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, mut events) = Nanocodex::builder(openai)
    .instructions("Answer concisely and preserve exact identifiers.")
    .build()?;
let turn = agent.prompt("Explain the identifier req_7f3.").await?;

while let Some(event) = events.next().await {
    if let AgentEventData::Assistant(AssistantEvent::Delta(delta)) = event.data()? {
        print!("{}", delta.text);
    }
    if event.kind.is_terminal() {
        break;
    }
}
let _result = turn.await?;
agent.shutdown().await?;
# Ok(())
# }
```

## Components

- [`events`](nanocodex_agent::events) contains the complete typed lifecycle
  event taxonomy.
- [`input`](nanocodex_agent::input) contains prompts and multimodal user input.
- [`session`](nanocodex_agent::session) contains session identities and
  serializable resume snapshots.
- [`execution`](nanocodex_agent::execution) is the neutral model/tool/checkpoint
  interception seam implemented by optional higher-layer policies.
- [`usage`](nanocodex_agent::usage) contains token accounting and USD estimates.
- [`rollout`](nanocodex_agent::rollout) records and restores Codex-compatible
  sessions.
- [`transport`](nanocodex_agent::transport) exposes advanced Responses and
  Tower configuration.
- [`tools`](nanocodex_agent::tools) exposes the complete tool implementation
  surface.

OpenAI API-key and managed ChatGPT credentials belong to
[`nanocodex_oai_api::auth`], independently of this lifecycle crate.
Portable journals, durable admission, and recovery policy belong to
`nanocodex-durability`, which depends on this crate; the agent never depends on
that optional layer. An attached execution policy is owned by exactly one
agent. A clean `spawn` deliberately creates an ordinary in-memory child without
that policy; `fork` returns an explicit error because inherited committed context
requires an independently owned policy.
