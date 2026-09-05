# nanocodex-subagents

`nanocodex-subagents` is an optional extension above `nanocodex-agent`. It adds
a shared task tree and seven agent-relative tools without making the core agent
depend on orchestration policy:

- `spawn_agent`
- `submit_result`
- `send_agent_message`
- `list_agents`
- `wait_agent`
- `interrupt_agent`
- `close_agent`

`spawn_agent` accepts optional `model` (`sol`, `terra`, `luna`, or `astra`) and
`thinking` (`none` through `max`) overrides. Omitted values inherit the
invoking agent's current settings; an override configures only the new child.

Create one channel for an application-owned agent family, then install fresh
tools for every driver with `NanocodexBuilder::tools_factory`:

```rust,ignore
use std::sync::Arc;
use nanocodex_agent::Nanocodex;
use nanocodex_subagents::{channel, install_tools, DEFAULT_MAX_SUBAGENTS};
use nanocodex_tools::Tools;

let (registry, control, mut updates) = channel(DEFAULT_MAX_SUBAGENTS);
let base_tools = Tools::builder().build()?;
let tool_registry = Arc::clone(&registry);
let (agent, events) = Nanocodex::builder(openai)
    .tools_factory(move |handle| {
        install_tools(base_tools.clone(), handle, Arc::clone(&tool_registry))
    })
    .build()?;

// Drain `updates` for child events and application UI state. Before stopping
// the root, close its complete task tree:
control.close_all(&agent.session_id().to_string()).await?;
agent.shutdown().await?;
```

The crate supports native executors and `wasm32-unknown-unknown`. JavaScript
consumers use the same runtime through `Subagents.create()` in the `nanocodex`
Node and browser packages.
