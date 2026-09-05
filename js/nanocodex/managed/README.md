# Managed Nanocodex agents

`nanocodex/managed` is the administration and extended control-plane client for
account-owned Nanocodex agents. Application turns normally use
`Agent.create({ transport: Transport.managed(...), tools })` from
`nanocodex/node`, `nanocodex/browser`, or `nanocodex/host`, which returns the
same lifecycle shape as local OpenAI and host-managed transports.

In a browser, authentication uses the current origin's HttpOnly account cookie:

```js
import { Agent } from "nanocodex/managed";

const agent = await Agent.create();
const turn = agent.turn.prompt({
  input: "Inspect the repository and summarize it.",
  idempotencyKey: crypto.randomUUID(),
});
const result = await turn.result();
console.log(result.finalMessage);
```

New managed agents may be created with an atomic settings object. Existing
agents retain their model and reasoning mode after their first accepted turn.

```js
const astra = await Agent.create({
  settings: {
    model: "gpt-6-astra",
    thinking: "high",
    reasoningMode: "standard",
    fastMode: false,
  },
});
```

GPT-6 Astra accepts `low`, `medium`, `high`, `xhigh`, or `max` thinking; it
rejects `none`. Fast mode is deliberately opt-in and is unavailable for Astra
when the OpenAI project uses EU data residency.

On a server, provide the managed origin and an `ncx_live_...` account API key:

```js
const agent = await Agent.get(process.env.NANOCODEX_AGENT_ID, {
  baseUrl: "https://nanocodex.example",
  apiKey: process.env.NANOCODEX_API_KEY,
});
```

The transport-facing constructor requires an explicit identity and never
infers creation from a missing ID:

```js
import { Agent, Transport } from "nanocodex/node";

const created = await Agent.create({
  transport: Transport.managed({
    agent: { create: true },
    baseUrl: "https://nanocodex.example",
    apiKey: process.env.NANOCODEX_API_KEY,
  }),
});
const existing = await Agent.create({
  transport: Transport.managed({
    agent: { id: process.env.NANOCODEX_AGENT_ID },
    baseUrl: "https://nanocodex.example",
    apiKey: process.env.NANOCODEX_API_KEY,
  }),
});
await existing.session.shutdown(); // closes the client; never deletes the Agent
```

The same account client can search all completed managed conversations without
opening an agent turn:

```js
const found = await Agent.findSessions(
  { query: "what did we decide about memory?", limit: 8 },
  { baseUrl: process.env.NANOCODEX_MANAGED_URL, apiKey: process.env.NANOCODEX_API_KEY },
);
const session = await Agent.readSession(
  {
    session_id: found.results[0].session_id,
    turn_ids: [found.results[0].turn_id],
  },
  { baseUrl: process.env.NANOCODEX_MANAGED_URL, apiKey: process.env.NANOCODEX_API_KEY },
);
```

Both methods derive the account scope from the cookie or API key; no scope or user
identifier is accepted from the caller.

Hosted durable memory is account-owned and independent from session history. The
memory panel can list records and compare-and-swap delete one current key:

```js
const memories = await Agent.listMemories({
  baseUrl: process.env.NANOCODEX_MANAGED_URL,
  apiKey: process.env.NANOCODEX_API_KEY,
});
await Agent.deleteMemory(memories[0].key, {
  baseUrl: process.env.NANOCODEX_MANAGED_URL,
  apiKey: process.env.NANOCODEX_API_KEY,
});
```

Managed agents access this same hosted store through their `memory` tool. It is
never mirrored into a browser, TUI, or other local persistence layer.

Account memory uses the same derived organization scope and supports scan, read,
put, and delete operations. Scan before writing so the service can reject
duplicates, and retain returned `{ id, version }` keys for compare-and-swap
updates:

