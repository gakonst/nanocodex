# Managed durable-agent Worker

This Worker is Nanocodex's account-owned hosted-agent surface on Cloudflare. It
authenticates public requests, projects the caller's authority, and routes work
to durable, account-scoped services.

Managed agents have a native `browseX` tool for public X posts, profiles, search,
followers, and following. `environment().apis` advertises the tool independently
of connector authentication. It calls the private [X Worker](../x-api/README.md)
through `NANOCODEX_X`; deploy it with `pnpm deploy:x` before `pnpm deploy:managed`.

## Ownership and security

`DurableAgentSession` exclusively owns an agent's mutable runtime: retained
history, turn admission and completion, ordered events, client sockets, tools,
and recovery. The edge Worker owns routing and authorization; an agent ID is a
routing identifier, never authority. Each agent route authenticates the account
or grant and forwards only its permitted slice.

### Short-lived request access

With `NANOCODEX_ACCESS_SECRET` configured (at least 32 random bytes), successful
live-authenticated agent HTTP responses issue a signed `x-nanocodex-access`
permission snapshot, valid for at most two minutes. The token is bound to the
origin and original login/key; Connect snapshots additionally bind all forwarded
grant restrictions. Clients retain the original credential for renewal. They
retry a 401 once only when ingress explicitly marks the snapshot as rejected
before admission, preserving the credential, body and operation identity.

Finite `/v1/agents` requests verify the signature locally. Their owning Session
still checks local owner, organization, team, epoch, lifecycle and operation
permissions. Account screen viewers can also reuse a snapshot; publishers,
ten-second viewer renewal, agent streams and account administration retain live
authentication. Existing accepted work retains its established execution policy.

The account Worker can verify viewer snapshots through the shared
`nanocodex/cloudflare/managed-access` module and reach its existing screen broker
directly. Configure the same `NANOCODEX_ACCESS_SECRET` in both the managed and
account Workers. Missing configuration or an invalid snapshot preserves the
original managed route and rejection protocol. This does not introduce a new
principal cache or extend the snapshot lifetime.

Account/key/membership changes prevent new snapshots immediately; an existing
snapshot may authorize requests until expiry. Rotating `NANOCODEX_ACCESS_SECRET`
in **both Workers** invalidates all snapshots once both updates are active. A
one-sided update does not invalidate the other verifier's accepted snapshots.
No per-user instant
revocation of issued snapshots is implied. Local Session fencing remains in
force. Tokens are never accepted for token renewal or as provider credentials.

Issuance piggybacks on an ordinary response, so there is no added cold-start HTTP
round trip. SDK caches are in memory; they renew via ordinary live authentication
near expiry. `managed.auth` logs and `managed_auth` Server-Timing distinguish live
and snapshot verification. Those timings measure authentication, not full request
or model latency. Missing configuration preserves the live-only path.

Connector credentials never enter this Worker, browser state, durable agent
state, or tool configuration. Model and connector access crosses the private
`NANOCODEX` Service Binding to `nanocodex-egress`, which owns credential routing
and injection.

### Session-owned credential subjects

The `MANAGED_AGENT_DIRECT_CREDENTIALS=true` setting makes each new
managed agent retain credential ownership in its existing Session DO. Its
private egress subject is `managed-session-v1_<Session DO id>`; credentials
remain in the broker. This removes the additional per-agent
`AgentSubjectDirectory` creation/binding. HTTP and live creation, models,
tools, and voice use the same retained strategy. Existing sessions keep their
directory subjects when the setting changes.

Wrangler enables this strategy in production and development. Before the first
deployment into an environment that predates the private ownership entrypoint,
bootstrap in this order using the existing build/deploy tooling:

1. Deploy compatible managed code with direct creation disabled using
   `--var MANAGED_AGENT_DIRECT_CREDENTIALS:false --containers-rollout none`.
   This exposes the private `ManagedAgentOwnership` entrypoint.
   Omit `NANOCODEX_SESSION_MODEL_EGRESS` from this bootstrap configuration if
   egress does not yet export `SessionModelEgress`; restore it in step 3.
2. Deploy egress with its `MANAGED_AGENT_OWNERSHIP` service binding to
   `nanocodex-durable-agent`, entrypoint `ManagedAgentOwnership`.
3. Deploy managed with the checked-in setting enabled. Development uses the
   same entrypoint through a local service binding.

The normal CI order (egress before managed) works after bootstrap. Code-only
manual deployments should use `--containers-rollout none` when the image has
not changed. Do not deploy an older experiment checkout over newer production
code. To stop new direct sessions, disable the setting while retaining both
Workers' direct-subject support: reverting to code predating that support
would break already-created sessions.

