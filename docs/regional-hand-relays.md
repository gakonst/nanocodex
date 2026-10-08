# Regional native Hand relays

Native account Hands connect to the existing authenticated
`GET /v1/account/tool-host` WebSocket endpoint. After authentication, account
capability checks and browser-origin checks, the managed service resolves the
publisher runtime’s retained destination. Eligible new runtimes can use a
regional `RegionalHandRelay` Durable Object. Account discovery and publication
ownership stay in the account's `AccountHostedTools` object.

## Placement and trust

Relay identity combines the authenticated account ID, a versioned relay prefix,
and one of nine bounded region keys: `wnam`, `enam`, `weur`, `eeur`, `apac`, `oc`,
`sam`, `afr`, or `me`. The selector uses Cloudflare's `request.cf` geography.
Caller headers and query parameters cannot select an account, region, or object
name. The authenticated router replaces internal owner and relay assertions
before forwarding the connection.

Regional-capable publishers supply both `x-nanocodex-hand-machine-id` and
`x-nanocodex-hand-runtime-id` before the WebSocket upgrade. The account directory
persists their destination before connecting, and the advertised catalog must
match that identity. Reconnecting the same machine/runtime pair always returns
to its retained destination, even if ingress geography or configuration changes.
Publishers without these headers keep the owner broker; incomplete or malformed
identities are rejected. A new runtime can receive a new regional destination.

The account website forwards the original Request through its managed service
binding. The managed route clones that Request to add principal assertions;
both operations retain Cloudflare metadata. A service binding must continue to
forward the Request itself rather than reconstructing it from only its URL and
headers. Local fixtures must supply synthetic `cf` metadata when exercising
regional routing. The preview bridge uses a separate HTTP transport; it does not
make signed assertions about the original client's geography.

Cloudflare location hints influence initial placement and are best effort. They
do not guarantee a specific execution colo or move an existing Durable Object.
For a new runtime, a disabled regional feature, absent relay namespace, or
unavailable trusted geography selects the existing account broker. A runtime
already pinned to a regional relay fails with `relay_unavailable` if the binding
is removed; it cannot silently move execution journals to the owner broker.

## Discovery and execution

The account directory records the current publication for each machine and
checks account-wide tool-name ownership. Publishing a replacement in another
region fences the previous publication before the replacement becomes visible.
Pending ownership changes and unresolved fences are persisted so interrupted
publication cannot silently expose two owners.

Session discovery reads account-owned metadata. The account hosted-tools provider
receives the optional relay namespace and uses the discovered route to send
native calls to the selected relay. Native secure-input submission, native file
reads and native app validation use the same provider configuration. The relay
runs the existing hosted-tools broker, preserving its command receipts,
reconnection and cancellation protocol. Session providers also persist the exact
relay and route token selected for each effect before dispatch. Recreating a
provider or discovering a replacement publisher cannot move an existing call to
a new shard and execute the same effect again. Session native app validation,
file reads and native-input submission share this durable call-route store.

Screen signaling has a separate regional opt-in described below. VM host pools,
session-owned publishers and their effect journals retain their existing routing.
Regional tool relays change the native account tool-host transport and the
provider's execution destination.

## Configuration and rollout

The managed Worker exports `RegionalHandRelay`. Production and development
Wrangler configurations bind it as `NANOCODEX_HAND_RELAYS`; migration `v15`
creates its SQLite-backed Durable Object class. Existing Durable Object classes
and their migration history remain intact.

The namespace is optional in the runtime environment. Regional selection for
new runtimes requires two distinct opt-ins:

- Production and development Wrangler configuration set
  `NANOCODEX_REGIONAL_HAND_RELAYS="true"` on the managed Worker, enabling new
  regional placements when that configuration is deployed. Custom environments
  must set the same value explicitly.
- Compatible native publishers use regional relays by default (opt out with `NANOCODEX_REGIONAL_HAND_RELAYS=0`) so
  it sends machine/runtime identity headers. The native driver snapshots this
  setting for its lifetime, including reconnects.

Legacy publishers and unconfigured environments continue to use the account
broker. Disabling the Worker flag prevents new regional placements while
retained runtimes continue using their original destination. Deploy the managed
service using the repository deployment command so its migration and bindings
are applied together. A Hand reconnect reuses its retained destination; a runtime
identity must not change merely because a transport reconnects. Removing a
binding does not delete existing relay storage or migrate retained sockets.

This staged rollout does not replace or restart an existing Linux Hand service.
An already-running older daemon continues its legacy protocol; changing the
Worker setting does not add identity headers to that binary or process. Use a
separate compatible publisher for a regional probe, with an independent state
directory, machine identity and workspace, leaving the existing service intact.
For an A/B comparison, run the same candidate binary in both modes, using
independent state and workspaces for each publisher and changing only the native
opt-in. Comparing an old daemon against a new binary cannot isolate the effect
of regional routing. Preserve existing daemon state and active command journals;
do not reuse them for a probe or change its live runtime identity.

### Explicit legacy retirement

A graceful shutdown sends `Drain`, removes the legacy catalog and permits a
new opted-in runtime to select a regional relay. For an older publisher that
exits without draining, explicit retirement is the recovery path. First inspect the authenticated account status endpoint to capture the exact current
machine and runtime IDs:

