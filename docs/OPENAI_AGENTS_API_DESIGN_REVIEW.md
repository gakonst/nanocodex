# OpenAI Agents API and Nanocodex TypeScript contracts

Reviewed 11 September 2026 against PR #307 at `178cbb7f`, the official OpenAI
guides, and the session-create API reference. This is an API design assessment,
not a claim about comparative model quality or production reliability. Earlier
live trials were smoke tests; they did not run matched Nanocodex workloads.

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

1. Before stabilizing the new SDK contract, reuse one settings type, normalize
   camelCase at the JS boundary, and make additive instructions unambiguous.
   Currently creation uses `reasoningMode`/`fastMode`, while saved configuration
   uses `reasoning_mode`/`fast_mode` and separately duplicates validation.
2. Add a concise tool-result helper and typed operational event/request views.
   Preserve the broker's existing validation and conflict rules.
3. Complete required-action webhooks and design durable HTTP-only function
   ownership. Managed delegation controls now reuse the existing subagent runtime.
4. Add explicit environment variants and policy-preserving template overrides
   when a native setup backend is implemented.
5. If supporting OpenAI as a hosted backend, define a common capability subset
   with backend-specific extensions. Test input correlation, disconnect recovery,
   tool-result retries and partial usage before advertising interchangeability.

Except for the subsequently implemented managed delegation controls noted above,
these remain design recommendations rather than additional APIs implemented in
PR #307. The Cloudflare measurement report tracks the live runtime changes.