In production, the private `NANOCODEX_SESSION_MODEL_EGRESS` binding targets
egress's `SessionModelEgress` entrypoint. New-strategy Sessions validate retained
ownership locally for each model WebSocket connection, reconnect, and HTTPS request, avoiding
a broker callback into the originating Session. This binding is not exposed to
tools. Credential selection remains live in the broker. Without the optional
binding, the transport retains the usual broker ownership lookup; legacy
directory subjects retain their existing authority.

Hosted Responses requests can fall back from WebSockets to streaming HTTPS through
that same private binding. Compaction permits the initial request plus two retries
per transport, matching codex-rs; after WebSocket exhaustion it switches to HTTPS
and replays the full retained history. The selected transport remains sticky for
the live model session. If compaction still fails, its failure receipt is retained
before the turn fails, so durable recovery replays the failure instead of starting
another provider retry cycle. In-flight interruption and storage failures remain
recoverable, and failed compaction preserves the conversation history.

Deploy egress and the ChatGPT HTTP relay support before the managed runtime.
Sponsored trial credentials currently reject HTTPS Responses before dispatch;
the WebSocket admission and metering policy cannot be bypassed by fallback.

Active clients call `POST /v1/agents/:id/prepare` (no body), or the managed SDK's
`agent.prepare()`, when opening a conversation. The authenticated mutation
requires `agents:write`, `tools:use`, and the ChatGPT connector for delegated
grants. It acknowledges with HTTP 202 `{ "state": "preparing" }`; this means
accepted, not provider-ready. One session-owned task starts runtime/socket
preconnection, personalization, and first-turn account metadata. Prompt and
voice media admission do not await the activation HTTP request. Passive event
and history subscriptions do not prepare models. Preparation installs an idle
alarm and expires after the configured runtime idle interval (30 seconds by
default); reopening the conversation renews it. No `generate:false` model
request is inserted before a prompt.

Connector/MCP discovery, hosted-tool snapshots, and startup account metadata
are retained in memory for at most `MANAGED_ACCESS_TTL_MS` (two minutes), measured
from the start of each read. Concurrent callers share reads. Startup metadata
is keyed by owner, organization, team, authorization epoch and exact turn
authorization; catalog reuse is owner/authority scoped. Runtime shutdown,
including settings replacement, invalidates these snapshots. Failed reads are
not retained as successful snapshots. Explicit account-info tools still force
live discovery, and tool invocation retains its existing live authorization.
These caches store discovery metadata, not credentials or an authorization
bypass. Egress must expose `/users/:user/catalog` for this startup path.

`managed.agent.transport` observations include the managed turn and runtime
request IDs, failure class/phase, retry delay, connection generation and whether
a retry opens a new socket. Raw provider frames and error strings remain
excluded from logs and replay storage.

The resolver reads retained ownership without constructing the agent runtime.
Deleted, exported, or pending-import sessions deny resolution; egress never
falls back to a directory entry after a direct-subject denial.

Reusable Hosted Tools protocol, broker-state, and durable-memory policy live in
`nanocodex-tools`. This Worker supplies their Durable Object SQL/WebSocket
adapters and retains account scope, Connect authorization, bindings, and
storage ownership.

## Prepared personalization

Managed admission no longer runs prompt-derived history search or memory scan.
The MemoryScope prepares deterministic snapshots of saved personal and team memories; Sessions
warm a disposable copy on create, open, or activity without awaiting it. Each turn
pins the eligible local copy or a cache miss. A miss proceeds without retrieval.
Explicit `find_session`, `read_session`, and memory tools remain available.

Snapshots carry organization/team/user scope, source versions, and a five-minute
lease. New memories coalesce until refresh; replacements and deletions invalidate
issued copies before the mutation succeeds. Failed invalidations retain durable
retry debt. Expiry is checked again before model injection. Previously delivered
conversation history cannot be erased; later prepared blocks replace or withdraw
prior prepared context. Existing team facts remain shared; personal facts are
stored separately for the authenticated user within their organization.
No conversation summarizer or inferred personal profile is added here.

Each source selection is indexed, limited to 32 facts and 8 KB of fact content,
and does not write scan/use counters. A team snapshot serves multiple agents;
active subscriber leases are bounded to 256 per memory store. Additional agents
proceed with a cache miss. Refresh is activity-driven, so idle users incur no
periodic job. Identical content is not appended again on later turns, and pinned
context is pruned when the associated turn receipts are archived.

