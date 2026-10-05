# Nanocodex

The batteries-included façade for the Nanocodex frontier-agent building blocks.

This crate contains no second runtime implementation. It re-exports the owned
agent lifecycle and gives the lower-level crates stable, named module paths.
Depending on `nanocodex-agent` directly creates the same agent.

Upgrading from 0.5? Read the [Rust API changelog and migration guide](https://github.com/gakonst/nanocodex/blob/v0.6.0/docs/MIGRATING_0_6.md)
for breaking signatures, changed defaults, and snapshot/tool migrations.

## Quick start

The native facade chooses a provider and installs its supported tools. The
workspace defaults to the current directory:

```rust,no_run
use nanocodex::{Backend, Nanocodex};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let backend = Backend::codex(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Nanocodex::builder(backend).build()?;
let result = agent.prompt("Read README.md and explain this project.").await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

Run this inside a Tokio runtime. `.workspace(path)` chooses another existing
working directory, and `.instructions(text)` replaces the system instructions.
The directory does not sandbox commands: tools run with the embedding process's
permissions. Applications that need isolation should configure a concrete
builder with their authorized host capabilities.

`Backend::codex` and `Backend::claude` return the same `Backend` type, so provider
selection works in a function or match without changing the calling lifecycle.
Enable Claude with `cargo add nanocodex --features claude`:

```rust,no_run
# #[cfg(feature = "claude")]
# async fn choose(use_claude: bool) -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{Backend, Nanocodex};
let backend = if use_claude {
    Backend::claude(std::env::var("ANTHROPIC_API_KEY")?)?
} else {
    Backend::codex(std::env::var("OPENAI_API_KEY")?)?
};
let (agent, _events) = Nanocodex::builder(backend).build()?;
let result = agent.prompt("Read README.md and explain this project.").await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

Codex installs the existing CLI tool catalog: Code Mode, command sessions,
patching, plan and file tools, provider-backed web/image tools, and subagents.
Claude installs native Bash, Read, Edit, Write, Glob, Grep, TaskCreate, TaskGet,
TaskList, TaskUpdate, TodoWrite, NotebookEdit, shared subagent callbacks, and
client-side tool discovery and deferred WebSearch. WebSearch makes a nested
Messages request with Anthropic server search; it is not attached to every root
request. Bash uses the CLI's retained workspace process
runtime with deadlines and cancellation. These are the supported SDK adapters;
this does not import every Claude Code product feature. External MCP servers,
computer/browser hosts, account integrations, and subscription login remain
explicit capabilities of the advanced builders.

The default models come from `HarnessFamily::default_model()` (`gpt-6-astra`
and `claude-opus-5-5`). Pin a same-family catalog model on the provider recipe
with `backend.model(HarnessModel::Codex(Model::Sol))?`. Credentials are explicit;
constructors do not load a local CLI login. `backend.endpoint(url)?` selects an OpenAI API base or complete Claude Messages URL,
including loopback endpoints for transport tests.

Awaiting `prompt` means the native driver accepted the turn. Awaiting its
returned `Turn` waits for the `TurnResult`, independently of event consumption.
Follow-on prompts keep the native conversation and transport. The `native`
feature is enabled by default; minimal builds without it retain the concrete
provider APIs. Await `agent.shutdown()` to stop the root and join its subagents
and retained shell processes. Cloned handles share that cleanup result; dropping
the final handle starts cleanup without a joinable receipt.

## Durability

The common builder preserves the existing durability extension:

```rust,no_run
# #[cfg(feature = "durability")]
# async fn durable() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{Backend, DurableAgentExt, Nanocodex, PromptRequest};
use nanocodex::durability::{DurableSession, MemoryStore};
let store = MemoryStore::new()?;
let state = DurableSession::open(store, "coding-session").await?;
let backend = Backend::codex(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Nanocodex::builder(backend)
    .durability(state).await?
    .build()?;
let result = agent.prompt(
    PromptRequest::new("Read README.md").request_id("read-project"),
).await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

`MemoryStore` retains state in memory; use a persistent host store to survive
process restarts. Claude uses the same extension when `claude` and `durability`
are enabled. Concrete `Nanocodex::builder(OpenAi::new(key)?)` and
`Nanocodex::builder(Claude::latest(client))` remain the advanced path for custom
transports, tools, authentication and execution policy.

## Reusable native harnesses

`Harness` composes explicitly registered construction recipes. Each recipe
keeps its concrete provider, service type, authentication, tools and execution
policy until `.build()` returns the common `(Nanocodex, AgentEvents)` lifecycle.
There is no provider-neutral builder that translates Messages into Responses.
Enable `claude` alongside `openai` to compose both families:

```rust,no_run
# #[cfg(all(feature = "claude", feature = "openai"))]
# async fn mixed() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{
    Claude, ClaudeModel, Harness, HarnessFamily, HarnessModel, Model,
    Nanocodex, OpenAi,
    agent::SpawnOptions,
    claude::ClaudeClient,
};

let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let claude = ClaudeClient::official(
    reqwest::Client::new(), std::env::var("ANTHROPIC_API_KEY")?,
);
let harness = Harness::builder()
    .register(HarnessFamily::Codex, move |request| {
        let openai = openai.clone();
        async move {
            let HarnessModel::Codex(model) = request.model else { unreachable!() };
            let mut builder = Nanocodex::builder(openai)
                .model(model).thinking(request.thinking)
                .host_context(request.host_context)
                .spawn_factory(request.spawn_factory);
            if let Some(snapshot) = request.snapshot {
                builder = builder.restore_runtime(snapshot)?;
            }
            builder.build()
        }
    })
    .register(HarnessFamily::Claude, move |request| {
        let claude = claude.clone();
        async move {
            let mut builder = Nanocodex::builder(Claude::new(claude, request.model.as_str()))
                .thinking(request.thinking)?
                .host_context(request.host_context)
                .spawn_factory(request.spawn_factory);
            if let Some(snapshot) = request.snapshot {
                builder = builder.restore_runtime(snapshot)?;
            }
            builder.build()
        }
    })
    .build();

