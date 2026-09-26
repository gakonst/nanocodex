# managed2 (greenfield)

One public API Worker → one Session Durable Object per agent → standard
`nanocodex/cloudflare` JS/WASM Agent → private `egress2` service binding.
No Just Bash, connector tools, or old-session migration in this slice.

## Authentication

Send `Authorization: Bearer ncx2_<43 base64url characters>` on each API call.
`AUTH_API_KEY_HASHES` is a required Worker **secret** containing a JSON object from
base64url SHA-256 of each whole API key to its canonical owner UUID. Managed2
hashes the presented key and resolves its owner locally in the Worker isolate;
there is no Account Worker or DO lookup per request. No raw API keys are stored
in the Worker. Replace the secret map to rotate/revoke keys. This dedicated
Managed2 key is distinct from production `ncx_live_*` keys (those need their
Account Worker directory lookup to resolve ownership).

The initial owner key is provisioned to a mode-0600 file on the user's Mac,
not in the repository or `/brain`. Anyone holding it has full read/write access
to that owner's Managed2 agents and credential-import endpoint. Keep it private.

## API and model boundary

`POST /v1/agents` with an optional `{ "input": "..." }` first turn,
`POST /v1/agents/:id/turns` (optional UUID `Idempotency-Key`),
`GET /v1/agents/:id/turns/:turnId`, and WebSocket
`GET /v1/agents/:id/events?cursor=...`. `PUT /v1/credentials/openai` uploads
a provider key; `PUT /v1/credentials/chatgpt` imports a subscription using
`{access_token, refresh_token, account_id, expires_at, fedramp}`. The API
briefly handles uploaded provider credentials, then sends them to Egress2's
per-owner credential DO. The Agent receives no provider credential.

The combined create request returns `202 { agent_id, turn_id, state: "accepted" }`;
create without a body still returns `201 { agent_id }`. A caller-provided UUID
`Idempotency-Key` on create fixes the agent ID (and first turn ID), so retrying
the same body does not create another agent or turn. The first turn is admitted
inside the same Session DO call; fetch `/v1/agents/:id/events?cursor=0` after
create to replay any events already emitted, or poll the returned turn ID.

The standard WASM Agent owns history, checkpoints, tool-step durability, and
cursor-resumable WebSocket events in Session SQLite. A turn's accepted ID/input
is retained for alarm reconciliation. Egress2 swaps the SDK placeholder for the
owner's active provider credential, with a 60-second isolate credential cache;
the credential DO encrypts the stored key and opaque Rust subscription payload.

This slice pins `gpt-6-sol`/low. It runs alongside, rather than replacing,
the existing managed API. The subscription path uses Egress2’s Cloudflare Gateway VPC binding and skips
the account-owned `ChatGptEgress` relay/container. That regional relay remains
a rollback route if the VPC binding is removed from Egress2. Managed2 uses a persistent
Responses WebSocket (`RESPONSES_TRANSPORT=websocket`).

```text
Client → Managed2 API → Session DO → Egress2 → VPC/Gateway → ChatGPT
                          state          │
                                    UserCredentials DO (cache miss/refresh)
```

The API Worker hashes its dedicated account key locally. The Session DO owns
turn admission, replayable events and the model socket. Egress2 substitutes the
provider credential; its isolate cache avoids a credential DO lookup on every
model/tool step. The credential DO retains the encrypted secret and refresh
token. Neither Managed2 nor Egress2 replaces the original Managed API or its
existing public ingress; clients must explicitly use the Managed2 hostname.
The private Egress2 service binding must not be exposed as a public HTTP route.

In a small staging two-tool trial, relay+WebSocket matched relay+HTTP at 6.53 s
median first-turn completion and measured 5.19 s versus 5.94 s on the second
turn. These samples do not prove a general latency improvement. Managed2 initially shipped with no tools. The first increment exposes only `current_time`
(UTC, no account access or filesystem), with HTTP and persistent-WebSocket
model→tool→model E2E coverage. Other tool families remain unavailable.

## Latency observation

Run `./managed2-demo "Reply with exactly: Timing check complete."` for a real
create, replayable event stream, final-text TTFT, and durable turn-status check.
The API returns `Server-Timing` for `auth`, `body_parse`, `session`,
`api_total`, `do_route`, `do_total`, `agent_init`, and `admission`. `session` includes the
Session DO wake, constructor, routing, initialization and turn admission; it is
**not** additive with `agent_init` or `admission`. `api_total` includes `session`
and excludes pre-Worker startup/network. `do_route` is the time from DO fetch
entry to the turn-dispatch path; `do_total` is the complete DO request. The
`session - do_total` residual approximates DO routing/scheduling and RPC
overhead, **not** a precise Worker cold-start measurement. A 101 upgrade cannot carry a constructed
`Server-Timing` response; `managed2.events_connect` logs auth, Session upgrade,
and total handshake time without owner IDs, prompts, or response contents.

