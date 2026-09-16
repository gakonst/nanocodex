# Nanocodex egress

This package contains the private Cloudflare credential broker and a small
service-binding example agent. Operational policy, deployment order, and
production evidence live in [../../AGENTS.md](../../AGENTS.md).

## Entrypoints

- `wrangler.broker.jsonc` deploys `src/egress.ts` as `nanocodex-egress`.
  It has `workers_dev = false` and no public routes; managed services reach it
  only through a Service Binding. Its named `ChiefOfStaffEgress` RPC can only
  idempotently install the separately configured Chief credential for a
  managed, server-generated Chief user ID.
- `wrangler.agent.jsonc` deploys `src/agent.ts` as the public
  `nanocodex-egress-agent-example`. Its `EGRESS` binding demonstrates the
  private call shape; it is not the broker or a production control surface.

The package scripts expose broker and example-agent dry runs and deployments
(`dry-run:broker`, `deploy:broker`, `dry-run:agent`, and `deploy:agent`). Use
the repository operator interface in `../../AGENTS.md` for actual operation.

Subscription voice can use `CHATGPT_VOICE_RELAY_RPC=true` to transfer the small
SDP request and answer through the private `ChatGptEgress.createRealtimeCall`
RPC. Deploy the account Worker's compatible relay method before enabling this
flag. Without the flag, egress uses the existing HTTP transport. Credential
resolution, regional placement, and provider rejection handling are shared;
an interrupted RPC is never retried through HTTP because the provider may have
already created the call. Other model traffic keeps its existing transport.
For a bounded comparison on one deployed version, `sample` selects RPC for
even-ending voice session UUIDs and HTTP for odd-ending UUIDs. Egress timing logs
include the voice session ID and selected transport so samples can be matched.

## Credential boundary

The broker owns per-user provider credentials, connector OAuth state, MCP
connection material, brokered SSH private keys, and each persistent account's
secp256k1 root wallet. Durable Objects encrypt that state with AES-256-GCM
before storage. Production requires
`CREDENTIAL_ENCRYPTION_KEY`; a static Secrets Store binding can supply it, and
`CREDENTIAL_ENCRYPTION_KEY_PREVIOUS` supports key rotation.

Chief of Staff deployments also require `CHIEF_OF_STAFF_OPENAI_API_KEY` on
this broker. The named RPC copies it directly into the generated user's
encrypted credential vault; the value is never returned to managed or Chief.

ChatGPT credentials form a per-user encrypted pool of up to 20 distinct account
IDs. Repeated CLI imports add accounts; importing a live account again retains
its rotating refresh token and subscription cooldown. Existing single-account
state remains readable. Device login also adds accounts. Token refresh and
refresh backoff are tracked independently for each account.

For user-owned credentials, egress reports explicit subscription exhaustion
(`usage_limit_reached`, `usage_limit_exceeded`, or `insufficient_quota`) to the
broker. The broker serializes selection and persists account cooldowns, fenced
by credential revision. HTTP rejections can be retried once per eligible account.
The Responses WebSocket forwards a retryable error after a successful switch,
using the SDK's existing reconnect/full-history recovery so provider checkpoint
IDs do not cross accounts. Once output has started, the original error is kept.
Generic 429s, authorization denials, and sponsored quota do not trigger this
pool failover. Public credential status includes account IDs, active/connected
flags, and `limited_until` timestamps, never tokens. ChatGPT disconnect removes
the complete pool.

The optional homepage demo sponsor is an ordinary Nanocodex account whose
ChatGPT connection remains in its own encrypted broker. After connecting that
account through the Account UI, configure its stable account ID on egress:

```sh
pnpm --filter nanocodex-egress-service exec wrangler secret put NANOCODEX_SPONSORED_CHATGPT_USER_ID --config wrangler.broker.jsonc
```

Egress uses that ChatGPT credential only when the requesting account has no
credential and the model subject is the exact 43-character browser identity.
Neither legacy 64-character identities nor versioned `managed-session-v1_`
identities used by durable managed agents can fall back to
the sponsor. User-connected ChatGPT or OpenAI credentials always take
precedence, and no sponsor token or account identifier is returned to callers.
Each SMS account may reserve exactly three sponsored root prompt IDs. The
per-account Durable Object serializes reservations, owns a heartbeat-renewed
attempt lease, permits at most one retry for an interrupted or orphaned root,
rejects completed replays, retains
single-use provider-issued tool and tool-search continuations across SDK
full-history reconnects, and rejects a fourth prompt before its generation
frame reaches OpenAI. Egress also forces sponsored frames to Luna, no thinking,
and standard service.

Credentials and encryption keys never enter browser code, managed Workers,
agent configuration, tool output, or status/control responses. The managed
account authenticates its own control request and supplies the resolved user
path; browser callers do not select users, subjects, upstreams, or credentials.
The development-only ChatGPT bootstrap claim is enabled only by the explicit
development/test environment and `ALLOW_LOCAL_CREDENTIAL_CLAIM=true`.

