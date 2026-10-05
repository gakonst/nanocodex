# Nanocodex

The batteries-included façade for the Nanocodex frontier-agent building blocks.

This crate contains no second runtime implementation. It re-exports the owned
agent lifecycle and gives the lower-level crates stable, named module paths.
Depending on `nanocodex-agent` directly creates the same agent.

Upgrading from 0.5? Read the [Rust API changelog and migration guide](https://github.com/gakonst/nanocodex/blob/v0.6.0/docs/MIGRATING_0_6.md)
for breaking signatures, changed defaults, and snapshot/tool migrations.

## Quick start

Build one owned agent, keep its cheap cloneable handle, and await typed turn
results. The independent event stream is optional:

```rust,no_run
use nanocodex::{Nanocodex, OpenAi};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let (agent, _events) = Nanocodex::builder(openai)
    .instructions(
        "You are a Rust coding agent. Preserve unrelated work and run relevant tests.",
    )
    .workspace(std::env::current_dir()?)
    .build()?;

let turn = agent
    .prompt("Explain the cause of the failing parser test.")
    .await?;
let result = turn.await?;

println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

Awaiting `prompt` means the private driver accepted and ordered the turn.
Awaiting the returned [`Turn`] waits for its complete [`TurnResult`]; it does
not wait for the turn's optional event stream to be consumed. Follow-on prompts
reuse the same retained context and transport without asking the caller to
manage response IDs or history.

`gpt-6-astra` with low reasoning is the SDK default; `.model(Model::Sol)` and
`.model(Model::Luna)` select the other supported models
when creating the agent. Model selection uses the catalog's default reasoning
unless an effort was explicitly selected. Astra requires low or greater reasoning. A caller may change the model
before the first turn is accepted; it then remains fixed for the thread so follow-on turns can continue from the provider
checkpoint without replaying the complete retained context.

## Claude

Enable `claude` on the facade to use Anthropic Messages with the same owned
agent lifecycle and durability extension:

```toml
[dependencies]
nanocodex = { version = "0.6.5", features = ["claude"] }
reqwest = "0.13"
```

```rust,no_run
# #[cfg(all(feature = "claude", feature = "durability"))]
# async fn claude_turn() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{Claude, DurableAgentExt, Nanocodex};
use nanocodex::claude::ClaudeClient;
use nanocodex::durability::{DurableSession, MemoryStore};

let client = ClaudeClient::official(
    reqwest::Client::new(),
    std::env::var("ANTHROPIC_API_KEY")?,
);
let state = DurableSession::open(MemoryStore::new()?, "claude-session").await?;
let (agent, _events) = Nanocodex::builder(Claude::latest(client))
    .system("Answer concisely.")
    .durability(state)
    .await?
    .build()?;
let result = agent.prompt("Explain durable request replay.").await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

`MemoryStore` retains state in memory; use a persistent host store when state
must survive process restarts. The facade automatically enables the Claude
adapter whenever `claude` and `durability` are enabled together.

Default features remain `durability`, `openai`, and `tools`. For the minimal
Claude provider path, use `default-features = false, features = ["claude"]`.
Add `durability` for the extension above; durability retains its existing
OpenAI dependency. On native targets, add `workspace-tools` to expose Claude's optional workspace
file tools as well as the standard workspace runtime. The `claude` feature
alone does not enable durability or workspace tools.

## xAI

Enable the default-off `xai` feature for the Grok Build Responses source
adaptation. `default-features = false, features = ["xai"]` selects this backend
without enabling the OpenAI runtime or durability:

```rust,no_run
# #[cfg(feature = "xai")]
# async fn xai_turn() -> Result<(), Box<dyn std::error::Error>> {
use nanocodex::{Nanocodex, Xai, XaiModel};
use nanocodex::xai::XaiClient;

let client = XaiClient::new(
    reqwest::Client::new(),
    "https://api.x.ai/v1/responses",
    std::env::var("XAI_API_KEY")?,
);
let (agent, _events) = Nanocodex::builder(Xai::new(client, XaiModel::Grok46.as_str()))
    .build()?;
println!("{}", agent.prompt("Explain this protocol.").await?.await?.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

The host supplies credentials and function handlers. Enable `xai-tools` for the
independent native file/shell and portable host adapters, and the `xai` feature
of `nanocodex-durability` for fenced persistent sessions. Manual and automatic
compaction, steering, snapshots, and shared subagents use the common lifecycle.
No `grok` executable is required. See the
[xAI backend guide](../../docs/NANOXAI.md) for the exact upstream revision,
license provenance, supported surface and limitations.

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
- `xai` — xAI Responses client, builder and host tool contracts when the
  default-off `xai` feature is enabled
- `xai_tools` — explicitly installed xAI host capabilities when `xai-tools` is enabled
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
