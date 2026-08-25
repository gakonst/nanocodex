# Nanocodex per-user credential broker

This private ordinary Cloudflare Worker owns provider credentials for managed
Nanocodex users. It has `workers_dev = false`, no routes, and is reachable only
through a Service Binding. It does not use Workers for Platforms.

Each local Nanocodex user ID selects one `UserCredentialBroker` Durable Object.
An `AgentSubjectDirectory` stores only the mapping from a hidden opaque Durable
Object subject to that user ID. Agent and room code retain the subject, never a
provider credential or credential selector.

## Private managed contract

These routes are Service-Binding-only. `:subject` is the raw opaque agent DO ID
and `:user` is the user ID resolved from the authenticated Nanocodex session by
managed. Neither value is accepted from a public browser request.

| Method and path | Request | Successful response |
| --- | --- | --- |
| `PUT /subjects/:subject` | `{ "user_id": "..." }` | `200 {"status":"bound"}` or idempotent `unchanged`; `409` if owned by another user |
| `DELETE /subjects/:subject` | `{ "user_id": "..." }` | `204`; `409` on owner mismatch |
| `GET /users/:user/credentials` | none | secret-free status |
| `PUT /users/:user/credentials/openai` | `{ "api_key": "..." }` | `204` |
| `DELETE /users/:user/credentials/openai` | none | `204` |
| `POST /users/:user/credentials/chatgpt/login` | none | pending device-login status |
| `POST /users/:user/credentials/chatgpt/login/status` | none | pending/authenticated/expired status; polling and token exchange stay server-side |
| `DELETE /users/:user/credentials/chatgpt` | none | `204` |
| `POST /users/:user/credentials/chatgpt/local-claim` | none | secret-free status; development only |

The local claim is enabled only when `ENVIRONMENT` is `local`, `development`,
or `test` and `ALLOW_LOCAL_CREDENTIAL_CLAIM=true`. It consumes
`LOCAL_CHATGPT_BOOTSTRAP` from the broker environment, accepts no provider
material in the request, and the subject directory permits one claiming user.
Production returns `404` even if the endpoint is called.
When this local-claim profile is enabled, starting an interactive ChatGPT
device login fails with `409 local_credential_claim_required`.

## Model egress

The managed runtime sends the exact hidden `x-nanocodex-subject` header and the
literal `Authorization: Bearer NANOCODEX_PROVIDER_CREDENTIAL` placeholder.
All model operations target `https://nanocodex.internal/v1/...`; callers cannot
select OpenAI versus ChatGPT or an upstream URL. The broker resolves subject to
user, selects that user's active credential, strips the subject, injects the
credential, and forwards only allowlisted headers to one exact provider URL.

Exact supported paths are `GET /v1/responses` with the required Responses
WebSocket beta/upgrade headers, plus JSON `POST /v1/search`,
`/v1/images/generations`, and `/v1/images/edits`. Queries, redirects, provider
headers, incorrect placeholders, other methods, paths, hosts, schemes, and
ports fail closed. The fixed ChatGPT relay configuration remains supported for
the environments where Cloudflare-to-ChatGPT WebSockets require it.

The approved OpenAI endpoint and configured terminating relay are trusted
credential recipients. Normal HTTP response headers are stripped of known
credential/cookie fields, but a WebSocket peer necessarily controls its frames;
bind the broker only to the owned managed Worker and use only an audited relay
that cannot reflect injected credentials.

### Direct subject ownership and cutover

The broker routes a subject directly to the existing
`agent-subject-v1:<subject>` `AgentSubjectDirectory` Durable Object. That object
accepts only its name-matching subject and atomically owns one state machine:
absent, bound to one user, or permanently tombstoned for that user. Same-owner
bind/unbind is idempotent, a foreign owner conflicts, and no operation can turn
a tombstone back into a binding. An unbind of an absent subject writes the
tombstone so a delayed bind whose response was lost cannot resurrect a failed
agent creation.