```js
const scanned = await Agent.memory(
  { operation: "scan", query: "production deploy schedule" },
  { baseUrl: process.env.NANOCODEX_MANAGED_URL, apiKey: process.env.NANOCODEX_API_KEY },
);
const stored = await Agent.memory(
  { operation: "put", content: "Production deploys happen on Tuesdays." },
  { baseUrl: process.env.NANOCODEX_MANAGED_URL, apiKey: process.env.NANOCODEX_API_KEY },
);
```

Browser account owners can read and rename their organization:

```js
const organization = await Agent.getOrganization();
await Agent.updateOrganization({ name: `${organization.name ?? "Personal"} workspace` });
```

Memory accepts either account-cookie or API-key authentication. Organization
metadata and mutation remain subject to the server's principal and owner policy;
the client sends supplied credentials without weakening that policy.

`Agent.list()` returns agent handles, `agent.state()` reads current state, and
`agent.delete()` removes the agent and its retained state. `agent.events.watch`
is an async iterator over durable events. Pass its last decimal `cursor` to a
later watcher to resume strictly after the acknowledged event. Network endings
reconnect automatically from that cursor. Watchers and independently awaitable
turn results on one agent handle share one replayable event connection; each
subscriber keeps its own cursor, so consuming one never steals another's events.
Pass `cursor: "latest"` to attach atomically at the durable head without
replaying retained history; a history page can then hydrate independently.

Returning browser clients may use `Agent.open(id)` to construct a retained
handle without a preliminary state request. The first operation on that handle
still verifies account ownership at the managed service boundary. Use
`Agent.get(id)` when an eager existence check is part of the caller's workflow.

The managed API never accepts model-provider credentials, egress bindings,
runtime environment objects, credential grants, or arbitrary request headers.

## Cron triggers

Schedules belong to an existing managed agent and run without an open client.
Use a stable trigger ID to create or replace a schedule idempotently:

```js
const agent = await Agent.get(agentId, { baseUrl, apiKey });
const morning = {
  cron: "0 7 * * MON-FRI",
  timezone: "Europe/Athens",
  input: "Review my running agents and summarize anything that needs attention.",
};
await agent.triggers.put("morning-review", morning);
await agent.triggers.list();
await agent.triggers.get("morning-review");
await agent.triggers.put("morning-review", { ...morning, enabled: false }); // pause
await agent.triggers.put("morning-review", morning); // resume
await agent.triggers.delete("morning-review");
```

| Method | Route | Result |
| --- | --- | --- |
| GET | `/v1/agents/:agent/triggers` | `{ data: [...] }` |
| GET | `/v1/agents/:agent/triggers/:id` | Trigger or 404 |
| PUT | `/v1/agents/:agent/triggers/:id` | Create (201) or replace (200) |
| DELETE | `/v1/agents/:agent/triggers/:id` | Idempotent deletion (204) |

The managed-agent web composer also has a **Schedules** button. It opens the
same account-authenticated API: create or edit a prompt and cron expression,
choose an IANA time zone, inspect the next dispatch time, pause/resume, or
delete a schedule. The last-dispatched time is not a completion receipt; read
the conversation for the result. The ephemeral homepage agent has no schedules.

PUT takes `{ cron, input, timezone?, enabled?, session_mode? }`. Expressions have five fields
(minute, hour, day of month, month, day of week), supporting numeric values,
lists, ranges, steps, and month/day names. The time zone defaults to UTC and
accepts IANA names. Local times follow daylight saving transitions. Seconds,
macros, and random `H` fields are rejected. Inputs are text up to 64 KiB;
there can be at most 32 triggers per agent. IDs use 1–64 letters, digits,
underscores, or hyphens. PUT is a full replacement; an identical retry preserves
the next occurrence. A changed configuration starts from the next matching time.

`session_mode: "new"` (the default for new configurations) creates a fresh managed
session for every occurrence. It copies the source agent's model settings at
dispatch time, but no conversation history or schedules. Account permissions,
connectors, and account memory still belong to the same account. Fresh sessions
run independently even when the source conversation is busy, and appear in the
normal agent list. Choose **New session each time** in the Schedules dialog.

