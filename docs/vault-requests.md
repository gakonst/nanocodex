# Vault requests from agents and native clients

Saved credentials are resolved by the egress broker, immediately before an
outbound request. Code Mode, the native CLI and the Rust client pass the same
opaque Vault ID and public templates. They do not retrieve the secret, put it in
a child process environment, or send it through model-visible tool arguments.

Direct account access with `agents:write` and `tools:use` is required. Connect
grants and shared-thread guests cannot use this interface. Vault ownership and
deletion are checked on every call. Browser requests additionally require the
same-origin mutation check.

## Account management API

The TUI and mobile app use the same account endpoints as account settings.
Authenticate with the existing account API key; credential writes and subscription
login polling require a persistent account with `agents:write` and `tools:use`.
These routes accept neither Connect grants nor shared-thread credentials.

| Operation | Endpoint |
| --- | --- |
| Safe Vault, SSH, and model-connection metadata | `GET /v1/credentials` |
| Save a login, API key, card, address, or phone | `POST /v1/credentials/vault/{kind}` |
| Delete an exact Vault item | `DELETE /v1/credentials/vault/{kind}/{id}` |
| Update a login's website hint | `PUT /v1/credentials/vault/login/{id}/origin` |
| Generate or import an SSH identity | `PUT /v1/credentials/ssh/{reference}` |
| Remove an SSH identity | `DELETE /v1/credentials/ssh/{reference}` |
| Start or poll ChatGPT device sign-in | `POST` or `GET /v1/credentials/chatgpt/login` |
| Disconnect ChatGPT accounts | `DELETE /v1/credentials/chatgpt` |
| Save or remove model API access | `PUT` or `DELETE /v1/credentials/openai` |
| Save a privately captured provider item | `POST /v1/vault/store` |
| Read or refresh a provider card | `POST /v1/vault/card` |

`kind` is `login`, `api_key`, `card`, `address`, or `phone`. Secret entry belongs
in a native private form that sends directly to the API. Responses return safe
metadata, not passwords, API keys, full card numbers, or private SSH keys. SSH
creation accepts `generate: true` plus the hostname, port, username, and trusted
host fingerprint; generation happens in the broker and refuses to replace an
existing reference. Import sends a PEM key privately instead of `generate`.

`/v1/vault/store` accepts `capture_id`, a stable UUID `operation_id`, optional
`name`, and optional `address_vault_id`. `/v1/vault/card` accepts `operation`
(`status`, `balance`, or `refresh`) and exactly one of `capture_id` or `vault_id`.
Refresh additionally requires a stable UUID `operation_id`. Both routes share
the agent tools' private broker and fixed receipts. Reuse the same operation ID
to recover a save or refresh; never issue a replacement purchase. A
`balance_pending` receipt means the issuer accepted a refresh, not that funds
arrived. The credential-capture ingress remains private to the trusted host.

Connector management uses `GET /v1/connectors/catalog`, `GET /v1/connectors`,
`POST /v1/connectors/{provider}`, and
`DELETE /v1/connectors/{provider}/connections/{connection_id}`. Native clients
use owner keys with `api_keys:write` and `tools:use`. OAuth start takes a
same-origin `return_to` path; native clients can forward the provider callback
with their account key. The broker still verifies one-use state, PKCE, provider,
and account binding. Successful account status establishes the connection.
Cloudflare instead takes a saved `vault_id` and, for account-owned tokens,
`account_id`. WhatsApp uses its private pairing panel; see
[WhatsApp](whatsapp-connector.md). Custom MCP connection management uses
`/v1/connectors/mcp-connections` and the exact connection ID's `start`, `callback`,
and deletion routes. External OAuth or provider consent can still require a
browser; account management itself does not require opening web settings.

## Native clients

The TUI opens private forms with `/vault add login|api_key|card|address|phone`,
`/vault add openai`, and `/vault ssh-add REFERENCE`. `/vault list` reads metadata;
`/vault delete KIND ID` and `/vault ssh-remove REFERENCE` remove exact items.
`/vault card status|balance ID`, `/vault card refresh ID OPERATION_UUID`, and
`/vault store CAPTURE_ID OPERATION_UUID` use fixed provider receipts. Use
`/vault capture status|balance CAPTURE_ID` before an issued card is saved.
These commands execute locally and never put secret values into a model prompt.

