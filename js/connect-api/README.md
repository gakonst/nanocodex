# Connect API

## Standalone Vault and phone services

An app requests `capabilities.services` with explicit scopes:

```ts
{
  vault: { ids: ["selected-vault-id"], origins: ["https://destination.example"], request: true },
  phone: { numberIds: ["selected-number-uuid"], read: true, provision: true, release: false }
}
```

The complete object is signed as one resource,
`urn:nanocodex:services:` followed by `encodeURIComponent(JSON.stringify(services))`.
The approval must also bind the app and exact app origin. A hosted service-only
approval omits agent resources and exchanges with `permission: "services.use"`.
It returns `grant.services` and omits `agent_id`; it provisions no managed agent,
conversation or session. Agent approvals may explicitly include the same service
resource. Existing grants gain no service authority, including through legacy
Vault egress or account-info. To change IDs or origins, request fresh approval.
The service resource is bounded to 12,288 characters; other resources retain the
512-character bound. Each scope accepts at most 64 unique IDs or origins.

All routes below start with `/v1/grants/:grantId/services` and require the opaque
grant bearer token, `X-Nanocodex-App-Id`, and exact signed `Origin`. Every request
resolves live grant state and rejects revoked or expired grants before contacting
a service. The private broker owner is derived from the grant; caller identity
headers are never forwarded. Responses are `no-store`.

| Method | Path | Required authority | Result |
| --- | --- | --- | --- |
| GET | (service root) | Current grant | Catalog containing only approved service types |
| GET | `/vault` | `vault.ids` | `{ vault }`, metadata only for selected IDs |
| POST | `/vault/request` | `vault.request`, selected ID and exact HTTPS origin | `{ status, ok }` only |
| GET | `/phone/numbers` | `phone.read` | `{ numbers }`, selected IDs only |
| GET | `/phone/numbers/:id` | `phone.read`, selected number | `{ number }` |
| GET | `/phone/numbers/:id/messages?limit=10&cursor=...` | `phone.read`, selected number | `{ messages, next_cursor? }` |
| GET | `/phone/numbers/available?country=US&area_code=415&limit=10` | `phone.provision` | Available SMS numbers |
| POST | `/phone/numbers` | `phone.provision` | Provisioning intent and concrete recurring quote |
| DELETE | `/phone/numbers/:id` | `phone.release`, selected number | Release intent |
| GET | `/phone/requests/:operationId` | Original intent's authority and grant | Intent status |

Vault requests accept `vault_id`, `url`, optional `method`, `headers`, `body`,
`body_encoding`, and `signing`. The credential broker validates templates and
injects secrets. A TOTP item supports `{{NANOCODEX_VAULT_TOTP}}` only at its saved
exact HTTPS origin, which must also appear in the grant. Seeds, codes, destination
response bodies and headers never pass back through Connect.

Provisioning takes `{ operation_id, phone_number, country: "US" }`; release takes
`{ operation_id }`. Both operation IDs are UUIDs. Connect derives a private broker
UUID from the grant and caller operation ID, and records the intent before
dispatch. Retry an uncertain submission only with the same original operation ID
and identical fields. Poll using the original caller UUID, also preserved in `request.operation_id`,
even if the submission response was lost. Use `request.approval_request_id` only
for the owner account UI; it identifies the private broker operation. Other grants
cannot poll it, even within the same app. Connect never automatically retries a
mutation. Intent creation does not buy or release a number: approval is available
only in the owner's authenticated account UI. Connect cannot call approval or
denial routes. A newly provisioned number needs a fresh signed ID scope before
an app may read its SMS. SMS compatibility depends on the destination service.

Run the public HTTP authorization journey with:

```sh
pnpm --filter @nanocodex/connect-api typecheck
node --experimental-strip-types --test js/connect-api/test/vaultRoutes.test.mjs
```

The journey runs the shipped Worker and real Durable Objects in workerd, using
synthetic account/provider services. It exercises service-only consent, filtered
metadata, exact item/origin bounds, phone intents, cross-app and cross-grant
isolation, revocation, legacy denial and uncertain responses without live spending.


## Fresh connector status for connected apps

`GET /v1/grants/:grantId/connectors?providers=spotify,soundcloud` returns:

- `account_id`, `agent_id`, and the current `grant` projection;
- `connectors`, containing only the requested API providers and their currently
  connected identities selected by this grant.