```sh
curl --fail-with-body \
  "$NANOCODEX_ORIGIN/v1/account/hand-relays" \
  --header "Authorization: Bearer $NANOCODEX_API_KEY"
```

This GET requires `agents:read` and `tools:use`, rejects Connect grants and query
parameters, and returns safe legacy metadata:
`{ "legacy": [{ "machine_id": "…", "runtime_id": "…", "generation": 1, "online": true, "pending_calls": 0, "retirable": false }] }`.
It does not require a browser Origin header. The status read does not reserve a
retirement; the retirement operation rechecks live state.

Stop the selected publisher through the normal service lifecycle. If its legacy
catalog remains in the status response, call the retirement endpoint with the
exact captured IDs:

```sh
curl --fail-with-body --request POST \
  "$NANOCODEX_ORIGIN/v1/account/hand-relays/retire" \
  --header "Authorization: Bearer $NANOCODEX_API_KEY" \
  --header 'Content-Type: application/json' \
  --data '{"machine_id":"EXACT_MACHINE_ID","runtime_id":"EXACT_STOPPED_RUNTIME_ID"}'
```

For pre-runtime publishers, status returns `runtime_id: null`. Capture the
positive safe-integer `generation` from that same status row and submit exactly
`{"machine_id":"EXACT_MACHINE_ID","runtime_id":null,"generation":OBSERVED_GENERATION}`.
A reconnect changes the generation; a stale request returns 409 and cannot
retire the new connection. An exact repeated retirement returns the original
receipt while that retired generation remains current. The receipt includes
`generation` for pre-runtime publishers. Versioned runtime requests retain their
existing two-field request and receipt format.

Retirement clears the selected route's catalog, removing its Hand from inventory,
while retaining its call ledger. It does not deduplicate devices by display name
or prevent a pre-runtime publisher from reconnecting later.

Use the observed IDs of the stopped legacy runtime. The endpoint requires
`agents:write` and `tools:use`, rejects Connect grants, and applies same-origin
checks to browser-session credentials. Query parameters and methods other than
POST are rejected. The account owner checks that the exact current legacy
runtime is offline and has no admitted or dispatched calls before retiring it.
A temporary disconnect never authorizes automatic retirement. Resolve pending
calls through the existing recovery protocol instead of discarding their state.

Regional registrations appear separately under `regional` in the same status
response. A confirmed row includes `machine_id`, `runtime_id`, `publication_id`,
`region`, `online`, `pending_calls`, and `retirable`. Submit exactly those first
four identity fields to the same retirement endpoint. The original regional
broker checks that this publication is disconnected with no pending calls;
only then does the account remove its directory entry and retire that runtime.
A missing relay or changing publication rejects retirement. Repeating the exact
request returns its receipt without touching a newer publication. Re-enrollment
uses a fresh runtime identity; it does not replay old calls.

After a successful drain or retirement, a compatible publisher can start
with native `NANOCODEX_REGIONAL_HAND_RELAYS=1` and a new runtime identity; the
Worker must also enable its `"true"` setting. Retiring the old runtime does not
move its effect receipts or authorize replaying its calls on the new relay.

## Validation

Use real workerd WebSockets and the shipped publisher/provider contracts to
exercise discovery, execution, reconnect, replacement fencing and cross-account
rejection. Change ingress geography across a reconnect and verify that the same
runtime keeps its destination and receipt journal. Test legacy identity, disabled
selection, missing geography, and an absent relay namespace as compatibility
paths, including failure when an already-pinned relay becomes unavailable. A forged placement header must not override synthetic trusted `cf`
metadata. Keep per-run wire logs and other evidence in ignored `output/`.

Local service-binding tests establish metadata propagation and routing behavior.
They cannot establish Cloudflare's production placement or latency; deployment
receipts and observed execution traces are required for those claims.

Owners can retire a permanently stopped runtime with `abandon_pending: true` in the exact observed retirement request. This remains forbidden for a connected runtime or a changed generation/publication. Admitted calls become unavailable and dispatched calls retain an ambiguous result; receipts and replay fences remain stored. Omit the flag to reject retirement while any call is pending. Regional retirement RPCs transmit identifiers only, so large saved device descriptions do not prevent cleanup.


## Regional screen signaling

`NANOCODEX_REGIONAL_SCREEN_RELAYS="true"` enables regional routing for native
account screen publishers on `/v1/account/hands/host`. The authenticated Worker
selects the region from trusted Cloudflare ingress metadata. New screen
generations and viewer connection IDs include `rs.<region>.`, so viewer upgrades
and lease renewals route directly to that regional broker. The account website's
verified viewer-access path uses the same destination; legacy identifiers still
use the account broker.

The account owner retains a directory of screen generations and fences a previous
publisher before exposing its replacement. Discovery merges regional and legacy
screens against that directory. Stale or replaced generations cannot reclaim a
screen, and account Hand removal fences regional screen publication too. CUA
uses the discovered route for that screen. VM publisher credentials continue to
use their existing broker.

This removes the account-owner detour from repeated screen signaling. It does
not remove ICE negotiation, network relay selection, capture startup or player
decoding. Measure admission and first decoded frame separately. Location hints
remain best effort; inspect actual Durable Object execution placement before
attributing a production latency change to geography.
