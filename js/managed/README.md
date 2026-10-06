# Managed durable-agent Worker

This Worker is Nanocodex's account-owned hosted-agent surface on Cloudflare. It
authenticates public requests, projects the caller's authority, and routes work
to durable, account-scoped services.

Managed agents have a native `browseX` tool for public X posts, profiles, search,
followers, and following. `environment().apis` advertises the tool independently
of connector authentication. It calls the private [X Worker](../x-api/README.md)
through `NANOCODEX_X`; deploy it with `pnpm deploy:x` before `pnpm deploy:managed`.

## Images

The hosted image tool relays generation and editing through the account's existing
subscription broker. `transparent_background: true` requests transparency;
omission or `false` explicitly requests an opaque background, including edits.
Edit references are exclusive `{ image_url }` inline data or `{ file_id }` objects
(legacy inline data strings remain accepted). Invalid targets fail before broker
I/O; the relay never uploads or downloads references.

Recent images come from this conversation's durable event history, including
archived history, scoped to the authorized invoking owner or non-guest child
session. In that scope, generated references are durably remembered before the
tool returns. Shared guests neither read nor append the owner's image history.
Reads inspect at most four 64-event pages within a 24 MiB history window and return at most five
references within a 24 MiB reference budget; older images outside that window are unavailable rather than silently
substituted. A selected legacy wait image whose operation identity is unavailable
requires reattachment rather than guessing or skipping a possible duplicate. Inline
edit references are limited to 20 MiB each. Prompt and steer image entries also accept
an exclusive `file_id` without fetching it.

Run the provider-mocked real Code Mode/SQLite and Workers SQLite/R2 journeys with
`pnpm --dir js/managed test:images`. The first journey also runs standalone with
`pnpm --dir js/managed test:images:node`; it reopens real disk SQLite but does not
claim to emulate Cloudflare or R2.

## Thread sharing tool

`thread_sharing` exposes `list`, `create`, `revoke`, and `revoke_all`. Omit
`session_id` to target the current thread; explicit targets must have the same
owner, organization, team, and authorization epoch. Calls require a direct
account root with `agents:read` and `tools:use`; mutations also require
`agents:write`. Connect grants, shared guests, and subagents cannot use it.

To disable all current sharing, call:

```js
await tools.thread_sharing({ operation: "revoke_all" });
```

`list` returns active link IDs, permissions, and creation times, never bearer
URLs. `revoke` takes a listed `link_id`. `create` returns a one-time bearer URL
and defaults to `permission: "read"`; `write` permits guest turn submission and
must be explicitly requested. Creating or distributing links requires user
authorization. Never automatically retry uncertain creation; inspect active
links and resolve the outcome first. Listing cannot recover a lost bearer URL.

The tool reuses the public owner-authenticated `/v1/agents/:id/share-links`
router. `DELETE` on that collection atomically revokes all active links and
returns `revoked_ids`, `revoked_count`, and `active_links: 0`. Live streams for
those links close immediately. Repeating it returns an empty result. Individual
`DELETE /share-links/:link_id` retains its existing 204/404 behavior. Revocation
does not cancel guest turns already admitted to the owner thread.

Guest metadata, history, and SSE redact share bearer tokens, including nested
Code Mode output and object keys, so creating another link cannot implicitly
redistribute write or cross-thread authority. Guest text updates use completed
assistant messages; assistant/reasoning delta fragments are owner-only because
tokens split across fragments could otherwise be reconstructed. Owner events
and ordinary shared tool results remain unchanged.

Run the Workerd route/tool journeys with
`pnpm --filter nanocodex-managed-service exec vitest run test/thread-share-links.test.ts`.
They emit sanitized journey traces for authorization, revocation, live-feed
closure, and bearer redaction.

## Ownership and security

`DurableAgentSession` exclusively owns an agent's mutable runtime: retained
history, turn admission and completion, ordered events, client sockets, tools,
and recovery. The edge Worker owns routing and authorization; an agent ID is a
routing identifier, never authority. Each agent route authenticates the account
or grant and forwards only its permitted slice.

### Short-lived request access

With `NANOCODEX_ACCESS_SECRET` configured (at least 32 random bytes), successful
live-authenticated agent HTTP responses issue a signed `x-nanocodex-access`
permission snapshot, valid for at most two minutes. The token is bound to the
origin and original login/key; Connect snapshots additionally bind all forwarded
grant restrictions. Clients retain the original credential for renewal. They
retry a 401 once only when ingress explicitly marks the snapshot as rejected
before admission, preserving the credential, body and operation identity.

Finite `/v1/agents` requests verify the signature locally. Their owning Session
still checks local owner, organization, team, epoch, lifecycle and operation
permissions. Account screen viewers can also reuse a snapshot; publishers,
ten-second viewer renewal, agent streams and account administration retain live
authentication. Existing accepted work retains its established execution policy.

The account Worker can verify viewer snapshots through the shared
`nanocodex/cloudflare/managed-access` module and reach its existing screen broker
directly. Configure the same `NANOCODEX_ACCESS_SECRET` in both the managed and
account Workers. Missing configuration or an invalid snapshot preserves the
original managed route and rejection protocol. This does not introduce a new
principal cache or extend the snapshot lifetime.

Account/key/membership changes prevent new snapshots immediately; an existing
snapshot may authorize requests until expiry. Rotating `NANOCODEX_ACCESS_SECRET`
in **both Workers** invalidates all snapshots once both updates are active. A
one-sided update does not invalidate the other verifier's accepted snapshots.
No per-user instant
revocation of issued snapshots is implied. Local Session fencing remains in
force. Tokens are never accepted for token renewal or as provider credentials.

Issuance piggybacks on an ordinary response, so there is no added cold-start HTTP
round trip. SDK caches are in memory; they renew via ordinary live authentication
near expiry. `managed.auth` logs and `managed_auth` Server-Timing distinguish live
and snapshot verification. Those timings measure authentication, not full request
or model latency. Missing configuration preserves the live-only path.

Connector credentials never enter this Worker, browser state, durable agent
state, or tool configuration. Model and connector access crosses the private
`NANOCODEX` Service Binding to `nanocodex-egress`, which owns credential routing
and injection.

### Session-owned credential subjects

The `MANAGED_AGENT_DIRECT_CREDENTIALS=true` setting makes each new
managed agent retain credential ownership in its existing Session DO. Its
private egress subject is `managed-session-v1_<Session DO id>`; credentials
remain in the broker. This removes the additional per-agent
`AgentSubjectDirectory` creation/binding. HTTP and live creation, models,
tools, and voice use the same retained strategy. Existing sessions keep their
directory subjects when the setting changes.

Wrangler enables this strategy in production and development. Before the first
deployment into an environment that predates the private ownership entrypoint,
bootstrap in this order using the existing build/deploy tooling:

1. Deploy compatible managed code with direct creation disabled using
   `--var MANAGED_AGENT_DIRECT_CREDENTIALS:false --containers-rollout none`.
   This exposes the private `ManagedAgentOwnership` entrypoint.
   Omit `NANOCODEX_SESSION_MODEL_EGRESS` from this bootstrap configuration if
   egress does not yet export `SessionModelEgress`; restore it in step 3.
2. Deploy egress with its `MANAGED_AGENT_OWNERSHIP` service binding to
   `nanocodex-durable-agent`, entrypoint `ManagedAgentOwnership`.
3. Deploy managed with the checked-in setting enabled. Development uses the
   same entrypoint through a local service binding.

The normal CI order (egress before managed) works after bootstrap. Code-only
manual deployments should use `--containers-rollout none` when the image has
not changed. Do not deploy an older experiment checkout over newer production
code. To stop new direct sessions, disable the setting while retaining both
Workers' direct-subject support: reverting to code predating that support
would break already-created sessions.

In production, the private `NANOCODEX_SESSION_MODEL_EGRESS` binding targets
egress's `SessionModelEgress` entrypoint. New-strategy Sessions validate retained
ownership locally for each model WebSocket connection, reconnect, and HTTPS request, avoiding
a broker callback into the originating Session. This binding is not exposed to
tools. Credential selection remains live in the broker. Without the optional
binding, the transport retains the usual broker ownership lookup; legacy
directory subjects retain their existing authority.

Hosted Responses requests can fall back from WebSockets to streaming HTTPS through
that same private binding. Compaction permits the initial request plus two retries
per transport, matching codex-rs; after WebSocket exhaustion it switches to HTTPS
and replays the full retained history. The selected transport remains sticky for
the live model session. If compaction still fails, its failure receipt is retained
before the turn fails, so durable recovery replays the failure instead of starting
another provider retry cycle. In-flight interruption and storage failures remain
recoverable, and failed compaction preserves the conversation history.

Deploy egress and the ChatGPT HTTP relay support before the managed runtime.
Sponsored trial credentials currently reject HTTPS Responses before dispatch;
the WebSocket admission and metering policy cannot be bypassed by fallback.

Active clients call `POST /v1/agents/:id/prepare` (no body), or the managed SDK's
`agent.prepare()`, when opening a conversation. The authenticated mutation
requires `agents:write`, `tools:use`, and the ChatGPT connector for delegated
grants. It acknowledges with HTTP 202 `{ "state": "preparing" }`; this means
accepted, not provider-ready. One session-owned task starts runtime/socket
preconnection, personalization, and first-turn account metadata. Prompt and
voice media admission do not await the activation HTTP request. Passive event
and history subscriptions do not prepare models. Preparation installs an idle
alarm and expires after the configured runtime idle interval (30 seconds by
default); reopening the conversation renews it. No `generate:false` model
request is inserted before a prompt.

Connector/MCP discovery, hosted-tool snapshots, and startup account metadata
are retained in memory for at most `MANAGED_ACCESS_TTL_MS` (two minutes), measured
from the start of each read. Concurrent callers share reads. Startup metadata
is keyed by owner, organization, team, authorization epoch and exact turn
authorization; catalog reuse is owner/authority scoped. Runtime shutdown,
including settings replacement, invalidates these snapshots. Failed reads are
not retained as successful snapshots. Explicit account-info tools still force
live discovery, and tool invocation retains its existing live authorization.
These caches store discovery metadata, not credentials or an authorization
bypass. Egress must expose `/users/:user/catalog` for this startup path.

`managed.agent.transport` observations include the managed turn and runtime
request IDs, failure class/phase, retry delay, connection generation and whether
a retry opens a new socket. Completed model calls also log `model_call_index`,
`duration_ms`, `time_to_first_event_ms`, `time_to_first_output_ms`, and the
provider `response_id`. These separate connection setup from a provider that
acknowledges a request promptly but produces output much later.

