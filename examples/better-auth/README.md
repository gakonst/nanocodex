# Better Auth host-principal example

This external-consumer example connects an existing, GitHub-backed Better Auth login to a
Nanocodex hosted agent. Better Auth owns the login and its D1 records. Nanocodex receives a
short-lived, single-use exchange derived on the server from the active session; the browser never
receives the Better Auth session token, GitHub OAuth tokens, GitHub client secret, or Nanocodex
host-project secret.

The UI includes a concrete durability check: run turn one, reload the page, and run turn two.
`Client.connection.reconnect()` obtains a fresh same-origin host exchange, validates the retained
grant, and opens the same hosted agent for the follow-on turn.

## Requirements

- Node.js 22.15+, 24, or 26 and npm.
- A Cloudflare account with Workers and D1.
- A GitHub OAuth app. Its callback URL must be exactly
  `http://localhost:5173/api/auth/callback/github` locally and
  `https://YOUR_HOST/api/auth/callback/github` in production. The GitHub integration must make the
  user's email available to Better Auth.
- A Nanocodex host project registered with the exact app ID, app origin, issuer
  `better-auth:github`, and tenant (the GitHub OAuth client ID) used here. Keep the issued project
  secret on the Worker.

This directory is an independent npm package. Run its commands here; do not add it to the pnpm
workspace.

## Local setup

1. Install exactly the locked dependencies.

   ```sh
   npm ci
   ```

2. Create the D1 database, then replace the zero `database_id` in `wrangler.jsonc` with the ID
   printed by Wrangler. The zero ID deliberately keeps a fresh checkout usable for local-only D1.

   ```sh
   npx wrangler d1 create nanocodex-better-auth-example
   npx wrangler d1 migrations apply AUTH_DB --local
   ```

3. Copy the secret template and fill in all four values.

   ```sh
   cp .dev.vars.example .dev.vars
   node --input-type=module --eval "import { randomBytes } from 'node:crypto'; console.log('v1.' + randomBytes(32).toString('base64url'))"
   ```

   Use the generated `v1.` value as `BETTER_AUTH_SECRET`. Set the GitHub OAuth client ID and secret,
   plus the secret issued for the Nanocodex host project. `.dev.vars` is ignored; the example values
   are placeholders, not credentials.

4. Set `NANOCODEX_HOST_APP_ID` in `wrangler.jsonc` to the registered Nanocodex app ID. Keep
   `NANOCODEX_HOST_APP_ORIGIN` at `http://localhost:5173` for local Vite, then start the Worker and
   browser app together.

   ```sh
   npm run dev
   ```

Sign in with GitHub, approve the hosted Nanocodex connection (including ChatGPT), run turn one,
reload, and run turn two. A successful second answer demonstrates durable state across a fresh
principal exchange and Connect session reconnect.

## Production deployment

Create a production D1 database, replace `database_id`, and set
`NANOCODEX_HOST_APP_ORIGIN` to the exact HTTPS deployment origin. Update both the GitHub callback
and Nanocodex host-project registration to that same origin before deploying.

Apply the migration and upload each secret explicitly:

```sh
npx wrangler d1 migrations apply AUTH_DB --remote
npx wrangler secret put BETTER_AUTH_SECRET
npx wrangler secret put BETTER_AUTH_GITHUB_CLIENT_ID
npx wrangler secret put BETTER_AUTH_GITHUB_CLIENT_SECRET
npx wrangler secret put NANOCODEX_HOST_PROJECT_SECRET
npm run deploy
```

The GitHub client ID is used server-side as the stable tenant namespace even though it is not a
credential. Keeping it in Worker secrets prevents the browser configuration endpoint from growing
beyond the public Nanocodex app ID and exact origin.

## Routes and security boundary

The Worker exposes only these application API routes:

| Route | Method | Browser-visible result |
| --- | --- | --- |
| `/api/config` | `GET` | `{configured}` plus public app ID/origin when ready |
| `/api/session` | `GET` | `{authenticated: boolean}` |
| `/api/auth/sign-in/social` | `POST` | Better Auth GitHub redirect response |
| `/api/auth/callback/github` | `GET` | Better Auth OAuth callback response |
| `/api/auth/sign-out` | `POST` | Better Auth sign-out response |
| `/api/nanocodex/host-principal` | `POST` | `{token, expires_at}` only |
| `/api/nanocodex/host-principal` | `DELETE` | empty `204` after exact-session revocation |

All other Better Auth routes—including session listing and provider token read/refresh routes—are
denied before `auth.handler()` runs. Sign-in accepts only GitHub and an exact same-origin callback.
State-changing browser requests require the configured `Origin` and same-origin Fetch Metadata.
The GitHub callback remains a GET because Better Auth validates its encrypted OAuth state.

The Worker maps the verified Better Auth session to four bounded opaque claims:

```text
issuer   = better-auth:github
tenant   = BETTER_AUTH_GITHUB_CLIENT_ID
subject  = session.user.id
session  = session.session.token
```

`HostPrincipal.handler()` creates exchanges from those claims. On logout, the browser first aborts
and awaits any in-flight Connect attempt, then calls `DELETE` while the Better Auth cookie is still
valid, clears the Nanocodex Connect session, and only then calls Better Auth sign-out. A failed
revocation blocks provider logout so the user can retry without silently losing the session needed
to fence the grant.

Better Auth stores users, sessions, encrypted OAuth tokens, and verification records in D1.
`account.encryptOAuthTokens` is enabled and `storeAccountCookie` is disabled, so OAuth tokens do not
move into browser-readable app state. The migration in `migrations/0001_better_auth.sql` matches the
pinned Better Auth version; regenerate and review it when upgrading Better Auth rather than editing
deployed tables ad hoc.

## Verification

```sh
npm run typecheck
npm test
npm run build
```

The focused tests cover four-claim mapping, secret-free public responses, exact auth-route and
provider allowlisting, same-origin exchange/revocation, fail-closed sessions, canonical secret
validation, and logout ordering under an in-flight Connect race. A production rollout should also
exercise the real GitHub callback and the two-turn reload journey against the deployed Worker.