The standalone `nanocodex2 vault` CLI provides `list`, `add KIND`, `delete KIND ID`,
`ssh-save`, `ssh-remove`, `store`, `card`, and the existing `request` and
`ssh-targets` commands. `add` requires an interactive terminal and masks each
field. `ssh-save --help` describes broker generation and optional PEM-file
import. Secret values are never command arguments. Provider mutations require
an explicit stable operation UUID; an uncertain response must be reconciled.

On iPhone/iPad, open Connectors → Vault for native item forms, SSH identities,
Cloudflare token selection, capture saving and observed card balances. ChatGPT
and Claude have native connector screens. OAuth consent can still open the
provider's browser; web account settings are not needed for Vault management.
See [connector commands](connector-accounts.md).

## Code Mode

```js
text(await tools.vault_request({
  vault_id: "SAVED_ITEM_ID_FROM_ENVIRONMENT",
  url: "https://service.example.com/action",
  method: "POST",
  headers: {
    authorization: "Bearer {{NANOCODEX_VAULT_API_KEY}}",
    "content-type": "application/json"
  },
  body: JSON.stringify({ name: "example" }),
  body_encoding: "json"
}));
```

Use the exact saved ID returned by `environment`; the example ID is illustrative.
The broker returns `{ "status": 201, "ok": true }` for a successful destination
response. Response bodies, headers, cookies and generated authentication tokens
are not returned. A status receipt does not prove a business operation completed.
There are no automatic retries. `vault_request_outcome_unknown` means dispatch
may have occurred; inspect the destination independently before trying again.

The same request JSON is accepted by the authenticated `POST /v1/vault/request`
API and `ManagedClient::vault_request`. The native CLI can read it from a file or
stdin with `nanocodex2 vault request --file FILE`. That file contains references
and templates only. It uses the CLI's existing Nanocodex account authentication;
no separate password prompt is opened for a saved Vault item.

Existing `/brain` HTTP calls using `x-nanocodex-vault-id` and supported placeholders
continue to work. Native third-party commands do not automatically inherit the
hosted HTTP proxy. Use the explicit Vault request API or CLI on a Hand. This
change does not turn native process environment variables into secret values.

## Public SSH targets

`nanocodex2 vault ssh-targets` uses the CLI's existing account authentication to
read `GET /v1/credentials`. It prints a JSON array containing only `reference`,
`hostname`, `port`, `username`, `host_key_sha256`, and optional `public_key`.
A legacy target whose public key is unavailable omits that field. The command
is read-only; it returns no private keys or other Vault entries. HTTP and decode
errors discard arbitrary response bodies.

## Substitution and signing

Login items support `USERNAME`, `PASSWORD` and `BASIC`; API-key items support
`API_KEY`; card items support `CARD_NUMBER`, `EXPIRY_MONTH`, `EXPIRY_YEAR`, `CVV`
and `BILLING_ZIP`. Address items support `ADDRESS_LINE_1`, `ADDRESS_LINE_2`,
`CITY`, `STATE`, `ZIP` and `COUNTRY`; phone items support `PHONE_NUMBER`.
Each is written as `{{NANOCODEX_VAULT_NAME}}` in a header or body template. Only the selected item's fields are available. The destination is
fixed public HTTPS; redirects are not followed. Existing reserved provider
boundaries remain in force.

Choose `body_encoding: "json"` or `"form"` for structured bodies. The broker parses
the template, substitutes string values and serializes it again, preserving
quotes, newlines, ampersands and other special characters in a credential.
Object keys and form field names cannot contain placeholders. `raw` retains
literal substitution for protocols that need it. URLs are not substituted.

For a request signature, store the signing key in an API-key Vault item and add:

```json
{
  "signing": {
    "algorithm": "HMAC-SHA256",
    "message": "public canonical message to authenticate",
    "encoding": "hex",
    "key_encoding": "utf8"
  },
  "headers": {
    "x-signature": "{{NANOCODEX_VAULT_SIGNATURE}}"
  }
}
```

Supported algorithms are HMAC-SHA256, HMAC-SHA512, RS256, ES256 and EdDSA
(Ed25519). Message signatures default to base64url; hex and base64 are optional.
HMAC keys default to UTF-8. Asymmetric keys default to unencrypted PKCS8 PEM;
explicit base64 or hex selects DER bytes. The private key is imported as a
non-extractable WebCrypto key inside the broker.