Voice startup receives optional prepared context in the existing context response.
Updated Rust/WASM and Apple voice clients accept it as bounded background data;
older clients ignore the optional field. Media readiness never awaits preparation.
Account/environment discovery remains a separate first-turn dependency.

### Personal memories and request attribution

`memory` accepts `scope: "personal" | "team"` (default `team`). Use personal for
private user preferences and facts, and team for shared knowledge. A scan receipt
and every memory key belong to their scope; keep it unchanged across scan, read,
put, and delete. Both scopes use the existing root-only write policy, capability
checks, secret filtering, version checks, and forget/correction invalidation.
Personal memory is isolated by authenticated user and organization and follows
that user across teams in that organization. Connected-app grants cannot access
personal memories or receive them in prepared context. Team memories are never
relabelled or copied into personal storage.

`GET /v1/memory?scope=personal`, POST operations with `scope: "personal"`, and
`DELETE /v1/memory/:id?version=:version&scope=personal` use the authenticated user;
a request cannot supply another user ID. The JavaScript managed SDK accepts
`{ scope: "personal" }` on `listMemories`, `memory`, and `deleteMemory`.

Clients may send bounded `x-nanocodex-client-context` JSON (`client`, `hand`,
logical `cwd`, `timezone`). The SDK exposes `requestOrigin`; the native CLI sets
its own context automatically. The authenticated edge overwrites the principal
assertion. HTTP, WebSocket, and voice admission pin caller context on the first
turn; reconnects and retries cannot replace it. The snapshot is appended once,
without rewriting baseline instructions, cache keys, or the conversation prefix.

## Public journeys and protocol boundaries

- SMS OTP/account and API-key routes establish the account identity that owns
  agents, organizations, connectors, memory, and history.
- `/v1/agents` lists or creates agents. `/v1/agent-runs` creates an agent and
  admits its first turn under one required stable key and one client request.
  Agent routes create later turns, read state,
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
- Turn input has no application byte ceiling. HTTP uses native JSON parsing;
  incoming WebSockets use Cloudflare's platform limit. SQLite stores large raw
  inputs and frozen dispatch inputs in Unicode-safe chunks below its row limit.
  Coordination scans load metadata; a receipt or dispatch hydrates its own turn.
  Terminal receipts archive sequentially to R2 before their local chunks are
  deleted. `/state.first_prompt` and the portability session's `first_prompt`
  are display previews, not prompt content. Accepted events and turn receipts
  retain the exact full input. Subagent authorization keeps task/role identity
  digests instead of duplicate content. Inline JSON still requires memory for
  the individual request; Cloudflare's shared 128 MB isolate heap applies.
  Cron schedules likewise have no prompt-size or schedule-count admission cap.
  Their input and frozen delivery snapshots use the same chunk placement; alarm
  scans page through indexed metadata and hydrate one occurrence at a time.
  Replacing a schedule releases its old input while queued deliveries retain
  their original payload until delivery is acknowledged.
- Agent events are a durable, ordered cursor stream. SSE resumes with `cursor`
  or `Last-Event-ID`; same-origin browser WebSockets carry the typed
  prompt/steer/cancel protocol. Realtime calls and sideband transport have
  separate agent-scoped WebSocket routes.
- API key resolution validates live account membership, scope, and authorization
  epoch inside the key object. This avoids serial edge-to-account round trips;
  raw-key resolution never caches authority. Short-lived signed snapshots above
  avoid repeating that resolution on every finite agent request. A response marker allows rolling
  deployments to fall back to the original checks against older key objects.
- Voice call creation derives a coarse relay region from trusted Cloudflare
  request metadata. A separate `voice-v1:<region>:<user>` relay prevents an old
  text relay from anchoring media in a distant region. Unknown geography uses
  the existing relay. Placement is a hint, and a sleeping relay still incurs
  container startup time; provider credentials remain server-side.
  After live Session ownership validation, managed calls use the private
  `ManagedRealtimeEgress` binding so the broker does not repeat that lookup.
  This also covers retained legacy subjects: call creation skips the redundant
  directory rebind and readback. Legacy sidebands still repair their mapping.
  Generic agent egress cannot use the owner assertion. Deploy egress before
  managed to install the entrypoint; without the binding, calls retain the
  generic broker path and its ownership check.
