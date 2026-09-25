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

`Server-Timing` on create and turn admission exposes `auth`, `session`,
`agent_init`, and `admission` durations. The private Egress2 Responses reply
adds `egress_credential`, `egress_dispatch`, `egress_upstream_headers`,
`egress_total`, fixed `egress_route`, and cache hit/miss; its structured logs
contain only route/status/durations. The Session emits a `managed2.model_route`
log with time from prompt to Egress fetch and time until upstream headers.
None of these logs contain keys, owners, prompts, or response text. Use
`wrangler tail nanocodex-managed2` and `wrangler tail nanocodex-egress2`
when diagnosing a turn. `0.0 ms` means below the runtime timer resolution,
not mathematically zero. HTTP header timing stops before model generation.

For lowest client-visible latency, open `/v1/agents/:id/events?cursor=0`
**before** posting the turn and render `assistant.delta` as it arrives;
polling turn status adds network and polling-delay overhead. With the current
subscription, the outbound destination is ChatGPT Codex via the existing
Linux relay, not the direct `api.openai.com` API-key endpoint.

Cloudflare Workers Logs are persisted at 100% sampling for this greenfield Worker.
Open its **Observability → Logs** tab in Cloudflare to search for
`managed2.model_route` or `responses_egress`; both are structured objects
with fixed labels and timing fields. Live `wrangler tail` is separate from
the persisted dashboard. Workers tracing is enabled at 100% sampling with
retention in Cloudflare Observability. The trace follows service bindings and
Durable Object calls across Managed2, Egress2, and the account-owned
`ChatGptEgress` Container DO when that Worker also has tracing enabled.
The external Linux container and ChatGPT provider do not propagate Cloudflare
trace context; inspect the Container DO and outbound-fetch spans plus Egress2
header timings there. A create request, turn request, and an alarm-resumed
execution can have distinct root traces. Search by the turn request CF-Ray;
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