Hosted WebSocket diagnostics are always enabled in Workers Logs. The
`managed.performance` stages `transport.socket.connecting`,
`transport.socket.opened`, `transport.request.sent`,
`transport.request.first_message`, `transport.request.first_output`, and
`transport.request.finished` describe each socket/request lifecycle.
`first_message` and `first_output` include an allowlisted `provider_event_type`;
`first_output` also identifies its `output_kind`. This historical output marker
includes empty item announcements, so it is not a first-token measurement.
`transport.request.first_reasoning_delta`, `first_answer_delta`, and
`first_tool_delta` separately mark the first nonempty string delta of each kind.
Waiting and finished records include the corresponding elapsed `*_delta_ms` values.
These are frame-arrival times at the host, before runtime consumption or UI rendering.
Classification inspects envelopes up to 16,384 UTF-16 code units; larger/unknown initial frames are
`unclassified`, and an absent delta marker does not prove the provider emitted none.
`transport.socket.connect_waiting`, `transport.request.send_waiting`, and
`transport.request.waiting` first emit after one second of silence, then at
2/4/5-second intervals. Each incoming frame resets the silence interval.
They never cancel a valid quiet model request. Waiting records include elapsed
time, received frame count, time since the last frame, queued frame count,
maximum queue residence, delivered frame count, and buffered send bytes.
Delivery and maximum residence are cumulative for the socket. A rising inbound
age with an empty queue means the host has no unread frames; queued frames and
large residence times instead point to consumption in our runtime.

`transport.provider.timing` emits the allowlisted provider timing metadata when
it arrives, including `pre_inference_ms`, `engine_queue_max_ms`, and
`engine_service_ttft_total_ms`. These are overlapping provider spans; do not add
them together. `transport.socket.closed` includes the close code, clean-close
flag, intentional-close flag, and final queue counters. Diagnostic hooks cannot
fail a model request. Raw frames, prompts, credentials, close reasons, and
provider error strings are excluded from these records.

