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
the existing managed API. The selected subscription path reuses the existing
account-owned Linux `ChatGptEgress` relay/container. Managed2 uses a persistent
Responses WebSocket (`RESPONSES_TRANSPORT=websocket`); Egress2 has no `GATEWAY`
VPC binding in its checked-in deployment config.

```text
Client → Managed2 API → Session DO → Egress2 → ChatGptEgress relay/container → ChatGPT
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
turn. These samples do not prove a general latency improvement. This initial
Managed2 service is deliberately configured with `tools: []`; adding tools
requires its own end-to-end validation.

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
`result_ms`. The model-send observation runs just after the persistent socket
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
