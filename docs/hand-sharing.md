# Hand sharing

A Hand owner creates a revocable link. A recipient signs in to Nanocodex and
redeems the link to make the shared machine available to their account. In a
browser, open the link, sign in, then select **Add Hand to my account**. Sharing
a Hand grants its published shell, process, preview and computer controls;
only share machines with people you trust to use them.

## CLI

Use the existing `nanocodex2 login` flow on each account. The commands use the
selected account credential and managed service, just like other account commands.
They print JSON and return a nonzero exit status on failure.

```sh
# Owner: use the machine_id of a Hand owned by your account.
nanocodex2 hand-share create MACHINE_ID
# => {"id":"SHARE_ID","url":"https://MANAGED_HOST/hand-share/OWNER_UUID#token=TOKEN"}

# Recipient: sign in to your own account, then paste the complete link.
nanocodex2 hand-share redeem 'https://MANAGED_HOST/hand-share/OWNER_UUID#token=TOKEN'
# => {"machine_id":"MACHINE_ID"}

# Owner: list active IDs without exposing the bearer URLs.
nanocodex2 hand-share list
# => {"data":[{"id":"SHARE_ID","machine_id":"MACHINE_ID","created_at":123,"revoked_at":null}]}

# Owner: revoke the link and access granted through it.
nanocodex2 hand-share revoke SHARE_ID
# => {"status":"revoked","id":"SHARE_ID"}
```

The token is in the URL fragment, so opening the link does not send it in the
page request. Keep the complete URL private: anyone with it can redeem it while signed in.
Creation returns the URL once; listing returns only active links, with
`created_at` in Unix milliseconds and `revoked_at: null`. Revoked links disappear
from the list. Revocation applies to the selected link, so other independently
granted access can remain.
The shared Hand must be online to execute work. VM provisioning, enrollment and
private secure-input broker operations are not granted. Shell access still has
the operating-system privileges of that Hand; this is not an OS sandbox between
accounts. Received Hands use stable `shared:SHARE_ID` aliases and separate routed
session identities. Their owner’s other machines stay private.

Revocation blocks new calls, cached routes, process continuation and later
redemption. An already-admitted command may finish. Turn cleanup and cancellation
continue to the original publisher. Forgetting a machine revokes its links;
ordinary disconnect/reconnect preserves grants. Each owner may have 100 active
links (1,000 lifetime), each link 1,000 recipients, and each recipient 100 received
links. Revoked references are reclaimed when recipient capacity is needed.

The CLI sends writes once. After a lost response, inspect the owner's share list
or the recipient's available Hands before retrying. A failed transport does not
prove that the service rejected the operation.

## API

All endpoints require the existing account authentication. Requests and responses
use JSON. The API returns `{ "revoked": true }` after revocation; the CLI prints
its own receipt containing the revoked ID.

| Method | Path | Request | Receipt |
| --- | --- | --- | --- |
| POST | `/v1/account/hand-shares` | `{ "machine_id": "…" }` | `{ "id": "…", "url": "…" }` |
| GET | `/v1/account/hand-shares` | — | `{ "data": [share metadata] }` |
| DELETE | `/v1/account/hand-shares/{id}` | — | `{ "revoked": true }` |
| POST | `/v1/account/hand-shares/redeem` | `{ "url": "…" }` | `{ "machine_id": "…" }` |

Redeem sends the URL to the configured managed service; the client does not
navigate to or fetch the supplied URL. The link must have the same origin as the
configured managed service. The server validates the link and enforces
ownership, account access, and revocation.

## Local verification

Prepare the generated assets with `pnpm --filter nanocodex build` and
`pnpm --filter nanocodex-managed-service prepare:code-evaluator`. Run
`cargo test -p nanocodex-bin --test nanocodex2_hand_share -- --nocapture`.
Then run the CLI against the Worker journeys:

```sh
NANOCODEX_TEST_CLI="$PWD/target/debug/nanocodex" \
  pnpm --filter nanocodex-managed-service test:hand-sharing
```

The journey uses synthetic account authentication and a synthetic WebSocket Hand
with the shipped CLI, account proxy, Worker router, SQLite stores, provider and
broker. HTTP receipts and publisher frames are saved under
`output/hand-sharing-journey/`. It covers legacy/regional execution, CUA and process
routing, revocation, cleanup, cancellation, authorization, CSRF and capacity
recovery. It does not sign into a live account or control a real desktop.
