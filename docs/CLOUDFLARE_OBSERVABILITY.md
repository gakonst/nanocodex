# Cloudflare Workers observability

Nanocodex uses Cloudflare's native Workers Logs and automatic traces for the
production Worker topology. Safe internal Workers and static surfaces retain
invocation logs and traces at a 100% head sampling rate; there is no application
telemetry backend or external export destination.

The public root Worker and Connect API are deliberate exceptions: they receive
OAuth callback query parameters and one-use WebSocket tickets. Cloudflare's
platform-generated invocation logs and automatic trace attributes include the
request URL, so those two Workers disable persistent observability. Use bounded
real-time tailing only during a controlled reproduction, then investigate the
sanitized correlated events emitted by the managed-agent and egress Workers.

## Operator access

Open the account-level [Workers Observability dashboard](https://dash.cloudflare.com/?to=/:account/workers-and-pages/observability/).
**Overview** spans all Workers in the account. Use **Investigate** for the query
language and **Invocations** to group the events and trace for one invocation.
Cloudflare controls the retained-data window for the account plan.

For agent-assisted, read-only investigation, connect Cloudflare's official
Workers Observability MCP server and complete its Cloudflare authorization:

```text
https://observability.mcp.cloudflare.com/mcp
```

Do not paste API tokens, session credentials, or production data into the MCP
configuration or prompts.

## Stable structured fields

Application operational events should use these top-level fields when the
corresponding identity exists:

| Field | Meaning |
| --- | --- |
| `user_id` | Opaque Nanocodex account ID |
| `organization_id` | Opaque organization ID |
| `team_id` | Opaque team ID; do not derive it from `organization_id` |
| `agent_id` | Opaque hosted-agent ID |
| `thread_id` | Opaque conversation/thread ID |
| `turn_id` | Opaque turn ID |
| `grant_id` | Opaque Connect grant ID, never the grant credential |
| `connector` | Bounded connector name, not provider configuration |
| `deployment_sha` | Exact 40-character deployed Git revision |
| `type` | Stable dotted event name, such as `connect.grant.create` |
| `outcome` | Bounded result such as `success`, `failure`, or `cancelled` |
| `status` | Bounded string state or safe error code, never raw error text |

Keep each field's type and meaning stable. Omit unavailable fields instead of
using guessed, empty, or repurposed values.

## Safe logging boundary

Logs and trace attributes contain operational metadata only. Never record
prompts, replies, tool arguments or results, memory, imported content, request
or response bodies, raw provider errors, or URLs containing query strings.
Never record Authorization or Cookie values, passkey material, app/grant bearer
credentials, provider credentials, connector tokens, private keys, or secret
configuration. Emit opaque IDs, bounded enums/codes, counts, sizes, and timings;
map failures to a safe code before logging.

Do not enable persistent logs or automatic traces on a Worker that terminates a
credential-bearing callback unless the callback is first moved behind a native
boundary that cannot retain its original URL.

## Native queries

Paste each query into the **Investigate** search bar and replace the quoted
placeholder with the exact opaque ID or revision.

User:

```text
user_id = "usr_01JEXAMPLE"
```

Team:

```text
team_id = "team_01JEXAMPLE"
```

Agent:

```text
agent_id = "agent_01JEXAMPLE"
```

Application failures plus uncaught Worker exceptions:

```text
outcome = "failure" OR status = "internal_error" OR $metadata.error EXISTS OR $workers.outcome = "exception"
```

Deployment:

```text
deployment_sha = "0123456789abcdef0123456789abcdef01234567"
```

Add `AND deployment_sha = "..."` or another identity field to narrow any query.
Cloudflare source maps make exception stacks readable in the dashboard without
publishing source maps to application clients.

## Embedded shell diagnostics

See [Just Bash execution logs](just-bash-observability.md) for `/brain` exit codes,
failure categories, call correlation, and queries for command retry sequences.


## Brain–Hand timing boundaries

Correlate `hand.call.broker` observations by `transport_call_id` and connection
identity. A session-attached Hand uses the session broker directly; an account
Hand uses the account broker.

On WebSocket `host_progress` and `receipt` observations:

| Field | Local boundary |
| --- | --- |
| `dispatch_to_message_ms` | Broker dispatch to entry into its message handler; omitted after owner recovery when the original monotonic clock is unavailable |
| `frame_decode_ms` | Parsing and validation of the incoming frame |
| `lease_validation_ms` | Awaited leased-attachment authority check, including any continuation scheduling |
| `message_to_handler_ms` | Message-handler entry through decode and lease validation, before progress/result processing |
| `roundtrip_ms` | Existing dispatch-to-result-processing duration, including the preceding receive work |
| `settlement_ms` | Synchronous result processing through the existing receipt observation boundary |

Handler entry is not a network arrival timestamp. These measurements exclude
Worker scheduling before entry and output-gate delays after a send. Native
`host_timing` ends around result encoding; subtracting it from the broker
roundtrip leaves combined transport, flush, scheduling, and storage overhead,
not a pure network RTT. Persisted diagnostic fields survive owner restart;
no tool inputs, results, or credentials are included.

Native attachment tracing additionally emits `attachment.transport_rtt` for a
matched heartbeat control ping/pong on the established socket. This measures the
WebSocket peer, which can be Cloudflare's transport endpoint rather than the
Durable Object application. `attachment.result_send_started`,
`attachment.result_flush_started`, and `attachment.result_sent` expose terminal
feed, flush, and total send durations. A local flush does not prove peer receipt
or ACK. Replays emit `attachment.result_replayed` without changing the immutable
journaled receipt. Pending terminal diagnostic frames share the result flush;
live execution progress still flushes promptly.

Run `pnpm --filter nanocodex-managed-service run test:hand-communication` for the
local workerd/account-broker journey and `test:hand-preparation` for managed
Code Mode route preparation. Their ignored `output/` artifacts contain source
hashes, timings, public diagnostics, and ownership/recovery evidence. Local
journey durations do not establish a production WAN latency improvement.

## Fresh session startup

Correlate the public live-create request, `DurableAgentSession`, and private
egress spans by their native trace ID. Keep client connection readiness,
`managed.turn.accepted`, provider socket readiness, and first answer output as
separate boundaries. A CLI that does not emit connection readiness has no
measurement for that boundary; provider socket readiness is not a substitute.

`session.create.commit` observes the storage synchronization promise without
adding an awaited application barrier. Its duration includes the Durable Object
output gate for the first write batch. Removing the observer does not remove
that gate. Synchronous constructor timings can be zero because the Worker clock
does not advance during synchronous execution; they do not establish zero CPU.
Use native invocation CPU measurements with their stated scope.

`managed.credential.prewarm` records the authenticated edge preparation's region,
fixed outcome, and elapsed duration. This preparation runs concurrently with
session creation. `egress.credential.snapshot` distinguishes a regional hit
(`snapshot`) from a canonical fill (`filled`) and reports the canonical duration
when applicable. A cold fill still reaches the canonical credential broker;
a warm regional hit does not. Neither event logs credential material.

Compare durations measured on one clock. Do not subtract timestamps on different
Workers to label the residual as an exact cold-start, routing, or scheduling
cost. Discovery, credential preparation, storage commit, and provider setup may
overlap: their durations are not necessarily additive. Confirm overlap with
causal span relationships and controlled runtime journeys, then measure actual
fresh-client startup on the deployed revision.