- Voice admission does not wait for the independent Responses preconnection.
  The shared Rust protocol delegates the first spoken question and gates reply
  playback until durable output is delivered. Its WASM plan searches memory and
  prior sessions using that question, including new calls in existing chats.
  Durable receipts retain each call's bounded lookups across retries. Existing
  first-turn environment and account context remains developer context.
  Voice start and stop retain the full session context without a conversation
  size rejection. Replies above 512 KiB are archived directly in R2 for exact
  replay instead of being inserted into a SQLite row.
  Successful memory puts and deletes emit authorized `managed.voice.context`
  events; Rust validates call scope, deduplicates cursors, and queues background
  context through reconnects. Retrieved context is data, never instructions.
- `/v1/history/*` exposes retained team history; `/v1/memory` exposes team or
  personal memories. `/v1/credentials` and `/v1/connectors` manage brokered
  credentials, OAuth connections, and MCP connections without exposing secrets.
- Managed agents can search completed team conversations with `find_session`
  (`find_sessions` remains available) and verify exact turns with `read_session`.
  Each call requires its own agent's `history:read` capability.
- Before the first model request, the host appends one durable developer message
  in `<startup_context>` tags after the baseline prompt and static runtime rules.
  It includes the startup UTC time, account/team/session scope, known request
  transport, authenticated principal, available Hands, connected accounts, and bounded
  prepared snapshots of personal and team memories when available. The CLI reports
  its project Hand and logical cwd; web and Apple clients report client type and
  timezone. Client/Hand attribution is explicitly client-reported, not proof of a
  physical device or person. Hand keys/cwd are matched against authorized Hands;
  missing or unmatched attribution remains unknown and never grants authority.
  `environment().hands` maps each Hand key to its logical `path`, capabilities,
  name, online status, and providers. Use that path as `exec_command.workdir`.
  Native paths use readable computer names, such as `/omarchy-desktop`. The
  first assignment is persisted by machine identity; duplicate names receive
  numeric suffixes and renames do not retarget existing paths. Previous opaque
  identity paths remain accepted by execution, preview and computer selection.
  New VM paths include their factory and purpose (`/vm-omarchy-desktop-demo`);
  Cloudflare sandboxes use `/cloudflare-demo`. Existing persisted VM roots keep
  their original spelling. VM display names also identify their provider.
  `environment().accounts[service].connections` lists exact account selectors;
  service entries also advertise deferred tools and documentation.
  XML data is escaped and explicitly carries no instructional authority.
  Startup does not search past threads using the current prompt: `find_session`,
  `read_session`, and `memory scan/read` provide scoped recall when needed.
  The environment and timestamp are frozen once, including across retries,
  reconnects, and pending-memory invalidation. Later turns append to the existing
  conversation without rewriting its cacheable prefix or changing cache keys.
  Existing memory correction/forget invalidation remains effective; it never
  refreshes the startup environment. Use `environment()` for an explicit refresh.
  Old configurations naming `accountInfo` are normalized to `environment`.
  User hands include `online` attachment status. Offline hands remain in the
  namespace so admitted calls can recover their receipts. A broker-confirmed
  unstarted call returns an unavailable-hand result for the agent to handle;
  transport failures with unknown admission retain the existing call identity.
  Subsequent turns use `memory` to scan, read, put/replace, and delete scoped facts;
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

### Spotify on iPhone

**Connect Spotify** uses browser OAuth with PKCE. The native app binds only
`127.0.0.1:8989`, opens Spotify in `SFSafariViewController`, and accepts one
state-matching `/login` callback. It forwards only code/state to the authenticated
`POST /v1/connectors/spotify/loopback/callback` route. The encrypted broker owns
PKCE, token exchange, refresh and API authorization; no tokens enter agent tools.
The listener stops on completion, cancellation, leaving settings, or timeout.

`GET /v1/connectors/spotify/loopback` returns connection metadata; `POST` starts
an authorization and `DELETE` disconnects the specified `connection_id`. These
routes require a persistent owner session or owner API key with account-management
and tool authority. Delegated Connect grants cannot start or complete the flow.
The web Connect Spotify card opens `nanocodex://connect/spotify` on the phone.

