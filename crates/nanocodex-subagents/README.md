# nanocodex-subagents

`nanocodex-subagents` is an optional extension above `nanocodex-agent`. It adds
a shared task tree and six tools in the `collaboration` namespace without making
the core agent depend on orchestration policy:

- `spawn_agent` (`task_name`, `message`, optional `fork_turns`, `model`, `reasoning_effort`)
- `send_message` (`target`, `message`), which does not wake an idle agent
- `followup_task` (`target`, `message`), which can start an idle child
- `list_agents` (optional `path_prefix`)
- `wait_agent` (optional `timeout_ms`), which waits for mailbox updates
- `interrupt_agent` (`target`), which interrupts only that agent

`submit_result` is additionally retained for structured children created through the SDK.

Model-directed children inherit all safe history by default and return their
final assistant text to their parent. `fork_turns` accepts `none`, `all`, or a
positive integer string. Full-history forks inherit the parent's model and
reasoning effort; other forks may supply explicit overrides. Targets accept
canonical task paths, direct-child names, or agent IDs.

`start_agent`, `start_agents`, and the numeric registry API preserve their
structured-result contracts. Embeddings requiring the old model tool schemas
can explicitly use `install_structured_tools`. Existing resident limits and cold-restored
child tombstones remain embedding-owned; retained topology alone does not reopen a
child driver for model-directed messages or follow-up tasks.

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
