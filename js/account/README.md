# Nanocodex web

`nanocodex-web` is the Cloudflare-hosted React application for Nanocodex. It
provides the public site, documentation, account and Connect experiences, and
live product demonstrations. It consumes the local `nanocodex`,
`nanocodex-react`, and `nanocodex-terminal` packages; it is not an SDK runtime
or a second agent backend.

## User surfaces

- **Home** (`/`) is the product homepage for every visitor; signed-in accounts
  see shortcuts to Agents and Account instead of the sign-in form.
- **Agents** (`/agents`, `/agents/:id`) is the durable-agent chat app. Agents
  include native `browseX` public X browsing, listed in `accountInfo.apis`
  without an X connection, and retain a thread only after the user connects
  their own ChatGPT or OpenAI credential.
- **Account** (`/account`, `/account/vault`, `/account/wallet`,
  `/account/access`) manages connections, Vault, wallet and API keys.
  Superseded `/agent…` and `/connect…` links redirect there, keeping query and
  fragment; `/connect?user_code=…` and `/connect/device` remain device
  authorization, and `/vault` and `/services/phone` remain hosted service pages.
- **Thread sharing** creates revocable view or write links for a durable
  agent conversation. Guests see the normal Chat transcript; write links admit
  real AI turns with attenuated account-tool permissions. See [thread sharing](../../docs/THREAD_SHARING.md).
- **Attached Tools**, **Multiplayer**, and **World** demonstrate browser-hosted
  tools, a shared managed-agent room, and an agent-populated world.
- **Account** and **Connect** handle SMS OTP account login, connection, device,
  and request-scoped Connect journeys. The Connect dialog is served at
  `/connect-dialog`; the separate Connect API routes remain behind the Worker.
- Signed-out visitors enter their phone and one-time code directly in the Home
  terminal before its three-prompt sponsored composer appears.
- **Docs**, **Evals**, **Source**, **Commits**, and **Changelog** present the
  product reference, evaluation evidence, and published repository data.

Remote Screens is available in signed-in Home chats, managed chats, and Connect.
The browser consumes the account screen catalog for Mac, iPhone, VM, and Linux
server publishers. Video and input use WebRTC; account requests authorize the
signaling and renewable viewer lease. Native screens require video; connection
or video failures remain visible and never fall back to JPEG polling. Only
Cloudflare sandbox desktops (`machine_id: "cf:…"`, desktop/VM surfaces) explicitly
advertising `transport: "frames-v1"` use that authorized WebSocket for JPEG frames
and the same control/input messages. This path makes no ICE requests, decodes
and draws one requested frame at a time, and requests at most ten frames per
second. Encoded frames are limited to 700,000 base64 characters; both JPEG
header dimensions and decoded dimensions must match and stay within 1280×1280.
A selected screen survives tab suspension
and disconnects. Recovery refreshes its publication generation and retries for
up to 90 seconds, with backoff capped at eight seconds and ten-second reconnect
attempts. A stopped recovery offers an explicit Reconnect button.

Hiding the tab pauses the connection. Losing focus, control, or the connection
discards unsent input; resuming requires taking control again. Disconnected
video is cleared and hidden. Screen availability still depends on the host
publisher; this viewer does not provision a Cloudflare desktop.

## Boundaries

The Vite application has one React root and owns browser presentation, routing,
and browser-agent integration. Documentation source lives in `docs/src/pages`
and is rendered in that application.

The Cloudflare Worker in `worker/` owns public HTTP routing and proxies its
scoped backend services: managed-agent access, Connect APIs and dialog,
repository and thread Git data, evaluation reads, and credential-backed model
operations. Provider credentials stay behind Worker bindings and are not part
of browser configuration.

