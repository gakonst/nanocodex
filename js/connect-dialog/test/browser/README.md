# Connect modal browser journey

Run from the repository root after installing dependencies and Playwright Chromium:

```sh
pnpm --filter nanocodex-connect-protocol run build
pnpm --filter nanocodex-connect-ui run build
pnpm --filter @nanocodex/connect-dialog run test:browser
```

The Vite fixture imports the built public `ConnectOnboarding` component and the shared stylesheet. It uses a synthetic request and host response callback. Only the account/SMS and external provider service transport is replaced by local HTTP endpoints; no SMS, account creation, or external authorization occurs. The `modal.nanocodex.localhost` hostname exercises the managed SMS flow instead of the loopback-only WebAuthn path. Playwright supplies its host resolution rule.

The journeys cover invalid phone input, a delivery failure and recovery, malformed and rejected codes, successful sign-in, explicit consent, consent cancellation, initial cancellation, and Escape. Light/dark desktop, mobile, short, and 320×360 keyboard-sized viewports assert edge-to-edge full-page geometry, no horizontal overflow, input/button keyboard focus, consent heading focus with reset scroll position, and mobile action footers that remain at the viewport bottom while content scrolls. Phone/code and consent omit account-switch actions. Synthetic HTTP requests, host receipts, screenshots, and traces are written to the ignored repository `output/connect-full-page/` directory.

This verifies the real React rendering and user flow. It does not test the external SMS provider, production account backend, or the SDK iframe transport. A separate journey opens the real SDK popup and verifies the centered two-column desktop authorization layout.

Four connection-list journeys also cover grouped GitHub/Google rows, the approval gate for missing connections, keyboard focus visibility, and cancellation.

The connection journeys continue through a synthetic Google provider popup, the real origin/source-validated completion message, connector refresh, and explicit Allow access. They capture requested access, waiting for Google, and approval ready on desktop/mobile in both themes. Cancellation and a Gmail-only partial grant keep approval disabled; connecting Calendar still requires explicit final app approval. The provider fixture is clearly labeled local test content, not a reproduction of Google’s consent page.

Appearance journeys verify developer color scheme, accent contrast, font family,
and corner radius across sign-in and approval on desktop/mobile. An SDK popup
journey exercises the URL transport through the hosted parser into the real UI.
Malformed CSS-bearing values fall back to native defaults.

Existing-session journeys use a fixture-only HttpOnly cookie and server-owned
session states. A persistent session with a canonical address opens app consent
without SMS in the dialog, SDK popup, and wizard. No authorization POST or host
approval occurs before Allow access. Coverage includes cancellation,
consent without account-switch actions, an account change in another tab requiring new consent,
initial expiry, expiry during approval, unavailable-session retry, anonymous or
address-less sessions, spending/fresh-auth policy gates, complete permission
visibility for focused requests, and request replacement during delayed hosted
authorization/exchange, closing Connect while authorization is pending, and
React StrictMode effect replay. These run the real public React component against local
HTTP account endpoints; they do not validate production cookie issuance or the
production account service. Per-journey synthetic request/receipt JSON,
screenshots, and Playwright traces are retained under the same ignored output
directory. Run only these journeys with:

```sh
pnpm --filter @nanocodex/connect-dialog run test:browser session.spec.ts
```

## Login cookie and latency

The HTTPS cookie journey runs the actual account proxy, managed auth routes and
SQLite Durable Objects in workerd, with production React and a real browser.
Only Twilio, wallet metadata and requesting-app metadata are synthetic. It checks
SMS cookie issuance, HttpOnly/Secure/SameSite attributes, reuse in a second page,
explicit consent, logout, expiry and the session route's `Server-Timing` header:

```sh
node --test js/managed/test/connect-browser-cookie-journey.test.mjs
```

Evidence is retained in `output/cookie-profile/`; no session cookie values are
included. This journey does not send live SMS or approve a production grant.

To profile the production-built UI with reproducible 0/150ms API latency:

```sh
pnpm --filter @nanocodex/connect-dialog run build
cd js/connect-dialog
pnpm exec vite build --config test/browser/profile.vite.config.ts
PROFILE_LABEL=baseline node test/browser/profile.mjs
```

Use distinct labels before/after a change. `output/connect-profile/<label>/`
contains API start/completion times, resource timings, screenshots and traces.
The regular Connect fixture exercises the real component, not SDK popup
transport. Automated milestones include browser-driver polling overhead; use
request dependency timings to compare network waits. This synthetic profile
cannot measure production backend latency. On successful `/v1/me` responses,
`connect_session`, `connect_metadata` and `connect_total` report server durations
in milliseconds without credentials or account identifiers.
