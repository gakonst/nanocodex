# Astra one-shot

This is a standalone third-party Nanocodex Connect application. It proves the
OAuth-like integration while keeping all trial state, MPP settlement, and
sponsor-account access outside the main Nanocodex application.

The visitor signs into Nanocodex Connect. The app sends the resulting app-scoped
grant token back to the Connect API on every protected request and accepts only
an active `agent.run` grant for this exact app ID and origin. The stable
Nanocodex account ID selects one `AstraTrial` Durable Object, so an account can
claim at most one prompt. Account creation, passkey/SMS verification, and login
rate limits remain owned by Nanocodex Connect rather than a parallel Twilio
identity system in this app.

The payment route is an MPP `tempo/charge` endpoint. Production challenges ask
for 50 MACH, require the paying address to match the connected Nanocodex account,
accept pull mode only, and request fee sponsorship from the Tempo relay. The
development environment issues a zero-value proof challenge: the same wallet
must sign, but no token transfer occurs.

Connect registers the production app ID and origin as one high-value OAuth
client. Its delegated key is limited to 50 MACH per day and one
`transferWithMemo` scope whose sole recipient is displayed in the consent UI.
That grant cannot use Connect's generic MPP route. Every other Connect client
keeps the existing 0.25-MACH request and 10-MACH daily policy. Local proof mode
uses that ordinary low-value policy because it transfers no tokens.

After payment, the Durable Object uses a server-only managed API key belonging
to the sponsor account. It creates a fresh managed agent with the complete fixed
settings below and submits exactly the visitor's one prompt:

```json
{
  "model": "gpt-6-astra",
  "thinking": "max",
  "reasoningMode": "standard",
  "fastMode": false
}
```

There is deliberately no public managed API proxy. The browser cannot select a
model, supply an agent ID, change settings, attach tools, read the sponsor's
agent list, or call another managed route. The sponsor key is read only inside
the Worker. Internal managed IDs are not returned by the public trial API.

## Local proof mode

```sh
cp .dev.vars.example .dev.vars
npm install
npm run dev
```

Set `NANOCODEX_ASTRA_MANAGED_API_KEY` to the sponsor account's managed key and
replace `NANOCODEX_ASTRA_MPP_SECRET` with at least 32 random characters. The
development recipient may be any valid Tempo address because a zero-value proof
does not transfer funds. `TEMPO_MPP_API_KEY` is not used by zero-value proof
mode, but is required by production.

Open `https://localhost:8787`. Connect runs as a top-level popup, which is the
third-party OAuth-style boundary; the app cannot embed the Nanocodex account UI.
Wrangler serves local TLS because Connect accepts dynamic third-party apps only
from secure origins. If Wrangler uses another port or hostname, change
`NANOCODEX_CONNECT_APP_ORIGIN` in the development environment to the exact
browser origin before connecting.

## Production configuration

The Cloudflare production workflow syncs these GitHub repository secrets to the
Worker after each deployment:

```sh
NANOCODEX_ASTRA_MANAGED_API_KEY
NANOCODEX_ASTRA_MPP_SECRET
TEMPO_MPP_API_KEY
```

`TEMPO_MPP_API_KEY` must have the `mpp:write` scope. The MACH policy settlement
address is committed as `NANOCODEX_ASTRA_MACH_RECIPIENT`. Confirm that
`NANOCODEX_CONNECT_APP_ORIGIN` exactly matches the deployed HTTPS origin. The
app fails closed when any sponsor, payment, origin, or Connect configuration is
absent or malformed.

`Add $50 MACH` invokes Connect's existing MACH funding dialog for the connected
account. The prompt submission itself uses MPP; its fee-payer transaction is
relayed server-side and the MPP receipt reference is retained with the one-shot
state.
