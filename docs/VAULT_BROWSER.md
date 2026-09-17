# Secure Vault intake and browser login

`request_vault_intake` renders an inline card in managed web and iPhone/iPad chat.
The card opens a client-owned form. Login passwords, API keys, card values,
addresses and phone numbers are submitted directly to the authenticated Vault
endpoint, never as conversation or tool arguments. Only an allowlisted saved
receipt is sent back into the original conversation. Closing the form or changing
accounts discards its input. Unknown save outcomes are not automatically retried.

For an existing login without a website binding, request
`{ operation: "authorize_origin", kind: "login", vault_id, origin }`.
The form retrieves the actual saved item's name, displays the exact HTTPS website,
and updates only `browser_origin`; the user does not reenter the password. Approval
replaces the prior website binding. There are no wildcard or subdomain grants.
Legacy entries remain usable for their existing HTTP Vault operations, but private
browser login requires website approval.

Account-session mutations retain same-origin protection. Native Vault forms use
persistent account API keys with `agents:write` and `tools:use`; Connect grants,
anonymous accounts and read-only keys cannot mutate Vault. Website approval and
Vault resolution use the current account's broker. The browser-only materialization
RPC is reachable through the managed service binding, never model HTTP egress.

## Private login workflow

1. Open the requested site with `browser_execute` and obtain its target ID.
2. Call `browser_vault_status` with the named item, target and approved exact origin.
   The result contains only fixed supported-field selectors and a small status enum.
3. Call `browser_vault_fill` with a username selector, password selector or both.
   `submit: true` performs a native same-origin POST; `false` fills without submitting.
4. Repeat the private status/fill calls for a two-step login. A submitted form or
   absence of supported fields is **not** proof of successful authentication.
5. `browser_vault_close` discards the retained browser session and allows ordinary
   browsing to start fresh.

The host obtains credentials privately, opens its own provider CDP connection, and
runs a fixed function in an isolated top-frame world. Selectors are data. The
function checks exact origin, uniqueness, visibility, input type, form ownership,
and a same-origin POST action before setting any value. Username-only steps do
not send the password into the page. Provider errors and return values are bounded
to fixed error/status output. No privileged command passes through Code Mode's
execution log.

Before injection, a durable session quarantine is written. Normal browser tools
remain blocked for that credential session, even after navigation or Worker
restart: a new page can echo credentials and history can restore a filled page.
Only private status/continuation for the same item, target and origin is allowed.
This intentionally does not enable general autonomous browsing in a signed-in
session. MFA, CAPTCHA, JavaScript-only/SPA forms, cross-origin login transitions,
and forms requiring submit-button callbacks need direct user interaction. No
live account password is required for testing.

## Validation and rollout

The Chrome fixture `js/managed/test/browser-vault.chrome.mjs` runs a temporary
HTTPS two-step form with fake credentials, and rejects cross-origin, GET, hidden
and duplicate fields. Worker tests cover owner/origin binding, deletion, rejected
model access, metadata projection, durable quarantine and cancellation. Web and
Swift tests cover secure form contracts and metadata-only receipts.

Deploy egress first, managed second, and account last. Ship the Apple app update
for native cards. Older clients do not gain the form from a Worker-only rollout.
The tools are unavailable in an already-running conversation until its tool catalog
is refreshed. Production login has not been exercised by the fixture tests.
