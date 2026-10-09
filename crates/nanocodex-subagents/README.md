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

Each subagent of a durable session is itself durable, exactly like a fork: it
records its own catalog session (listed with lineage origin `subagent` and its
parent's session ID), its own Codex-format rollout and its own resumable state,
so it can be read and resumed by session ID. A durable parent that cannot record
children rejects the spawn instead of starting an unsaved child, and a batch
spawn starts every requested child or none. Children of an ephemeral parent are
ephemeral. Completed or interrupted children can receive more work while the
parent runtime remains alive. Idle child drivers may be evicted and rehydrated
under the same session ID to limit resident resources. For an ephemeral parent,
restarting the runtime drops the task tree, child history, messages, and
results, and a fresh registry starts empty. Historical agent IDs do not identify recovered children;
use `list_agents` to discover the current live registry before addressing agents.

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

The default `list_agents` directory includes pending, running, and recoverable
interrupted children, including journal-restored children awaiting resume. Their
status remains `interrupted` until a turn actually starts. `include_completed: true`
adds all other retained states. Visibility does not change `can_message`,
`can_manage`, self exclusion, or task-tree authorization.
