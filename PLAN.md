# Nanocodex roadmap

Updated: 2026-08-27

## North star

Give a person one Nanocodex account and let the same hosted agent show up
everywhere: the CLI, Nanocodex2, a third-party website, a browser extension, a
phone, a native application, or a game engine. Identity, conversations, memory,
connected accounts, MCPs, and hosted tools follow the account. A client may
attach its own local capabilities without becoming a second hosted backend.

The headless Rust SDK remains the reusable core. The managed platform owns
accounts, durable execution, grants, secret routing, hosted tools, and events.
Applications and language bindings remain thin consumers of those boundaries.

## Roadmap shape

There is one current gate, followed by two parallel product tracks:

1. **Current phase — production foundation and Connect.** Make the complete
   cloud platform deployable, make CLI and web connection flows excellent, and
   prove every already-shipped surface and supported agent configuration in
   production.
2. **Customer-driven track — Nanocodex anywhere.** Embed the established
   account and hosted-agent experience in more applications and runtimes.
3. **Customer-driven track — deeper product capabilities.** Improve memory,
   thread finding, imports, context, triggers, tools, and synchronization.

Tracks 2 and 3 have no fixed ordering. They can proceed independently or
interleave according to customer pull. “Anywhere” is not blocked on completing
the deeper product backlog, and deeper product work is not blocked on shipping
every possible client surface.

## Operating model

The normal operator interface is deliberately small:

- `npm run dev --prefix web` starts one complete, instance-isolated local platform.
- `node scripts/deploy-cloudflare.mjs` builds, deploys, and verifies one coherent production platform.
- `node web/scripts/down-local.mjs` stops only the local platform owned by the current checkout.

Package builds, pinned Wrangler calls, storage provisioning, migrations, Worker
ordering, container rollout, and health probes are implementation details of
those commands. Work lands as focused chronological commits on `master`. A
dirty primary checkout is never mixed into platform work; use a clean worktree
and push its exact reviewed HEAD.

## Phase 1 — production foundation and Connect

### Outcome

By the end of this phase, a new user can install Nanocodex, establish or recover
one passkey-backed account from either the CLI or a web application, connect
hosted OAuth and MCP accounts, approve an exact agent grant, and immediately use
the resulting local or hosted agent. The same account and connection model must
work from the first-party Account page and the embedded Connect dialog.

The phase is not complete merely because every Worker deployed. It is complete
when the production behavior matrix below passes through real public entry
points, with browser and CLI evidence.

### How Connect works

#### CLI device connection

`nanocodex login` and connector-focused CLI flows create a signed JSON-RPC
`wallet_connect` request. The request identifies the Nanocodex CLI app and
origin, asks for `agent.run`, and enumerates the exact resources needed: output
visibility, history, memory, connector names, MCP connection IDs, and any
explicit spending authority. The CLI registers that request with device
authorization and opens the hosted Connect page with the terminal code.

The hosted page lets the user:

- continue with the current remembered passkey;
- see and choose any other remembered passkey;
- invoke the platform passkey chooser for another credential; or
- create a fresh account and passkey.

It then shows the requested grant and any missing hosted connections. The user
can complete requested GitHub, Gmail, Google Drive, X, ChatGPT import, or MCP
setup inside the shared connection surface. Before minting anything durable,
the API rechecks the live connector and MCP state so a stale approval cannot
grant a disconnected capability.

Approval returns a grant bound to the exact app identity/origin, account,
managed agent, resources, connector/MCP set, and expiry. The CLI validates the
result before replacing its prior login and stores only its delegated bootstrap
material. Its simple client can then use the hosted agent, memory, and granted
tools without receiving provider credentials.

#### Embedded Connect from another web app

A third-party web application imports the shared JavaScript/React Connect
client, submits its own `wallet_connect` request, and opens the hosted dialog.
The request has the same `agent.run` top-level permission and a precise resource
list: final replies, action summaries, conversation history, raw traces, memory
read/write, named connectors, exact MCP IDs, and optional explicitly bounded
MPP policy.