The durable `GET /v1/agents/:id/turns/:turnId` response also includes a
`timing` object with a random `trace_id`, `agent_init_ms` duration, and elapsed
milliseconds since admission started for `accepted_ms`, `model_send_ms`,
`first_provider_event_ms`, `first_delta_ms`, `first_answer_delta_ms`, and
`result_ms`. The tool increment also records `first_model_call_ms`,
`first_tool_call_ms`, `first_tool_result_ms`, `post_tool_model_call_ms`,
`post_tool_model_send_ms` (WebSocket only), `tool_calls`, and total
`tool_duration_ms`. Each durable turn status also contains a `tool_timing` array
with one content-free row per call: call ID, name, start wall-clock and elapsed
turn time, result time/status, Rust-measured duration, and named handler phases
with their measured durations and invocation counts. It never stores arguments
or results. `managed2.tool_call` and `managed2.tool_result` logs carry the same
trace ID and call ID to correlate a particular call without logging its input.
A tool can record phases such as VFS hydrate, interpreter setup, execution,
persist/flush, upstream dispatch, and result parsing. Missing phases are
unknown, not zero. The model-call timestamps are logical Agent events, not
wire-send observations. The post-tool send distinguishes local tool execution
from the second provider round trip; null means unobserved, not zero.

The model-send observation runs just after the persistent socket
sends `response.create`. The first provider event is the first inbound
`api.event` seen by the Agent, not a raw socket-read timestamp; the difference
approximates upstream wait plus frame parsing. If concurrent turns make socket
attribution ambiguous, `model_send_ms` stays null. The content-free send
observer remains active past 32 turns on a persistent socket. Missing milestones remain `null`;
rehydration may prevent a first-delta observation. The first-delta clock is
server-side emission, **not** client receipt. On a cold agent `agent_init_ms`
includes WASM restore/startup and the persistent Responses socket preconnect;
it cannot separate them from this public Agent boundary. A warm turn can reuse
that socket without a new Egress fetch. Client-to-server residuals include
network, edge dispatch, and Worker activation, not just platform startup.

`managed2.model_route`, `managed2.agent_ready`, `managed2.model_send`,
`managed2.first_provider_event`, `managed2.turn_first_delta`,
`managed2.turn_first_answer_delta`, and `managed2.turn_result` share the trace
ID. The initial model-route log is a socket **preconnect**, not a per-turn
model request. Egress2 logs the same validated trace ID, its own relay request
ID, credential cache/lookup, upstream handshake, and one 401 recovery/retry
when applicable. The account-owned `ChatGptEgress` log links that relay request
ID to its locally generated relay ID; the relay-container log reports DNS,
TCP, TLS and upstream upgrade timings for the same relay ID. Egress `101`
timing exists in logs, not headers. Egress header time ends at WebSocket handshake or HTTP response headers.
The new per-turn send/first-inbound-event spans separate local turn preparation
from upstream wait but still do **not** isolate provider inference, queueing,
network, or raw first-frame transit within that upstream interval. Do not subtract an
Egress preconnect span from a warm turn or claim these spans are all additive.

No log contains keys, owner/account IDs, prompts, message text, upstream
headers or bodies. Persistent Worker logs/traces are sampled at 100%; use
Cloudflare Observability or `wrangler tail` to filter the fixed event names.
External container/provider spans are not in the Cloudflare trace. A request,
stream connection, and alarm-resumed turn may have separate trace roots;
`0ms` spans can be timer-resolution artifacts.

## Parallel rollout

These Workers have distinct names and Durable Object namespaces from the existing
managed API. They do not change its account ingress, service bindings or
migrations. Egress2 is private (`workers_dev: false`) and must be deployed before
Managed2, whose dedicated `workers.dev` endpoint is the opt-in public ingress:

```sh
pnpm --filter nanocodex-egress2-service run build
pnpm --filter nanocodex-managed2 run build
# After verifying the existing secret names and intended owner/test account:
cd js/egress2 && npx wrangler deploy --env="" --config wrangler.jsonc
cd ../managed2 && npx wrangler deploy --env="" --config wrangler.jsonc
```

Egress2 requires its existing `CREDENTIAL_ENCRYPTION_KEY` secret to read stored
credentials. Managed2 requires `AUTH_API_KEY_HASHES`; Wrangler secret values are
not stored in source and are not inherited from the original managed API. Do not
rotate either casually: an encryption-key change loses access to existing sealed
records, and an auth-map change can revoke every Managed2 client. Before and
after deployment, check unauthenticated `POST /v1/agents` returns 401, run an
authorized create/turn/replay with `./managed2-demo`, and verify the original
account `/api/health` still returns 200. Neither endpoint's success alone proves
model completion; check the turn status and streamed answer.

