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

A durable task tree retains child identities, ownership, mailboxes, accepted
results, and per-child execution checkpoints. Reopening the same tree recovers
its existing children and reconciles admitted work through the child journals.
A completed spawn receipt therefore continues to identify the original child.
Current host capabilities and authorization are applied again on reconstruction.

The JavaScript hosts configure child durability when the root has a durability
store. On native targets, calling `.durability(state)` through
`nanocodex::DurableAgentExt` automatically installs the registry, tools, per-child durable factory, and foreground ownership
barrier. It supports same-family OpenAI children and, with the facade's `claude`
feature, same-family Claude children. Caller tools and tool-factory recipes remain
installed. Custom spawn factories are rejected before child-tree mutation;
mixed-family routing requires explicitly configured recipes.

Lower-level embedders using `nanocodex_durability::DurableAgentExt` must use
`Registry::enable_durability` with the existing `StateStore`, install a per-child
durable harness factory, and recover before admitting work. Attaching
`RegistryOwnership` to `turn_ownership` provides the native startup and settlement
hooks. An ordinary `channel()` without this configuration stays in memory. Idle
drivers can be unloaded in either mode; durable execution history belongs to
each child's session rather than the resident driver.

Native facade builds start child recovery immediately after the owner is bound.
`agent.ready().await` reports completion or the retained recovery error; prompts
also await readiness. Pending background children can resume even when their
root turn already completed, without a synthetic root prompt. Shutdown and
last-handle drop cancel unfinished startup recovery.

Children select a `foreground` or `background` lifetime. Background work requires
a durable tree. The managed host retains a scheduler wakeup so admitted background
work can resume after the foreground turn ends or its runtime disappears.
A successful root turn waits for foreground children; failure or cancellation
stops them before settlement. Explicit interrupt and close requests retain their
own recovery state. A process exit is not an implicit cancellation of durable work.

`send_agent_message` keeps message intent (`purpose`) separate from thread
correlation (`in_reply_to`). Referencing a message continues its existing two-party
thread: coordination, findings, questions, and authorized delegation may flow in
either direction, including follow-ups to the sender's own messages. An explicit
`purpose: "reply"` requires `in_reply_to` and must reverse the referenced message's
direction. Unknown references and references from a different participant pair
are rejected. Thread correlation never grants delegation authority or changes the
message's purpose.

`spawn_agent` accepts `model` (`astra`, `sol`, `luna`, `glm-5.3`, `kimi`,
`mimo`) and `thinking` (`none` through `max`) overrides. Set either to `null`
to inherit the invoking agent's current settings; an override configures
only the new child.

Create one channel for an application-owned agent family, then install fresh
tools for every driver with `NanocodexBuilder::tools_factory`:

```rust,ignore
use std::sync::Arc;
use nanocodex_agent::Nanocodex;
use nanocodex_subagents::{channel, install_tools, DEFAULT_MAX_SUBAGENTS};
use nanocodex_oai_tools::Tools;

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

The model-facing `spawn_agent` declaration uses strict function arguments. It
accepts `output_contract`, a closed recursive shape with `kind: "object"` and
`fields: [{name, schema, required}]`, `kind: "array"` with `items`, or scalar
kinds `string`, `string_enum` (with `values`), `integer`, `number`, `boolean`,
`null`, and `any`. Set `model` and `thinking` to `null` to inherit the parent's
settings. The runtime compiles the contract into the same JSON Schema validator
used for `submit_result` before reserving a child. For example:

```json
{
  "role": "auditor", "task": "Review the subsystem",
  "model": null, "thinking": null,
  "output_contract": { "kind": "object", "fields": [
    { "name": "summary", "schema": { "kind": "string" }, "required": true },
    { "name": "issues", "schema": { "kind": "array", "items": { "kind": "string" } }, "required": true }
  ] }
}
```

Trusted Rust and JavaScript `spawn`/`spawnMany` APIs continue to accept raw JSON
Schema for advanced constraints. Old tool calls containing `output_schema` may
finish after upgrade, but new model-visible declarations advertise only
`output_contract`. Strict provider generation does not constrain calls from
Code Mode JavaScript; the runtime still parses and validates before child launch.
