# Nanocodex

The batteries-included façade for the Nanocodex frontier-agent building blocks.

This crate contains no second runtime implementation. It re-exports the owned
agent lifecycle and gives the lower-level crates stable, named module paths.
Depending on `nanocodex-agent` directly creates the same agent.

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

`gpt-5.6-sol` is the SDK default; `.model(Model::Terra)`, `.model(Model::Luna)`,
and `.model(Model::Astra)` select the other supported models when creating the
agent. Astra requires low or greater reasoning. A caller may change the model
before the first turn is accepted; it then remains fixed for the thread so follow-on turns can continue from the provider
checkpoint without replaying the complete retained context.

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
`nanocodex_oai_api`, `nanocodex_tools`, `nanocodex_agent`, or
`nanocodex_durability`.
