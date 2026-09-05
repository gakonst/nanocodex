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

The optional homepage demo sponsor is an ordinary Nanocodex account whose
ChatGPT connection remains in its own encrypted broker. After connecting that
account through the Account UI, configure its stable account ID on egress:

```sh
pnpm --filter nanocodex-egress-service exec wrangler secret put NANOCODEX_SPONSORED_CHATGPT_USER_ID --config wrangler.broker.jsonc
```

Egress uses that ChatGPT credential only when the requesting account has no
credential and the model subject is the exact 43-character browser identity.
The 64-character identity used by durable managed agents cannot fall back to
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
