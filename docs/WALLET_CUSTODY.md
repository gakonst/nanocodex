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

The root wallet has only two signing uses:

- `wallet_connect`, to authorize the exact requested delegated access key and
  its resources, expiry, call scopes, and spending limits;
- `wallet_revokeAccessKey`, to revoke the exact account access key named by the
  request.

There is no generic sign-message, transaction-signing, export, import, or raw
RPC escape hatch. The managed Worker exposes the account-authenticated,
same-origin routes `POST /v1/wallet/connect` and
`POST /v1/wallet/revoke-access-key`; `GET /v1/wallet` returns only public wallet
metadata. It derives the persistent user from the HttpOnly session and forwards
the bounded request over the existing private `NANOCODEX` Service Binding.
Egress derives the per-user Durable Object from that trusted user identifier
and never accepts a browser-selected storage owner or private key.

Across the private binding, egress exposes `GET /users/:userId/wallet` for
public metadata, idempotent empty-body `PUT /users/:userId/wallet` for
provisioning, and the corresponding `/connect` and `/revoke-access-key` POST
operations. These are trusted service operations, not public browser routes.

Connect may return a sanitized `wallet_connect` result containing the address,
signed authorization, and delegated access-key metadata. That delegated key is
the installation authority; it is not the root key. Revocation accepts the
original bounded `wallet_revokeAccessKey` request and returns only the public
operation result.

## Operational evidence

For a change to this boundary, verify a new OTP account, a returning login, an
access-key Connect approval, reload/reconnect, and access-key revocation against
the real Workers. Also test two accounts to prove cross-account isolation.
Inspect browser network, console, storage, Worker logs, and Durable Object state
to confirm that only public addresses and sanitized protocol results leave
egress and that stored key material is an authenticated ciphertext envelope.
