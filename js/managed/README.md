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

Provider credentials never enter this Worker, browser state, durable agent
state, or tool configuration. Model and connector access crosses the private
`NANOCODEX` Service Binding to `nanocodex-egress`, which owns credential routing
and injection.

Reusable Hosted Tools protocol, broker-state, and durable-memory policy live in
`nanocodex-tools`. This Worker supplies their Durable Object SQL/WebSocket
adapters and retains account scope, Connect authorization, bindings, and
storage ownership.

## Public journeys and protocol boundaries

- Passkey/account and API-key routes establish the account identity that owns
  agents, organizations, connectors, memory, and history.
- `/v1/agents` lists or creates agents. Agent routes create turns, read state,
  cancel or steer work, delete an agent, and support explicit durability import
  and export. Stable `Idempotency-Key` values make create and turn retries safe.
- Agent events are a durable, ordered cursor stream. SSE resumes with `cursor`
  or `Last-Event-ID`; same-origin browser WebSockets carry the typed
  prompt/steer/cancel protocol. Realtime calls and sideband transport have
  separate agent-scoped WebSocket routes.
- `/v1/history/*` and `/v1/memory` expose organization- and team-scoped
  retained context. `/v1/credentials` and `/v1/connectors` manage brokered
  credentials, OAuth connections, and MCP connections without exposing secrets.
- `/v1/rooms` creates, joins, observes, and deletes multiplayer rooms. A
  `MultiplayerRoom` owns room chat and its private agent; `MultiplayerQuota`
  enforces deployment-wide room and turn limits. Room WebSockets use their own
  replay cursor and `say`/`ack` protocol.

The small root page is an operator surface; it is not a second application
protocol. `/health` is the service health endpoint.

## Cloudflare bindings

| Binding | Role |
| --- | --- |
| `NANOCODEX` | Private Service Binding to `nanocodex-egress`. |
| `NANOCODEX_SESSIONS` | One `DurableAgentSession` per managed agent. |
| `NANOCODEX_ROOMS`, `NANOCODEX_MULTIPLAYER_QUOTA` | Multiplayer state and global quota. |
| `NANOCODEX_AUTH`, `NANOCODEX_USERS`, `NANOCODEX_API_KEYS`, `NANOCODEX_ORGANIZATIONS`, `NANOCODEX_MEMORY` | Account, key, organization, and durable-memory ownership. |
| `NANOCODEX_HISTORY`, `HISTORY_AI_SEARCH` | R2 history archive and production history retrieval. |

`wrangler.jsonc` is the binding and migration source of truth. Development
uses the same Worker role with local Durable Objects, local egress binding, R2,
and shorter idle timing; AI Search is a production binding.

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
