# Nanocodex web

`nanocodex-web` is the Cloudflare-hosted React application for Nanocodex. It
provides the public site, documentation, account and Connect experiences, and
live product demonstrations. It consumes the local `nanocodex`,
`nanocodex-react`, and `nanocodex-terminal` packages; it is not an SDK runtime
or a second agent backend.

## User surfaces

- **Home** shows an ephemeral browser-agent demo. Both browser chats and durable
  agents include native `browseX` public X browsing, listed in `accountInfo.apis`
  without an X connection. **Durable Agent** retains a
  thread only after the user connects their own ChatGPT or OpenAI credential.
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
signaling and renewable viewer lease. Hosts explicitly advertising
`transport: "frames-v1"` instead use that authorized WebSocket for JPEG frames
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

The viewer tests cover recovery deadlines, server publication changes, stale
callbacks, control release, and authorization expiry. Chromium checks exercised
real video/data channels, actual tab hide/show, discarded drafts, a 12-second
publisher outage, and reopening the viewer. That lifecycle test uses a synthetic
publisher. Separate live VM checks received 1600×900 video, renewed the viewer
lease for three minutes, and used browser pointer/text/keyboard input to create
and list a marker file in the VM terminal. A live factory restart with a
12-second shutdown gap cleared the old frame and resumed decoded video with
the new publication in about 18 seconds, retaining selection without acquiring
control.
The frame-transport Chromium fixture also exercised real JPEG decoding,
pointer/text/keyboard input, actual tab hide/show, and a 12-second publisher
outage with zero WebRTC peers or ICE requests. The host and broker remain
responsible for provisioning and publishing Cloudflare desktops.

## Boundaries

The Vite application has one React root and owns browser presentation, routing,
and browser-agent integration. Documentation source lives in `docs/src/pages`
and is rendered in that application.

The Cloudflare Worker in `worker/` owns public HTTP routing and proxies its
scoped backend services: managed-agent access, Connect APIs and dialog,
repository and thread Git data, evaluation reads, and credential-backed model
operations. Provider credentials stay behind Worker bindings and are not part
of browser configuration.

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

Use the checkout-level operator interface in [AGENTS.md](../../AGENTS.md) for
local development, checks, deployment, and verification. This package exposes
the supporting `dev`, `build`, `test`, `typecheck`, `check:docs`, and `deploy`
scripts, but the repository instructions own how they are run.
