# Nanocodex per-user credential broker

This private ordinary Cloudflare Worker owns provider credentials for managed
Nanocodex users. It has `workers_dev = false`, no routes, and is reachable only
through a Service Binding. It does not use Workers for Platforms.

## Manual API secrets

The managed account may submit an OpenAI API key through the authenticated
`PUT /v1/credentials/openai` route. The broker accepts that request only from
the managed account session, encrypts the key before Durable Object storage,
and returns `204` without a secret-shaped response. `GET /v1/credentials`
returns connection state only. Model egress resolves the opaque subject to its
owning user and injects the decrypted key only into the fixed OpenAI request;
the browser, CLI, agent tools, and connector callers receive neither the key
nor a credential-bearing response.

Cloudflare Secrets Store is an account-level, statically bound Worker resource:
its binding exposes `get()` to the Worker, and the value is not viewable after
creation. It is therefore appropriate for the broker's encryption key, but not
for dynamically creating one account secret per user from a request. The vault
accepts either the existing `CREDENTIAL_ENCRYPTION_KEY` Worker secret or a
Secrets Store binding with that name, so deployments can move the encryption
key to Secrets Store without changing the per-user Durable Object contract.
Keep the binding on the private broker Worker only; do not add it to managed,
browser, or agent configurations.

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
| `PUT /users/:user/credentials/chatgpt` | exact bounded server credential document | empty `204` |
| `POST /users/:user/credentials/chatgpt/login` | none | pending device-login status |
| `POST /users/:user/credentials/chatgpt/login/status` | none | pending/authenticated/expired status; polling and token exchange stay server-side |
| `DELETE /users/:user/credentials/chatgpt` | none | `204` |
| `POST /users/:user/credentials/chatgpt/local-claim` | none | secret-free status; development only |
| `PUT /users/:user/credentials/ssh/:reference` | target-bound private-key document | `204` |
| `DELETE /users/:user/credentials/ssh/:reference` | none | `204` |

The local claim is enabled only when `ENVIRONMENT` is `local`, `development`,
or `test` and `ALLOW_LOCAL_CREDENTIAL_CLAIM=true`. It consumes
`LOCAL_CHATGPT_BOOTSTRAP` from the broker environment, accepts no provider
material in the request, and the subject directory permits one claiming user.
Production returns `404` even if the endpoint is called.
The local-claim request is always bodyless; it never accepts credential material
from its caller.
The claim is missing-only: a healthy retained ChatGPT credential is left
unchanged, while a missing or dead credential may be installed from the local
bootstrap. Repeated explicit claims are therefore idempotent.
When this local-claim profile is enabled, starting an interactive ChatGPT
device login fails with `409 local_credential_claim_required`.

The ChatGPT import route is available only on this private Worker through its
Service Binding. Its user path component comes from the authenticated server
session, never from the submitted document. It accepts exactly `access_token`,
`refresh_token`, `account_id`, `expires_at` (Unix epoch milliseconds), and
`fedramp`. The whole JSON document is limited to 64 KiB, each JWT to 32 KiB,
and the account ID to 256 UTF-8 bytes. Both tokens must be three-part JWTs with
object payloads; access-token account, FedRAMP, and integer expiry claims must
match the document exactly, and expiry must be more than five minutes away.
Unknown fields and malformed or inconsistent claims fail with a stable,
secret-free error.

A missing or dead ChatGPT credential is encrypted with the existing mandatory
production vault key and atomically committed with its refresh alarm. Import
also clears a pending device login. Re-importing the same account while its
credential is healthy is an idempotent no-op, so a stale caller cannot replace
a newer token or revision. A different healthy account returns
`409 chatgpt_account_conflict`. Every success is an empty `204` with `no-store`;
request bodies and provider material are never logged or reflected.

## Brokered SSH identities

An authenticated account may store an SSH identity through
`PUT /v1/credentials/ssh/:reference`. The JSON document contains
unencrypted PEM `private_key`, canonical lowercase `hostname`, `port`, `username`, and the
server's `host_key_sha256` fingerprint. The private broker encrypts the whole
record with the same per-user credential vault used for provider credentials.
Status returns only the reference and target metadata.

Managed Just Bash then uses the opaque reference:

```sh
ssh -p 2222 -o IdentityRef=production deploy@ssh.example.com -- uname -a
```

During command execution the shell sends the private egress Worker only the
reference, exact target, and remote command. Egress refuses any target that
differs from the stored record, verifies the stored host fingerprint, performs
key authentication itself, and returns only bounded stdout, stderr, and exit
status. Private-key bytes never enter the execution Worker, agent context,
durable workspace, shell environment, tool output, or audit log. Direct `-i`
SSH remains a separate host-owned mode; `IdentityRef` never falls back to a
workspace file or disables host checking.

Account-owned managed turns may use stored SSH identities. Capability-bound
Connect turns fail closed for `IdentityRef` until the signed Connect resource
protocol can enumerate the exact approved SSH identity references; account
entitlement alone never broadens an existing app grant.

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

All API keys, SSH private keys, ChatGPT access/refresh state, device-login state, connector
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

Gmail and Google Drive retain multiple Google identities per Nanocodex account.
Their public status includes a secret-free `connections` list with stable opaque
IDs and display labels. A provider request uses the sole connection
automatically; once more than one is present, callers select one with
`X-Nanocodex-Connector-Connection: <id>`. Connect grants snapshot those IDs, so
authorizing another Google account later cannot broaden an existing grant.
Disconnecting `DELETE /v1/connectors/{gmail|gdrive}/<id>` revokes only that
Google identity and its sibling Gmail/Drive grant for the same Google subject.

Register these exact callbacks on the provider applications, replacing the
origin with the deployed website origin:

```text
https://<origin>/v1/connectors/github/callback
https://<origin>/v1/connectors/gmail/callback
https://<origin>/v1/connectors/gdrive/callback
https://<origin>/v1/connectors/x/callback
```

For a local stack, register the exact `nanocodex.localhost` origin and port
printed at startup; neither Portless nor a public tunnel is required:

```text
http://nanocodex.localhost:5173/v1/connectors/github/callback
http://nanocodex.localhost:5173/v1/connectors/gmail/callback
http://nanocodex.localhost:5173/v1/connectors/gdrive/callback
http://nanocodex.localhost:5173/v1/connectors/x/callback
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