Send the grant's opaque bearer token, `x-nanocodex-app-id`, and the registered app
`Origin`, as with other grant routes. The endpoint validates the current grant,
expiry, app binding, and live connector identities on every request. Responses
are `no-store`; consumers must not cache the authorization decision.

`providers` is a required, comma-separated, non-duplicated list of API connector
capabilities. ChatGPT is excluded because its credential status lives in a
separate broker. This endpoint never reads Vault credentials, account balances,
or the account's authorization index. It returns no provider credentials or grant
bearer token. Use account-info when the full account summary is needed.

### Live output checkpoints

`GET /v1/grants/:grantId/agents/:agentId/checkpoints?turn_id=<id>&after=<revision>`
returns the newest complete intermediate output for one retained turn. It requires
`agent.output.final` plus either `agent.output.actions` or `agent.trace.read` on
an active grant. No new grant approval is needed. Cross-grant turns are not visible.

The agent writes immutable files under
`/brain/connect/<grantId>/outputs/<turnId>/checkpoints/r<N>/`, then atomically
writes `checkpoints/latest.json` last:

```json
{"revision":1,"files":[{"path":"r1/model.step","sha256":"<lowercase SHA-256>","size":123}]}
```

A manifest has 1–8 files, at most 1 MB each and 4 MB total. Names are bounded ASCII
basenames with no nested paths. The service verifies size and SHA-256 and atomically
retains one coherent bundle per turn. A partial, invalid, stale, or replayed revision
leaves the last validated bundle available. The JSON response includes `turn_id`,
`revision`, and each file's metadata plus `data_base64`. `204` means no validated
checkpoint yet; `304` means no revision newer than `after`. Responses are `no-store`.

Checkpoints survive observer disconnects and turn archival. They are intermediate
previews, not completion receipts. The final immutable artifact publication excludes
the `checkpoints` directory; agents must still publish the requested final outputs.
Session deletion removes retained snapshots and fences pending reads.

## App-scoped conversations

Apps can explicitly request `urn:nanocodex:agent:threads:app` in the signed
`capabilities.auth.resources` array and set
`capabilities.agent.conversationHistory: true`. Connect displays this permission
as **App conversations**. The resulting grant contains `agent.threads.app` and
`agent.history.read`. This cannot be combined with a single `conversationId`.
Existing grants do not acquire multi-thread authority from history or trace
access alone; they require a fresh approval with the new resource.

Send the captured grant bearer token, `X-Nanocodex-App-Id`, and exact approved
`Origin`, as for other grant routes. Under `/v1/grants/:grantId`:

| Method | Path | Body | Response |
| --- | --- | --- | --- |
| GET | `/threads` | — | `{ threads, next_cursor? }` |
| POST | `/threads` | `{ operation_id, title? }` | 201 `{ thread, connection }` |
| GET | `/threads/:threadId` | — | `{ thread, connection }` |
| PATCH | `/threads/:threadId` | `{ title }` | `{ thread }` |
| DELETE | `/threads/:threadId` | — | 204 |

A thread is `{ id, title, created_at, updated_at }`. Its ID is a UUIDv4;
timestamps are epoch milliseconds. Titles contain 1–200 characters and are
trimmed. A missing creation title becomes `New conversation`. Listing returns
at most 100 storage rows per page in ID order; follow `?cursor=next_cursor` until
omitted, including when a page contains no live rows. Deletion tombstones can
occupy a page. `updated_at` records creation, rename, or deletion, not message
activity. Apps may sort the complete list by that timestamp.

`connection` is the normal Connect wire response, preserving the grant ID and
bearer token while selecting `agent_id` and `grant.conversation_id`. The managed
agent ID is distinct from the thread UUID. A client using published SDK 0.6.6 can
call these endpoints through authenticated `client.fetch`, adapt the normal wire
response to `Connection`, and pass that selected connection to
`client.agent.create({ connection, tools })`. Capture that connection and its
token in each agent transport; a mutable global selection must never retarget
an existing agent instance. The initial grant's default agent is not an app
thread: list/open or create a thread before using the app-thread agent API.

The server chooses a persistent namespace from the exact app ID, exact origin,
broker account, and authenticating owner. Wallet addresses are case-normalized;
host owners include issuer, tenant and principal ID. Host sessions are validated
live, while new valid sessions for the same owner can reopen that owner's
threads. Callers cannot provide a namespace, owner, or agent ID when creating a
thread. Different linked wallets or host principals sharing a broker account
remain isolated. Existing private account conversations and legacy Connect
agents are not imported into this namespace.