This flow uses the public ncspot client registration also used by
[spotify-player](https://github.com/aome510/spotify-player/blob/master/spotify_player/src/auth.rs).
Its client ID and exact loopback redirect are fixed in the broker, and the client
ID is retained with each grant for refresh. Spotify consent identifies ncspot;
its availability and shared API quotas remain outside Nanocodex's control.
The separately configured hosted Spotify OAuth flow remains supported by the
broker. Vault password logins are separate from OAuth connector status.

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

### Sandbox development tools

New Cloudflare Sandbox images include Swift 6.3.3 (Ubuntu 22.04), Go 1.26.5,
Node 24.19.0, pnpm 11.25.0, and Rust 1.97.0 with rustfmt, Clippy, the
`wasm32-unknown-unknown` and `x86_64-unknown-linux-musl` targets, and
wasm-bindgen-cli 0.2.126. Rust, Go, Node, pnpm, and wasm-bindgen versions align
with the repository CI configuration; Swift is a pinned Linux toolchain, while
Apple CI uses the Swift bundled with its Xcode runner. Python, uv, C/C++ build
tools, CMake, Ninja, and musl-tools are also available.

Run `sh /usr/local/bin/nanocodex-check-dev-stack` in a sandbox to check the
installed tools and compile small Swift/Foundation, Go, Rust, musl, and WASM
programs without fetching package dependencies. The image build runs the same
check and fails if a compiler or required runtime library is missing.

Linux Swift supports portable Swift packages. AppKit, SwiftUI, iOS simulators,
Apple SDKs, and `xcodebuild` still require a Mac Hand or Apple CI; installing
Swift does not make all `apple/` packages Linux compatible.

These tools become available after the managed container image is built and
rolled out. Existing running sandboxes need recreation with the updated image.
When changing tool versions in CI, update the corresponding image pins too.

### Original media attachments

The authenticated `/v1/agents/:id/attachments/:uuid` route stores original
image or MP4/MOV bytes in the agent's existing `/brain/attachments/:uuid/original.*`
filesystem. `POST` accepts `{name, media_type, size}` and returns the file path,
part size, next part number, and completion state. Parts are normally 8 MiB and scale up to 100 MB to fit R2’s 10,000-part limit. The current Free/Pro 100 MB request ingress limit therefore permits files up to 1 TB. Parts stream through hashing to R2 with backpressure, and the service serializes ingestion across attachments. `PUT .../parts/:number`
accepts exact binary chunks in order; identical retries are safe and conflicting
bytes are rejected. `POST .../complete` finalizes the file idempotently. `GET`
returns private, uncached bytes and supports ranges. Multipart upload IDs remain
server-side. Image previews use a separate immutable authenticated endpoint; original bytes remain unchanged.

Account ownership, organization, team, authorization epoch, and capabilities
are checked before filesystem access. Connect grants cannot use this route.
Session deletion fences new work, cancels body readers, drains pending writes,
and aborts incomplete uploads before the existing `/brain` cleanup. The
`attachments.test.ts` Worker tests exercise real local R2 multipart behavior,
reconstruction, retries, filesystem reads, deletion fencing, and account isolation.

## Browser on the Cloudflare sandbox desktop

The AMD64 Sandbox image includes Google Chrome. From the remote desktop's
terminal, open a visible browser with:

```sh
google-chrome --no-sandbox --ozone-platform=wayland --disable-dev-shm-usage \
  --no-first-run --start-maximized about:blank
```

The terminal inherits the running desktop's Wayland environment. A separate
shell execution does not automatically inherit that environment. The Sandbox
runs as root, so this command disables Chrome's process sandbox; use it only
inside the isolated Sandbox container. It does not disable TLS verification.
The Debian server Hand image separately provides `chromium`.

Reusable definitions, environment templates, signed lifecycle webhooks, usage
inspection, immutable turn artifacts and HTTP tool results are documented in
[Managed agent configuration and operations](../../docs/MANAGED_AGENT_CONFIGURATION.md).


### Connected-account tool discovery

Managed agents discover first-party `github_request`, Google Workspace capability
`*_request`, `slack_request`, `x_request`, `spotify_request`, and
`soundcloud_request` tools through the same `tool_search` used by connected MCPs.
`environment().accounts` advertises tools for connected, grant-visible services;
`accounts[service].connections` supplies exact account selectors. Each call uses authenticated
egress with live grant and connection checks, broker-owned token refresh, fixed
provider origins, bounded JSON bodies/responses, and no automatic write retries.
Provider scopes and endpoint availability still apply. Spotify connection links
open `nanocodex://connect/spotify` to complete OAuth on the phone.


SoundCloud also supports phone-local OAuth through
`/v1/connectors/soundcloud/loopback` and its `/callback` route, using the same
owner-only authorization and bounded payload policy as Spotify. The broker uses
its configured SoundCloud app and the fixed `http://127.0.0.1:8788/callback`
redirect. Both music providers' connection tools return native app links; no
credentials or renewable tokens pass through the agent or phone API.