The dialog validates the app identity, origin, signed request, and return
boundary. It uses the same passkey chooser and connection components as Account,
but filters the UI to the requesting app's needs and adds approval, cancellation,
and return hooks. After approval, the host receives a capability-bound agent
object. Server-side event and history projection exposes only the approved
visibility, and tool routing exposes only the approved connector/MCP slice.

Logging out of the account ends the browser account session; it does not
implicitly revoke an app grant. Revocation is explicit. An expired persistent
passkey session must reauthenticate the same account and must never silently
fall back to a new anonymous account.

#### Account is the general case

The first-party Account page and embedded Connect dialog import the same visual
and action library. Account shows the whole account catalog and updates a
connector card in place. Embedded Connect supplies request context, filters,
permission copy, approval hooks, and return behavior. It is not a second
implementation.

Account-level entitlements can make a new hosted tool or memory service
available without client credential setup. They do not broaden an already
issued app grant beyond its exact approved resources. Provider tokens always
remain behind the credential broker and never enter CLI output, app JavaScript,
browser storage, or browser network payloads.

### One-command production topology

`node scripts/deploy-cloudflare.mjs` must recreate or advance the complete Cloudflare topology from an
exact clean `origin/master` revision:

1. load the canonical deployment environment without printing secrets;
2. validate authentication, required secrets, source invariants, and resource
   configuration before the first mutation;
3. install pinned dependencies and build shared WASM/web artifacts once;
4. provision or adopt storage and apply state-preserving migrations;
5. deploy Connect assets and a root bootstrap when a fresh service-binding
   graph requires it;
6. deploy the private egress broker and its connector secrets;
7. deploy the private managed-agent Worker and owned durable objects;
8. deploy Connect API, Connect dialog, Connect playground, and final root;
9. roll out the root container exactly once for that revision; and
10. attest the Git SHA, routes, bindings, direct/proxied assets, private managed
    boundary, connector boundary, and canonical live URLs.

Preflight failure leaves production unchanged. Because Cloudflare production
Service Bindings are deployed as separate Workers, a later failure is not
transactional; the command must report the exact completed component, failed
component, and safe same-revision resume path.

### Production behavior matrix

Maintain one checked-in matrix of supported combinations. Every row names its
entry point, account state, runtime, durability, tool placement, visibility,
device class, expected result, and required absence/security assertions.
Unsupported combinations are marked unsupported instead of being silently
skipped. The exit run covers at least the following.

#### Identity, grants, and connected accounts

- CLI install/update, including `nanocodex update --nightly` installing both
  `nanocodex` and `nanocodex2`.
- CLI login, connector-focused connect, MCP connect, logout, relogin, grant
  replacement, and prompt use after authorization.
- Account and embedded Connect with new, current, remembered-other, and system
  passkeys; multiple accounts; sign-out/reload; cancelled ceremony; denied
  request; expiry; reauthentication; explicit revoke; and account switching.
- In-place Account connect/disconnect and request-scoped playground completion
  for every configured built-in connector: GitHub, Gmail, Google Drive, X, and
  ChatGPT as an optional post-account import rather than default onboarding.
- Generic hosted MCP creation and connection, including Linear as the canonical
  real MCP proof, exact MCP grant scoping, reconnect, failure, and revocation.
- Exact `wallet_connect` projection for final output, actions, history, traces,
  memory read/write, connectors, MCPs, and optional payment policy.

#### Agent configurations and tools

- The homepage local browser/WASM agent as an ephemeral application-owned
  session.
- The Account-backed managed agent as a durable hosted conversation, including
  create/list/select, multi-turn history, reload, reconnect, cancellation,
  steering, and account isolation.
- The Connect playground's capability-bound durable agent, including each
  visibility configuration and persistence across host reload/reconnect.
- Voice mode on every agent source that advertises voice support, proving real
  microphone/call setup, start/stop/cancel, one completed voice turn, reconnect,
  and actionable permission/device failure behavior.