Every agent relay and WebSocket ticket checks the selected agent's live thread
membership. Ticket redemption checks it again; ongoing grant socket checks
also revalidate membership. SSE relays check before their first event and at most
every five seconds thereafter, including idle streams, and close on token
revocation, expiry, host-session invalidation or deleted membership. Revoking/expiring the grant removes authority without
deleting its owner's conversations. A new explicitly approved grant in the same
scope can reopen conversation history, read agent state, and send new turns.
Reconnect the SDK tool host using the current grant and approved app-tool catalog
before sending those turns. The managed runtime binds each tool-host route to
its grant ID and catalog digest; a socket from the previous grant cannot serve
a new grant's turn. Existing managed-runtime
per-grant fences still apply to idempotent replay, steering/withdrawal receipts,
artifacts and checkpoints from turns issued by another grant; this endpoint does
not substitute an old grant's authority for the active one. Plain turn cancellation
uses the managed runtime's existing authorization behavior. Missing and foreign
IDs both return `404`.

Deletion revokes membership before managed-agent cleanup. A cleanup failure
returns `503 thread_delete_unavailable`; retrying the same DELETE is safe and
completes the cleanup. The hidden thread cannot be reopened or used meanwhile.
Creation requires a client-generated UUIDv4 `operation_id`, retained for the
intended creation until it resolves. Repeat the same ID and original title after
an uncertain result; the operation is reserved durably before provisioning. Only
the first reservation dispatches creation, with a scope-derived managed
idempotency key as an additional fence. A pending or unknown creation returns
`503 thread_creation_unresolved`; replays only check for a published receipt and
never repeat upstream provisioning. If the first dispatch cannot publish its
receipt, the operation remains fenced and requires operator reconciliation;
automatic retries cannot recover it by creating a new operation. Concurrent
retries can return this pending response until the first request publishes. A changed title for
the same operation returns `409 thread_operation_conflict`; retrying creation
after that thread was deleted returns `410 thread_deleted`. Use a fresh operation
only for a new intended thread. Replays return 201 with the same thread and the
current grant's connection wire.

Run `node --test test/appThreadsWorker.test.mjs` from this package (Node 24).
The journey starts an actual local workerd HTTP listener and real Durable Object
storage. Synthetic external identity and managed-agent providers exercise the
Connect trust boundary without live accounts or model calls. HTTP status traces
are emitted as test diagnostics; capture generated evidence under root `output/`.

## Remote MCP with Connect OAuth

The canonical account Worker forwards `/mcp`, `/.well-known/oauth-protected-resource/mcp`,
`/.well-known/oauth-authorization-server`, and `/oauth/*` to this Worker. The public
origin is preserved throughout discovery, consent, code redemption, and resource
requests. See [client setup and supported tools](../../docs/connect-mcp.md).

