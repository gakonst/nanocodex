# Persistent account wallet custody

Every persistent SMS account has one platform-owned secp256k1 root wallet. The
managed Worker asks the private egress broker to create it after Twilio Verify
approves the account's first OTP login and before the persistent session is
issued. Provisioning is idempotent: a retry or later login returns the same
wallet address and must never replace the existing key. If provisioning fails,
login fails closed with `wallet_unavailable` and the browser-bound OTP challenge
remains retryable.

The egress broker stores the root private key in that user's existing
`UserCredentialBroker` Durable Object. The existing `CredentialVault` seals it
with the `CREDENTIAL_ENCRYPTION_KEY` AES-256-GCM envelope, including its
per-user scope as authenticated data. `CREDENTIAL_ENCRYPTION_KEY_PREVIOUS`
continues to support lazy envelope-key rotation. This needs no new binding,
Durable Object class, or migration.

## Custody and threat model

This is custodial, server-side encryption, not user-held end-to-end encryption.
The private key is decrypted inside the trusted egress Worker when it must sign.
An operator or attacker able to control that Worker or its encryption secret is
inside the trust boundary. The envelope protects stored Durable Object data; it
does not make the platform unable to use the key.

The root private key is never returned to the browser, account application,
managed Worker, Connect API, agent, tool, logs, or status response. Public
surfaces may receive the account address and the bounded signed protocol result
needed to complete an approved operation. Browser storage must contain neither
the root key nor an export of the encrypted envelope.

Configurable accounts are not silently assigned a wallet by a read path.
Migrating those accounts is future work and must define identity matching,
recovery, and rollout behavior before deployment.

## Allowed operations

The original account wallet signs account identity operations:

- `wallet_connect`, to authorize the exact requested delegated access key and
  its resources, expiry, call scopes, and spending limits;
- `wallet_revokeAccessKey`, to revoke the exact account access key named by the
  request.

There is no generic sign-message, transaction-signing, export, import, or raw
RPC escape hatch. The managed Worker exposes the account-authenticated,
same-origin routes `POST /v1/wallet/connect` and
`POST /v1/wallet/revoke-access-key`; `GET /v1/wallet` returns public metadata
for the selected payment wallet. It derives the persistent user from the HttpOnly session and forwards
the bounded request over the existing private `NANOCODEX` Service Binding.
Egress derives the per-user Durable Object from that trusted user identifier
and never accepts a browser-selected storage owner or private key.

Across the private binding, egress exposes `GET /users/:userId/wallet` for
public metadata, idempotent empty-body `PUT /users/:userId/wallet` for
provisioning, `GET /users/:userId/wallet/identity` for the original account
identity, and the corresponding `/connect` and `/revoke-access-key` POST operations.
Provisioning always returns the original wallet, even when an external payment
wallet is linked. These are trusted service operations, not public browser routes.

Connect may return a sanitized `wallet_connect` result containing the address,
signed authorization, and delegated access-key metadata. That delegated key is
the installation authority; it is not the root key. Revocation accepts the
original bounded `wallet_revokeAccessKey` request and returns only the public
operation result.

## Linking an existing Tempo Wallet

A persistent account can select **Link Tempo Wallet** in its Wallet card. The
broker creates a new secp256k1 access key and uses the Accounts SDK device-code
adapter at `https://wallet.tempo.xyz/api/auth/device`. Tempo Wallet presents the
approval. The requested grant has unlimited token spending, unrestricted
contract calls, and no expiry. This grants ordinary access-key authority, not
root or access-key administration authority. It does not expand existing
Connect installation grants.

The five-minute approval deadline is separate from the key lifetime. The SDK
request uses `expiry: 0`; the retained signed authorization uses canonical
absence of expiry. Omitted spending limits and call scopes mean unrestricted
access. Empty arrays mean denied access and must not be substituted for omission.
Nanocodex verifies the root signature, chain 4217, generated access-key address,
and exact approved permissions before selecting the wallet. A reduced or finite
grant leaves the existing payment wallet unchanged.

The new access-key secret and signed authorization use the same per-user
encrypted custody as the original wallet. The browser sees only operation IDs,
the approval prompt, and public wallet metadata. It never receives either
wallet's private key. `POST /v1/wallet/link`, `/link/poll`, `/link/cancel`, and
`/v1/wallet/unlink` require the persistent owner's browser session and a matching
origin. Agents, API keys, and Connect grants cannot initiate these mutations.

A link operation has one durable UUID. Repeating it retrieves the same result;
it cannot generate another key or silently repeat a wallet approval. Browser
reload resumes polling. The Accounts transport currently has no durable resume
API; losing the broker's live exchange marks it interrupted. Starting a new
attempt requires an explicit user action. Cancellation fences a late approval
from changing the active wallet.

Linking preserves the original wallet and its funds. `/v1/me`, login, and
outbound Connect identity operations continue to use the original address.
Balances, MACH checkout recipients, and explicitly authorized Mercator payments
use the selected payment wallet. A stale checkout request is rejected with
`wallet_changed` instead of being redirected to a newly selected address.
Revoked keys and unavailable chain state fail closed when signing; they never
cause an automatic payment from the original wallet.

**Disconnect** removes the linked key from Nanocodex's active custody and selects
the original wallet. It does not revoke the key onchain. The user can revoke the
key in Tempo Wallet. Pending unpublished grants retain their signed authorization
while linked, so the SDK can attach it when the key is first used.

Deploy the egress broker and control service before Managed, then build and
deploy Account last. Managed identity reads require the broker's `/wallet/identity`
route. No existing root keys, account identifiers, or onramp bindings need to be
reprovisioned.

## Operational evidence

For a change to this boundary, verify a new OTP account, a returning login, an
access-key Connect approval, reload/reconnect, and access-key revocation against
the real Workers. Exercise external-wallet approval, rejected or reduced grants,
cancellation, reload/interrupted approval, delegated signing, revoked keys, and
disconnection with synthetic wallet identities. Also test two accounts to prove cross-account isolation.
Inspect browser network, console, storage, Worker logs, and Durable Object state
to confirm that only public addresses and sanitized protocol results leave
egress and that stored key material is an authenticated ciphertext envelope.
