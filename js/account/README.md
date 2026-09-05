# Nanocodex web

`nanocodex-web` is the Cloudflare-hosted React application for Nanocodex. It
provides the public site, documentation, account and Connect experiences, and
live product demonstrations. It consumes the local `nanocodex`,
`nanocodex-react`, and `nanocodex-terminal` packages; it is not an SDK runtime
or a second agent backend.

## User surfaces

- **Home** shows an ephemeral browser-agent demo. **Durable Agent** retains a
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

Use the checkout-level operator interface in [AGENTS.md](../../AGENTS.md) for
local development, checks, deployment, and verification. This package exposes
the supporting `dev`, `build`, `test`, `typecheck`, `check:docs`, and `deploy`
scripts, but the repository instructions own how they are run.