let (codex, _events) = harness.start(HarnessModel::Codex(Model::Sol)).await?;
let (claude, _events) = harness.start_with(
    SpawnOptions::new().harness(HarnessFamily::Claude)
        .harness_model(HarnessModel::Claude(ClaudeModel::Sonnet55)),
).await?;
println!("{}", claude.prompt("Explain the parser.").await?.await?.final_message());
claude.shutdown().await?;
codex.shutdown().await?;
# Ok(())
# }
```

Install host capabilities through each concrete builder's `.tools_factory(...)`.
The callback receives a weak `AgentHandle` for that particular root or child;
attach the same `nanocodex-subagents::Registry` to both families to share child
IDs, messaging, structured submission, waiting, interruption and close. Codex
returns `Tools`; Claude returns native `ClaudeTools`. The host owns any callback
bridge and its authorization. Registration alone supplies no tools or credentials.
`request.spawn_factory` must be attached to each recipe so descendants can route
through the same harness. `Harness::spawn_factory()` also attaches that router
to an independently constructed concrete builder.

Omitting child overrides inherits the live parent's family, model and effort.
Selecting another family uses that family's model and effort defaults; selecting
another model uses that model's effort default. Explicit family/model mismatches,
unsupported effort and unregistered routes fail before construction. The model
stays within the thread's native family. Codex `fork` remains a native history
operation; mixed-family spawning starts a clean conversation. Weak handles and
routed factories reject construction and restoration once their owner stops.

Idle residency checkpoints retain provider-native state and child identity in
memory. Recipes restore `request.snapshot` with newly authorized host tools and
credentials; they must preserve the selected model, effort and native state.
Residency restoration does not establish process-restart durability. Attach the
durability extension separately when that is required.

The [public library journey](tests/it/harness.rs) runs both real native builders
against localhost Responses HTTP and Messages SSE fixtures, exercises shared
registry completion and idle restoration, and checks stopped-owner fencing.
Run `cargo test -p nanocodex --all-features --test it harness:: -- --nocapture`;
the provider request transcript is retained in ignored `output/library-harness/`.
These boundaries do not establish full tool, fork, transport or Claude Code parity.

## Usage and USD estimates

When the provider reports aggregate usage for a completed turn, cost remains
explicit: Nanocodex automatically applies the selected model's published
standard or priority rates. Every supported model, including Astra, uses its
own published rates and long-context multipliers.

```rust,no_run
use nanocodex::{Nanocodex, OpenAi};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Nanocodex::builder(openai)
    .instructions("Answer concisely and preserve exact identifiers.")
    .build()?;

let result = agent.prompt("Explain the identifier req_7f3.").await?.await?;
if let Some(usage) = result.usage() {
    if let Some(cost) = usage.estimated_cost() {
        println!("estimated {}", cost.amount());
    } else {
        println!("cost unavailable: {}", usage.cost_status().as_str());
    }
}
agent.shutdown().await?;
# Ok(())
# }
```

## Progressive disclosure

The root exports only the golden-path types. Reach for a named module when an
embedding needs more control:

- [`agent`] — lifecycle policy, events, input, sessions, usage, and rollout
- [`durability`] — optional durable admission, effect replay, checkpoints, and
  host-store contracts layered over an agent
- [`oai`] — managed Responses sessions and the concrete Tower boundary
- `claude` — Anthropic Messages client, builder, protocol, and authentication
  when the default-off `claude` feature is enabled
- [`tools`] — tool contracts, built-ins, Code Mode, and MCP
- `observability` — native tracing and OTLP setup when the default-off
  `observability` feature is enabled
- [`prelude`] — common imports for the owned-agent path

Detailed items retain the documentation from their owning crate. Each lower
crate also includes its own focused guide and can be documented or consumed
without the facade.

## Canonical imports

Use the crate root for the common agent path and the module that owns a concept
when reaching for its detailed API:

```rust
use nanocodex::{Nanocodex, OpenAi};
use nanocodex::agent::{events::AgentEvent, session::SessionSnapshot};
use nanocodex::durability::{DurableSession, MemoryStore};
use nanocodex::oai::tower::ResponsesAttempt;
use nanocodex::tools::mcp::Mcp;

# fn type_check(
#     _: Option<Nanocodex>,
#     _: Option<OpenAi>,
#     _: Option<AgentEvent>,
#     _: Option<SessionSnapshot>,
#     _: Option<DurableSession>,
#     _: Option<MemoryStore>,
#     _: Option<ResponsesAttempt>,
#     _: Option<Mcp>,
# ) {}
```

The root convenience path and its owning module name the same type; for
example, [`OpenAi`] and [`oai::OpenAi`] are identical. The [`agent`] module
intentionally does not repeat sibling convenience exports: provider
configuration belongs under [`oai`], tool implementation belongs under
[`tools`], and lifecycle state belongs under [`agent`]. Applications that need
only one component can depend on its package directly and use
`nanocodex_oai_api`, `nanocodex_oai_tools`, `nanocodex_agent`, or
`nanocodex_durability`.
