# Standalone agent services

Nanocodex services can be used from another agent runtime without creating a Nanocodex agent, conversation, or model turn. The `nanocodex/services` entry point has no WASM dependency. The account API and Connect share the same credential broker and phone backend. See the [JavaScript quickstart](../js/nanocodex/README.md#standalone-account-services) and [React controls](../js/nanocodex-react/README.md#standalone-vault-and-phone-services) for integration examples.

## Authentication and authority

An account API key calls `/v1/services` on the account origin. Reads require `data:read`; Vault requests require `tools:use`; phone intents require `data:write` and `tools:use`. These routes do not require `agents:write`. Browser mutations require the authenticated account session and exact same-origin `Origin` header. Service principals and Connect tokens cannot use the broader account routes.

A third-party application uses a Connect grant under `/v1/grants/:grantId/services`. Each request validates the grant bearer, registered app ID, exact app origin, expiration, revocation and current host principal when applicable. A `services.use` connection does not provision an agent. The app requests explicit service scope, encoded in the signed authorization resources:

```json
{
  "vault": { "ids": ["selected-vault-id"], "origins": ["https://service.example"], "request": true },
  "phone": { "numberIds": ["selected-number-uuid"], "read": true, "provision": false, "release": false }
}
```

A selected Vault item may be used only for the listed HTTPS origins. A TOTP item also enforces its own saved origin. Listing returns only selected items/numbers. Existing grants acquire no new service permissions automatically. Provisioning a number does not automatically add it to a grant: the user grants access to its returned ID separately. OAuth connector requests retain their existing connection selections and provider scopes.

The hosted Vault and phone pages provide private enrollment and human approval. Callback messages contain a fixed metadata projection, bound to the destination origin, popup window and random state; they never contain seeds, codes, credentials, or cookies. The hosted pages retain their framing protections. Embedded SDK controls open those pages in a trusted popup.

## Vault and TOTP

Account routes:

- `GET /v1/services` — service catalog.
- `GET /v1/services/vault` — safe metadata.
- `GET /v1/services/vault/:id` — one account-owned item's metadata.
- `POST /v1/services/vault/request` — brokered request; returns only `{status, ok}`.

Connect exposes its scoped catalog, Vault listing, item metadata and request operation under the grant's services path. Private enrollment and deletion remain on the existing account Vault routes. Use `/vault?service=totp` for authenticator enrollment; the ordinary `/connect/vault` page also manages these entries. The hosted `/vault?service=select` picker lets a user explicitly share an existing item's ID, kind and name with the displayed app origin. Selection does not create a grant; the app must still request the selected item through Connect consent.

TOTP enrollment accepts either `{name, origin, otpauth_uri}` or `{name, origin, seed, issuer, account, algorithm?, digits?, period?}` through private native/web input. Defaults are SHA1, six digits and thirty seconds. SHA256/SHA512, eight digits and periods from 15 to 120 seconds are supported. The seed is validated and encrypted in the existing per-account credential broker; safe metadata contains the issuer, account label, origin, algorithm, digits and period.

Use `{{NANOCODEX_VAULT_TOTP}}` in a brokered HTTP request template. The broker generates the current code immediately before substitution and dispatches only to the saved exact HTTPS origin. It does not return a raw code, expose the seed, forward response bodies/cookies, follow redirects, or automatically retry. `body_encoding: "json"` or `"form"` preserves proper escaping. Existing API-key, login, card and signing templates continue to work.

For a retained Nanocodex private browser, `browser_vault_fill_totp` accepts the selected login `vault_id`, `target_id`, `expected_origin`, a separate `totp_vault_id`, and a stable `operation_id`. The private runtime binds the current OTP document before resolving a code, adds the code to its redactor before filling, and records a durable operation receipt. Replaying the same operation retrieves the receipt without generating or submitting another code. A changed document or lost runtime redaction state fails closed. Inspect the resulting private snapshot to verify account access; a submitted form is not proof of authentication. This browser continuation remains a full-account private-browser tool, not a raw-code export through Connect or the standalone SDK.

This is credential delegation: a grant that can use both a password and TOTP can authenticate with both. Encryption and separate model context do not make those two capabilities independent factors against compromise of that grant. Use narrower grants and independent human approval for sensitive account actions. A broker HTTP request is not a generic browser-session transfer or proof of successful login.

## Dedicated numbers and inbound SMS

The initial provider adapter supports long-lived US local Twilio SMS-capable numbers. It does not promise acceptance by any particular 2FA sender. There is no outbound SMS endpoint and no automatic release. Existing outbound voice calling retains its separate deployment and authorization contract; it is not silently enabled by an SMS service grant.

Account paths are under `/v1/services/phone`; Connect uses the equivalent grant services path. Search available numbers, create a purchase intent with one stable `operation_id`, and inspect its quote. The quote contains recurring monthly rental and additional inbound SMS charges. The authenticated user reviews and approves on `/services/phone`; neither a model tool, account API key nor a Connect token can approve on the user's behalf. Approval rechecks availability, quote expiration and current prices. Release also needs an explicit human confirmation because losing a number can break recovery for other accounts.

Before any provider mutation, the phone service durably records its operation fence. Unknown outcomes remain reserved. Polling reconciles only matching provider evidence, without issuing a replacement purchase or release. Connect retains the caller's operation UUID in public receipts; `approval_request_id` identifies the owner-side approval request. If an initial reply is lost, poll with the original caller UUID. Never invent a new UUID to retry the same intent.

The public webhook `/v1/services/phone/webhook` verifies Twilio's signature against the exact configured URL, provider account and current exclusive number ownership. Inbound content is encrypted, bounded and expires after at most twenty-four hours. Persistent message-SID receipts reject signed replays after expiry. Messages are untrusted data, not agent instructions. Empty TwiML does not send an automatic response. Released numbers retain tombstones and are not reassigned through this service.

See [the phone contract](../js/egress/PHONE_SERVICE.md) for endpoints, limits, prices, configuration and recovery behavior. The managed `phone_numbers` tool exposes availability, listing, intents, receipt inspection and inbox reads. It has no approval operation.

## Deployment and operation

The phone backend uses the existing broker encryption key. Its private `TWILIO_PHONE_PROVIDER` service binding calls the managed Worker's named `PhoneProvider` entry point, keeping existing Twilio credentials in the managed Worker. Public ingress cannot invoke this entry point. The provider allows only the supported number and pricing API paths and signature verification, not arbitrary HTTP or messaging.

For the first release, deploy the managed Worker containing the named provider entry point, then egress (including its new `PhoneServiceAccount` migration/binding), Connect API/dialog, and the account app last. Publish SDK packages separately using the repository's normal release process. Native source validation is not device installation.

Number provisioning is **disabled by default**. Enabling it requires an explicit operator decision and positive USD caps for monthly rental and inbound SMS prices. Configure the canonical webhook URL, per-owner number limit and retention. Existing provider account billing and fraud controls remain necessary: webhook rate limits cannot prevent carrier charges incurred before a message reaches Nanocodex. End-user subscription billing and number portability are not implemented by this service. Do not enable unrestricted resale based only on a passing synthetic provider test.

Validation includes real workerd HTTP journeys, actual account authentication, encrypted durable storage, RFC6238 vectors, Connect grant isolation/revocation, lost-response recovery, signed callbacks, SDK HTTP/type contracts, rendered consent/enrollment/approval flows, native client HTTP tests and an iOS Simulator build. Provider fixtures are synthetic and make no live purchases. Carrier compatibility and live production provisioning require separate authorized tests with explicit costs.

## Account management and browser links

`GET /v1/account/links` returns ordinary Connections, Vault, Wallet, and API access URLs. It is public and grants no access. `connect=cloudflare` focuses Cloudflare enrollment; `add=totp` opens an authenticator form after sign-in. See [account navigation](account-navigation.md).

Authenticated account callers can request a specific hosted form with `GET /v1/services/links`. Query parameters are `service=vault`, `action=enroll|select`, and `kind=login|api_key|card|address|phone|totp`; defaults are Vault enrollment and TOTP. For a phone approval use `service=phone&operation_id=UUID`. The response contains a clickable `url` and its flow metadata. Obtaining a URL performs no enrollment, connection, grant, or provider mutation.

For popup integration, supply the exact `app_origin` and a unique URL-safe `state` (16–128 characters). The page names that recipient and shares only a fixed metadata receipt after an explicit user action. Ordinary links omit callback parameters and work from terminals, mobile apps, and browser tabs. Caller-supplied destination hosts, duplicate parameters, unsupported kinds, and orphan callback state are rejected.

Account keys retain the existing management permissions: connector management uses `api_keys:write` and `tools:use`; credential management uses `agents:write` and `tools:use`. The latter permission name does not allocate an agent or require a conversation. These owner-only methods remain unavailable to a scoped Connect grant. Scoped Vault requests and metadata continue to use the narrower service permissions above.

| Capability | REST contract | Human interface |
| --- | --- | --- |
| Provider catalog, connection status, OAuth start/reconnect/exact disconnect | `/v1/connectors/catalog`, `/v1/connectors`, `/v1/connectors/{provider}` and connection ID routes | `links.connections`, optionally focused on a provider; start returns provider consent URL |
| Cloudflare enrollment | `POST /v1/connectors/cloudflare` with saved `vault_id` | Cloudflare form selects a saved token |
| Custom MCP connections | `/v1/connectors/mcp-connections` and ID start/callback/delete routes | MCP section and provider consent |
| WhatsApp pairing | `/v1/connectors/whatsapp/start` and private pairing/status routes | Private native panel or account connector flow |
| Model sign-in and API access | `/v1/credentials`, model-specific login and mutation routes | ChatGPT, Claude, and OpenAI forms |
| Vault creation, exact deletion, login origin update | `/v1/credentials/vault/{kind}` and exact ID routes | Hosted enrollment for all six kinds; Vault workspace |
| SSH identity generation/import/removal | `/v1/credentials/ssh/{reference}` | Vault SSH form or native private form |
| Brokered requests, signing, and TOTP | `POST /v1/services/vault/request` | Private input only when enrolling a new credential |
| Trusted captured-card save and observed balance/refresh | `POST /v1/vault/store`, `POST /v1/vault/card` | Native Vault management; safe status receipts |

Model sign-in codes and secret request bodies belong only in trusted private client controls. General agent output and diagnostic logging must not serialize them. Connector callbacks retain state, owner and PKCE checks; receipt delivery alone does not prove that a connection completed. Read the resulting account or connection status.

### Rust account client

The `nanocodex-managed` client exposes these REST operations without creating an
agent. Load the API key through your application's private configuration; never
include it in diagnostic output.

```rust,no_run
use nanocodex_managed::{AccountLinkOptions, ManagedClient, ServiceLinkOptions};

async fn account_setup(client: &ManagedClient) -> Result<(), nanocodex_managed::ManagedError> {
    let links = client.account_links(AccountLinkOptions {
        connect: Some("cloudflare"), add: Some("totp"),
    }).await?;
    println!("Connections: {}", links.connections);
    let enrollment = client.services_link(ServiceLinkOptions {
        service: Some("vault"), action: Some("enroll"), kind: Some("totp"),
        ..Default::default()
    }).await?;
    println!("Enroll authenticator: {}", enrollment.url);
    let credentials = client.credentials_overview().await?;
    println!("ChatGPT connected: {}", credentials.chatgpt.connected);
    let _catalog = client.connector_catalog().await?;
    let _connections = client.connector_list().await?;
    let _vault_metadata = client.services_vault_list().await?;
    Ok(())
}
```

Use `services_vault_get` for one item and `services_vault_request(&VaultRequest)`
for a brokered request returning only its status and success flag. Connector
methods include `connector_start`, exact `connector_disconnect`,
`connector_cloudflare` with a saved Vault reference, MCP connection management,
and `whatsapp_start` with a stable operation UUID. Display returned authorization
URLs and re-read status after consent.

`credentials_overview` omits private pending login codes. Trusted private clients
use the existing `private_connector_input` for ChatGPT start/status and
`claude_login_start`, `claude_login_status`, and `claude_login_complete` for Claude.
Save an OpenAI key with `private_input_post`, a caller-created `PrivateInputRequest`
whose kind is `PrivateInputKind::Credential("openai".into())`, and a
`PrivateInputBody` containing the private form's `api_key` JSON field. That body
has no `Debug` implementation and erases its encoded bytes on drop. Never log
private form values or responses. `openai_disconnect`, `chatgpt_disconnect`, and
`claude_disconnect` remove the respective credentials; reconcile an uncertain
write by reading status before retrying. Vault enrollment, deletion, SSH identity
management, and captured-card operations retain the existing `vault_*` methods.

The Rust `ManagedClient` also provides `services_phone_available`,
`services_phone_list`, `services_phone_provision`, `services_phone_get`,
`services_phone_release`, `services_phone_messages`, and
`services_phone_request_get`. Persist a canonical operation UUID before submitting
an intent; after an uncertain write, poll that same UUID. Each method dispatches
once without automatic retries. Use `services_link(ServiceLinkOptions {
service: Some("phone"), operation_id: Some(operation_id), ..Default::default()
})` to obtain the human approval URL, then poll the request after the user acts.
There is no client approval operation. Typed receipts preserve request and number
statuses, decimal price strings, message expiration and opaque `next_cursor`
values. Pass the cursor unchanged through `PhoneMessagesQuery` for the next page.
SMS bodies are private untrusted content; these types have no `Debug`
implementation and must not be serialized into logs or agent diagnostics.

### REST and JavaScript handoff

For a normal link, no browser, callback URL, agent, or model is required:

```sh
curl --fail 'https://nanocodex.gakonst.workers.dev/v1/account/links?connect=cloudflare&add=totp'
```

Use the returned `connections` or `vault` URL. The person signs in and explicitly
connects or saves the item. Account links convey no account credentials or consent.
For a specific enrollment, authenticated `GET /v1/services/links?service=vault&kind=card`
returns a `url`. Supply the account key in the Authorization header, never in a URL.

```js
import { createServicesClient } from 'nanocodex/services'

// Server or trusted native client: read apiKey from private configuration.
const services = createServicesClient({ apiKey })
const links = await services.links({ connect: 'google', add: 'login' })
const form = await services.hosted({ service: 'vault', kind: 'card' })
console.log(links.connections, form.url)

const catalog = await services.account.connectors.catalog()
const connections = await services.account.connectors.list()
const metadata = await services.vault.list()

// A trusted provider capture is stored without moving card details through the caller.
const saved = await services.account.vault.store({ capture_id, operation_id })
if (saved.status === 'saved' && saved.vault_id) {
  const observed = await services.account.vault.card({
    operation: 'balance', vault_id: saved.vault_id,
  })
  // Inspect status, freshness, and observed_at before interpreting the balance.
}
```

`services.account` also exposes OAuth start/exact disconnect, Cloudflare token
selection, Link status, WhatsApp start/status, MCP create/start/delete, model
sign-in/status/disconnect, OpenAI key save/delete, six-kind Vault creation/deletion,
login origin updates, and SSH generation/import/removal. These owner methods
reject Connect transports before dispatch. OAuth callbacks run in the consent
browser; clients re-read connection status afterwards. Private native input and
the clickable hosted forms use the same authenticated REST contracts.

The standalone entry point includes TypeScript declarations and runs with fetch
in Node, Workers, and trusted native bridges. It does not load the agent runtime.
Reads have bounded response bodies; account writes are never automatically
retried or redirected. Keep an uncertain operation's identifiers, inspect its
status, and reconcile it before submitting another mutation.
