# Privy host authentication for Nanocodex

This standalone Cloudflare/Vite example lets a browser user keep an existing
Privy login while connecting a hosted Nanocodex agent. It demonstrates the
`Principal.host` browser adapter and `HostPrincipal` server exchange, automatic
grant reconnect after reload, two turns against one durable conversation, and
revoke-before-logout fencing.

## Security boundary

The browser calls the exact same-origin `/api/nanocodex/host-principal` route.
On `POST`, the Worker verifies the `privy-token` cookie with
`PrivyClient.utils().auth().verifyAuthToken` and passes only the bounded
`{ issuer: "privy", tenant: PRIVY_APP_ID, subject: user_id, sessionId: session_id }`
claims to `HostPrincipal`. The browser receives only a short-lived opaque
`{ token, expires_at }` exchange.

`PRIVY_APP_SECRET` and `NANOCODEX_HOST_PROJECT_SECRET` remain Worker-only. The
provider SDK owns its session token; this example never copies it into public
config, HTML, Connect requests, application state, or Vite variables. The Worker
keeps the verified claims only in an AES-GCM-sealed, HttpOnly, same-site cookie
so it can revoke the exact old session after provider expiry or a cold-start
account switch. Public `/api/config` contains only the identifiers, exact origin,
and public Nanocodex service URLs needed by the browser.

The **Securely log out** action aborts and awaits an in-flight connect or turn, revokes
the exact Nanocodex host session while the old Privy cookie is still valid,
clears the persisted Connect session, and only then calls Privy logout. A failed
revocation blocks provider logout so it can be retried. Use the same ordering
before adding any account-switch control. Provider-driven logout and account
replacement use the sealed old-session cookie to apply the same revocation
fence before local state is reused.

## Configure Privy and Nanocodex

Use Node 24 or newer. In the Privy dashboard, create a development app, allow
the exact local/deployed origin, and enable cookie-backed browser sessions.
Production cookie mode requires Privy's verified custom app-domain setup; a
random `workers.dev` preview is not production-cookie evidence.

Copy `.dev.vars.example` to the ignored `.dev.vars` file and replace both
secrets. Update the public values in `wrangler.jsonc`:

- `PRIVY_APP_ID`: the matching Privy app ID.
- `NANOCODEX_HOST_APP_ID`: the app ID registered with Nanocodex.
- `NANOCODEX_HOST_APP_ORIGIN`: the exact browser origin, with no trailing slash.
- `NANOCODEX_API_URL` and `NANOCODEX_CONNECT_DIALOG_URL`: the public service and
  approval-dialog locations.

Register this project with the Nanocodex managed service. It needs the exact
app ID, origin, issuer `privy`, tenant (the Privy app ID), and only the
base64url SHA-256 digest of the project secret:

```sh
printf %s "$NANOCODEX_HOST_PROJECT_SECRET" | openssl dgst -sha256 -binary |
  openssl base64 -A | tr '+/' '-_' | tr -d '='
```

The registry entry has this shape:

```json
{
  "app_id": "privy-example",
  "app_origin": "http://localhost:5173",
  "issuer": "privy",
  "tenant": "<PRIVY_APP_ID>",
  "secret_sha256": "<43-character-base64url-digest>"
}
```

For deployment, store secrets without echoing them into shell history:

```sh
npx wrangler secret put PRIVY_APP_SECRET
npx wrangler secret put NANOCODEX_HOST_PROJECT_SECRET
```

Deploy the managed registry and Connect API before deploying this Worker.

## Run and verify

```sh
npm install
npm test
npm run build
npm run dev
```

Open `http://localhost:5173`, sign in, and approve the hosted ChatGPT capability.
Run **Turn 1 · remember**, reload the page, wait for the reconnect status, then
run **Turn 2 · recall**. The second reply should be `PRIVY_HOST_OK`, showing both
turns used the same server-retained conversation. Finally use **Securely log
out**; do not use a provider-only logout or account switch.

Before production rollout, repeat that exact journey on the verified Privy app
domain and inspect browser network/storage: only the opaque exchange and grant
session may appear, and neither provider/project secret nor verified Privy
claims may be present.
