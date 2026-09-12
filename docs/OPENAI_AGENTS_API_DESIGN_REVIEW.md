# OpenAI Agents API and Nanocodex TypeScript contracts

Initially reviewed 11 September 2026 at `178cbb7f`; expanded 12 September against
PR #307's SDK at `cc70d18e` and the official OpenAI session, input-event and
streaming-event references. API judgments below are separate from the subsequent
[600-trial deployed comparison](../output/cloudflare-agents-2026-09-11/README.md).
Stronger API contracts alone do not establish greater implementation reliability.

## Assessment

OpenAI has a cleaner declarative resource model and more complete hosted setup.
Nanocodex has a useful higher-level turn handle and explicit durable observation
contracts. Keep those strengths while improving the new configuration surface.
The concepts mostly correspond; the types and lifecycle guarantees do not map
one-to-one. An OpenAI backend would need a capability-aware adapter, not a cast
to our existing `Agent` or a replacement Responses transport.

OpenAI runs the harness itself. A Responses transport supplies model inference to
our harness. Substituting one for the other changes orchestration ownership.
[OpenAI overview](https://developers.openai.com/api/docs/guides/agents-api/overview)

## JavaScript request shapes used in the live comparison

The OpenAI path uses a streamed HTTP request; the Nanocodex path uses the managed
SDK. These are the corresponding configurations, with delegation explicitly off:

```js
const instructions = "Use only the supplied data and return JSON.";
const prompt = "Compute 17 * 19.";

// POST https://api.openai.com/v1/agents/sessions
// Authorization: Bearer OPENAI_API_KEY
// OpenAI-Beta: agents=v1
const openAIRequest = {
  agent: {
    model: "gpt-5.6-luna",
    instructions,
    reasoning: { effort: "high" },
    service_tier: "default",
    tools: [],
  },
  environment: { type: "none" },
  input: prompt,
  stream: true,
}; // OpenAI omission disables delegation.

// await Agent.create(nanocodexOptions), then agent.turn.prompt({input: prompt})
const nanocodexOptions = {
  baseUrl: process.env.NANOCODEX_MANAGED_URL,
  apiKey: process.env.NANOCODEX_API_KEY,
  settings: { model: "gpt-5.6-luna", thinking: "high", reasoningMode: "standard", fastMode: false },
  configuration: {
    instructions,
    tools: [],
    multi_agent: { enabled: false },
    environment: { network: { access: "disabled" } },
  },
};
```

OpenAI's request can create the session and submit input together. Nanocodex
currently creates the handle, then returns an explicit turn handle on submission.
The [runnable benchmark](../js/nanocodex/scripts/cloudflare-agents.bench.mjs)
contains the real `fetch`, SSE consumption, root-turn correlation, usage reads,
timing and session deletion for both APIs. A complete managed create/run/delete
example is in [the configuration guide](MANAGED_AGENT_CONFIGURATION.md).

## Resource and type mapping

| OpenAI concept | Nanocodex equivalent | Mapping |
| --- | --- | --- |
| Saved agent | `Agent.definitions` | Similar reusable configuration. Their saved agents can be updated; ours are immutable while retained. |
| Session | Managed `Agent` handle | Similar durable conversation. OpenAI's "agent" is not our live managed handle. |
| Agent override | `CreateOptions.configuration` | Both replace supplied object/array fields. Our top-level `settings` introduces a second precedence layer. |
| Environment | `/brain` setup plus separately provisioned hands | Partial. A declarative embedded-shell template cannot represent their hosted Linux setup or executor connection. |
| Turn | `agent.turn.prompt(...)` / `Turn` | Related work unit, different submission semantics. Requires careful correlation and recovery. |
| Saved items / live events | Conversation history / durable event API | Different recovery contracts; cannot promise replay from their live stream. |
| Function required action | Hosted Tools call / `requiredActions` | Similar function payloads. Our HTTP facade still depends on a pinned active attachment. |
| Turn artifact | `agent.artifacts` | Similar immutable output concept, different path, IDs, limits and storage. |

Their explicit separation of agent configuration, session state and environment
is worth adopting in our terminology. We need not rename existing runtime
handles to achieve it. Keep `Agent.create` compatibility and make the saved
definition versus running-session distinction clear in types and docs.
[OpenAI configuration](https://developers.openai.com/api/docs/guides/agents-api/configuration)

## Where a field adapter works, and where it loses information

The session-create reference exposes `reasoning.effort`, `text.format`, typed tool
variants and explicit environment variants. A supported reasoning effort maps to
our `thinking`, and a JSON output schema maps to `output_schema`. However, their
reasoning summaries, text verbosity, metadata and full service-tier selection
have no equivalent in this managed configuration. A `fastMode` boolean cannot
represent every service tier. Their model string also has a wider type domain
than our four-model union. Unsupported values must be rejected, not silently
discarded. Text/image input can be translated; our audio prompt variant has no
counterpart in the inspected Agents input schema.
[Session-create reference](https://developers.openai.com/api/reference/resources/beta/subresources/agents/subresources/sessions/methods/create)

Our own instruction semantics also need attention: core `AgentOptions.instructions`
replaces the model prompt, while `additionalInstructions` appends. The new managed
`configuration.instructions` appends. Sharing the field name does not make these
interchangeable. See [core types](../js/nanocodex/types.d.mts) and
[managed construction](../js/managed/src/index.ts).

## Preserve our turn and recovery contracts

OpenAI sends messages through the session event endpoint. Input to an idle session
starts work; input during work steers it. Its documented stream recovery rebuilds
from saved items because missed intermediate events are not replayed. Closing a
stream or observing idle does not prove a successful turn.
[Sessions](https://developers.openai.com/api/docs/guides/agents-api/sessions),
[events and recovery](https://developers.openai.com/api/docs/guides/agents-api/sessions/events)

Our explicit `Turn` with `accepted()`, `result()`, `steer()` and `cancel()` is a
good application interface. Managed prompt IDs/idempotency and resumable cursors
are meaningful contracts. An adapter must not relabel an arbitrary next terminal
event as the caller's result, treat a state-dependent send as guaranteed new-turn
admission, or invent replayed events. It must expose weaker guarantees where the
provider cannot supply ours. See [managed types](../js/nanocodex/managed/Agent.d.mts)
and [result recovery](../js/nanocodex/managed/Agent.mjs).

## Protocol versus SDK: what is actually better?

Keep our explicit turn controls. They express application intent more precisely:
`prompt` admits work, `steer` addresses existing work, and `cancel` targets a known
turn. OpenAI's session event protocol is a reasonable transport design, and a
higher-level SDK could wrap it. Method names versus typed events are not the
fundamental distinction; targeting, admission, retries and recovery are.

For example, a user corrects turn A just as A completes. Their
`agent.session.input.message` can start another turn because the session is now
idle. Our `turnA.steer(...)` fails if A is no longer steerable. Likewise, their
cancel input targets the currently active turn; our handle targets a specific ID.
Neither choice is universally right, but the explicit target is preferable for
applications that must not redirect a delayed correction or cancellation to new
work. Reading session state before sending a message does not eliminate that race.
[OpenAI session semantics](https://developers.openai.com/api/docs/guides/agents-api/sessions)

| Dimension | Assessment | Action |
| --- | --- | --- |
| Explicit start, steering and cancellation | Prefer our turn-targeted controls. | Preserve them and their admission/conflict behavior. |
| Awaiting a result | Our `Turn.result()` packages correlation and recovery for callers. | Keep it; do not map any next terminal event to success. |
| Replaying intermediate events | Our durable cursors provide the stronger observation contract. | Keep bounded replay/backpressure behavior explicit. |
| Creating and starting work | Their initial `input` avoids a separate client mutation. | Implement the combined operation only after the recovery design is complete. |
| Browsing old turns | Their dedicated list/retrieve API is more complete. | Add public turn listing and opening an existing turn by ID. |
| Hosted setup and HTTP functions | Their integration surface is more complete. | Complete durable function ownership/action webhooks; grow environment support with a real backend. |

These are design judgments. They are not evidence that our service has fewer bugs
or is globally faster. The benchmark applies to its measured workloads and regions.

## Side-by-side JavaScript lifecycle

In the OpenAI column, `S = client.beta.agents.sessions`; `sid`/`tid` are session
and turn IDs. `sendMessage` below is an application helper, not a native SDK method.
Nanocodex calls refer to the current managed SDK, not proposed follow-ups.

| Operation | OpenAI Agents | Nanocodex managed |
| --- | --- | --- |
| Create a session | `await S.create(options)` | `await Agent.create(options)` |
| Create and start | `await S.create({ ...options, input })` | Create, then `agent.turn.prompt({ input })`; two client mutations. |
| Start another turn | `sendMessage(sid, input)` while idle. | `const turn = agent.turn.prompt({ input })` |
| Confirm admission | Correlate session events and turn records. | `await turn.accepted()` returns the admitted ID. |
| Inspect a turn | `await S.turns.retrieve(tid, { session_id: sid })` | `await turn.state()` |
| List turns | `await S.turns.list(sid)` | No dedicated public SDK method yet. |
| Await final output | Observe the identified turn's outcome and retrieve its saved output items. | `await turn.result()` returns final text, usage and citations. |
| Steer | `sendMessage(sid, input)` while busy. | `await turn.steer({ input, messageId })` |
| Withdraw a correction | No equivalent in the inspected input schema. | `await turn.withdrawSteer({ messageId })` reports whether withdrawn. |
| Cancel | Send `agent.session.input.cancel` to the session. | `await turn.cancel()` targets that turn. |
| Live stream | `await S.events.stream(sid)` | `agent.events.watch({ cursor })` |
| Disconnect recovery | New stream plus saved session/items; no missed-event replay. | Resume after a durable cursor; result observation can recover retained terminal state. |
| Delete session | `await S.delete(sid)` | `await agent.delete()` |

OpenAI has first-class turn records, including state, timestamps, usage and errors.
The difference is not that they lack turns: it is how callers start/control them.
[Retrieve turn](https://developers.openai.com/api/reference/typescript/resources/beta/subresources/agents/subresources/sessions/subresources/turns/methods/retrieve),
[list turns](https://developers.openai.com/api/reference/typescript/resources/beta/subresources/agents/subresources/sessions/subresources/turns/methods/list),
[stream/recovery guide](https://developers.openai.com/api/docs/guides/agents-api/sessions/events)

```js
// OpenAI: same helper starts a new turn or steers, depending on server state.
const S = client.beta.agents.sessions;
async function sendMessage(sessionId, text) {
  await S.events.create(sessionId, {
    events: [{
      type: "agent.session.input.message",
      input: [{ role: "user", content: [{ type: "input_text", text }] }],
    }],
  });
}
await sendMessage(sid, "Use 23 instead of 19.");
await S.events.create(sid, {
  events: [{ type: "agent.session.input.cancel" }],
});
```

```js
// Nanocodex: obtain a handle for explicitly admitted work.
const turn = agent.turn.prompt({
  input: "Compute 17 * 19.",
  id: "job-42:first",
  idempotencyKey: "prompt:job-42:first",
});
const turnId = await turn.accepted();
await turn.steer({ input: "Use 23 instead of 19.", messageId: "correction-1" });
const { withdrawn } = await turn.withdrawSteer({ messageId: "correction-1" });
await turn.cancel();
```

These control examples illustrate separate operations, not a recipe requiring
withdrawal and cancellation for each prompt. Withdrawal does not undo effects
already produced. A cancellation request does not itself establish terminal
cancellation. Aborting a result observer also does not cancel durable server work.

## Exact input-event inventory

The inspected `POST /v1/agents/sessions/{session_id}/events` schema accepts an
`events` array with exactly three variants:

| Type | Payload | Meaning |
| --- | --- | --- |
| `agent.session.input.message` | `input`: user messages with text/image content. | Start when idle, steer when working. |
| `agent.session.input.cancel` | No additional fields. | Cancel the current active turn; no turn ID selector. |
| `agent.session.input.tool_result` | Required `turn_id`, `call_id`, `success`; optional `output` and `error`. | Supply a requested function result. |

There is no distinct `start_turn`, `steer`, or `withdraw_steer` input variant in
this schema. The endpoint accepts an optional 1–256-character `Idempotency-Key`
header. This establishes that they expose request idempotency, not that its
retention, conflict or crash guarantees equal ours.
[Input-event reference](https://developers.openai.com/api/reference/resources/beta/subresources/agents/subresources/sessions/subresources/events/methods/create)

```js
await S.events.create(sid, {
  events: [{
    type: "agent.session.input.tool_result",
    turn_id: "turn_123",
    call_id: "call_456",
    success: true,
    output: '{"value":437}',
  }],
});
```

Our `requiredActions.submit(callId, outcome)` is conceptually related but uses the
strict Hosted Tools outcome and active attachment ownership described below. It
is not an interchangeable tool-result payload.

## Output-event inventory and correlation

For each row, concatenate the prefix with each suffix to obtain the exact event
names. These are the stream variants in the inspected reference, not webhook types.

| Prefix | Suffixes |
| --- | --- |
| `agent.session.` | `created`, `idle`, `in_progress`, `requires_action`, `failed` |
| `agent.session.turn.` | `created`, `in_progress`, `completed`, `failed`, `cancelled` |
| `agent.session.turn.item.` | `added`, `done` |
| `agent.session.turn.content_part.` | `added`, `done` |
| `agent.session.turn.output_text.` | `delta`, `done` |
| `agent.session.turn.reasoning_summary_part.` | `added`, `done` |
| `agent.session.turn.reasoning_summary_text.` | `delta`, `done` |
| `agent.session.environment.` | `pending`, `ready`, `connected`, `disconnected`, `failed` |
| `agent.session.subagent.` | `created`, `active`, `closed` |

Two standalone variants complete the inventory:
`agent.output.command_execution_output.delta` and `error`.
[Streaming-event reference](https://developers.openai.com/api/reference/resources/beta/subresources/agents/streaming-events)

Session identity scopes the stream, but individual events carry or embed turn,
item and subagent identities as appropriate. Text updates use `item_id`,
`output_index` and `content_index`; the event's identity is not a durable replay
cursor. Distinguish root and child turns and correlate the requested turn. An idle
session, stream closure, or a child's completion does not prove the root succeeded.
After disconnect, subscribe/buffer first, retrieve saved state/items, then reconcile
buffered updates. Saved output does not reconstruct every missed intermediate event.
[Event handling and recovery](https://developers.openai.com/api/docs/guides/agents-api/sessions/events)

## Resource APIs outside the event channel

Their execution controls are typed session inputs; resource management remains
conventional HTTP CRUD/list operations:

| Resource | Operations |
| --- | --- |
| Saved agents | Create, retrieve, update, delete, list. |
| Sessions | Create with optional initial input, retrieve, update, delete, list. |
| Root turns | List and retrieve status/timestamps/usage/error. |
| Session items | List saved messages and tool calls; filter by turn ID. |
| Subagents | List/retrieve, plus their item histories and turn list/retrieval. |
| Artifacts | List, retrieve, download content and delete. |
| Environments | Retrieve state; create/list files; manage reusable templates. |
| Vaults and credentials | Manage stored integration credentials. |

See the [Agents resource reference](https://developers.openai.com/api/reference/resources/beta/subresources/agents/subresources/sessions/subresources/events/methods/create).
This distinction matters for adapters: not every action is an event, and the
hosted runtime owns execution rather than merely supplying model inference.

## Learn from their HTTP integration

OpenAI lets callers declare function schemas in configuration, discover pending
calls from session state and submit success/output or failure/error. Its
action-required webhook tells a disconnected backend when to retrieve that state.
[Functions](https://developers.openai.com/api/docs/guides/agents-api/tools/functions),
[session webhooks](https://developers.openai.com/api/docs/guides/agents-api/sessions/webhooks)

Our draft's `tools: string[]` only selects existing built-ins; it is not equivalent
to declaring functions or MCP servers. `requiredActions.submit` exposes the full
Hosted Tools wire outcome, including nullable process metadata. It also requires
the active attachment, and our webhook set has no required-action notification.
Consequently this is not yet a complete stateless HTTP tool integration.

Add an ergonomic SDK result helper that normalizes optional metadata into the
existing strict wire protocol. A genuinely HTTP-only tool lifecycle additionally
needs durable registration/ownership, action notification, deadlines and duplicate
result handling; removing the attachment check alone would not implement it.
See [broker outcome](../js/nanocodex-tools/src/hosted/protocol.ts).

## Environment and delegation lessons

Their hosted environment supports package lists, ordered commands with working
directories, binary/file-ID inputs and reusable templates. Template network
overrides cannot widen the inherited policy. Our embedded-shell template is a
useful subset, but native package support requires an execution backend, not more
fields that the current shell cannot honor. If we add template overrides, define
the same narrowing rule explicitly rather than treating policy as ordinary JSON.
[Hosted environments](https://developers.openai.com/api/docs/guides/agents-api/environments/openai-hosted)

They expose delegation enablement and a concurrency limit on session creation.
The managed `Configuration.multi_agent` now exposes the same explicit enable/disable
and concurrency shape through our existing runtime. Explicit enablement defaults
to six children. One compatibility difference remains: omitting the field preserves
Nanocodex's legacy enabled default, while omission disables delegation in OpenAI.
Set the field explicitly when comparing or mapping providers.
Conversely, their documented subagents cannot use application function tools;
do not assume all tool capabilities translate across providers.
[OpenAI delegation](https://developers.openai.com/api/docs/guides/agents-api/multi-agent),
[Nanocodex subagents](../js/nanocodex/runtime/subagents.d.mts)

## Keep precise accounting; improve the public types

OpenAI usage is best-effort and can arrive late. It omits cache-write counts, and
trace retrieval/export is not a supported beta API. Mapping a missing cache-write
count to zero would make our `TurnUsage` misleading. Preserve nullable/partial
provider usage separately and only derive costs supported by the data.
[OpenAI observability](https://developers.openai.com/api/docs/guides/agents-api/observability)

Our accounting includes cache writes and explicit cost status, but the inspector's
request payload is `Record<string, unknown>` and nested events are `unknown`.
Add typed projections for common model/tool/child events while preserving a raw
escape hatch. Do not claim that a JSON schema automatically infers a typed final
result: the current API returns `finalMessage: string`.

## Recommended follow-up, in order

1. Preserve explicit turn controls and implement combined create-and-prompt using
   the durable workflow in the second design exercise. Add turn listing and
   opening an existing turn by ID; those methods are proposals, not current APIs.
2. Before stabilizing the new SDK contract, reuse one settings type, normalize
   camelCase at the JS boundary, and make additive instructions unambiguous.
   Currently creation uses `reasoningMode`/`fastMode`, while saved configuration
   uses `reasoning_mode`/`fast_mode` and separately duplicates validation.
3. Add a concise tool-result helper and typed operational event/request views.
   Preserve the broker's existing validation and conflict rules.
4. Complete required-action webhooks and design durable HTTP-only function
   ownership. Managed delegation controls now reuse the existing subagent runtime.
5. Add explicit environment variants and policy-preserving template overrides
   when a native setup backend is implemented.
6. If supporting OpenAI as a hosted backend, define a common capability subset
   with backend-specific extensions. Test input correlation, disconnect recovery,
   tool-result retries and partial usage before advertising interchangeability.

Except for the subsequently implemented managed delegation controls noted above,
these remain design recommendations rather than additional APIs implemented in
PR #307. The Cloudflare measurement report tracks the live runtime changes.

## Second design exercise: first-turn admission

[Creating a session and admitting its first turn](MANAGED_AGENT_START_DESIGN.md)
compares four API shapes, specifies crash/retry behavior across Cloudflare owners,
and defines a performance experiment. Its combined operation remains a proposal.
This pass implements the smaller prerequisite: optional caller-owned creation
idempotency keys in the existing SDK, with validation and conflict propagation.