Screen viewer upgrades with a valid two-minute access snapshot can reach the
existing account screen broker directly. This requires the same
`NANOCODEX_ACCESS_SECRET` as the managed Worker; keep it server-side and rotate
both copies together for explicit snapshot invalidation. Missing or invalid
snapshots retain the managed route. See [short-lived request access](../managed/README.md#short-lived-request-access)
for expiry, recovery and renewal behavior.

The deployment may sponsor exactly three prompts in the signed-in Home demo
from one operator-connected ChatGPT account. The per-account allowance is
reserved atomically at egress. That fallback is restricted to the ephemeral
browser model subject, uses Luna with thinking disabled, and cannot authorize
Durable Agent or the other managed demos. User-connected credentials take
precedence and are not subject to the sponsored allowance.

Persistent SMS accounts use a Worker-owned secp256k1 root wallet. The account
UI presents it simply as `Wallet`, shows its dollar-denominated balance, and
does not render the crypto address. The app may send exact `wallet_connect` or
`wallet_revokeAccessKey` requests to the authenticated managed-Worker routes,
but it never receives the root private key or its encrypted envelope. The key
is custodial and server-side encrypted in the per-user egress Durable Object;
it is not user-held end-to-end encryption. Configurable-account migration is
future work. See [the wallet custody contract](../../docs/WALLET_CUSTODY.md).

Vite uses the Cloudflare and React plugins plus `nanocodex-vite`. Local
development also serves the Connect dialog and starts the egress, managed, and
Connect API auxiliary Workers. Build output includes a Cloudflare Wrangler
configuration and deployment attestation.

## Development and deployment

React server state follows the [Wagmi TanStack Query patterns](https://wagmi.sh/react/guides/tanstack-query):
reusable typed query options, deterministic keys, declarative enabled conditions,
and mutation-driven invalidation. `BrowserApplication` provides one QueryClient
shared with route prefetches. Account metadata uses account ID and endpoint keys;
credentials and vault views select from the same cached response. Reads are fresh
for 30 seconds by default and inactive queries expire after 10 minutes. Public
repository metadata and changelog reads use five minutes of freshness; immutable
commit pages and file contents use revision/object keys. Evals retains its live
polling policy. Session transitions cancel and remove the previous account's
queries. Private data stays in memory; form secrets and one-time API keys stay
in component state. Thread lists and state/settings use account-and-thread keys;
sidebar hover/focus prefetches thread state. Switching back restores retained
history immediately and resumes the managed stream after its last durable
cursor, including events received while another thread was open. Detached
history expires after ten minutes; account changes remove it. Live turn events
invalidate the list and selected thread state. Streaming transport and
OAuth/device lifecycles retain their existing protocol ownership.

Use the root [README.md](../../README.md) for checkout setup and the root
[package scripts](../../package.json) for repository commands. Follow
[AGENTS.md](../../AGENTS.md) for deployment order and verification guidance. This package exposes
the supporting `dev`, `build`, `test`, `typecheck`, `check:docs`, and `deploy`
scripts, but the repository instructions own how they are run.

## Standalone services

`/vault?service=totp` opens authenticator enrollment without an agent conversation.
The form accepts an authenticator URI or a Base32 setup key with issuer and account
labels. Both require an exact HTTPS website origin (for example,
`https://login.example.com`, without a path or trailing slash). The setup material
is sent directly to the authenticated Vault endpoint and cleared from the form.
Saved items expose metadata; the broker supplies verification codes only in an
authorized request to the saved origin.

`/services/phone` lists dedicated numbers and incoming SMS. Search currently offers
US local numbers. Selecting a number requests a quote; approval requires a separate
checkbox and button after reviewing the recurring monthly and incoming SMS prices.
Release likewise requires a separate confirmation that linked accounts may lose
verification access. Browser operation IDs survive reloads, and an uncertain result
only exposes a status check for that same operation. Carrier configuration and
provisioning limits must be enabled by the operator. Website acceptance of these
numbers for two-factor authentication varies.

Hosted enrollment accepts `enrollment_origin` and a 16–128 character URL-safe
`state`. Hosted forms open in a popup; cross-origin framing remains blocked by
CSP and X-Frame-Options. `/vault?service=totp` requires an unchecked, explicit
recipient checkbox before saving and sharing metadata. A general `/vault` URL
never sends enrollment callbacks, even if it carries callback parameters.
A successful explicitly shared authenticator save posts a `nanocodex:service-enrollment`
message to that exact origin with `service: "vault"`, `state`, `vault_id`, `kind`,
`name`, and `origin`.

`/vault?service=select` lets a user choose an existing item. It shows the exact
recipient origin, requires a radio selection and a separate “Share selected item”
action, and returns the same envelope with `action: "select"` and only `vault_id`, `kind`,
and `name` as item metadata.
It does not create an item or grant access. Apps use the selected ID to request
scoped Connect consent separately. Missing or invalid callback parameters do not
expose a selectable list.

Phone approval uses the same parameters plus `operation_id`;
for Connect requests, set the hosted `operation_id` to the returned
`approval_request_id`, which is distinct from the app’s original operation ID.
Explicitly shared terminal results post `service: "phone"`, `state`, `operation_id`, and `status`.
Requesting apps must validate the message source, origin, and state. Setup keys,
verification codes, and inbox contents are never included in these callbacks.

Run `pnpm --filter nanocodex-web test:services` after building the workspace UI
packages and installing Playwright Chromium. This journey renders the production
React forms in Chromium and sends their HTTP requests through the real managed
authentication routes, egress broker, and Durable Objects in workerd. Only sign-in
enrollment and the external carrier are synthetic. It covers seed/URI intake,
invalid origins, recipient consent, general-Vault callback suppression, existing-item
selection without grants, cross-origin popup and frame behavior, number search, priced approval, denial,
incoming SMS, release confirmation, and reload/status recovery after an uncertain
purchase. Screenshots and a redacted HTTP transcript are saved in ignored
`output/services-ui/`; no live number is purchased.

Run `pnpm --filter nanocodex-web test:chat-performance` after installing Playwright
Chromium to exercise chat rendering at scale. A synthetic agent replays a capped
200-entry history of rich answers, grouped and nested tools, subagents, and previews
through the real controller and terminal view, then streams a live turn one event per
frame at 4× CPU throttling (`CPU_THROTTLE`, `RUNS`, `LABEL` override). It asserts live
status, reading position, jump-to-latest, and completion behavior; render, script, and
long-task costs are recorded, not asserted, in ignored `output/chat-performance/`.
`CPU_PROFILE=1` also saves an unminified CPU profile of the streaming phase.
