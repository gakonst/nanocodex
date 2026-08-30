# Connect with existing authentication

These three server adapters keep the website's existing login as the source of
truth and use Nanocodex only for connector authorization:

- `auth0/session.mjs` reads the server-side Auth0 session and bridges `user.sub`.
- `better-auth/session.mjs` resolves the incoming request through
  `auth.api.getSession` and bridges `session.user.id`.
- `privy/session.mjs` verifies the Privy access token on the server and bridges
  the returned `userId`.

Each adapter plugs into the same application-owned route:

```js
import { Session } from "nanocodex/connect/server";
import { createAuth0SessionRoute } from "./auth0/session.mjs";

const sessions = Session.create({
  appId: "acme",
  appOrigin: "https://app.example.com",
  secret: process.env.NANOCODEX_PROJECT_SECRET,
});

export const POST = createAuth0SessionRoute({ auth0, sessions });
```

The browser is provider-independent:

```js
import { Client, Identity } from "nanocodex/connect";

const connect = Client.create({
  appId: "acme",
  identity: Identity.host(),
});

await connect.connect({
  capabilities: { cloudAccounts: { github: true, gmail: true } },
});
```

Run the provider-boundary tests with `npm test` from this directory. They use
the real Nanocodex session handler and mock only the external auth SDK response
and Connect HTTP endpoint.