For an asymmetric JWT, replace `message` with
`jwt: { header: { kid: "public-key-id" }, payload: { /* public claims */ } }`
and use `Bearer {{NANOCODEX_VAULT_JWT}}`. Supply the intended audience, issuer and
expiry in the claims. The algorithm is bound to the signing request; conflicting
`alg`, `crit` and `b64` headers are rejected. JWT and message modes are mutually
exclusive. Neither the signature nor the JWT is exposed to the caller: the
broker inserts it into the exact outbound request and returns only its status.

## Private browser field injection

`browser_login_inject_fields` uses a retained login request ID;
`browser_vault_inject_fields` uses the existing named Vault browser identity.
Both require a fresh private snapshot ID, a stable operation ID, and
`fields: [{ref, vault_id, field}]`. Each mapping selects one safe snapshot reference
and one field of an account-owned Vault item. Use the tool's declared field enum.
All five item kinds are supported, subject to compatible visible browser controls.

The broker and private browser host resolve and inject values without returning
them to the model. The result is a fixed fill status. Re-read the private snapshot
before taking a later action. Injection does not submit a form or authorize a
purchase. Document changes, deleted items, wrong owners and incompatible field
mappings fail closed. Replaying the same operation retrieves its receipt instead
of injecting again. An uncertain outcome must not be retried with a new ID.

The [terminal private overlay](architecture/tui-private-input.md) uses the same
private destination bindings and can save newly entered reusable fields with a
visible opt-out. Verification codes and card security codes are not automatically
saved; a stored card without a security code cannot supply that field.

## Apple authentication and artifact signing

This API supports broker-side ES256 authentication for an already-provisioned
App Store Connect API key. It does not convert an Apple account password into
such a key, issue Apple certificates, or sign an IPA.

Stock xtool password login performs GrandSlam SRP, Anisette and optional 2FA
before issuing authenticated requests. Substituting an HTTP header cannot
replace that local cryptographic protocol. It needs a separate trusted protocol
adapter. Native app signing likewise needs the existing local signing identity
or a trusted artifact-signing integration. No raw Vault secret or browser cookie
export is provided as a workaround. See [iPhone delivery](iphone-delivery.md).

## Validation

The local Vault journeys use synthetic accounts and keys, actual Worker storage,
public managed HTTP authentication and the shipped native CLI. The controlled
upstream verifies generated signatures independently and attempts to echo
credentials. Client results and traces must contain only the closed receipt.
Tests also cover wrong owners, deleted items, denied destinations, malformed
signing requests, escaping and uncertain dispatch without replay.

## Provider-issued cards

The managed host's default Mercator MCP transport privately captures Laso US
card issuance results before projecting either MCP text or structured results
into model context and tool history. The supported plan contains one node for
service `x402-laso-finance-9ad65ae7`, `GET /get-card`, with `amount` and
`format: "json"`. Issuance requires user authorization for the purchase and the
normal Mercator payment approval. Keep the same `idempotency_key` when recovering
an uncertain job. A repeated operation returns its known job for `get_job` recovery
without dispatching issuance again. If the first response was lost before its job
ID was recorded, the receipt remains `outcome_unknown`; reconcile through
Mercator before further purchases. Do not issue another card to recover a failed
save.

The host binds the account, request, node and returned card ID in encrypted
account storage, so a pending job can resume from another conversation. Laso's upstream
payer identity is not asserted by this integration. It reads only that exact
card ID and never enumerates the token holder's cards. Provider tokens and card
credentials remain in the encrypted credential broker. This boundary applies to
the default hosted transport; Hand-routed and third-party MCP connections do not
have this capture policy and must not be used for secret-bearing card requests.

`vault_store` accepts an opaque `capture_id` and stable `operation_id`, with an
optional `address_vault_id` for a saved billing address. It accepts no raw secret.
The authorized issuance workflow attempts the save automatically. A pending
card can be resumed using `provider_card`; an absent billing ZIP remains
`awaiting_billing_address` until a real provider or saved address is available.
Only a `saved` receipt with `vault_id` confirms Vault storage.

`provider_card` accepts either `capture_id` or `vault_id` and an operation of
`status`, `balance`, or `refresh`. Balance receipts include the observation time
and the provider update time when supplied. Refresh requires a stable UUID and
requests an asynchronous issuer update; acceptance does not mean the balance is
fresh or a payment has been credited. The tool never purchases, funds, or
charges for authentication. Connect grants and shared guests cannot use these
private continuations.

Ordinary Mercator history and results remain available. For an older unbound
job without payment service metadata, read its history or request its plan with
`get_job(include_plan: true)` to establish the service before accessing details.
Unbound Laso jobs remain private and cannot be adopted by supplying a job ID.
