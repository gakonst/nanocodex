# Migrating from Nanocodex 0.2.x

PR #50 replaces the old monolithic crate surface with a library-first stack.
It is source-breaking and must ship under a version newer than the published
`0.2.0`; the workspace version on the refactor branch is not a release claim.

## Construct the OpenAI recipe explicitly

Agent construction now separates provider and transport policy from agent
lifecycle policy.

Before:

```rust,ignore
let (agent, events) = Nanocodex::builder(api_key)
    .instructions("Preserve exact identifiers and run relevant tests.")
    .build()?;
```

After:

```rust,ignore
use nanocodex::{Nanocodex, OpenAi};

let openai = OpenAi::new(api_key)?;
let (agent, events) = Nanocodex::builder(openai)
    .instructions("Preserve exact identifiers and run relevant tests.")
    .build()?;
```

The removed `Nanocodex::new(auth)` convenience follows the same migration:
construct `OpenAi` first, then provide explicit agent instructions.

## Configure transport and Tower on `OpenAi`

The removed `Responses::builder()` wrapper and
`NanocodexBuilder::responses(...)` adapter are now one `OpenAiBuilder`.

Before:

```rust,ignore
let responses = Responses::builder()
    .transport(ResponsesTransport::Https)
    .store(false)
    .layer(TimeoutLayer::new(Duration::from_secs(180)))
    .build();

let (agent, events) = Nanocodex::builder(auth)
    .responses(responses)
    .build()?;
```

After:

```rust,ignore
use std::time::Duration;

use nanocodex::{
    Nanocodex, OpenAi,
    oai::transport::ResponsesTransport,
};
use tower::timeout::TimeoutLayer;

let openai = OpenAi::builder(auth)
    .transport(ResponsesTransport::Https)
    .store(false)
    .layer(TimeoutLayer::new(Duration::from_secs(180)))
    .build()?;

let (agent, events) = Nanocodex::builder(openai)
    .instructions("Preserve exact identifiers and run relevant tests.")
    .build()?;
```

Use `OpenAiBuilder::service` for a caller-defined
`Service<ResponsesAttempt>`. The factory still runs once for every root,
spawned sibling, and fork.

## Await turns or stream them

`prompt().await` still means accepted and ordered, not completed. `Turn` now
implements both `Future<Output = Result<TurnResult, NanocodexError>>` and
`Stream<Item = AgentEvent>`.

```rust,ignore
let turn = agent.prompt("Explain the failing parser test.").await?;
let result = turn.await?;
println!("{}", result.final_message());
```

`turn.result().await` remains the equivalent convenience method. Result
readiness is independent from event consumption: awaiting a `Turn` does not
drain or wait for its per-turn event stream. Consume `AgentEvents` when the
application needs the complete session-wide event record.

The result's assistant text is now private typed state. Replace direct field
access:

```rust,ignore
println!("{}", result.final_message);
```

with:

```rust,ignore
println!("{}", result.final_message());
```

## Use canonical component paths

The facade root now contains only the golden path. Detailed imports move under
their owning component:

| Old root import | New canonical import |
| --- | --- |
| `ChatGptLogin`, `load_chatgpt_auth` | `nanocodex::oai::auth::{ChatGptLogin, load_chatgpt_auth}` |
| `ResponsesAttempt`, `ResponsesClient` | `nanocodex::oai::tower::{ResponsesAttempt, ResponsesClient}` |
| `ResponsesHistory`, `ResponsesTransport` | `nanocodex::oai::transport::{ResponsesHistory, ResponsesTransport}` |
| `Mcp`, `McpServer` | `nanocodex::tools::mcp::{Mcp, McpServer}` |
| `SessionSnapshot` | `nanocodex::agent::session::SessionSnapshot` |
| `RolloutConfig` | `nanocodex::agent::rollout::RolloutConfig` |

Common `Nanocodex`, `OpenAi`, `Tool`, `Tools`, and `tool` imports remain at the
facade root. A lower-level consumer can instead depend directly on
`nanocodex-agent`, `nanocodex-oai-api`, or `nanocodex-tools`.

## Resume through the same recipe

Snapshots remain caller-owned and unredacted. Resumption now uses the explicit
OpenAI recipe:

```rust,ignore
use nanocodex::{Nanocodex, OpenAi};

let openai = OpenAi::new(api_key)?;
let (resumed, events) = Nanocodex::builder(openai)
    .instructions(
        "You are a Rust coding agent. Preserve unrelated work and run relevant tests.",
    )
    .tools(tools)
    .workspace(workspace)
    .resume(snapshot)
    .build()?;
```

Configure the instructions, tool definitions, and handlers that the resumed
runtime should use. They may differ from the completed snapshot's original
runtime contract; the current prefix is rebound while authoritative typed
history and cache lineage are retained. Workspace and explicit cache-key
changes remain incompatible. The first request after restore replays complete
client-owned typed history; callers never supply response IDs or prior
messages.

## Removed package boundaries

The former `nanocodex-core`, `nanocodex-service`, `nanocodex-mcp`, and
`nanocodex-macros` packages are not compatibility layers. Their supported
contracts moved into `nanocodex-oai-api` and `nanocodex-tools`.
`nanocodex-tools-macros` is an implementation package re-exported through
`nanocodex-tools`; applications should not depend on it directly.