`session_mode: "continue"` submits a turn to the existing conversation, retaining
its history. Choose **Continue this conversation** in the dialog. Schedules saved
before modes were introduced retain this behavior. Omitting the mode when
updating an existing schedule preserves its mode, including for older clients.
Set `session_mode` explicitly to switch modes.

Durable Object alarms dispatch occurrences. Continued turns advance the schedule
atomically with turn admission. Fresh runs use a durable outbox and deterministic
session/turn IDs: delivery retries reuse the same session without duplicating
its turn. Only one undelivered occurrence per schedule is retained. Inspect
`last_turn_id` on `last_agent_id` through the normal turn and event APIs, or use
**Open run** in the UI. Times are Unix milliseconds; `last_run_at` is the scheduled
time of the last accepted occurrence, and `next_run_at` is null while paused.
Last dispatched does not mean the model response has completed.

Missed ticks coalesce; they are not replayed individually. In continue mode,
unfinished work or a failed event stream causes an occurrence to be skipped and
recorded in `last_skipped_at`. New-session mode does not skip because the source
is busy. Pausing or deleting prevents future dispatches; a turn already accepted
or a fresh session already claimed for delivery still completes. Resume does
not backfill. Deleting the source removes its schedules and pending deliveries;
sessions already created remain independent conversations.

Read requests require `agents:read`; writes require `agents:write`, and PUT also
requires `tools:use`. The usual ownership and browser-origin checks apply.
Triggers are account-owned standing instructions with the creator's capability
scope, and persist after the creating API key or browser session ends. Connect
grants and multiplayer rooms cannot create schedules. Delete the trigger to
revoke its standing instruction. Provider credentials are resolved by the usual
managed execution path and are never stored in trigger definitions or returned
by these endpoints.

Deleting an agent removes its triggers. Portable durability export currently
returns `409 cron_triggers_present` while any trigger or pending delivery exists: delete
triggers and let claimed deliveries finish before transfer, then recreate schedules at the destination. This avoids
silently dropping schedules or running the same schedule on two agents.


## Cloud browser control (experimental)

The Cloud browser button in an account agent opens an owner-only page viewer.
It uses the same retained Cloudflare browser session as `browser_execute`.
The panel supports tab selection, navigation, refreshed screenshots, click/touch,
scroll, private input, and common keyboard keys. This is currently a screenshot
viewer, not a video stream.

`browser_handoff({ reason })` pauses the browser task until the owner selects
**Return to agent**. The control gate is persisted in the session Durable Object;
model browser operations cannot run while the owner controls the page. Returning
control rotates its generation, rejecting input from stale clients. A completed
handoff call is remembered to avoid prompting again if that call is replayed.
The model must inspect the resulting page: returning control does not establish
that sign-in succeeded.

```ts
const view = await agent.browser.state();
const control = await agent.browser.takeover();
const frame = await agent.browser.action("frame", {
  target: view.tabs[0].id,
  generation: control.generation,
});
await agent.browser.release(control.generation);
```

These routes require full account authority with `agents:read`, `agents:write`,
and `tools:use`; Connect grants cannot view or operate a signed-in browser.
Mutations apply the existing same-origin checks. Private input travels through
the owner API, never through an agent tool argument or conversation event.
The trusted executor checks the destination URL and focused field before filling;
CDP errors are replaced with generic errors and debug buffers are cleared.

Current limitations: no external password-manager integration or structured login
form yet; private input cannot fill cross-origin iframe fields; no profile sharing
between agents or restoration of logins after Chromium expires. Closing the panel
leaves human control active, allowing reconnection. Return control explicitly.
If the browser expires, return control so the agent can reopen the page and ask
for authentication again. Test real sign-in, Worker restart/replay, mobile input,
and reload/reconnect before enabling production use.
