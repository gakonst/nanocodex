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

## Correlating public API curl runs

Retain each synthetic run's request body, response headers, timestamped SSE
frames, and terminal receipt under ignored `output/`. Use the same request body
and idempotency key to reconcile a disconnected admission. Record these client
boundaries independently: request start, HTTP headers, `run` admission receipt,
first nonempty assistant text for the requested turn, and terminal event.
HTTP time-to-first-byte and reasoning/tool deltas are not assistant text TTFT.

The connected `cloudflare_request` tool can query retained logs with
`POST /client/v4/accounts/ACCOUNT_ID/workers/observability/telemetry/query`.
Use a narrow UTC interval covering the curl request; `from` and `to` are Unix
milliseconds. A minimal request is:

```json
{
  "queryId": "managed-curl-investigation",
  "timeframe": { "from": 1791538800000, "to": 1791539100000 },
  "view": "events",
  "limit": 200,
  "ignoreSeries": true,
  "parameters": {
    "datasets": ["cloudflare-workers"],
    "filterCombination": "and",
    "filters": [
      { "key": "thread_id", "operation": "eq", "type": "string", "value": "THREAD_ID" }
    ]
  }
}
```

Replace the example interval and thread ID. Discover fields from returned
`events.fields` or the telemetry keys endpoint before adding filters. Returned
application fields are in each event's `source`; native invocation metadata is
in `$workers` and `$metadata`. Save the query and results with the curl evidence.
If the page reaches the limit, continue with `offset` set to the final event's
`$metadata.id` and `offsetDirection: "next"`, preserving the filters and interval.
A missing record can reflect retention, ingestion delay, sampling, or an
incomplete query; it does not establish zero duration or successful execution.

Use the SSE receipt's `agent_id` as the thread ID and its `turn_id` for turn
correlation. Capture `x-nanocodex-request-id`, `cf-ray`, and `server-timing` from
response headers when present. `managed.proxy` records correlate by
`request_id`; performance scopes may instead use the turn ID as `trace_id`.
That application `trace_id` is distinct from Cloudflare's `$metadata.traceId`.
Follow a matching event's native trace ID to inspect related invocations and
record `$workers.scriptVersion.id` to identify the deployed Worker version.
Request IDs are boundary-specific: do not assume the HTTP request ID, runtime
request ID, and `egress_request_id` are interchangeable.

Keep provider connection, provider first output, first answer delta, client
first text, and durable completion separate. Parent and child model calls can
share a thread; group transport records by `socket_id` and
`socket_request_index` before comparing durations. Use only explicit terminal
observations for completion timing. Local synthetic-provider runs isolate
runtime overhead; production runs additionally include inference, geography,
and network variability. Compare matching models and settings and report the
sample count rather than presenting one run as a benchmark.

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

## Managed recovery inspection

The guarded administrator `admin_threads` diagnostics response includes a
`recovery` snapshot of retained turn safety counters and Code Mode effect
metadata. The request's `limit` bounds each collection (maximum 100), ordered
by newest insertion; `has_more` explicitly marks omitted older rows. This
snapshot does not include archived turns or parse the root runtime head.

Compare `abrupt_attempts` with the ordinary `attempt_count`: they measure
separate recovery paths. Missing safety rows remain null. Effect receipt
inspection returns chunk counts, never receipt contents, inputs,
source, hashes or credentials. Receipt scans stop at 257 chunks; `truncated`
means the reported count is a lower bound, not verified integrity. Receipt metadata uses
the chunk-key index and never reads or casts receipt bodies. A failed receipt
query is marked unavailable on that effect; other safety/effect metadata remains
readable. Snapshot-level failures include a fixed stage code without SQL errors.
Unavailable tables or snapshots are explicit. Reads do not acquire runtime
ownership, settle operations or replenish recovery budgets. Ordinary owner
and Connect diagnostics do not expose this administrator snapshot.

`stopped_root_effects` additionally selects up to ten stopped operations from
the newest 100 safety rows. Each operation returns at most ten effects (or the
smaller requested limit), ordered by descending model ordinal and parent call.
Lookups use the retained root runtime session ID and original operation ID, so
newer child activity cannot hide these root effects. Missing root identity makes
this collection explicitly unavailable; it never guesses a session ID. Collection
and per-operation truncation are explicit. No runtime-head payload is loaded.