- Ordinary built-in tools, hosted connector tools, hosted MCP tools, and a real
  host-defined/reverse-attached tool. Prove catalog-before-ready, successful
  calls/results, cancellation, detach/reconnect, stale-host fencing, and that an
  ungranted or detached tool cannot be called.
- Every supported local/managed, ephemeral/durable, text/voice, and
  ordinary/attached-tool combination. Do not invent a Cartesian product where
  the product intentionally forbids a combination; record that boundary in the
  matrix and test its refusal.

#### Shipped web surfaces and demos

- `/` local agent experience;
- `/agent` managed durable agent and conversation rail;
- `/connect` Account plus `/connect/device` CLI authorization;
- the direct Connect dialog and deployed Connect playground;
- `/multiplayer` durable room creation, join, managed turn, reload/reconnect,
  quota/error behavior, and cleanup;
- `/world` interaction, local speech/orchestration, agent actions, responsive
  controls, and retained deterministic behavior;
- Docs and direct documentation routes, Evals, Source, Commits, Changelog, and
  all published navigation/direct-link states.

Each interactive route is exercised on desktop and representative touch/mobile
dimensions. Verification includes visible controls, reload and independent
contexts where durable/multi-user behavior matters, console errors, failed
requests, WebSocket/event continuity, CSP/framing, and provider-secret absence.

### Current state

- Shared Account/Connect identity and connector UI is on `master`.
- One-account caps were removed and sign-out preserves remembered accounts.
- Local browser evidence retained two passkeys across sign-out and reload.
- ChatGPT import is no longer a default onboarding action.
- The Cloudflare account and all project storage were intentionally reset; empty
  production storage was recreated and no old account, passkey, memory,
  connector, or history record was restored.
- That reset also deleted Worker secret values. A coherent deploy must reseed
  the broker from the canonical source or stop before mutation; it must not
  claim readiness with placeholder credentials.
- The nightly release at the recorded checkpoint publishes both CLI binaries.
- The observed live Account page still reported `managed service unavailable`;
  that screenshot remains proof that the current phase has not passed.

### Phase 1 exit and Codex alignment

Phase 1 exits only when `node scripts/deploy-cloudflare.mjs` succeeds from a clean checkout and the
complete supported behavior matrix passes against the exact deployed SHA. Any
failure produces a focused fix, commit, redeploy, and rerun of the affected row
plus its owning boundary.

The Codex closeout ledger now reconciles every commit after the prior checkpoint
through `13bc770eaf0ad8548776bde59c3d6e5316406279`, classifies the latest 358
commits in addition to the earlier 1,357, and advances the reviewed checkpoint
with code and regression evidence. Keep classifying every later commit before
advancing it again. This is not permission to import Codex's app server,
provider portability, approval framework, or unrelated UI architecture.

## Track A — Nanocodex anywhere

This track takes the now-proven Account, `wallet_connect`, managed-agent, event,
voice, and attachment contracts into more customer surfaces.

### Third-party web applications

Treat the production Connect playground as the reference integration and
publish its React/JavaScript host contract. A real app should need only app
identity, requested resources, dialog hooks, and optional attached tools to get
the same account-owned hosted agent.

### Browser extension

Productize the narrow MV3 side-panel agent with shared hosted Connect, bounded
page inspection, and user-approved site recipes. Keep broader user-Chrome
automation as a separate product choice; if adopted, use explicit tab leasing,
visible ownership, interruption, and deterministic cleanup rather than ambient
default-profile CDP takeover.

### Android and phones

Use the existing Kotlin/React Native shell, Android Assist, voice/realtime, and
fenced device-tool host. Replace its PoC callback loop with canonical reverse
attachment and prove reconnect, offline behavior, background/lock-screen policy,
and packaging.

### Native and game-engine bindings

Promote AgentWorld's panic-contained C ABI and Unreal consumer into a stable C
foundation, ergonomic C++, C#/.NET, Unreal, and Unity bindings. Polling,
callbacks, cancellation, threading, ownership, and application-defined tools
must remain safe for a game loop. Hosted account/agent use is the default while
application-owned local agents remain deliberate.