The wallet is generated idempotently in the existing per-user
`UserCredentialBroker` after the managed Worker confirms a successful OTP login.
Its private key is sealed in the same user-scoped credential envelope and never
leaves egress. The only wallet signing operations are exact `wallet_connect`
access-key authorization and `wallet_revokeAccessKey`; there is no generic
signing, transaction, import, or export surface. Public callers receive only
the address and sanitized signed operation results.

This is custodial server-side encryption, not user-held end-to-end encryption:
egress can decrypt the key in Worker memory and the deployment encryption key
is part of the trust boundary. See
[persistent account wallet custody](../../docs/WALLET_CUSTODY.md) for the
lifecycle, allowed operations, and migration boundary.

## Direct, fail-closed egress

`AgentSubjectDirectory` maps each opaque subject directly to one user. Binding,
unbinding, and resolution are private control operations; tombstones prevent a
deleted subject from being rebound. Managed code retains the subject, never a
credential or credential selector.

Session-owned subjects use `managed-session-v1_<Session DO id>` and
resolve through the optional `MANAGED_AGENT_OWNERSHIP` service binding to
`nanocodex-durable-agent`'s private `ManagedAgentOwnership` entrypoint.
They cannot be bound or unbound through `/subjects`; the Session's retained
ownership and deletion tombstone are authoritative. Missing bindings, malformed
subjects, and denied resolutions fail closed without directory fallback.
Legacy managed and browser subjects keep the directory path. See the
[managed bootstrap and rollback instructions](../managed/README.md#session-owned-credential-subjects)
before enabling the binding and new-session strategy in production.

Managed Sessions may use the private `SessionModelEgress` entrypoint for the
fixed Responses WebSocket route after validating their own retained ownership.
This avoids calling back into the originating Session. Only that dedicated
service binding accepts the Session owner assertion; the general broker rejects
it. Model credentials are still resolved live. Tool, connector, voice, and legacy
directory traffic keep their existing ownership checks. Deploy egress before
enabling the managed binding.

The internal `GET /users/:user/credentials/vault` control route returns only
the existing public vault metadata projection. Managed startup uses it instead
of full credential status, which can also derive legacy SSH public keys.

The internal `GET /users/:user/catalog` route reads public connector and MCP
metadata together from their account-owned broker. It never includes MCP
endpoints or credentials. Deploy egress before managed consumers of this route.
Model connection logs include `credential_broker_ms`, measured inside the exact
RPC that returned the snapshot; `credential_ms` also includes time outside that
method, such as routing and object activation. Activation duration and the
broker's age since activation are returned with the same result, so a cold
object can be distinguished from a slow call to an already-active object.
`credential_broker_resolve_id` joins that connection to the broker's
`egress.credential.rpc` log and Cloudflare invocation wall/CPU timings. Internal
zero-duration timers alone do not establish zero elapsed work. Restoring a
credential broker preserves its persisted alarm, repairing it only when missing;
activation does not rewrite an already scheduled refresh alarm.
Credential RPC calls the credential operation directly, without constructing or
parsing local HTTP bodies. It shares the HTTP path's serialized mutation queue
and durable-state recovery after failures.
`upstream_ms` includes our
subscription relay and must not be interpreted as provider-only latency.

Model traffic accepts only the fixed internal URLs, methods, headers, and
credential placeholder. The broker resolves the subject, chooses that user's
active credential, injects it only for the approved upstream or configured
relay, and strips sensitive response headers. It rejects caller-selected
destinations, provider headers, redirects, and malformed WebSocket handshakes.

Connector, MCP, and SSH egress use the same subject boundary. Connector and
MCP requests are allowlisted and owner-checked. SSH accepts an opaque identity
reference and exact target, keeps the private key in the broker, verifies the
stored host fingerprint, and returns bounded command results.

OAuth connections use opaque 43-character base64url IDs and bounded labels.
Provider calls select an identity with
`X-Nanocodex-Connector-Connection`; a selector is required when more than one
eligible identity exists. The private control routes are:

- `GET /users/:user/connectors` for capability-projected status.
- `POST /users/:user/connectors/:provider` and the corresponding `/callback`
  route, where provider is `github`, `google`, `slack`, or `x`.
- `DELETE /users/:user/connectors/:provider/connections/:connectionId` for one
  exact grant.

Google is a control-only provider. One Google identity is projected under each
scope actually granted: `gmail`, `gdrive`, `gcalendar`, `gtasks`, `gdocs`,
`gsheets`, `gslides`, and read-only `gcontacts`. The legacy `gmail` and
`gdrive` control aliases remain readable during migration. Connector state and
all access/refresh tokens remain encrypted in the per-user credential vault;
status exposes only connection ID, label, account ID, and capability names.

Before enabling Slack in production, configure `SLACK_OAUTH_CLIENT_ID` and
`SLACK_OAUTH_CLIENT_SECRET` as secrets on the `nanocodex-egress` broker (never
on an application or managed Worker). The Slack app must register the canonical
`/v1/connectors/slack/callback` URL for the deployed Nanocodex origin and allow
the user scopes requested in `src/connectors/slack.ts`. Local development uses
the Vite-owned loopback relay instead of the production callback.

## Checks

`typecheck` and `test` cover this package. For a changed Worker boundary,
exercise the deployed flow and inspect browser/network, Worker logs, bindings,
and credential absence as required by `../../AGENTS.md`.

### Manual API keys

The account Vault supports `api_key` entries alongside username/password logins.
Create one through `POST /v1/credentials/vault/api_key` with
`{ "name": "Service", "api_key": "<key>" }` using the account's authenticated,
same-origin session. Delete it with `DELETE /v1/credentials/vault/api_key/:id`.
List/account-info responses contain only `id`, `kind`, `name`, and `created_at`;
the key is stored in the encrypted per-entry envelope.

For a brokered request, supply the entry ID in `x-nanocodex-vault-id` and use
`Authorization: Bearer {{NANOCODEX_VAULT_API_KEY}}` or a custom header such as
`x-api-key: {{NANOCODEX_VAULT_API_KEY}}`. The broker substitutes the key only at
the final fetch. Existing destination policy and status-only responses apply;
API-key entries cannot satisfy login/password placeholders.

The native iPhone Spotify flow uses ncspot's public PKCE registration and its
fixed `http://127.0.0.1:8989/login` redirect. It does not require
`SPOTIFY_OAUTH_CLIENT_ID`; the broker stores the registration with each connection
so refresh cannot accidentally use a different client. The phone relays only the
one-time code and matching state through an owner-authenticated managed route.
It never accepts arbitrary client IDs or callback URLs. Shared-client quotas and
Spotify endpoint restrictions still apply.

Spotify reads from agents, direct Connect API calls, and provider SDKs pass through
one user broker. Identical successful JSON reads for the same connection and
request headers reuse a result for one second (at most eight entries of 1 MiB
per user). Writes and credential changes clear these reads; account selection and
credential validity are checked before reuse. Large responses still stream in full.

A `SpotifyRateLimit` Durable Object per OAuth client ID remembers `Retry-After`
plus a one-second margin across every Nanocodex user of that registration. The
shared object stores timing only. Cooldowns survive eviction and deployment;
accounts and response bodies remain isolated in their user brokers. Reads retry
at most twice with a ten-second cooldown-wait budget. Longer cooldowns return 429
and the remaining `Retry-After` without contacting Spotify. Writes are never
queued or retried, and are rejected locally during an active cooldown. A 429
without valid timing uses a conservative 30-second cooldown plus the margin.
OAuth identity reads share the same policy; code exchanges are never retried.
This coordinates our own traffic, but cannot manage other apps using ncspot's
registration or increase Spotify's quota.


### SoundCloud app registration

SoundCloud uses Authorization Code + PKCE with a confidential app registration.
Configure `SOUNDCLOUD_OAUTH_CLIENT_ID` and `SOUNDCLOUD_OAUTH_CLIENT_SECRET` as
broker secrets. Keep the secret in the broker; neither the phone nor the agent
receives it. The phone flow uses the registration's fixed
`http://127.0.0.1:8788/callback` redirect. The foreground iPhone listens there and
forwards only the one-time code and state to the owner-authenticated broker route.
A separately configured hosted flow can use
`https://nanocodex.gakonst.workers.dev/v1/connectors/soundcloud/callback`.

The [official registration CLI](https://developers.soundcloud.com/docs/api/register-app)
supports `--remote` phone pairing and returns an existing registration when one
already exists. Registration currently requires Artist Pro. Its bundled public
client is scoped to app registration; it is not a general SoundCloud connector.
The Rust [soundcloud-tui](https://github.com/7ito/soundcloud-tui) likewise requires
an app's own credentials.

After registration, **Connect SoundCloud** opens Nanocodex on the phone and
presents the provider consent page using its mobile `display=popup` layout. The broker exchanges the code with PKCE, stores
encrypted user tokens, and rotates the single-use refresh token. Verify the
connection with `soundcloud_request` reads of `/me` and `/me/playlists` before
reporting it connected. OAuth transport tests use fixtures and do not replace
this authenticated production check.

Managed sessions may retain `configuration.chatgpt_account_id` at creation. Their
private egress uses `x-nanocodex-chatgpt-account-id` to select that account from the
owner's pool, without changing its preferred account. This pin disables failover
and sponsored fallback; missing accounts fail closed and quota exhaustion returns
`chatgpt_account_exhausted`. The selector is stripped before provider forwarding.
