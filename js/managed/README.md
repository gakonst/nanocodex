# Managed durable-agent Worker

This Worker is Nanocodex's account-owned hosted-agent surface on Cloudflare. It
authenticates public requests, projects the caller's authority, and routes work
to durable, account-scoped services.

## Ownership and security

`DurableAgentSession` exclusively owns an agent's mutable runtime: retained
history, turn admission and completion, ordered events, client sockets, tools,
and recovery. The edge Worker owns routing and authorization; an agent ID is a
routing identifier, never authority. Each agent route reauthenticates the
account or grant and forwards only its permitted slice.

Connector credentials never enter this Worker, browser state, durable agent
state, or tool configuration. Model and connector access crosses the private
`NANOCODEX` Service Binding to `nanocodex-egress`, which owns credential routing
and injection.

Reusable Hosted Tools protocol, broker-state, and durable-memory policy live in
`nanocodex-tools`. This Worker supplies their Durable Object SQL/WebSocket
adapters and retains account scope, Connect authorization, bindings, and
storage ownership.

## Public journeys and protocol boundaries

- SMS OTP/account and API-key routes establish the account identity that owns
  agents, organizations, connectors, memory, and history.
- `/v1/agents` lists or creates agents. Agent routes create turns, read state,
  cancel or steer work, delete an agent, and support explicit durability import
  and export. Stable `Idempotency-Key` values make create and turn retries safe.
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
- Agent events are a durable, ordered cursor stream. SSE resumes with `cursor`
  or `Last-Event-ID`; same-origin browser WebSockets carry the typed
  prompt/steer/cancel protocol. Realtime calls and sideband transport have
  separate agent-scoped WebSocket routes.
- `/v1/history/*` and `/v1/memory` expose organization- and team-scoped
  retained context. `/v1/credentials` and `/v1/connectors` manage brokered
  credentials, OAuth connections, and MCP connections without exposing secrets.
- Managed agents can search completed team conversations with `find_session`
  (`find_sessions` remains available) and verify exact turns with `read_session`.
  Each call requires its own agent's `history:read` capability.
- The first admitted prompt automatically calls `find_session` and `memory`
  (`operation: "scan"`) before the model starts, using a bounded query from
  that prompt. The normal tool handlers enforce the caller's capabilities.
  Retrieval runs in parallel with runtime and account discovery. A durable
  developer message injects the results and the safe `accountInfo` snapshot,
  including connected hands, logical mounts, and capabilities, before the first
  model request. Retrieved content is explicitly untrusted data. Bootstrap emits
  no tool events and leaves the user prompt unchanged. Stable instructions stay
  first; the snapshot is appended once, preserving the cached conversation prefix.
  Durable receipts and checkpoint reconciliation prevent duplicate injection on
  recovery or reconnect. Later connection changes are available through `accountInfo`.
  Subsequent turns use `memory` to scan, read, put/replace, and delete team facts;
  mutations require root-agent `memory:write` authority and puts require a scan.
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
| `NANOCODEX_HISTORY`, `HISTORY_AI_SEARCH` | R2 history archive and production history retrieval. |
| `NANOCODEX_WORKSPACES`, `NANOCODEX_WORKSPACES_*`, `NANOCODEX_BRAIN` | Retained per-hand workspaces, read-only peer aliases, and the durable agent's shared writable `/brain` scratch. |
| `BROWSER`, `LOADER` | Browser Run and the sandboxed Worker loader used by the official Agents browser runtime. |

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

`MANAGED_BROWSER_PROVIDER` is deployment policy and accepts `cloudflare` or
`browserbase`; it is never a browser-tool argument. Cloudflare is the default
and uses the `BROWSER` binding. Browserbase uses the same official Agents CDP
runtime through a Worker-side binding adapter. Set its API key only as a
Wrangler secret (never in `vars`, logs, or tool configuration):

```bash
pnpm exec wrangler secret put BROWSERBASE_API_KEY --config wrangler.jsonc
```

`BROWSERBASE_PROJECT_ID` is optional and can also be supplied as a secret.
Browserbase sessions explicitly disable CAPTCHA solving, advanced stealth,
verified-browser mode, proxies, and provider recording. Both providers retain
one session per durable agent for bounded reuse. Signed CDP/Live View URLs and
cookie-bearing CDP fields are redacted at the tool adapter boundary; human
handoff remains disabled until there is an account-authenticated first-party
handoff route that can resolve provider URLs without crossing model results.

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
running as an independent product surface. Use the repository operator commands,
deployment order, secret handling, and required browser evidence in
[`../../AGENTS.md`](../../AGENTS.md). The package scripts provide its focused
typecheck, test, and Wrangler dry-run build when that boundary changes.

### Original video attachments

The authenticated `/v1/agents/:id/attachments/:uuid` route stores original
MP4/MOV bytes in the agent's existing `/brain/attachments/:uuid/original.*`
filesystem. `POST` accepts `{name, media_type, size}` and returns the file path,
8 MiB part size, next part number, and completion state. `PUT .../parts/:number`
accepts exact binary chunks in order; identical retries are safe and conflicting
bytes are rejected. `POST .../complete` finalizes the file idempotently. `GET`
returns private, uncached bytes and supports ranges. Multipart upload IDs remain
server-side. No image frames or audio conversions occur at this boundary.

Account ownership, organization, team, authorization epoch, and capabilities
are checked before filesystem access. Connect grants cannot use this route.
Session deletion fences new work, cancels body readers, drains pending writes,
and aborts incomplete uploads before the existing `/brain` cleanup. The
`attachments.test.ts` Worker tests exercise real local R2 multipart behavior,
reconstruction, retries, filesystem reads, deletion fencing, and account isolation.
