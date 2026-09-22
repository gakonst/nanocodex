# Local viewer authority removes a measured screen-startup wait

Cached viewer admission fell from a **1,058 ms median to 366 ms** in matched
four-sample cohorts: **692 ms, or 65%, lower**. Live-authenticated viewers stayed
near their previous latency. The change is deployed and retained.

The account Worker now verifies the existing 120-second signed viewer snapshot
locally and sends the upgrade directly to the account's existing screen broker.
A public shared module owns the signing, verification and Hand policy used by
both Workers. The managed Worker still handles live authentication, publishing,
renewal and viewers without a usable snapshot.

## Matched measurements

On 2026-09-16, the same Mac, public origin, client binary, account and
metadata-only publisher fixture ran four live and four cached viewer upgrades,
interleaved with catalog GETs. Each viewer used a fresh connection and waited for
the real broker `ready` message before closing. The publisher sent no images or
input. Both cohorts reached SJC.

| Viewer authority | Before: 23:10:59–23:11:14 UTC | After: 23:13:15–23:13:27 UTC |
| --- | --- | --- |
| Cached snapshot, all ready samples | 1,064 / 1,185 / 1,052 / 847 ms | 331 / 356 / 377 / 426 ms |
| Cached snapshot, median | **1,058 ms** | **366 ms** |
| Live authentication, all ready samples | 1,169 / 1,223 / 1,079 / 1,377 ms | 1,174 / 1,084 / 1,571 / 1,061 ms |
| Live authentication, median | 1,196 ms | 1,129 ms |
| Mac one-minute load average, ten cores | 23.8 | 18.9 |

Every cached after-request logged `route: local_access`; live requests continued
to log `route: managed`. Exact request-ID matches show the account's awaited
backend interval falling from 678–966 ms to 172–186 ms. The broker route itself
was 176–188 ms before and 172–186 ms after. No managed handler ran for the direct
cached admissions. Snapshot verification reported zero observed I/O wait.

These are small local-client cohorts, not p95 estimates or physical-phone
measurements. Host load varied, but all cached samples improved while the live
control retained its prior range. The client result supports retaining this
change; it does not identify a particular Cloudflare internal delay mechanism.
Worker clocks advance at I/O boundaries: zero does not mean zero CPU, and
cross-isolate epoch subtraction cannot split dispatch and return latency. See
[Cloudflare's timer documentation](https://developers.cloudflare.com/workers/runtime-apis/performance/).

## Real decoded frames and recovery

The existing Mac-viewer → Linux-native runtime test delivered three decoded
`frames-v1` frames in **796 / 581 / 562 ms**, median **581 ms**, after the change.
All were 1280×800. This is a measured product journey, but its before-cohort was
not repeated immediately before this change; the strongest matched improvement
claim above is viewer readiness.

The real native-client rejected-snapshot test passed: the initial rejected
upgrade was retried with live credentials before admission, and the resulting
viewer connected. Expiry, future issuance, secret rotation, changed credentials,
missing authority, browser Origin, stale generation and no-replay behavior were
covered by focused tests. The real retry test injected an invalid snapshot; it
did not wait 120 seconds for natural expiration.

No new physical-phone latency claim is made. Earlier Linux-VM decoded-frame
journeys passed; this final cohort did not create another VM.

## Authority and rollout

- HMAC-SHA256 signature, public-origin audience, maximum lifetime, expiration and
  credential hash are unchanged. The hash includes the current login/key and all
  forwarded Connect inputs, preserving the live authenticator's precedence.
- A cookie-authenticated viewer must provide the exact public Origin. Viewers
  need `agents:read` and `tools:use`; Connect grants remain forbidden.
- Broker ownership assertions come only from the verified principal. The shared
  forwarding helper removes caller-supplied VM authority and Connect assertions.
- Absent, expired, future, changed-credential, malformed or rotated-key snapshots
  go to the original managed route as the exact original Request. The existing
  401/rejected-snapshot marker and one-time live retry remain intact.
- Once a direct broker request starts, failure never replays the upgrade through
  the managed Worker. The broker retains surface, generation and lease checks.
- Snapshot reuse does not refresh its expiration or issue a replacement token.
  Live renewal remains separate; the viewer renews every ten seconds.
- Both Workers require the same snapshot signing secret. Updating that secret
  invalidates optimization tokens only; primary API keys and sessions remain
  unchanged. The finite snapshot lifetime limits normal permission staleness;
  explicit signing-key invalidation must update both Workers.
- The production account binding points to the same `AccountHostedTools` namespace
  already used by managed. Development omits the optional shortcut binding.

Integrated validation passed 26 managed runtime tests, account proxy tests,
shared SDK runtime and type contracts, packed entrypoints, artifact checks and
the full account build. A workerd test preserves a real 101 socket and receives
its first `ready` message through the local-verification path.

## Provenance

- Clean before deployment: managed `db72d6ed-4196-47fd-bd5c-bc64bc7ae81f`,
  account `7d7fbc78-6310-4f90-b0b1-d52653cac8a4`.
- After deployment, source `c0d049f68`: managed
  `e1877ce4-0170-4de6-9cf9-ccfc003b7ece`, account
  `83d1e7ce-0db2-4637-9de8-8e9fd5f6968f`.
- Implementation `c7a3265c`; package/production-header contract follow-up
  `ab570fce`.
- [Curated paired traces](screen-local-authority-traces.json) retain exact request
  IDs, safe account/managed spans, client timing, load and first-frame events.
  They exclude credentials, query strings, SDP and captured media.
- [Earlier routing experiments and controls](screen-upgrade-boundary.md) explain
  why two RPC-based alternatives were rejected before testing local verification.