## Tool observability and sandbox-free tools

`GET /v1/agents/:id/turns/:turn` includes `tool_timing` for each provider call ID: name,
status, start/result offset, duration, and accumulated phase timings. These rows are
persisted in Session SQLite without arguments, results, file contents, or search queries.
The generic wrapper applies to every registered tool. `exec_command` lazily loads an
in-process Bash interpreter and a per-agent durable `/brain` VFS; its phases are
`setup`, `vfs_hydrate`, `execute`, `vfs_flush`, and outer `handler`. It does not
start a sandbox or offer an unmediated network. `web__run` uses Egress2's private
fixed search route and reports `preparation`, `egress_dispatch`, `parse`,
`egress_credential`, `egress_upstream`, `egress_parse`, and outer `handler`.
Nested spans are not additive (dispatch includes upstream).

The `clock: "io_gated"` marker matters: deployed Cloudflare Workers freeze
`performance.now()` and `Date.now()` during CPU-only work. Thus a `0ms` Bash
setup/VFS/execute phase is **unmeasurable**, not proof that it was free. I/O
spans such as search upstream are measurable. For CPU hotspots use workerd's
local CPU profile plus Cloudflare invocation CPU metrics; client-side tool-call
to-result elapsed includes event delivery and is not a pure handler duration.

## Regional subscription relay placement

Only new agents record a relay region derived from `request.cf.colo` at the
authenticated create endpoint. Caller-supplied region headers are ignored.
The selection persists in the Session SQLite row for alarm/reconnect traffic;
preexisting agents keep their legacy route. The region is only a Cloudflare
best-effort placement hint, not a residency guarantee. Egress2 picks a fresh
account-owned regional Container class and identity for ChatGPT subscription
calls; API-key calls remain direct. This is not a claim that OpenAI inference
occurs in the selected region or that provider response time will improve.

## Experimental asynchronous tools (disabled)

`POST /v1/agents` with `{ "async_tools": true }` returns HTTP 501
`typed_async_tool_ingestion_unavailable` before creating a Session. Ordinary
agents still use synchronous tools. The old pilot's synthetic *user turn* has
been removed; old rows are quarantined, never replayed as typed tool output.

The dormant `AsyncJobs` ledger persists a stable turn, original provider call
ID, job ID, arguments, correlation, and bounded result before dispatch. It
registers `current_time`, `web__run`, and the local Just Bash `exec_command`,
with eight active jobs and seven-day *delivered*-row retention. Results checkpointed without a model wake remain durable and visible but may consume capacity. The read-only
operations may be retried after a stale lease (at most three attempts). An
uncertain or cancelled shell execution **must not be rerun**: its terminal
result explicitly says the side effect may have happened. Cancel is a durable
fence, not rollback. The exact Unreal pending text is staged under the original
function-call ID; the job ID is available through the jobs status API and is
not appended to that provider-visible text. Managed2 currently exposes no
CUA or remote Hands tool; they are not covered by this ledger.

The private JS/WASM bridge stages the original-call pending output and
admits terminal output under that same call ID. The Rust driver can inject a
settled result at the original turn's next model-request boundary, or start a
prompt-less idle continuation when the turn has ended. Same-source idle
results are staged in a bounded cohort before one wake. Host SQL marks a job
`delivered` only after the exact native active/idle receipt identifies a
completed model step, not merely after checkpoint submission. A durable Rust
journal supports replay through transcript compaction and cold recovery.

**This is still not a usable production async mode.** The Worker E2E suite
(`test:async-e2e` and `test:async-e2e:websocket`) exercises synthetic Egress2
provider fixtures, including nonblocking pending/terminal, active-boundary
uptake, two-result idle coalescing, mixed completed/cancelled cohort fencing, and HTTP lost-SQL
completion replay after DO eviction. These do not establish real provider
acceptance of duplicate original-call-ID outputs, all crash/compaction
interleavings through the Worker, or production end-to-end performance. The
API stays 501 pending authorized live API/subscription HTTP/WS compatibility,
full correctness/performance review and green exact-head CI. A gated
three-route canary lives in `scripts/provider-canary`; its structural fixture
tests do not establish upstream acceptance.

A separate opt-in **synthetic-only** latency probe is available with
`pnpm run test:async-perf-fixture` (optionally
`MANAGED2_TEST_TRANSPORT=websocket`). It alternates three synchronous and
three asynchronous runs of a 2500-ms fixture search through both Workers and
records original-turn completion, host job `completed`/`checkpointed`/`delivered`
observations, and end-to-end terminal uptake. Do not interpret its fixture
model/tool timings as live provider performance or as a release gate.