In [Cloudflare Observability's Query Builder](https://developers.cloudflare.com/workers/observability/query-builder/),
select the incident time range and the Events view. Use the following filters
and add the listed fields as columns:

| Investigation | Filters | Useful columns |
| --- | --- | --- |
| One thread's timeline | `thread_id` Equals the managed agent ID | `type`, `stage`, `message_type`, `turn_id`, `request_id`, `model_call_index`, `tool_call_id`, `source_call_id` |
| Slow output | `type` Equals `managed.agent.transport`; `message_type` Equals `model.call.completed`; `time_to_first_output_ms` Greater than `1000` | `duration_ms`, `time_to_first_event_ms`, `time_to_first_output_ms`, `response_id`, `attempt_count` |
| A request still waiting | `stage` Equals `transport.request.waiting` | `elapsed_ms`, `last_message_age_ms`, `received_message_count`, `queued_message_count`, `socket_queue_residence_max_ms` |
| Socket/provider timing | `socket_id` Equals the socket ID | `socket_request_index`, `stage`, `provider_request_id`, `response_id`, `egress_request_id` |
| Broker and relay | `egress_request_id` Equals the ID from the socket record | `type`, `relay_id`, `outcome`, `duration_ms` |
| Every tool in a thread | `thread_id` Equals the managed agent ID; `type` Equals `managed.agent.tool` | `tool`, `message_type`, `tool_call_id`, `parent_call_id`, `agent_id`, `runtime_session_id`, `managed_turn_id`, `runtime_turn_id`, `duration_ms`, `started_after_ms` |
| Brain-to-Hand call | `source_call_id` Equals the runtime `tool_call_id` | `type`, `stage`, `thread_id`, `transport_call_id`, `outcome`, `admission_ms`, `roundtrip_ms`, `settlement_ms`, `host_timing`, `transit_return_overhead_ms` |
| Hand disconnect/reconnect | `hand_id` Equals the target ID; `type` Equals `hand.connection` | `stage`, `reason_code`, `connection_id`, `host_connection_id`, `host_runtime_id`, `lease_id`, `runtime_generation`, `connected`, `active`, `pending_call_count` |
| A missing Hand result | `transport_call_id` Equals the broker call ID | `stage`, `host_stage`, `host_elapsed_ms`, `reason_code`, `outcome`, `connection_id`, `connection_generation` |
| Screen input/result | `thread_id` Equals the managed agent ID; `type` Equals `hand.remote` | `stage`, `source_call_id`, `request_id`, `hand_id`, `connection_id`, `remote_generation`, `reason_code`, `close_code` |

`managed.agent.tool` records every delivered call and result, including nested
Code Mode and child agents. Names retain the canonical tool identifier. Rust
owns result `duration_ms` and nested `started_after_ms`; the original managed
turn and runtime turn remain separate. `tool.waiting` begins at one second,
backs off to five-second intervals, and never expires or cancels a tool. Replay
records retain `event_seq` and `replayed`; a result after recovery can have
`start_observed: false`. Missing completion after a reset means unknown outcome.

`managed.tool.invocation` records the actual awaited handler boundary with its
thread, runtime session, host turn and call IDs. `host_turn_id` is the runtime's
execution/gate token (for example `session:1`), distinct from the event's
`runtime_turn_id` and the API's `managed_turn_id`. Hand protocol/stage `turn_id`
also carries that host token; join these boundaries by runtime session and call
ID. `hand.tool.stage` separates namespace
preparation/routing, account ownership/resolution/fetch/decode, and sandbox SDK
invocation. Provider/account summaries include their complete local wait.
`hand.call.broker` records admission, actual socket send, host progress, receipt,
ACK delivery, terminal outcome, cancellation, durable replay and late/duplicate
receipts. The SQLite ledger retains thread and connection identities across
hibernation and restart.

### Service boundaries and connection evidence

The model path is account proxy → managed Session → egress → account relay →
relay container → provider. The tool path is managed runtime → namespace/tool
handler → account tool broker → authenticated Hand socket → Hand executor, then
the return path. Sandbox SDK and remote-screen calls have separate handler and
transport observations. Use native spans for the real Worker/DO subrequests and
logical IDs for retained sockets and external processes.

| Boundary | Evidence | What it establishes |
| --- | --- | --- |
| API to managed Session | `managed.proxy`, native fetch/DO spans, managed admission/stages | The request reached this service and its local wait |
| Runtime to handler | `managed.agent.tool`, `managed.tool.invocation` | Every delivered root, nested and child call/result; awaited handler duration |
| Namespace to account | `hand.tool.stage`, `hand.call.provider`, `hand.provider.invoke` | Routing/preparation and complete account fetch/decode wait |
| Account authorization/resolution | `hand.tool.stage`, `hand.account.invoke` | Ownership/catalog resolution and complete admitted handler wait |
| Account request decoding | `account.decode_input`, `input_decode_ms` | Awaited request-body decoding, measured separately from ownership authorization |
| Durable broker admission | `received`, `admitted`, `dispatched` | The broker retained an intent and dispatch decision; these do not prove a socket write |
| Broker socket write | `send_started`, `sent`, `send_failed` | `sent` means the transport accepted the frame, with no execution claim |
| Hand receipt | `host_progress` / `received` | The selected Hand parsed the call |
| Hand execution | `host_progress` / `execution_started`, `execution_finished` | The local executor started/finished, with monotonic elapsed time |
| Hand result preparation | `host_progress` / `result_prepared` | A result was prepared; it may still be lost in transit |
| Broker result/ACK | `receipt`, `ack_attempt`, `ack_sent`, `ack_failed`, `terminal` | Receipt settlement and ACK write are separate boundaries |
| Connection loss/recovery | `transport_lost`; `hand.connection` lifecycle | Fixed failure cause, pinned generation, lease/liveness evidence and whether a new connection became ready |
| Managed model socket | `transport.request.*`, queue/delivery counters | Send, first event/output and whether frames are absent or awaiting consumption |
| Egress/relay/provider | `egress.*`, `responses.relay.*`, `transport.provider.timing` | Routing/upgrade, relay first bytes/close and allowlisted provider timing |
| Screen host | `hand.remote` call and connection stages | Input send, result, cancellation, timeout or transport loss; this protocol has no execution acknowledgment |

Call observations pin `hand_id` (canonical target), `host_runtime_id` (Hand
process), `host_connection_id` (client connection attempt), `connection_id`
(broker socket), and `lease_id` + `runtime_generation` (dispatch ownership).
`connection_generation` remains a compatibility alias for that ownership epoch.
Reconnects have new client/socket IDs. A recovery-capable publisher with the
same authenticated route, runtime ID and catalog retains its ownership
generation; a new runtime starts a new generation. Recovery requests refer to
durably admitted command IDs and never instruct the Hand to execute again.
Screen sockets use their own `connection_id` and `remote_generation`.

Account ownership is immutable after its durable claim. Each broker instance
loads the owner once and compares it synchronously for subsequent calls;
reconstruction reloads it from SQLite. `ownership_ms` begins after input decoding,
so body scheduling does not appear as authorization work.

Connection records distinguish accepted/ready/resumed, transport error/close,
replacement, draining, authority expiry and fencing. Broker restart and
transport loss preserve dispatched recovery-capable commands until their
admitted deadlines. The same Hand can prove that a command is running or return
its retained receipt. Missing journal entries, changed runtimes and expired
deadlines retain an uncertain outcome without rerunning the command. A Hand
that reported `execution_finished` and
`result_prepared` without a broker `receipt` identifies a lost return path,
rather than proving execution never occurred.

The Node Hand emits content-free `hand.attachment` records for **every** local
connection attempt, including attempts that never reach Cloudflare, errors,
reconnect scheduling, successful readiness and retained or replayed command
results. Native Hand lifecycle tracing carries the same runtime/attempt IDs;
its connector reports combined TLS + HTTP-upgrade duration. Cloudflare cannot
observe DNS/TCP failures that never reach it; correlate local Hand logs when
there is no server acceptance record. Neither an unexpired lease nor an open
socket proves a provider is responsive. Use local control-pong evidence,
connection lifecycle records and the last acknowledged call phase. Control
frames do not enter the broker's application logs. Silence observations begin
at one second; authority expiry and command deadlines remain independent of
transport liveness.

The broker persists a command ID once for each source session/call identity.
The Hand retains the command's immutable input and running task or completed
receipt in its living executor runtime until acknowledgment. A reconnect's
`recover` frame contains only command IDs; the Hand returns `status` (`running`
or `missing`) or the original `result`. Lost acknowledgments cause receipt
replay, which the durable broker accepts idempotently. The Hand journal is not
disk persistence: a daemon restart loses running execution proof, while the
broker's admitted identities and terminal receipts remain durable.

[Cloudflare handles WebSocket control ping/pong automatically](https://developers.cloudflare.com/durable-objects/best-practices/websockets/#automatic-pingpong-handling),
including during Durable Object hibernation. Native and Node transports with
control-frame APIs detect missed pongs locally. Standard browser WebSockets
rely on platform close/error signals and admitted command deadlines; they do
not send an application heartbeat. Provisioned VM authorization renewal and
revocation continue separately from transport liveness.

Older publishers without `command_recovery` still receive matching JSON `pong`
replies to their bounded JSON `ping` frames. Ownership and provisioned VM
authorization checks also apply to these heartbeats. They do not introduce an
ordinary Hand liveness lease or opt an older runtime into command recovery;
the broker sends no `recover` frames to those catalogs.

While a VM command is admitted, cached authorization expiry triggers a fresh
authority check. An independently renewed VM lease preserves the runtime epoch;
revocation fences it. Authority lookups have a finite timeout, and cannot delay
or extend the command's original deadline.

### Durable per-thread diagnostics

`GET /v1/agents/:id/diagnostics` returns content-free stored boundary evidence
from the managed Session and account Hand broker, independently of Cloudflare
log ingestion or a live tail. It requires the owning authority with
`agents:read`; Connect grants cannot read it. Existing ownership, organization,
team and authorization-epoch checks apply. Deleted/exported sessions are not
readable. Responses use `Cache-Control: no-store`.

```sh
curl --fail-with-body "$NANOCODEX_ORIGIN/v1/agents/$THREAD_ID/diagnostics?limit=256" \
  -H "Authorization: Bearer $NANOCODEX_TOKEN"
```

The response has `thread_id` and a `services` array. Each service includes
`events`, `available`, `next_after`, `history_truncated`, retention/bounds and,
when readable, `write_failed`. Events contain local `seq` and server
`created_at`; order each service by `seq`. IDs and monotonic durations join
machines; do not subtract different hosts' clocks to infer one-way latency.
The Hand page additionally exposes `connections` and `remote_connections`:
current account-owned socket, lease and pending-call snapshots. `connected`
means the retained socket's local state is open; `active` also requires its
valid ownership/lease. Neither field is a physical liveness acknowledgment.

Paginate independently with `after_managed` and `after_hand` set to the
corresponding service's `next_after`. `limit` is per service, from 1 to 1024
(default 256). Relevant connection lifecycle records may precede the first call;
calls from another thread sharing that connection are excluded. Events are
retained for seven days, bounded to 20,063 rows per journal. A service read
failure returns `available: false` while the other service remains readable.
Retention gaps and detected journal write failures set `history_truncated`;
write failures are marked durably when storage permits. Total process/storage
loss can also lose the final observation. Missing phases/completions are
unknown evidence, never proof that a side effect did not execute. These records
cannot reconstruct phases missing from an incident before their rollout.

Arguments, results, prompts, screenshots, machine names, paths, credentials and
raw transport error/close text are excluded by an explicit projection. Inspect
the thread/call, then its pinned connection and last acknowledged phase. Follow
`egress_request_id`/`relay_id` into Workers/container logs for the model return
path; those external-service records are not copied into this SQLite journal.

Hand receipts report monotonic `scheduler_ms`, `execution_gate_ms`,
`execution_ms`, `result_encode_ms`, `result_queue_ms` and `host_elapsed_ms` in
`host_timing`, separately from the business outcome. `roundtrip_ms` starts at
the broker's dispatch and ends at receipt. `transit_return_overhead_ms` is the
signed difference between that duration and reported Hand elapsed time. A
nonnegative value combines outbound/return transit, socket handoff and
unmeasured metadata work; it cannot separate one-way latency. Negative values
indicate inconsistent timing or host reporting, and must not be read as network
latency. Workers clocks can report zero for synchronous work between I/O events.
Rust and JavaScript Hands publish `diagnostics: true`, their client
`connection_id`, and `command_recovery: true` in the catalog. The current wire
contract carries progress, receipt timing, and retained command recovery
without compatibility negotiation or a legacy fallback. Deploy the broker
before updating Hands.

### Native distributed traces

All three Workers already enable tracing with head sampling set to one.
[Cloudflare propagates native context](https://developers.cloudflare.com/changelog/post/2026-05-07-automatic-tracing-across-do-and-worker-subrequests/)
through Service Binding and Durable Object subrequests; [RPC sessions and method
calls](https://developers.cloudflare.com/changelog/post/2026-09-17-javascript-rpc-session-spans/)
are also instrumented automatically. Keep calls on these bindings so the
platform records their real caller/callee relationship.

Custom spans wrap `managed.proxy`, managed operations/stages, `nanocodex.tool`,
`hand.provider.invoke`, `hand.account.invoke`, `egress.request`, and
`responses.relay.request`. They await the actual operation and retain bounded
`nanocodex.thread_id`, `nanocodex.tool_call_id`, host/managed turn IDs, and
egress/relay IDs where available. Account Hand spans also retain pinned Hand,
client/socket, runtime, lease and generation attributes. Logs inside a span
inherit its native context.
Search span attributes for the thread/call, then open the native trace waterfall
and follow automatic fetch, Durable Object and RPC spans between custom spans.
The existing `managed.performance.trace_id` is an application operation ID;
it is **not** Cloudflare's native trace ID.

A durable turn can outlive its HTTP 202 admission and use a retained WebSocket,
later messages, alarms or recovery invocations. Use logical thread/call IDs to
join those invocations and inspect `managed.agent.tool` and
`managed.tool.invocation` for the complete tool timeline.
The pinned local runtime can finalize an admission tracer when a later request
takes over the DO's background work, leaving truncated custom spans or missing
attributes. The journey verifies the full awaited account `/invoke` span and
native ancestry, and uses lifecycle records for complete background-tool timing.
[The custom-span API](https://developers.cloudflare.com/workers/observability/traces/custom-spans/)
does not expose native trace/span IDs or manual parent/link injection, and
[external W3C propagation](https://developers.cloudflare.com/workers/observability/traces/known-limitations/)
is not supported yet. Hand and Node relay records join through the private
call/egress/relay IDs, rather than a fabricated `traceparent`.

The broker creates `egress_request_id` on each upgrade; it is returned privately
to the managed host and passed to the relay. Account Worker `responses.relay`
records join it to `relay_id`. Relay container `responses.relay.upstream` records
handshake timing, and `responses.relay.stream` records first bytes in each
direction and one terminal close/error with byte/chunk counts and last-byte
ages. Container byte arrivals are transport activity, not model output.
Use the response ID and request index to distinguish calls on a reused socket.
Missing terminal records after a process reset do not prove an upstream timeout.

These records are emitted during the wait; Cloudflare ingestion and WebSocket
invocation buffering determine when they appear in the dashboard. Prefer stored
Workers Logs for incident investigation. Adding/removing live tails can reset
the relay Durable Object and disturb active requests.

Reproduce the quiet-provider journey locally with
`pnpm --filter nanocodex-managed-service run test:model-pause`. It runs the public
Cloudflare SDK and real WASM/SQLite runtime over service-binding WebSockets,
holds the synthetic provider silent for 3.2 seconds, checks the first observation
at one second before completion, and checks continuous output and a follow-up
on the same socket. Its runtime transcript
and extracted records are retained in ignored `output/model-pause-journey/`.

`pnpm --filter nanocodex-managed-service run test:tool-timing` exercises the
shipped account proxy, managed Session, WASM/Code Mode, child agent and account
tool broker against a real Node shell Hand. It inserts 250 ms outbound and
150 ms return delay, verifies every call/result and handler rejection, and
reconstructs the account DO to verify replay without executing the command
twice. It also tests a socket error without a close event, two failed reconnect
attempts followed by recovery, an execution whose receipt is lost, and a screen
click whose return socket closes. The real public diagnostics API must preserve
the distinct boundaries, isolate thread calls, paginate and enforce ownership/
capabilities; uncertain inputs must not repeat. Native Cloudflare spans,
ancestry, span-attributed logs, runtime history, diagnostic pages and Hand
receipts are retained in ignored `output/thread-tool-timing-journey/`.

`pnpm --filter nanocodex-managed-service run test:hand-owner-restart` kills the
actual account broker runtime process group while a shell command is executing,
then restarts on the same HTTP port and SQLite storage. The surviving publisher
reconnects. The original command ID and ownership epoch recover from SQLite,
the completed output and exit code return, the original effect occurs once,
and a fresh command succeeds. Evidence is retained in ignored
`output/hand-owner-restart-journey/`.

`pnpm --filter nanocodex-managed-service run test:hand-reconnect` also runs a
frozen publisher from `546bec456` against the current broker over real Workerd
WebSockets and native `/bin/sh`. It verifies matching legacy JSON heartbeats
across hibernation and reconnect, shell progress while a command waits, and
ambiguity after disconnecting a pending legacy command without replay. A fresh
command succeeds with no recovery frames for the older catalog. The journey
also verifies strict malformed-frame rejection.
Evidence is retained in ignored `output/hand-legacy-heartbeat/`; the existing
recovery journey continues to cover current publishers and control heartbeats.

`pnpm --filter nanocodex-managed-service run test:hand-communication` measures
warm calls, shell calls while CUA waits, and a 50-command burst over the real
namespace/provider/account/Hand transports. It reuses fresh discovery, retains
every correlated diagnostic phase, and verifies account ownership before and
after broker reconstruction. `NANOCODEX_BENCHMARK_SOURCE_ROOT` selects archived
implementation sources for comparisons; `NANOCODEX_BENCHMARK_LABEL` names the
run. Commands, source hashes, timings, diagnostic pages and wire transcripts
are retained in ignored `output/hand-communication-journey/`.

`pnpm --filter nanocodex-managed-service run test:hand-leased-recovery` exercises
a quiet shell poll across VM authorization expiry, a real broker process kill
and SQLite restart after cached expiry, a stalled authority lookup with an
independent command deadline, and authoritative revocation. The external VM
authority is synthetic; HTTP, WebSockets, the broker, SQLite, the Hand journal
and shell execution are real. Evidence is retained in ignored
`output/hand-leased-recovery-journey/`.

`pnpm --filter nanocodex-managed-service run test:hosted-tools` runs account,
broker and wire contracts with a focused real Hand worker. It remains part of
the package's default test command. WebSocket clients acknowledge server close
and cleanup waits for a bounded close handshake, including replaced sockets.

The resolver reads retained ownership without constructing the agent runtime.
Deleted, exported, or pending-import sessions deny resolution; egress never
falls back to a directory entry after a direct-subject denial.

Reusable Hosted Tools protocol, broker-state, and durable-memory policy live in
`nanocodex-tools`. This Worker supplies their Durable Object SQL/WebSocket
adapters and retains account scope, Connect authorization, bindings, and
storage ownership.

## Markdown memory

Managed agents use `memories__read`, `memories__search`, and `memories__write` for editable
curated Markdown and daily notes stored in Durable Object SQLite. Read and search
keep the Codex argument/result contracts. Background indexing and daily consolidation
use the existing AI Search and Workers AI
bindings. Compaction is independent of memory; agents save useful context during
their work. `memories__status` reports availability and
durable job receipts. Set `NANOCODEX_MEMORY_AUTOMATION=false` to disable background
consolidation. See the
[design and API](../../docs/workers-markdown-memory.md) for ownership, simple writes,
startup excerpts, and implementation boundaries. The four baseline Codex memory
APIs are preserved; versioned legacy CRUD is retired.

## Prepared personalization

Managed admission does not run prompt-derived history search or memory scan.
MemoryScope prepares bounded snapshots of canonical personal/team Markdown;
sessions warm a disposable copy on create, open, or activity without awaiting it.
Each turn pins an eligible local copy or a cache miss. A miss proceeds without
retrieval. Explicit `find_session`, `read_session`, and memory tools remain available.

Snapshots carry organization/team/user scope, source versions, and a five-minute
lease. Markdown changes invalidate prepared copies in the background; failed
notifications remain retryable without failing canonical note writes or reads.
Expiry is checked before injection. Later prepared blocks replace or withdraw
prior prepared context; already delivered conversation text cannot be erased.
Private notes remain separate from shared team knowledge.

Each scope receives at most 12 KiB of serialized Markdown excerpts. Subscriber
leases are bounded; extra agents proceed with a cache miss. Refresh is driven by
activity, so idle users incur no periodic job. Identical context is not appended
again, and prepared rows are pruned with archived turn receipts.

Voice startup consumes already-prepared context without waiting for memory.
A background refresh can also send prepared context to an active voice session.
Rust/WASM and Apple voice clients accept it as bounded background data. Media
readiness and prompt admission never await memory preparation.
Account/environment discovery remains a separate first-turn dependency.

### Personal memories and request attribution

The canonical `/v1/memories/{list,read,search,add_ad_hoc_note,write,status}` API
uses the authenticated user's private root for direct account calls. Shared
notes are available through `team/` paths; writes to shared Markdown require
`scope: "team"` and the user's request. Connected-app grants have only their
authorized team root. Personal memory follows its user across teams in the same
organization; request arguments cannot name another user.

The former `/v1/memory` versioned CRUD API and SDK methods are removed. Legacy
fact/scan tables and old fact-bearing context are retired on activation while
canonical notes, append-only Codex notes, conversation history, and the original
startup environment remain.

Clients may send bounded `x-nanocodex-client-context` JSON (`client`, `hand`,
logical `cwd`, `timezone`, optional `location`). Location contains numeric `latitude`,
`longitude`, `accuracy_meters`, Unix-millisecond `timestamp_ms`, and boolean
`approximate`. Only finite coordinates in geographic range, accuracy from 0 to
100,000 meters, and samples at most five minutes old or 30 seconds in the future
are retained. Invalid location is omitted without losing other context; freshness
is checked again at startup projection. Location is unverified client-reported
data and is never inferred from an attached Hand. The SDK exposes `requestOrigin`; the native CLI sets
its own context automatically. The authenticated edge overwrites the principal
assertion. HTTP, WebSocket, and voice admission pin caller context independently
for each accepted request; retries cannot replace that request's origin. A new
turn from another device receives its own `request_origin`, while the initial
startup snapshot remains historical. The request context travels with its durable
dispatch input, including queued and recovered turns. `environment()` refreshes
the authorized Hand catalog and returns the current tool turn's origin. Missing
or legacy attribution remains unknown instead of inheriting another device.
A successfully routed voice steer updates the effective origin for subsequent
work, while retaining the original admission/retry provenance and existing
command bindings. Replaying an older voice receipt cannot change that origin.

Native execution and interactive browser work prefer an explicit task target or
existing workspace/session, then a suitable submitting Hand, then another capable
online user Hand. `execution_preferences` exposes separate advisory candidate
lists for native execution and CUA, with explicit logical `workdir` values. It
excludes offline user Hands, favors user Hands over sandboxes, accounts for
reported low disk/memory, and compares observed hardware size plus fresh free-capacity measurements. Task
requirements still determine the appropriate OS, workspace, and capacity; the
recommendation does not route tools or reserve resources. `/brain` remains the
shell default, and submitted commands, process sessions, and captured Code Mode
connections never migrate after a disconnect.

Hand publishers may include bounded `resources` observations: timestamp, logical
CPUs, memory totals/availability, load, and workspace filesystem totals/availability.
The environment marks samples `fresh`, `stale`, or `unknown`. Missing values are
not zero capacity. Older publishers remain compatible and show unknown resources;
new metadata requires an updated publisher. Deploy the managed service (including
its Hand catalog normalizer) before updating/restarting CLI or desktop publishers:
older brokers reject unknown catalog fields. Existing old publishers work with
the updated service. Refresh or inspect a Hand when a
capacity decision depends on current free memory/disk.

Interactive website workflows prefer a headed browser through the selected
Hand's supported background tabs and agent-owned tab groups. Headed browsing
does not imply bringing a window to the foreground. The prompt and CUA discovery
guidance prohibit taking over the user's browser as a fallback when dedicated
browser APIs are disabled; a native window is not background tab isolation.
Use another supported background surface or isolated desktop, or report the
limitation. Hosted browsers remain available for unavailable non-disruptive CUA
and supported private credential workflows. Playwright/Puppeteer and headless
browsers are not the default for interactive user tasks; repository browser test
suites may retain their automation. Vault and secure-input boundaries continue to
apply to credentials. Before provisioning a sandbox, inspect configured SSH
recovery targets and attempt the exact task-authorized server when available.
Do not infer a hostname from an offline Hand label or assume SSH provides CUA.
Account SSH identities are generated/stored by the credential broker, and only
the public key is installed on the target.

## Public journeys and protocol boundaries

Create an agent, durably admit its first turn, and stream its output in one request:

```sh
curl --no-buffer --fail-with-body "$NANOCODEX_ORIGIN/v1/agent-runs" \
  -H "Authorization: Bearer $NANOCODEX_API_KEY" \
  -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' \
  -H 'Idempotency-Key: example-run-001' \
  --data '{"input":"Reply with a short greeting.","settings":{"model":"gpt-6.1-sol","thinking":"low","reasoning_mode":"standard","fast_mode":false}}'
```

Reuse that key with the same body after an uncertain response. Choose a new key
for a new run; the command exits after the requested turn's terminal event.
Explicit GPT settings avoid the automatic model-selection catalog read. Omit
settings to select from the account's available models. Credential authorization
still occurs at dispatch.

Run `node js/managed/benchmark/curl-ttft.mjs --mode=stream --family=codex` from
the repository root to measure the normal account proxy, Managed API, Session
and Egress path with curl against local workerd and a synthetic external provider.
Use `--mode=combined` for the existing JSON-then-events path, `--mode=legacy`
for separate create/submit/events, and `--root=/path/to/checkout` for a baseline.
The harness records first assistant text, durable completion, source hashes and
raw traces under ignored `output/managed-api-ttft/`; it does not measure live
inference, network geography or production cold activation.

- SMS OTP/account and API-key routes establish the account identity that owns
  agents, organizations, connectors, memory, and history.
- `/v1/agents` lists or creates agents. `/v1/agent-runs` creates an agent and
  admits its first turn under one required stable key and one client request.
  The default response remains JSON. Send `Accept: text/event-stream` to receive
  the admission receipt and live output in that same POST; streaming additionally
  requires `agents:read`. Both agent creation and turn acceptance are durable
  before the response starts. The first `event: run` contains the JSON receipt
  (`agent_id`, `session_id`, `turn_id`, `turn_idempotency_key`, `accepted_cursor`
  and turn state); it has no event ID. Subsequent frames are the ordinary managed
  durable events, starting with the accepted turn, with their original cursors.
  The response is `201` for first acceptance and `200` for an idempotent replay;
  validation/authorization failures remain JSON. `Location` points to the agent's
  event endpoint and `x-nanocodex-agent-id` / `x-nanocodex-turn-id` identify the run.
  A disconnect leaves the accepted turn running. Retry the same POST and key to
  recover its receipt, or reconnect to `GET /v1/agents/:id/events`; both accept a
  numeric `Last-Event-ID` and resume after that cursor. Without a cursor, a repeated
  streaming POST replays from the first turn's acceptance. The POST stream closes
  after delivering that turn's durable `turn_completed`, `turn_failed` or
  `turn_cancelled` event, so curl exits normally. Resuming at/after a retained
  terminal cursor returns the receipt then closes. The existing GET event stream
  stays open. A changed input under the same key remains an idempotency conflict.
  Agent routes create later turns, read state,
  cancel or steer work, delete an agent, and support explicit durability import
  and export. Stable `Idempotency-Key` values make create and turn retries safe.
- `POST /v1/agents/:id/turns` also accepts `Accept: text/event-stream` to
  admit a later turn and receive its output in one request. The default stays
  JSON; both formats return `202` for new acceptance and `200` for replay. Supply
  a stable body `id`, an `Idempotency-Key`, or both. Streaming requires
  `agents:read`, `agents:write` and `tools:use` before admission. Its initial
  `event: run` identifies the actual retained turn; `turn_idempotency_key` is
  omitted when no key was supplied. The retained event cursor, terminal close,
  and disconnect/reconnect behavior match `/v1/agent-runs` above. Malformed
  `Last-Event-ID` is rejected before admission. A valid but ahead cursor or
  subscriber limit can fail after durable acceptance: recover with the **same
  input and id/key**, or use GET events, rather than submitting a new identity.
- `POST /v1/agents/:id/forks` accepts an empty body and a required stable
  `Idempotency-Key`. Full account `agents:read`, `agents:write` and `tools:use`
  authority is required; Connect grants and configured/routed sessions are not
  supported. It copies the latest Rust-owned committed model boundary into a
  separate durable child; no rendered event transcript or parent prompt is
  replayed. It returns `201` with an ordinary agent receipt plus
  `parent_agent_id`, or `409` if no safe boundary is available. Retry an
  uncertain response with the *same* key. Partial model output and unfinished
  tool effects are not inherited. Forks share account tools and `/brain`, so
  independent conversations do not isolate external side effects.
  The JavaScript managed SDK exposes `agent.fork({idempotencyKey})` and the
  native Rust SDK exposes `ManagedClient::fork(parent_id, key)`.
- `GET /v1/agents/:id/capacity` requires `agents:read` for that agent and returns
  storage byte counts, hot receipt counts, and archive counts without loading
  the runtime or returning conversation contents.
- Managed agents execute Just Bash in durable `/brain` without a hand.
  `exec_command` defaults there; `/brain` and `.` also select the brain. File
  metadata and small bodies live in the owning agent's SQLite storage. Bodies
  above 1 MiB and streaming uploads remain in R2; this selects storage and does
  not reject larger files. Existing R2 trees are indexed without copying their
  bodies. Native hand mounts use the SDK's S3 protocol through trusted RPC to
  that same actor, preserving prefix and read-only fences without a remote R2
  request for every filesystem stat. Listings refresh between commands. Local
  Sandbox SDK replication continues using its existing R2 binding. Text/file
  processing, HTTP, and supported Git/GitHub commands run
  here; native binaries, package installs, builds, and process sessions need a
  hand. The agent reuses a suitable attached hand or mounts one when needed.
  Known native work such as `cargo test` can go directly to a hand; an
  unsupported brain capability can also trigger that choice after a probe.
  `exec_command` always honors its selected cwd; the agent owns the fallback.
  Brain execution requires `tools:use`, with connector authority taken
  from the exact calling root or subagent.
  Shell and Git transfers stream through the account's egress broker without an
  application byte ceiling. Browser runtime and Cloudflare Sandbox HTTP traffic
  use the same broker; native `gh` receives a public marker so authentication is
  injected only at the provider boundary. Exact Connect identities and revocation
  apply to both GitHub API calls and Git smart HTTP. Connect grants cannot use
  Vault-backed shell requests or SSH identities.
  The pnpm patch for Sandbox SDK 0.12.4 preserves S3FS `x-amz-meta-*` metadata
  through R2 uploads, metadata-replacing copies, multipart uploads, and reads.
  Without it, native permissions and timestamps disappear after revalidation.
  `sandbox-r2-metadata.test.ts` exercises the SDK proxy against the Worker R2
  binding; remove the patch when an SDK release passes that contract unpatched.
  Shell execution, workspace traversal, and subagent admission have no implicit
  application quota; caller-specified limits, cancellation, and platform capacity
  still apply.
  The provider-neutral `mount` model
  tool provisions and attaches named execution hands on demand. `cf_sandbox`
  names the built-in Cloudflare Sandbox factory (`cloudflare` remains a legacy
  input alias); any other provider value is the exact name of a connected VM
  factory. Several agent-, account-, or system-scoped factories may coexist,
  and repeated mount names resolve idempotently.
- Code Mode routes each command from the root of its `cwd`: mounted roots may
  live on different factories while remaining visible in one namespace.
  Subagents inherit the spawning turn's exact namespace authorization, so a
  long-lived child cannot borrow capabilities from a later root turn.
- Turn input has no application byte ceiling. HTTP uses native JSON parsing;
  incoming WebSockets use Cloudflare's platform limit. SQLite stores large raw
  inputs and frozen dispatch inputs in Unicode-safe chunks below its row limit.
  Coordination scans load metadata; a receipt or dispatch hydrates its own turn.
  Terminal receipts archive sequentially to R2 before their local chunks are
  deleted. `/state.first_prompt` and the portability session's `first_prompt`
  are display previews, not prompt content. Accepted events and turn receipts
  retain the exact full input. Subagent authorization keeps task/role identity
  digests instead of duplicate content. Inline JSON still requires memory for
  the individual request; Cloudflare's shared 128 MB isolate heap applies.
  Cron schedules likewise have no prompt-size or schedule-count admission cap.
  Their input and frozen delivery snapshots use the same chunk placement; alarm
  scans page through indexed metadata and hydrate one occurrence at a time.
  Replacing a schedule releases its old input while queued deliveries retain
  their original payload until delivery is acknowledged.
- Agent events are a durable, ordered cursor stream. SSE resumes with `cursor`
  or `Last-Event-ID`; same-origin browser WebSockets carry the typed
  prompt/steer/cancel protocol. Realtime calls and sideband transport have
  separate agent-scoped WebSocket routes.
- API key resolution validates live account membership, scope, and authorization
  epoch inside the key object. This avoids serial edge-to-account round trips;
  raw-key resolution never caches authority. Short-lived signed snapshots above
  avoid repeating that resolution on every finite agent request. A response marker allows rolling
  deployments to fall back to the original checks against older key objects.
- Voice call creation derives a coarse relay region from trusted Cloudflare
  request metadata. A separate `voice-v1:<region>:<user>` relay prevents an old
  text relay from anchoring media in a distant region. Unknown geography uses
  the existing relay. Placement is a hint, and a sleeping relay still incurs
  container startup time; provider credentials remain server-side.
  After live Session ownership validation, managed calls use the private
  `ManagedRealtimeEgress` binding so the broker does not repeat that lookup.
  This also covers retained legacy subjects: call creation skips the redundant
  directory rebind and readback. Legacy sidebands still repair their mapping.
  Generic agent egress cannot use the owner assertion. Deploy egress before
  managed to install the entrypoint; without the binding, calls retain the
  generic broker path and its ownership check.
- Voice admission does not wait for the independent Responses preconnection.
  The shared Rust protocol delegates the first spoken question and gates reply
  playback until durable output is delivered. Its WASM plan searches memory and
  prior sessions using that question, including new calls in existing chats.
  Durable receipts retain each call's bounded lookups across retries. Existing
  first-turn environment and account context remains developer context.
  Voice start and stop retain the full session context without a conversation
  size rejection. Replies above 512 KiB are archived directly in R2 for exact
  replay instead of being inserted into a SQLite row.
  Successful memory puts and deletes emit authorized `managed.voice.context`
  events; Rust validates call scope, deduplicates cursors, and queues background
  context through reconnects. Retrieved context is data, never instructions.
- `/v1/history/*` exposes retained team history; `/v1/memory` exposes team or
  personal memories. `/v1/credentials` and `/v1/connectors` manage brokered
  credentials, OAuth connections, and MCP connections without exposing secrets.
- Managed agents can search completed team conversations with `find_session`
  (`find_sessions` remains available) and verify exact turns with `read_session`.
  Each call requires its own agent's `history:read` capability.
- Before the first model request, the host appends one durable developer message
  in `<startup_context>` tags after the baseline prompt and static runtime rules.
  It includes the startup UTC time, account/team/session scope, known request
  transport, authenticated principal, available Hands, connected accounts, and bounded
  prepared snapshots of personal and team memories when available. The CLI reports
  its project Hand and logical cwd; web and Apple clients report client type and
  timezone. Client/Hand attribution is explicitly client-reported, not proof of a
  physical device or person. Hand keys/cwd are matched against authorized Hands;
  missing or unmatched attribution remains unknown and never grants authority.
  `environment().hands` maps each Hand key to its logical `path`, capabilities,
  name, online status, and providers. Use that path as `exec_command.workdir`.
  Native paths use readable computer names, such as `/omarchy-desktop`. The
  first assignment is persisted by machine identity; duplicate names receive
  numeric suffixes and renames do not retarget existing paths. Previous opaque
  identity paths remain accepted by execution, preview and computer selection.
  New VM paths include their factory and purpose (`/vm-omarchy-desktop-demo`);
  Cloudflare sandboxes use `/cloudflare-demo`. Existing persisted VM roots keep
  their original spelling. VM display names also identify their provider.
  `environment().accounts[service].connections` lists exact account selectors;
  service entries also advertise deferred tools and documentation.
  XML data is escaped and explicitly carries no instructional authority.
  Startup does not search past threads using the current prompt: `find_session`,
  `read_session`, `memories__search`, and `memories__read` provide scoped recall when needed.
  The environment and timestamp are frozen once, including across retries,
  reconnects, and pending-memory invalidation. Later turns append to the existing
  conversation without rewriting its cacheable prefix or changing cache keys.
  Existing memory correction/forget invalidation remains effective; it never
  refreshes the startup environment. Use `environment()` for an explicit refresh.
  Old configurations naming `accountInfo` are normalized to `environment`.
  User hands include `online` attachment status. Offline hands remain in the
  namespace so admitted calls can recover their receipts. A broker-confirmed
  unstarted call returns an unavailable-hand result for the agent to handle;
  transport failures with unknown admission retain the existing call identity.
  `online` reports connection presence, not provider responsiveness. Discovery
  may use a cached snapshot; `environment()` explicitly refreshes it. Hand
  transports use WebSocket control ping/pong when their API supports it.
  Ordinary Hand ownership has no heartbeat lease expiry. Transport failures
  reconnect and reconcile retained command IDs, running work and receipts;
  admitted command deadlines still apply. A changed runtime, revoked authority
  or protocol violation uses terminal code 1008. Missing execution proof
  remains uncertain, and commands are not automatically executed again.
  Subsequent turns use the `memories__*` tools for scoped recall and Markdown
  updates. Writes require root-agent `memory:write` authority. Markdown writes
  default to private memory for direct accounts and shared memory for Connect;
  shared writes also require an explicit user request.
- `create_cron` saves a recurring prompt through the same durable scheduler as
  `/v1/agents/:id/triggers/:triggerId`. Supply a stable `id`, five-field `cron`,
  and `input`; optional `timezone`, `enabled`, and `session_mode` default to UTC,
  true, and `new`. Identical retries return the saved schedule; conflicting IDs
  fail without replacement. Creation requires account `agents:write` and
  `tools:use` authority; Connect grants and shared rooms cannot create schedules.
  Use the triggers API or UI to edit, pause, or delete a saved schedule.
- `/v1/rooms` creates, joins, observes, and deletes multiplayer rooms. A
  `MultiplayerRoom` owns room chat and its private agent; `MultiplayerQuota`
  enforces deployment-wide room and turn limits. Room WebSockets use their own
  replay cursor and `say`/`ack` protocol.

The small root page is an operator surface; it is not a second application
protocol. `/health` is the service health endpoint.

## Cloudflare bindings

| Binding | Role |
| --- | --- |
| `NANOCODEX` | Private Service Binding to `nanocodex-egress` for credentials and persistent-account wallets. |
| `NANOCODEX_SESSIONS` | One `DurableAgentSession` per managed agent. |
| `NANOCODEX_ROOMS`, `NANOCODEX_MULTIPLAYER_QUOTA` | Multiplayer state and global quota. |
| `NANOCODEX_AUTH`, `NANOCODEX_USERS`, `NANOCODEX_API_KEYS`, `NANOCODEX_ORGANIZATIONS`, `NANOCODEX_MEMORY` | Account, key, organization, and durable-memory ownership. |
| `NANOCODEX_CRM` | D1 storage for private-account CRM people, companies, and dated notes. |
| `NANOCODEX_USER_DATA`, `NANOCODEX_USER_DATA_OBJECTS` | One SQLite-backed data scope per user and its opaque R2 object bodies. |
| `NANOCODEX_HISTORY`, `HISTORY_AI_SEARCH` | R2 history archive and production history retrieval. |
| `NANOCODEX_WORKSPACES`, `NANOCODEX_WORKSPACES_*`, `NANOCODEX_BRAIN` | Retained per-hand workspaces, read-only peer aliases, and the durable agent's shared writable `/brain` scratch. |
| `BROWSER`, `LOADER` | Browser Run and the sandboxed Worker loader used by the official Agents browser runtime. |

### Persistent prompt apps

The `apps` agent tool generates account-private Swift source for the native
`swift-v1` runtime. `NANOCODEX_CRM` stores source and JSON state in `prompt_apps`
(migration `0011_prompt_apps.sql`). Source is untrusted data; the native host
validates it against the supported Swift language subset before execution.
There is no HTML, JavaScript, WebKit, or web runtime fallback.

All routes require direct account authorization: `agents:read` for reads or
`agents:write` for mutations, plus `tools:use`. Connect grants are rejected.
Cookie mutations require an Origin matching the request origin. Responses are JSON
with `Cache-Control: no-store`; Swift source is returned as JSON data and is never
rendered as a web page.

| Method and path | Input | Result |
| --- | --- | --- |
| `GET /v1/apps` | `limit` (1–100, default 30), `cursor` | `{apps, next_cursor}`; summaries include runtime and omit source |
| `POST /v1/apps` | `{title, description?, runtime:"swift-v1", source}` | Full manifest, HTTP 201 |
| `GET /v1/apps/:id` | None | Full manifest |
| `PUT /v1/apps/:id` | `{title, description?, runtime:"swift-v1", source, revision}` | Replaced manifest |
| `DELETE /v1/apps/:id` | `{revision}` JSON, or `?revision=N` | `{deleted:true,id}` |
| `POST /v1/apps/:id/restore` | `{revision}` | Previous source restored, new manifest revision |
| `GET /v1/apps/:id/data` | None | `{value,revision,updated_at}` |
| `PUT /v1/apps/:id/data` | `{value,revision}` | Saved state receipt |

Manifests contain `id,title,description,runtime,source,revision,created_at,updated_at`.
App revision starts at 1; empty state is `{value:null,revision:0,updated_at:null}`.
Source and data revisions are independent. Stale writes/deletes/restores return
HTTP 409 `revision_conflict`; read the current revision before retrying. A source
edit retains one previous title/description/source version. Restore swaps the two
versions, increments the app revision and preserves state; a new app returns
409 `no_previous_revision`. Deletion atomically removes source, previous source
and state. Missing or other-account IDs return 404.

Limits: 100 apps per account, 256 KiB UTF-8 Swift source, 256-byte title, 2048-byte
description, and 256 KiB JSON state with at most 64 nesting levels. The JSON
request envelope is limited to 2 MiB to allow escaped text. Unknown fields,
invalid revisions, duplicate/unknown query parameters and non-JSON inputs fail.

Every save requires `runtime: "swift-v1"` and `source`. Missing or unsupported
runtimes return HTTP 400 `unsupported_runtime`; the former `html` field and
unknown fields return HTTP 400 `invalid_input`. Runtime and source validation
failures never change the existing document, retained source, or account data.
The service stores source without compiling it; unsupported Swift syntax is
reported by the native runtime when the app opens.

Apps declare one Swift `struct Name: View` with a `body`, native controls and
bounded Swift expressions and actions. `@State` is session-only;
`@Persisted("stable-key")` loads and saves account JSON through the native host.
Use stable keys across source revisions. In a button action,
`Task { answer = try await Agent.run("prompt") }` calls the existing signed-in agent
and returns text. The host owns credentials, progress, cancellation and error
reporting. See [the Swift authoring contract](../../apple/NanocodexApps/AUTHORING.md)
for supported syntax, controls, modifiers and runtime limits.

Run `pnpm --filter nanocodex-managed-service run test:apps` for the real worker
HTTP/D1 journey, including website proxy forwarding, ownership, origin checks,
optimistic write races, Swift source recovery, legacy-contract rejection, and
deletion. Synthetic authentication is the only fixture at this boundary;
production proxy/router/storage run intact.

### Private-account CRM database

`NANOCODEX_CRM` is a `D1Database` owned by this Worker. CRM records belong to
persistent private accounts; sessions use the account identity to reach the same
records. Multiplayer agents and Connect grants do not receive CRM access.
CRM record names are searchable with Greek or Latin spelling, independent of
case, tonos, dialytika, or composed/decomposed accents. In a synthetic example, `Giannis`,
`Yiannis`, and `Chalkidis` find `Γιάννης Χαλκίδης`; `xalkidis` and
`khalkidis` are also accepted. This is query-time normalization: saved names,
identities, and source evidence are unchanged. The online CRM screen uses the
same `/v1/crm` search as `crm_search`; it needs only a managed Worker rollout,
with no schema migration, backfill, or client release. Offline client filtering
is unchanged.

`crm_graph` also expands names on source-managed record nodes. Freeform graph
text/metadata and other record fields retain their existing literal substring
search. `%`, `_`, `*`, `?`, brackets, and backslashes remain literal in all queries.
Normalization is intended for common Greek names, not arbitrary phonetic or typo
matching. Exact SQL matches and normalized name matches are deduplicated before
pagination, retaining account/filter-scoped cursors. Name expansion reads narrow
ID/name candidates in batches of 1,000; existing exact-result lookahead bounds
that scan when possible. A missing query still scans the account's record names,
so large-account latency should be checked before rollout. It never scans graph
documents or aggregates notes into a database string.

Run `pnpm --filter nanocodex-managed-service run test:crm-search` after building
the managed runtime prerequisites to exercise authenticated HTTP, managed tools,
D1, pagination, updates, account isolation, and a 2,000-record/10,000-node fixture.
Each run writes its request/event transcript, timings, and runtime log under
`output/crm-greek-search/`. A live authorized smoke test remains required before
production rollout.

The agent tools are `crm_search`, `crm_get`, `crm_save`, `crm_save_note`,
`crm_delete`, and `crm_delete_note`. They save people and companies, link people
to companies, and retain dated notes with optional source URLs. Search/list and
note reads are paginated. Saves preserve omitted fields; `null` clears optional
fields and an empty tags array clears tags. A company deletion unlinks its
people and preserves those people. Tool arguments cannot choose another account.
CRM records and notes remain untrusted content, never tool authority.

Account clients can browse the same data through read-only HTTP routes. All
require direct account authorization with `agents:read` and `tools:use`; Connect
grants are rejected. Responses use `Cache-Control: no-store`.

| GET route | Query parameters | Response |
| --- | --- | --- |
| `/v1/crm` | `q`, `kind` (`person` or `company`), `tag`, `company_id`, `limit`, `cursor` | `records`, `next_cursor` |
| `/v1/crm/:id` | `notes_limit`, `notes_cursor`, `timeline_limit`, `timeline_cursor` | `record`, `notes`, notes `next_cursor`, research, `identities`, `facts`, `relationships`, and each collection's `*_next_cursor`; people also include `timeline` and `timeline_next_cursor` |
| `/v1/crm/:id/identities` | `limit`, `cursor` | `identities`, `next_cursor` |
| `/v1/crm/:id/facts` | `limit`, `cursor` | `facts`, `next_cursor` |
| `/v1/crm/:id/relationships` | `limit`, `cursor` | `relationships`, `next_cursor` |

Relationship rows include account-scoped `from_name` and `to_name` alongside
`from_id` and `to_id`. Continue notes with `/v1/crm/:id?notes_cursor=...`.
Cursors are opaque and bound to the account and query; preserve filters between
pages. Page limits range from 1 to 100. Unknown or repeated query parameters
return 400; missing authentication returns 401, insufficient authorization 403,
missing or another account's records 404, unsupported methods 405, and unavailable
storage 503.

Calendar collection is opt-in through `crm_automation` (`enable`, `status`,
`disable`). Enable selects one connected Google account and one or more calendars
(default `primary`), and creates an hourly durable agent schedule. The first
collection can run immediately with `crm_sync`; subsequent collections continue
when the user disconnects. The importer reads Calendar through the existing
account connector, with a default window of 30 days back and 14 days ahead.
Follow returned sync cursors until `complete=true`. `limited=true` separately
reports incomplete attendee coverage: partial or over-200-guest invitations are
not used to create new meetings/contacts, and existing attendee links are retained
until a complete snapshot arrives. Other events on the page still import. Calendar API/scope failures
are errors, not successful empty calendars. The Google OAuth project must have
Google Calendar API enabled, and the connection must grant Calendar access.

`crm_meetings` lists and reads imported events, records user-supplied meeting
notes, and explicitly skips/reopens note collection. Attendees match people by
exact normalized email; ambiguous existing matches remain unresolved. Recurring
instances have separate meeting IDs. Repeated imports preserve manual profile
fields and meeting notes, and cancellations or declined invitations are excluded
from the missing-notes queue. A scheduled event is not proof of attendance.

`crm_research` queues profiles needing enrichment, reads research, and saves a
sourced summary, company, title, website and evidence references. The scheduled
agent combines invitation context, relevant email threads and corroborating
public sources; uncertain identity is saved as `needs_review`. Sources retain
Calendar event IDs, Gmail message IDs or public URLs. Imported content is data,
never permission to act. Research stays separate from user-authored contact
fields, and a biography or invite description never satisfies meeting notes.
`crm_get` includes the separate research profile; `crm_search` also matches its
company, title, website and summary. Completed research projects title and website
into empty person fields on reads; a unique current, matching `works_at` edge
projects `company_id`. `field_origins` marks those derived values without writing
over manual contact fields. All-day events retain their original date strings and
are excluded from the default missing-notes queue.

The data model also supports multiple identifiers through `crm_identity`: alternate
emails, GitHub/X/LinkedIn/Telegram profiles, websites, domains and known-as names.
Exact email matching includes these aliases, while identifiers shared by multiple
people remain ambiguous. Names alone never merge profiles.

`crm_facts` stores structured JSON facts with dotted predicates, origin (`user`,
`source` or `inferred`), evidence, confidence and effective dates. Examples include
expertise, education, location, company sector and founding year. Inferences need
a rationale; they remain distinct from user observations. `crm_relationships`
retains dated employment roles and explicit knows/worked-with/referral links.
Listing by either endpoint retrieves a person's history or a company's roster.
Identities, facts and relationships use the same private D1 database and
conversational tools. Simple collections use tags.
`crm_get` includes bounded pages of identities, facts and relationships, with
separate continuation cursors; `crm_search` matches these details too.

`crm_events` (`list`, `get`, `save`, `delete`) stores conferences and other event
containers with title, start/end, location and provenance. `get` returns a bounded
participation roster; pass its `next_cursor` as `roster_cursor` to continue.
`crm_event_participation` (`list`, `save`, `delete`) links an event to a `record_id`:
a person, or a company with `role="organizer"`. Role is separate from attendance
status (`unknown`, `invited`, `expected`, `attended`, `declined`). An invitation,
public attendee list, or organizer role does not establish that someone attended.
List existing participation before saving; edit its ID to update the assertion.

`crm_interactions` (`list`, `get`, `save`, `delete`) stores one shared observation
with `participants: [{record_id, role}]`, flexible `type`, optional `summary`,
`body`, and `occurred_at`. A single `person_id` is a convenience for one participant.
For example, a synthetic proposal can have `type="proposal"`, proposer and
recipient roles, and `occurred_at="2026-09-20"`; the same interaction appears in
both people's timelines. `YYYY-MM-DD` preserves date-only precision; RFC3339
preserves a supplied time. Date-only entries sort at UTC midnight and expose
`precision="date"`. Do not invent a time when the user supplied only a date.

Interactions optionally link an owned `event_id`, an imported `meeting_id`, or
an imported email's paired `connection_id`/`message_id`. Meeting/email links must
relate to a participant. Event membership is not required to record an independent
observation. Participant/link identity and origin are immutable on edits.
Deleting an event or imported source detaches its link and preserves the
independent interaction; deleting one participant preserves shared history for
remaining participants. Interactions with no remaining participants remain in
account-wide history. Shared interaction provenance also covers participant roles.

Events, participation and interactions keep `origin` (`user`, `source`,
`inferred`), `sources`, `confidence`, and `rationale`. Source assertions need
references; inferences also need confidence and rationale. User statements stay
separate from research. An interaction does not fill a meeting's missing notes
or implicitly assert attendance.

`crm_timeline({person_id?, event_id?, limit, cursor, from, to})` reads a bounded
newest-first history. Omit person_id for account-wide history; event_id restricts
event participation and associated interactions; it does not transitively include
linked native Calendar or email records. A shared interaction appears
once in the global timeline. `crm_get` includes its first page as `timeline`, with
`timeline_next_cursor`; continue with `timeline_limit`/`timeline_cursor` or use
the dedicated tool. Notes retain their independent cursor. Timeline cursors are
scoped to the account, person, event and time filters; `from` is inclusive and `to`
exclusive. Equal timestamps use stable kind/ID ordering. The timeline queries
native Calendar meetings, meeting notes and surviving contact/email notes alongside
event participation and shared interactions, without duplicating source records.
Legacy contact notes have no recorded origin, so the timeline does not classify
them as user observations.
Repeated attendee aliases yield one Calendar entry. Calendar entries retain the
matched person’s response status and the account’s declined flag; acceptance
does not assert attendance. Conflicting alias responses remain unresolved. Newly imported emails retain
the provider's receipt timestamp; older imports use their import timestamp with
an explicit `timestamp_basis`. Deleting an imported note does not resurrect it
from its import receipt. Timeline reads do not fetch a mailbox or expand Calendar
collection beyond its existing opt-in scope.

For example, after "automatically collect my meetings", the agent enables the
schedule, imports events and researches attendees. "Which meetings need notes?"
uses `crm_meetings({operation:"list",needs_notes:true})`. "For Jamie's meeting,
we discussed benchmarks and I owe them the results" creates a note for that
specific meeting; an ambiguous name/date is resolved before saving. The meeting
then leaves the missing-notes queue. The workflow has no UI and sends no messages
to other people.

The schema lives in `migrations/`; D1 SQL migrations are separate from the
Durable Object migration tags in `wrangler.jsonc`.

Use `pnpm deploy:managed` from the repository root for production. Both this
command and the normal Cloudflare release job run
`scripts/cloudflare/managed-crm.mjs deploy`. The helper resolves
`nanocodex-crm-production` in `CLOUDFLARE_ACCOUNT_ID`, creates it only when the
provider reports it missing, then applies pending migrations with `--remote`
before uploading the Worker. The deploy token needs D1 edit access in addition
to the existing Worker/container permissions. With an environment token, supply
both `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN`. Without that token, local
production deployments use the existing Wrangler login through `d1 list --json`;
Wrangler owns credential resolution and refresh. This path requires the named
production database to exist already. If it is absent, provision it explicitly
with Wrangler before retrying. Lookup/authentication failures, duplicate names,
invalid UUIDs and mismatched configured IDs stop before migrations or upload.
The database UUID is resolved at deployment time and pinned in an ephemeral
Wrangler config shared by migrations and upload; no production UUID needs to be
committed. CI checks the current release before each mutation. Failed migration
or provisioning stops the upload. An uncertain creation is not retried in the
same run; the next release resolves the database by name.

From `js/managed`, run `pnpm run db:migrate:local` before development. This
applies the same migrations to `nanocodex-crm-development` in
`js/account/.wrangler/state`, the Vite development stack’s persistence directory.
For a standalone managed `wrangler dev`, run the same Wrangler migration command
without `--persist-to ../account/.wrangler/state` to use managed’s own state. The test binding uses the distinct local identity `nanocodex-crm-test`.
Neither local identity is a cloud database UUID.

`pnpm --filter nanocodex-managed-service run preview` applies migrations to
`nanocodex-crm-preview` locally, then dry-runs the full managed Worker bundle.
The CI preview job uses this same command. It replaces the production D1
binding, removes named environments and cloud credentials, and never provisions
or migrates a remote database. Managed previews remain validation-only because
this Worker uses Durable Objects and containers.

Use the deployment helper for uploads, rather than invoking `wrangler deploy`
directly: Wrangler's automatic resource provisioning does not apply the schema.
Keep future migrations compatible with the currently deployed Worker, since
migrations finish before the new Worker receives traffic. Rollbacks do not undo
SQL migrations.

### SMS OTP delivery

Set `NANOCODEX_OTP_HMAC_KEY` to at least 32 random bytes and create a Twilio
Verify Service configured for SMS with six-digit codes. Provide its
`TWILIO_VERIFY_SERVICE_SID` with `TWILIO_API_KEY_SID` and
`TWILIO_API_KEY_SECRET` Worker secrets. `TWILIO_ACCOUNT_SID` plus
`TWILIO_AUTH_TOKEN` is accepted as a fallback credential pair when an API key
is not configured.

The checked-in `development` Wrangler environment uses `123456` as a local
Verify fixture and does not contact Twilio. The fixture is active only when
`ENVIRONMENT` is exactly `development`; production ignores the fixture value
and still requires a valid Verify Service and credentials.

Twilio Verify generates, delivers, and checks each code, automatically upgrading
eligible SMS requests to RCS. Nanocodex retains a five-minute opaque local
challenge so a successful verification can be bound to the initiating browser,
and stores the phone only as a keyed HMAC digest for identity and abuse limits.
It never logs phone numbers, codes, provider responses, or credentials. Keep the
HMAC key stable; rotating it requires an identity migration or known phones will
resolve to new accounts.

### Persistent account wallet

After Twilio Verify approves an OTP, the Worker provisions that persistent
account's secp256k1 root wallet through the existing `NANOCODEX` Service
Binding before issuing the account session. Provisioning is idempotent. A
wallet failure returns `wallet_unavailable`, issues no session, and leaves the
browser-bound challenge retryable. `GET /v1/wallet` returns public metadata;
same-origin authenticated `POST /v1/wallet/connect` and
`POST /v1/wallet/revoke-access-key` authorize and revoke exact access keys.

The private key is encrypted and used only inside the per-user egress Durable
Object. It never enters this Worker or the browser. This is custodial
server-side encryption, not user-held end-to-end encryption. See
[the wallet custody contract](../../docs/WALLET_CUSTODY.md). Existing
configurable-account migration is future work.

`wrangler.jsonc` is the binding and migration source of truth. Development
uses the same Worker role with local Durable Objects, local egress binding, R2,
and shorter idle timing; AI Search is a production binding. The wallet reuses
the existing `NANOCODEX` binding, so it adds no managed-Worker secret, binding,
or Durable Object migration.

### Managed browser provider

The managed Worker exposes `browser_execute` through `env.BROWSER` and the
Agents SDK CDP runtime, without provisioning a VM or desktop Hand. Production
and development Wrangler configuration select `MANAGED_BROWSER_PROVIDER=chromium`.
Local development uses a remote Browser Run binding and requires Cloudflare access.

`MANAGED_BROWSER_PROVIDER` is host deployment policy, never a tool argument:

- `kitesurf`: Browser Run with `browser=kitesurf` on its CDP requests.
- `chromium`: Browser Run's default Chromium engine through the same direct
  upstream runtime as Kitesurf, with no browser engine override.
- `cloudflare`: the existing retained Chromium provider with the managed CDP
  restrictions and private browser flows.
- `browserbase`: the existing Browserbase binding adapter; requires the
  `BROWSERBASE_API_KEY` Wrangler secret and optionally `BROWSERBASE_PROJECT_ID`.

`kitesurf` and `chromium` pass `env.BROWSER` directly to the upstream Agents SDK
runtime and use one-shot sessions: complete navigation and extraction in one
`browser_execute` call. Page state does not persist between calls. These providers
use the upstream tools, descriptions, CDP commands, and result handling without
Nanocodex's CDP filter or result proxy. Their default execution timeout is 90 seconds;
`MANAGED_BROWSER_TOOL_TIMEOUT_MS` can override it. Chromium also exposes a
separate retained private session through `browser_vault_open`, existing Vault
login tools, and generic snapshot/actions. Authenticated browsing can navigate,
fill ordinary forms, select choices, and activate user-authorized booking or
checkout controls. Login and action UUID receipts prevent replay after lost
responses; requested actions require subsequent outcome verification. Private
phone takeover handles unsupported controls and payment iframes. Credentials
never enter public CDP or model text arguments, and no VM is required. This is
not a universal automatic-payment or Stripe Link integration.

The older one-shot `browser_private_checkout_inspect` and
`browser_private_waitlist` remain available with their narrower contracts.
Kitesurf has no private Vault tools. See [Vault browser operations](../../docs/VAULT_BROWSER.md)
for the general session contract and [the private checkout journey](scripts/private-checkout-smoke.md)
for legacy opt-in browser validation.

`cloudflare` and `browserbase` retain bounded sessions per durable agent, with
separate storage for each provider. Their existing CDP restrictions and private
browser flows remain in place. Use workdir-scoped CUA when operating an existing
computer's browser. An unset `MANAGED_BROWSER_PROVIDER` selects `chromium`,
matching the checked-in production and development Wrangler configurations.

[Kitesurf is currently beta](https://developers.cloudflare.com/browser-run/kitesurf/).
It does not support every Chromium feature or long-running authenticated state.
Unsupported sites return their browser errors; there is no automatic VM allocation
or silent provider fallback. Operators can explicitly select `kitesurf` for its
beta engine or `cloudflare` for the retained private-browser integration. Neither
Browser Run engine requires a Nanocodex VM.

Run the [local hosted browser smoke test](scripts/kitesurf-smoke.md) to verify the real
remote binding through the managed runtime before rollout.

### Spotify on iPhone

**Connect Spotify** uses browser OAuth with PKCE. The native app binds only
`127.0.0.1:8989`, opens Spotify in `SFSafariViewController`, and accepts one
state-matching `/login` callback. It forwards only code/state to the authenticated
`POST /v1/connectors/spotify/loopback/callback` route. The encrypted broker owns
PKCE, token exchange, refresh and API authorization; no tokens enter agent tools.
The listener stops on completion, cancellation, leaving settings, or timeout.

`GET /v1/connectors/spotify/loopback` returns connection metadata; `POST` starts
an authorization and `DELETE` disconnects the specified `connection_id`. These
routes require a persistent owner session or owner API key with account-management
and tool authority. Delegated Connect grants cannot start or complete the flow.
The web Connect Spotify card opens `nanocodex://connect/spotify` on the phone.

This flow uses the public ncspot client registration also used by
[spotify-player](https://github.com/aome510/spotify-player/blob/master/spotify_player/src/auth.rs).
Its client ID and exact loopback redirect are fixed in the broker, and the client
ID is retained with each grant for refresh. Spotify consent identifies ncspot;
its availability and shared API quotas remain outside Nanocodex's control.
The separately configured hosted Spotify OAuth flow remains supported by the
broker. Vault password logins are separate from OAuth connector status.

### Host-principal project registry

Applications that exchange an existing Privy, Better Auth, Auth0, or other
verified host login must be registered in the Worker-only
`NANOCODEX_HOST_PROJECTS` value. Each entry binds one exact app, HTTPS origin,
identity issuer, and tenant to the SHA-256 digest of that application's project
secret:

```json
[{"app_id":"app-id","app_origin":"https://app.example","issuer":"identity-provider","tenant":"tenant-id","secret_sha256":"<43-character-base64url-SHA-256-without-padding>"}]
```

Produce the required digest from the exact secret bytes with no newline:

```bash
printf %s "$NANOCODEX_HOST_PROJECT_SECRET" | openssl dgst -sha256 -binary |
  openssl base64 -A | tr '+/' '-_' | tr -d '='
```

For local Wrangler development, put the one-line JSON value in the ignored
`js/managed/.dev.vars` file. For a deployment, set it before deploying this
Worker:

```bash
pnpm exec wrangler secret put NANOCODEX_HOST_PROJECTS --config wrangler.jsonc
```

Register every issuer/tenant pair an application can emit. The raw project
secret belongs only in that application's Worker; this registry contains its
digest, and the browser receives neither value. Deploy this managed Worker
before the Connect API and the host application so exchanges do not fail with
`invalid_project`.

## Development and operation

This package participates in the checkout-isolated local platform rather than
running as an independent product surface. Use the root
[README.md](../../README.md) for checkout setup, the root
[package scripts](../../package.json) for repository commands, and
[AGENTS.md](../../AGENTS.md) for deployment order and verification guidance.
The package scripts provide its focused
typecheck, test, and Wrangler dry-run build when that boundary changes.

### Sandbox development tools

New Cloudflare Sandbox images include Swift 6.3.3 (Ubuntu 22.04), Go 1.26.5,
Node 24.19.0, pnpm 11.25.0, and Rust 1.97.0 with rustfmt, Clippy, the
`wasm32-unknown-unknown` and `x86_64-unknown-linux-musl` targets, and
wasm-bindgen-cli 0.2.126. Rust, Go, Node, pnpm, and wasm-bindgen versions align
with the repository CI configuration; Swift is a pinned Linux toolchain, while
Apple CI uses the Swift bundled with its Xcode runner. Python, uv, C/C++ build
tools, CMake, Ninja, and musl-tools are also available.

Run `sh /usr/local/bin/nanocodex-check-dev-stack` in a sandbox to check the
installed tools and compile small Swift/Foundation, Go, Rust, musl, and WASM
programs without fetching package dependencies. The image build runs the same
check and fails if a compiler or required runtime library is missing.

Linux Swift supports portable Swift packages. AppKit, SwiftUI, iOS simulators,
Apple SDKs, and `xcodebuild` still require a Mac Hand or Apple CI; installing
Swift does not make all `apple/` packages Linux compatible.

These tools become available after the managed container image is built and
rolled out. Existing running sandboxes need recreation with the updated image.
When changing tool versions in CI, update the corresponding image pins too.

### Opening files from another Hand

`GET /v1/agents/:id/files?path=<logical absolute path>` serves private, uncached
file bytes after checking account, organization, team, authorization epoch,
`agents:read`, and `tools:use`. Connect grants cannot use this route. `/brain`
reads stream from the conversation's R2 prefix. Hand paths resolve through the
conversation's durable mount identities and use a captured execution route to
read bounded binary chunks; filenames are quoted as data on POSIX and Windows.
Missing or offline Hands fail explicitly. Only `file_path_unmapped` permits a
client to try its own local filesystem.

The terminal client downloads a complete file into a private temporary directory
before invoking the local viewer, preserves the filename, and removes failed or
cancelled downloads. Successful copies remain available to the viewer after the
terminal exits. File links may include the documented `:line` or `:line:column`
suffix. This behavior requires both the updated managed Worker and terminal
client; no update to an existing Hand is required.

### Original media attachments

The authenticated `/v1/agents/:id/attachments/:uuid` route streams original
image and MP4/MOV files into the existing R2 binding. It does not buffer complete
files or parts in Worker memory, and requires no S3 signing keys. Files keep
their `/brain/attachments/:uuid/original.*` paths.

`POST` accepts `{name, media_type, size}` and returns the path, part size, next
part number, and completion state. The Apple client uploads file-backed parts
with `PUT .../parts/:number`; the service hashes each incoming stream while
forwarding it to R2 with backpressure. Only one part body is ingested at a time
per agent. Identical retries are safe and conflicting bytes are rejected.
`POST .../complete` finalizes the R2 multipart upload and records it in the brain
filesystem catalog. Parts are normally 8 MiB and grow up to 100 MB to fit R2's
10,000-part limit. The Worker ingress limit applies per request, not per file;
this permits originals up to 1 TB. Original and preview bytes remain separate.

The phone prepares an oriented JPEG inspection fallback bounded to 2048 pixels
and 2 MiB. Preview uploads stream through the same R2 binding. Original downloads
remain private and support ranges; preview downloads are immutable and
account-scoped. Account, organization, team, authorization epoch, and capability
checks apply before upload. Connect grants cannot use this route. Deletion
cancels readers and aborts incomplete uploads before brain cleanup.

Default `view_image` passes the original R2 body stream to Cloudflare Images,
which decodes and resizes it outside the brain's JavaScript heap. Only the bounded
model image is encoded in the Worker. If the original cannot be transformed,
the tool can return the attachment's labeled JPEG fallback. Exact
`detail: "original"` reads preserve original bytes and supported-format behavior,
with an early 10 MiB size check; use default inspection for larger originals.
The production Wrangler configuration declares `NANOCODEX_ATTACHMENT_IMAGES`.
No R2 access key or new upload credential is needed.

Tests cover multipart streaming and retries, image transformation without
original-body buffering, bounded preview handling, cancellation, filesystem
visibility, and preserved original bytes on Apple clients.

## Browser on the Cloudflare sandbox desktop

The AMD64 Sandbox image includes Google Chrome. From the remote desktop's
terminal, open a visible browser with:

```sh
google-chrome --no-sandbox --ozone-platform=wayland --disable-dev-shm-usage \
  --no-first-run --start-maximized about:blank
```

The terminal inherits the running desktop's Wayland environment. A separate
shell execution does not automatically inherit that environment. The Sandbox
runs as root, so this command disables Chrome's process sandbox; use it only
inside the isolated Sandbox container. It does not disable TLS verification.
The Debian server Hand image separately provides `chromium`.

Reusable definitions, environment templates, signed lifecycle webhooks, usage
inspection, immutable turn artifacts and HTTP tool results are documented in
[Managed agent configuration and operations](../../docs/MANAGED_AGENT_CONFIGURATION.md).


### Connected-account tool discovery

Managed agents discover first-party `github_request`, Google Workspace capability
`*_request`, `slack_request`, `x_request`, `spotify_request`, and
`soundcloud_request`, and `cloudflare_request` tools through the same `tool_search` used by connected MCPs.
`environment().accounts` advertises tools for connected, grant-visible services;
`accounts[service].connections` supplies exact account selectors. Each call uses authenticated
egress with live grant and connection checks, broker-owned token refresh, fixed
provider origins, bounded JSON bodies/responses, and no automatic write retries.
Provider scopes and endpoint availability still apply. Spotify connection links
open `nanocodex://connect/spotify` to complete OAuth on the phone.


SoundCloud also supports phone-local OAuth through
`/v1/connectors/soundcloud/loopback` and its `/callback` route, using the same
owner-only authorization and bounded payload policy as Spotify. The broker uses
its configured SoundCloud app and the fixed `http://127.0.0.1:8788/callback`
redirect. Both music providers' connection tools return native app links; no
credentials or renewable tokens pass through the agent or phone API.

## Connect sandbox execution

A Connect app can request `urn:nanocodex:agent:execution:sandbox` in its hosted approval resources. The dialog displays **Cloud sandbox**; only the signed resource grants `agent.execution.sandbox`. Existing grants do not acquire this permission automatically.

The verified grant can provision `cf_sandbox` execution hands owned by that approval. Discovery, command dispatch, process sessions, captured Code Mode cells, and native peer mounts remain in that authorization. Personal computers, VM factories, account-owned sandbox workspaces, and desktop enrollment are unavailable. Sandbox network traffic uses public egress without account connector or Vault injection; approved connector tools continue through the brain's existing grant checks.

The durable agent's `/brain` remains shared across its conversations and authorization cohorts. Native workspace isolation does not make separate brain storage. Mount names and namespace slots remain agent-wide; another approval cannot adopt an existing approval's named mount.

Connect apps with sandbox execution can upload durable inputs through
`PUT /v1/grants/{grant}/agents/{agent}/inputs/{generationUUID}/{filename}`.
The JSON body contains exactly `data_base64` (canonical base64) and `sha256`
(lowercase SHA-256 hex). The server verifies the digest and derives the path
`/brain/connect/{grant}/inputs/{generationUUID}/{filename}`. Generation IDs are
UUIDs; filenames contain 1–128 ASCII letters, digits, dots, underscores or
hyphens and start with a letter or digit. Paths and URL-encoded names are not
accepted. The response is `{path,sha256,size}`: 201 on creation, 200 on an
identical retry, and 409 if an immutable name is reused with different bytes.
Retry an interrupted upload with the same body before admitting a turn.

Uploads allow 600,000 decoded bytes and a 1,000,000-byte JSON body. Quotas are
8 files/4.8 MB per generation, 256 files/30 MB per grant, and 1,024 files/120 MB
per agent. Incomplete reservations count toward these quotas and remain
retryable. Reservations and inputs are removed when the agent is deleted.

Connect turns publish only `/brain/connect/{grant}/outputs/{turn_id}/`, with
ownership derived from retained turn authorization. Apps should put this exact
output directory in the turn prompt. The ordinary immutable artifact limits
remain 50 files, 1 MB per file, 10 MB total. With `agent.output.final`, the app
can request `GET /v1/grants/{grant}/agents/{agent}/artifacts?turn_id={turn_id}`
and `GET /v1/grants/{grant}/agents/{agent}/artifacts/{artifact_id}/content`.
The turn filter is required; metadata, publication status and bytes are
restricted to that agent and grant, including after turn archival. Downloads
retain digest ETags and content lengths. Revoked/expired grants cannot access
these routes. Generic `/files`, attachments and configuration stay unavailable
to Connect. These HTTP boundaries do not change the agent's shared `/brain`
execution model described above.

## Native meeting library

See [Account meeting library](MEETING_LIBRARY_API.md) for recording persistence, revision-safe synchronization, summary generation, limits and the reusable local HTTP fixture.
