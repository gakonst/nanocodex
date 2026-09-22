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

Subagents are ephemeral and exist only within the running parent runtime. Completed
or interrupted children can receive more work while that runtime remains alive.
Idle child drivers may be evicted and rehydrated from in-memory snapshots to
limit resident resources; those snapshots are never persisted. Restarting the
parent runtime drops the task tree, child history, messages, and results. A fresh
registry starts empty. Historical agent IDs do not identify recovered children;
use `list_agents` to discover the current live registry before addressing agents.

`spawn_agent` accepts optional `model` (`sol`, `luna`, or `astra`) and
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

Completion requires an accepted `submit_result({output})` for the active trusted
instruction revision. The model does not supply a turn token. The tool returns
`{accepted: true, status: "accepted"}` on acceptance. A superseded model request
returns `{accepted: false, status: "superseded"}` as normal continuation: incorporate
the updated instructions and submit again. Plain
assistant JSON is not accepted implicitly. Rejected submissions expose a stable
`CompletionErrorCode`, `recoverable`, and `recovery` guidance; native callers can
downcast the underlying `io::Error` to `CompletionError` or serialize it for
structured diagnostics. Tool-error text stays readable in failure cards. Schema diagnostics contain bounded instance
and schema paths, never rejected values. Schema corrections can be submitted again within the current turn.

`recoverable` refers to correcting a submission in the current turn, not replaying
the delegated task. Missing-result completion remains fail-closed: the reusable
agent's prompt API does not enforce a formatting-only tool allowlist, so an
automatic follow-up prompt could repeat side effects. Callers should inspect the
child evidence before assigning recovery work. Completion instructions refer to
the actual callable tool catalog rather than assuming a Code Mode binding.

If cancellation or closure wins settlement after result acceptance, execution
keeps its interrupted/closing status and `last_output` retains the accepted result
as evidence. The active revision and submission slot are cleared; that result cannot
satisfy the next turn's contract.