There is no production request path through the old `agent-subjects-v1`
singleton. Keep the deployed class name, `AGENT_SUBJECTS` binding, v2 SQLite
migration, and exact shard prefix stable: changing any of them abandons the
active state. The singleton storage remains orphaned as forensic evidence.

This cutover is roll-forward-only. The pre-cutover reconciler can interpret
shard-only state as deletion and can overwrite a shard tombstone from stale
legacy state, so it is not a safe rollback target and must never receive a
gradual version split with the direct router. Deploy direct-routing broker
versions at 100%, preserve the named shard namespace, and repair forward.
Dormant pre-sharding agents remain recoverable because their AgentDO retains
the authoritative owner and idempotently binds its direct shard before model
use; deletion similarly creates the permanent shard tombstone.

All API keys, ChatGPT access/refresh state, device-login state, connector
access/refresh tokens, PKCE verifiers, OAuth state, and refresh markers are
AES-256-GCM encrypted before Durable Object storage. Production
requires `CREDENTIAL_ENCRYPTION_KEY`; `CREDENTIAL_ENCRYPTION_KEY_PREVIOUS`
supports online key rotation. Status and control responses never return an API
key, access token, refresh token, device auth ID, verifier, or challenge.

## Account connectors

The account profile supports GitHub, Gmail, Google Drive, and X authorization.
The browser starts an account-authenticated flow and receives only the fixed
provider authorization URL. The private per-user connector Durable Object owns
PKCE/state validation, code exchange, identity lookup, encrypted token storage,
and disconnect. OAuth callbacks return only a relative profile destination and
connection result through the managed Worker.

Register these exact callbacks on the provider applications, replacing the
origin with the deployed website origin:

```text
https://<origin>/v1/connectors/github/callback
https://<origin>/v1/connectors/gmail/callback
https://<origin>/v1/connectors/gdrive/callback
https://<origin>/v1/connectors/x/callback
```

For the canonical local stack, register the corresponding OrbStack HTTPS
callbacks; neither Portless nor a public tunnel is required:

```text
https://nanocodex.local/v1/connectors/github/callback
https://nanocodex.local/v1/connectors/gmail/callback
https://nanocodex.local/v1/connectors/gdrive/callback
https://nanocodex.local/v1/connectors/x/callback
```

Google Web clients require every development URI to match exactly, including the
scheme, host, port, and path. Keep GitHub wildcard callback matching disabled.

GitHub requests only the classic `repo` and `workflow` OAuth scopes for cloning,
pushing, repository API work, and workflow-file updates. It does not request
organization administration, account administration, package management, or
repository deletion. Gmail requests
`https://mail.google.com/`, and Drive requests full `drive` access. These grants
permit destructive writes but never exceed the authorizing user's own provider
permissions. The Google scopes are restricted and require the corresponding
verification and data-handling review for a public production application. X
requests read/write scopes for posts, follows, likes, bookmarks, lists, direct
messages, media, and offline refresh. Agents poll or act through the allowlisted
X API paths when invoked.

## Validation and deployment

```sh
npm ci
npm run check
```

Production deployment requires the encryption key, private readiness probe
token, and the GitHub/Google OAuth application client IDs and secrets. X OAuth
application credentials are optional, but its client ID and secret must be
configured together. The
deployment input names are `NANOCODEX_GITHUB_OAUTH_CLIENT_ID`,
`NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET`, `NANOCODEX_GOOGLE_OAUTH_CLIENT_ID`,
`NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET`, `NANOCODEX_X_OAUTH_CLIENT_ID`, and
`NANOCODEX_X_OAUTH_CLIENT_SECRET`; the deployment script maps them to the
private Worker bindings and strips them from child-process environments. User
provider credentials are still provisioned per account only after interactive
authorization; no user token or deployment-global provider credential reaches
the browser or managed Worker.