`oauthMcp.mts` owns public-client registration, exact redirect binding (with only
RFC 8252's native loopback port exception), S256 PKCE, one-time authorization
codes, resource-bound access tokens, rotating refresh tokens, and RFC 7009
revocation. Registration metadata grants no account authority. The existing
hosted account authorization service must exchange a user-approved code whose
resources include this exact pending request. The user can select a nonempty
subset of requested scopes; the signed resources and issued scope must match.

MCP OAuth tokens are separate from internal Connect grant credentials. Every
resource call resolves the token family and current grant; revocation, expiry,
app/owner binding, and approved capabilities remain live checks. Refresh-token
reuse fences the entire family before revoking its underlying grant. MCP tool
adapters construct managed assertions internally and reuse the connector broker's
provider and selected-identity enforcement. They never forward caller-supplied
internal authorization headers or return the underlying Connect token.

`mcpServer.mts` implements stateless JSON Streamable HTTP for MCP 2025-03-26,
2025-06-18, 2025-11-25, and the MCP released revision 2026-07-28. Legacy clients
continue to use `initialize`. MCP2 clients use `server/discover` and send these
fields on every request:

```http
POST /mcp
Authorization: Bearer <OAuth access token>
Content-Type: application/json
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: events/list
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "events/list",
  "params": {
    "_meta": {
      "io.modelcontextprotocol/protocolVersion": "2026-07-28",
      "io.modelcontextprotocol/clientCapabilities": {}
    }
  }
}
```

`tools/call` additionally requires `Mcp-Name` matching the tool name. MCP2
responses include `resultType: "complete"` and server metadata in
`_meta["io.modelcontextprotocol/serverInfo"]`. Header/body mismatches return
HTTP 400 with JSON-RPC `-32020`; unsupported revisions return HTTP 400 with
`-32022`; unknown MCP2 methods return HTTP 404 with `-32601`. Requests are
stateless; notifications return 202 and GET/DELETE return 405.

### MCP Events

The event catalog and advertised capability reflect the current OAuth grant.
`agent:run` with the approved model capability exposes `agent.turn.completed`;
other grants receive an empty `events/list` catalog. Events observe only turns
started through this MCP grant, on its approved agent. Completion includes
`completed`, `failed`, and `cancelled` terminal states. A subscription starts
future observation; it does not replay historical completions. `cursor` is
currently null and `truncated` is false.

Use `events/subscribe` with the metadata above and these additional parameters:

```json
{
  "name": "agent.turn.completed",
  "arguments": { "turn_id": "<optional exact turn ID>" },
  "delivery": {
    "mode": "webhook",
    "url": "https://callbacks.example.com/nanocodex",
    "secret": "whsec_<base64 encoding of 24–64 random bytes>"
  },
  "ttlMs": 3600000
}
```

Omit `arguments.turn_id` to observe all MCP-started turns for this grant. The
response contains `{ id, refreshBefore, cursor: null, truncated: false }`.
Repeat the same event name, normalized URL and arguments to refresh the same
subscription. OAuth access-token rotation retains its identity. Refresh before
`refreshBefore`; the default lifetime is one hour, requested lifetimes are
bounded to one second through 24 hours, and grant expiry remains an upper bound.
A changed secret rotates the signing key with a five-minute overlap in which
callbacks carry both signatures.

Before accepting a new callback destination, the server POSTs a signed
`{ "type": "verification", "challenge": "..." }` body. The receiver must return
2xx with `{ "challenge": "<same value>" }` within ten seconds. Verification
responses are limited to 4 KiB. Successful checks are cached briefly for the
same grant and URL; failures return `CallbackEndpointError` (`-32015`) and do
not create the subscription. Receivers should verify signatures on verification
requests as well as events.

Callbacks use Standard Webhooks headers `webhook-id`, `webhook-timestamp` and
`webhook-signature`, plus `X-MCP-Subscription-Id`. Decode the secret after
`whsec_` as base64. Compute HMAC-SHA256 over the exact bytes
`<webhook-id>.<webhook-timestamp>.<raw request body>` and compare a base64
signature from the space-separated `v1,<signature>` values in constant time.
Validate timestamp freshness and deduplicate by `webhook-id`. Neither OAuth
access/refresh tokens nor internal Connect credentials are sent to callbacks.

A delivery has this shape:

```json
{
  "eventId": "evt_<stable event ID>",
  "name": "agent.turn.completed",
  "timestamp": "2026-10-06T12:00:00.000Z",
  "data": {
    "agent_id": "<approved agent ID>",
    "turn_id": "<MCP operation ID>",
    "status": "completed",
    "completed_at": "2026-10-06T12:00:00.000Z"
  },
  "cursor": null
}
```

Read full output through `nanocodex_agent_status`. The callback is a completion
notification; it grants no additional account authority.

`events/unsubscribe` takes the same `name`, `arguments` and `delivery.url`
(`delivery.mode` may be `webhook`), with no secret. It is idempotent and returns
an empty result. Missing and already expired subscriptions can be removed safely.
Subscription IDs and signing secrets never grant permission to another principal.

`McpEvents` is a separate SQLite Durable Object bound as `MCP_EVENTS`. Real
alarms poll retained turn status and process a durable outbox after the caller
disconnects. A successful callback acknowledges an event. Transient errors use
bounded retries with stable event IDs and payloads; HTTP 410 and 413 discard
that event while preserving the subscription for subsequent events. Retries can duplicate a delivery, so receivers must deduplicate. Exhausted
retry budgets can lose events; this event type does not support replay. Live
OAuth-family and Connect-grant checks fence both polling and delivery after
revocation. Expired subscriptions stop delivery automatically.

The callback transport accepts public HTTPS DNS names only, without credentials,
fragments, IP literals, or private host suffixes. It sends one POST using global
`fetch()` with manual redirects, an allowlist of webhook headers, a 64 KiB request
limit, a 4 KiB verification response limit, and a 10-second deadline. Delivery
responses are acknowledged by status without buffering their bodies. TLS hostname
verification and public DNS routing are enforced by the runtime.

Both production and development explicitly enable
[`global_fetch_strictly_public`](https://developers.cloudflare.com/workers/configuration/compatibility-flags/#global-fetch-strictly-public).
This is a security requirement: requests to the Worker's own zone pass through
Cloudflare's public front door, avoiding the legacy origin bypass described in
Cloudflare's [bindings security explanation](https://blog.cloudflare.com/workers-environment-live-object-bindings/).
Webhook delivery never uses an origin or service binding, nor caller-provided
routing metadata. Cloudflare-proxied HTTPS receivers use the same public fetch
path as other receivers. Standalone workerd deployments must retain a public-only
`globalOutbound` network; its [runtime contract](https://github.com/cloudflare/workerd/blob/main/src/workerd/io/compatibility-date.capnp)
rejects DNS destinations resolving to private addresses. The compatibility flag
controls Cloudflare routing and does not change standalone workerd networking.

Run from the repository root under Node 24 with workspace dependencies installed:

```sh
node --test js/connect-api/test/mcpServerWorker.test.mjs
node --test js/connect-api/test/mcpEventsWorker.test.mjs
```

The event journey requires Linux with `unshare`, `mount`, `umount`, `ip` and
`openssl` (the `util-linux`, `iproute2` and `openssl` packages on Ubuntu), and
permission to create unprivileged user, network and mount namespaces. Its runner
creates these namespaces automatically and fails explicitly if unavailable.
The fixture binds synthetic public and private IPs and a DNS hosts mapping only
inside those namespaces; the host network and DNS configuration stay unchanged.
The dedicated MCP CI workflow prepares these permissions on its disposable
Ubuntu runner and publishes the journey log.

Both journeys run the shipped Worker on an actual workerd listener and real
SQLite Durable Object storage. The legacy journey also uses the official MCP
JavaScript client. The event journey exercises public OAuth/MCP requests,
signed verification and callbacks, actual turn polling alarms, token/key
rotation, retry identity, exact-turn filtering across restart, recovery from
invalid source timestamps, loopback/RFC1918/metadata DNS and wrong TLS hostname
rejection, redirect refusal, oversized and stalled verification response rejection,
unsubscribe, expiry and revocation. Only external
account, DNS and callback services are synthetic fixtures. Diagnostics provide
HTTP, managed polling and callback evidence; capture generated evidence in
ignored `output/` or CI artifacts.

## MACH wallet funding

The account site's `/v1/machine-usd/config` and `/v1/machine-usd/orders` routes
use MACH's private `OnrampApi` contract: `GET /v1/config`, `POST /v1/orders`,
and `GET /v1/orders/ord_<32 lowercase hex digits>`. Mercator's public
`/v1/onramp` routes are not an onramp transport.

Configure one transport on the Connect Worker, including each preview environment
that should support funding:

- In the MACH Worker's Cloudflare account, add an optional `MACH_ONRAMP` service
  binding to the MACH Worker with `entrypoint: "OnrampApi"`.
- Across Cloudflare accounts, use MACH's authenticated onramp relay. Configure
  `MACH_ONRAMP_RELAY_URL` as its fixed HTTPS origin (no path, query, or userinfo)
  and provision the same secret of at least 32 characters as
  `MACH_ONRAMP_RELAY_TOKEN` on both Workers using secret management. The relay
  binds to `OnrampApi` inside MACH's account. See
  [MACH relay setup](https://github.com/tempoxyz/mach/blob/main/docs/nanocodex-onramp-relay.md).

Production operators use the manual **Configure MACH onramp relay** workflow
(`mach-onramp-config.yml`) on `master`. In the existing `cloudflare-production`
environment, securely provision `MACH_ONRAMP_RELAY_TOKEN` with the same value as
the issuer relay, and set the nonsecret variable `MACH_ONRAMP_RELAY_HOST` to the
exact `workers.dev` hostname from the verified issuer deployment receipt. The
workflow reuses `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`; credentials
must never be supplied as workflow inputs, command arguments or files.

Dispatch with `action=configure` and `relay_origin=https://<verified-host>`
(without a trailing slash). Before writing, the operator checks the account's
`gakonst` subdomain, the existing `nanocodex-connect-api` Worker and its local
`CONNECT_STATE` / `ConnectNonceStorage` binding, anonymous relay
rejection, and authenticated canonical MACH configuration. It writes only
`MACH_ONRAMP_RELAY_TOKEN` and `MACH_ONRAMP_RELAY_URL` as `secret_text` bindings,
then checks the fixed public Connect config endpoint. Secret writes accept HTTP
200 or 201 only with a successful response envelope and the matching binding
name and `secret_text` type. The URL is intentionally
stored as a secret binding so normal Wrangler deployments preserve it along
with the token ([Cloudflare configuration](https://developers.cloudflare.com/workers/wrangler/configuration/#source-of-truth));
do not duplicate either binding in Wrangler `vars` or a regular
deployment's secret payload. Configuration shares the production deployment
concurrency group and does not build or redeploy source.

The workflow reports HTTP status and bounded result codes only. It never creates
orders or payments. Writes are sequential, not atomic, and never automatically
retried. After an interrupted or unconfirmed write, dispatch `action=verify`
first: this checks relay and public configuration without writing. A failed
verification does not prove a write failed; inspect Cloudflare deployment and
secret-name metadata without reading values before explicitly repairing a
partial configuration.

For an applied token write with no origin binding, explicitly dispatch
`action=configure-origin` with the same verified `relay_origin`. This recovery
checks Worker settings metadata for an existing `MACH_ONRAMP_RELAY_TOKEN`
`secret_text` binding and requires `MACH_ONRAMP_RELAY_URL` to be absent. It then
checks anonymous rejection and authenticated canonical relay configuration,
writes only the origin binding once, and verifies public config. It never reads
secret values or rewrites the token. Metadata confirms the token binding's
presence and type; successful public verification confirms the configured
transport works. If any origin binding already exists, recovery refuses to
write: use `verify`. Missing or incorrectly typed token bindings also stop
before any write. An uncertain origin write must be reconciled through metadata
and `verify`, never blindly repeated.

A public config check can also fail during propagation;
use `verify` again instead of repeating writes. Successful verification means
authentication is required and public config enables canonical MACH on chain
4217, token `0x20c000000000000000000000f37de3740adec032`, bounds 500–10000
USD cents, and a `pk_live_` publishable key; it does not prove checkout or
issuance works. The issuer omits `onramp_enabled`; the public SDK normalizes its
absence to `onrampEnabled: true`. An explicit disabled or malformed flag fails
verification.

Run the synthetic HTTP operator journeys with
`node --test scripts/cloudflare/configure-mach-onramp.test.mjs` from the repository
root. No production credentials are needed.

The private binding takes precedence. An absent or invalid transport returns
`503 machine_usd_unavailable`; there is no public-endpoint fallback. Adding a
binding name without provisioning its target does not enable funding. Relay
credentials never reach the browser, and account cookies never reach MACH.
Only the three config/order routes cross the relay; redirects are rejected.
A transport failure can mean order creation succeeded. Retain the exact body,
capability and idempotency key when retrying.

The account proxy resolves the persistent account's Worker-owned wallet through
`/v1/me`, overwrites client-supplied recipients, requires the same browser Origin
for creates, and scopes idempotency keys to the wallet. Status requires both the
account session and order capability. Responses must match the wallet, amount,
order ID and canonical MACH amount. Completion requires fulfilled issuance and
a transaction hash, not payment success alone.

The UI retains its account-bound intent in session storage before dispatch,
opens Stripe through an explicit link in a new tab, and polls on the original
Nanocodex page. Reloading or a temporary failure can be recovered by selecting
Add funds again: the existing order is checked and identical create inputs are
replayed when necessary. Closing checkout does not complete or cancel an order.
Polling stops after 15 minutes and retains the intent for recovery. A fulfilled
order refreshes the wallet balance. Hosted checkout's return destination is
controlled by MACH; arbitrary browser return URLs are not forwarded.

Validation (Node 24, installed workspace dependencies):

```sh
node --experimental-strip-types --test js/connect-api/test/machFunding.test.mjs
MACH_RELAY_SOURCE=/absolute/path/to/mach/src/onramp-relay.ts \
  node --experimental-strip-types --test js/connect-api/test/machFunding.test.mjs
WALLET_BROWSER_CHANNEL=chrome node js/account/scripts/wallet-smoke.mjs
```

The HTTP journey bundles the shipped Connect Worker and runs the account handler
over local HTTP, with synthetic account authentication and an external MACH
provider fixture. `MACH_RELAY_SOURCE` adds the actual companion relay to that
journey. The browser journey exercises the production funding hook and card with
synthetic HTTP responses on desktop and mobile. Neither journey charges a card
or deploys a Worker. Logs and browser screenshots belong under ignored `output/`.