### Nanocodex2 and hosted dynamic apps

Keep Nanocodex2 managed-only and ship it with every nightly. Missing capability
is fixed on the shared platform, not by adding another local engine. Resume
tenant/team and isolated-branch dynamic apps when customer demand selects them;
they receive the same explicit principal, bounded grant, source ancestry,
isolation, and revocation as handwritten apps.

## Track B — deeper product capabilities

This track makes the shared hosted agent more useful in every existing and
future surface.

### Memory, history, imports, and session finding

- iterate on account/team memory quality, citations, deletion, and bounded
  mutation;
- make finding and reopening sessions/threads a first-class hosted tool and UX;
- import ChatGPT and other agent/application history only as an explicit
  post-account action;
- preserve source identity, revision, cursoring, deduplication, retention,
  deletion, re-import, and retrieval provenance;
- distinguish conversation evidence, durable memory, company context, and live
  connector results.

### Company context

Port useful Centaur `company_context` behavior and company-context ETLs into the
account/team data model. Context remains cited, refreshable, scoped, and
deletable rather than becoming an untraceable prompt blob.

### Tool reconciliation and workspace sync

Build account-visible reconciliation around reverse attachment: immutable
catalog revisions, source identity, presence, readiness, revocation, reconnect,
and stale-host fencing. Keep workspace sync a separate recoverable protocol with
explicit direction, object identity, conflicts, deletions, offline queues,
provenance, and recovery. Tool attachment alone never implies file copying or a
merged workspace.

### Triggers and event-driven agents

Port Centaur's useful behavior as adapters over ordinary durable turns:

- authenticated webhooks with one-time secret receipts and idempotency;
- five-field UTC cron with deterministic occurrence IDs;
- connector/service events after the generic boundary is proven;
- company-context refresh and ETL triggers; and
- observable execution, retry disposition, disable/delete, and account policy.

Do not create a second agent runtime or generic workflow DAG. A trigger admits a
normal durable turn with an explicit source, occurrence ID, input, and grant.
Missed cron occurrences skip unless a product explicitly chooses catch-up.

## Shared invariants across both tracks

- One passkey account owns hosted identity, agents, memory, connectors, MCPs,
  imports, and tool entitlements.
- Every app gets an exact app/origin/agent/expiry/resource-bound grant; account
  availability never bypasses app authorization.
- Provider credentials stay in the broker.
- Client-owned typed history and durable hosted event ordering remain
  authoritative at their respective runtime boundaries.
- Reverse-attached capabilities are explicit, fenced, cancellable, observable,
  and revocable.
- Local and hosted work may diverge after detach; reconnection never pretends
  their workspaces already merged.
- New surfaces consume the same contracts instead of creating another identity,
  agent, permission, memory, or tool backend.

## Recovered source anchors

This roadmap preserves ideas recovered from surviving work:

- stash `19249630ebdd2c166a07200f883fc60a2982b2f4`, untracked `notes`: Connect
  anything, local/cloud continuation, memory, crons/workflows, and company
  context ETLs;
- `codex/dynamic-apps-poc`: tenant/team and isolated-branch hosted apps;
- `codex/chrome-extension-master-integration`: MV3 side-panel agent;
- `feat/android-device-host` and `nanocodex-android`: phone tools and Android;
- `agent-world`: reusable C ABI plus Unreal consumer;
- commits `db80fc4e`, `b5c984d2`, and `cbd7ca14`: reverse attachment/readiness;
- `.worktrees/basic-triggers`: webhook and UTC cron work awaiting integration;
- historical `docs/NANOCODEX2_PLAN.md` and current Nanocodex2 commits.

User direction from 2026-08-27 adds explicit C++/C#, Unity/game-engine, Centaur
`company_context`, and exhaustive first-phase production-matrix requirements
where surviving repository evidence is partial.
