# Tact-compatible subagents

Nanocodex’s native CLI ports the subagent runtime from
[`clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c`](https://github.com/clabby/tact/tree/1d9ccaefd1d8613dab020812af04a91cd9b4c52c)
under Apache-2.0. The runtime source, schemas, prompts, lifecycle, messaging,
capacity policy, and focused tests are retained 1:1. The integration changes
are limited to Nanocodex module paths, removal of Tact’s separate memory-tool
coupling, CLI configuration, event draining, shutdown wiring, and explicit
propagation of Nanocodex’s originating tool span into the child harness.

Subagents are enabled by default for TUI and one-shot runs:

```sh
nanocodex --max-subagents 32
nanocodex run --max-subagents 32 "implement the change"
```

Pass `--subagents false` or set `NANOCODEX_SUBAGENTS=false` to disable the
general-purpose subagent tools for a session.

When enabled, the root agent also receives Tact's fixed orchestration guidance:
delegate only meaningful separable work, run independent children concurrently,
use their typed outputs for dependent stages, avoid repeating delegated work,
verify their findings, and keep concurrent write scopes disjoint.

`--max-subagents` bounds active child turns across the complete task tree. Idle
reusable sessions consume no capacity. Lowering the limit does not cancel work;
new reservations fail until active work falls below the limit.

## Tool contract

An enabled runtime installs seven tools for root and child agents:

| Tool | Contract |
| --- | --- |
| `spawn_agent` | Create a clean child session with a role, focused task, required output schema, and optional model/thinking overrides. |
| `submit_result` | Submit the active child turn’s final JSON value against its schema and turn token. |
| `send_agent_message` | Send a bounded directed message within the current task tree. |
| `list_agents` | List visible agents, status, topology, and caller authority. |
| `wait_agent` | Wait until any selected agent reaches a terminal state. |
| `interrupt_agent` | Stop an active subtree while keeping its sessions reusable. |
| `close_agent` | Close an agent and its descendants permanently. |

Each child starts without inherited conversation history. Its initial prompt
contains its role, task, tree identity, coordination rules, output schema, and
a monotonically changing turn token. A successful model turn must call
`submit_result` exactly once with a schema-valid value and the current token.
Steering rotates the token, preventing a superseded turn from submitting the
new turn’s result.

Model and thinking overrides apply only to the new child. When either is
omitted, the child inherits the invoking agent’s current value at the spawn
boundary.

## Tree authority and messaging

Every root session owns an isolated task tree with IDs local to that tree. The
root can manage all descendants; a child can manage only its descendants, not
siblings or ancestors. Ordinary coordination may cross sibling branches.
Authorization is enforced by the runtime.

Messages are limited to 2 KiB of UTF-8 and retain typed priority, purpose, and
reply metadata. Deferred delivery starts an idle recipient or queues behind an
active turn. Urgent delivery steers a running turn at the next safe model
boundary. Delegate messages replace the recipient’s task while preserving its
output schema and require management authority. Deferred and urgent mailboxes
are independently bounded.

## Lifecycle

One active child turn reserves one shared capacity slot. Completion, failure,
interruption, or closure releases it. `wait_agent` defaults to 30 seconds and
caps waits at 300 seconds without cancelling on timeout. Interrupt and close
operate in descendant-first order with a 30-second cleanup deadline.

The CLI closes all remaining descendants before shutting down the root runtime.
Child harness tasks, model/tool work, and event-forwarding tasks are joined or
bounded during cleanup. Subagents share the root’s provider, workspace, base
tools, and process authority; clean conversation context is not a security
sandbox.

When the root agent uses `nanocodex-durability`, every clean child and
grandchild also persists its execution state under its own agent session ID.
That does not make the in-memory subagent registry durable: tree-local
`AgentId`s, topology, mailboxes, roles, status, and their mapping to session IDs
still require a separate orchestrator-owned store for cold tree recovery.

Tact’s subagent tree TUI is presentation owned by Tact and is not copied into
Nanocodex’s existing Ratatui application. Nanocodex drains the same typed
runtime updates so lifecycle observation remains independent of the scheduler.
