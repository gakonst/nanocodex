# Screen startup: test the apparent Worker boundary cost

## Outcome

This report records the first two rejected experiments. A subsequent bounded
[local-verification change](screen-local-authority.md) demonstrated a real win:
cached viewer readiness fell from 1,058 ms to 366 ms in fresh matched cohorts.
That final change is deployed; the experiments below remain reverted.

Two deployed routing experiments failed to demonstrate a screen-startup win and
were reverted. The earlier roughly 555 ms residual is **not proven to be
service-binding overhead**. Unauthenticated health requests carrying WebSocket Upgrade headers show similar
delay using both Apple's URLSession and Node, but that health route itself makes
a managed service call. A separate finite 409 route with no application I/O is
faster. These controls did not support rewriting the native client. They motivated a
separate bounded local-verification test, whose successful measurements are
reported in the final comparison linked above.

The retained changes add safe request-ID-correlated timing fields. Existing
short-lived screen authority caching remains enabled; it previously removed
roughly 190–200 ms of repeated live authentication. This investigation does not
invalidate that separate improvement.

## Measurements

All timestamps below are UTC on 2026-09-16. The client was the same Mac and public
origin, `nanocodex.gakonst.workers.dev`. A temporary publisher advertised metadata
only; viewer probes received the real broker `ready` message, then closed.
Catalog GET and viewer upgrade alternated live and cached authority, four of each
per cohort. No remote input was sent.

| Routing | Start UTC | Cached viewer median | Live viewer median |
| --- | --- | ---: | ---: |
| Existing managed fetch route | 22:39:55 | 999 ms | 1,153 ms |
| Existing route, additional timing fields | 22:52:44 | 1,051 ms | 1,189 ms |
| Private authorization RPC returning a Request; account upgrades existing broker DO directly | 22:53:33 | 1,225 ms | 1,225 ms |
| Same direct route, finite URL/method/header tuples across RPC | 23:02:49 | 1,139 ms | 1,076 ms |

The Request experiment's cached samples were 998 / 1,393 / **4,544** / 1,058 ms;
the tuple experiment's were 1,331 / 948 / 870 / 1,621 ms. These small, variable
cohorts support no improvement claim. The one-minute Mac load average fell from
296 at the first baseline to 39 at the final cohort on ten cores; client
scheduling conditions were not controlled.

Initially, matching request IDs showed account binding wait minus managed
handler duration of 2–5 ms for catalog GETs, versus 466–638 ms for viewer upgrades.
Cached catalog GETs completed in 196–214 ms. Moving the upgrade into the account
Worker did not remove the residual: cached finite admission still reported
hundreds of milliseconds even when managed authorization reported zero.

### Why the subtraction does not locate the delay

Cloudflare's Worker clocks advance at I/O boundaries. A reported zero means no
observed I/O wait at that clock's resolution, not zero CPU. Starting a measurement
before its first await can also retain an earlier request timestamp. Explicit
epoch logging across isolates produced impossible negative return intervals;
those epoch differences must not be used to split dispatch and return latency.
Use client monotonic timing and same-context durations, and do not add overlapping
spans. See Cloudflare's [performance and timer documentation](https://developers.cloudflare.com/workers/runtime-apis/performance/)
and [security model](https://developers.cloudflare.com/workers/reference/security-model/).

## Controls: distinguish service calls from a finite local response

The `/api/health` route calls `managedModelStatus` through a service binding
even without credentials. It is not a no-managed-call control. Fresh URLSession
sessions at 23:00:45 produced:

| `/api/health` request | Samples |
| --- | --- |
| Ordinary HTTPS GET | 105 / 93 / 83 ms |
| WSS task receiving a finite HTTP 200 | 747 / 861 / 651 ms |

The WSS request-to-response waits were 682 / 782 / 599 ms; TCP connection times
were 58 / 74 / 47 ms. Failed WebSocket tasks can reuse connections: an earlier
shared-session run yielded 42 / 33 / 49 ms for this same WSS health control.
Reusing that run as a fresh-connection baseline would have hidden the delay.

Fresh Node 24.19 HTTP/1.1 connections at 23:04:46, all reaching SJC, produced:

| `/api/health` request | Samples |
| --- | --- |
| Ordinary `https.request` GET | 278 / 113 / 98 / **946** ms |
| `ws` client, finite HTTP 200 | 704 / 730 / 103 / 631 ms |
| `https.request` with manual WebSocket Upgrade headers | 724 / 729 / 643 / 717 ms |

Each request created a new socket; the large ordinary-HTTP outlier is retained.
The corresponding delay in Node and manual-header requests rules out an
Apple-only explanation. This control alone cannot distinguish the service-call path from edge dispatch.

A second control used `GET /api/auth/chatgpt`, whose configured managed-mode path
returns a finite 409 without awaited application I/O. At 23:06:40–23:06:45:

| Finite409 request | Samples |
| --- | --- |
| Fresh URLSession HTTP (HTTP/3) | 84 / 86 / 113 / 97 ms |
| Fresh URLSession WSS (HTTP/1.1) | 270 / 165 / 174 / 230 ms |
| Fresh Node HTTP/1.1 GET | 97 / 102 / 109 / 93 ms |
| Fresh Node `ws`, HTTP/1.1 | 169 / 218 / 176 / 150 ms |
| Fresh Node manual Upgrade, HTTP/1.1 | 223 / 246 / 248 / 241 ms |

All returned 409. The extra delay is substantially smaller than on health or
successful viewer requests. Native plainHTTP negotiates a different protocol;
Node holds HTTP/1.1 constant across all three variants. The remaining cause is
unresolved: these results do not establish a universal 600 ms edge-upgrade cost,
and the failed routing experiments do not eliminate every possible service-call
effect.

Run the credential-free control using normal account development dependencies:

```sh
node docs/performance/2026-09-16-cross-platform-hands/screen-upgrade-control.mjs
```

It records monotonic DNS/TCP/TLS/request/response boundaries and keeps all samples.
An optional URL argument repeats the same test on `/api/health`. The companion
[Swift control](screen-upgrade-control.swift) uses fresh URLSession sessions:

```sh
swiftc -parse-as-library docs/performance/2026-09-16-cross-platform-hands/screen-upgrade-control.swift -o /tmp/nanocodex-screen-control
/tmp/nanocodex-screen-control
```

## Runtime correctness and decoded frames

Both experiments retained centralized auth, capabilities, session origin,
credential-bound snapshot expiry and broker ownership/generation checks. Policy,
proxy, package, type and real-socket tests passed. The final tuple experiment
passed rejected-snapshot recovery and delivered real decoded frames:

| Final tuple route | Three connect-to-first-decoded-frame samples |
| --- | --- |
| Mac viewer → Linux native, `frames-v1` | 1,341 / 1,201 / 1,323 ms |
| Mac viewer → Linux VM, `frames-v1` | 1,174 / 1,359 / 1,184 ms |

The earlier Request route delivered Linux-native frames in
1,229 / 1,152 / 1,345 ms and same-Mac WebRTC frames in
1,953 / 2,060 / 1,551 ms. These verify the deployed journeys; they do not establish
a latency improvement. The Linux fixture VM was deleted after viewers closed.
No physical-phone latency measurement succeeded in this investigation.

## Provenance and retained evidence

- Request route: source `1dada0ac3`, managed
  `30220bd9-b9f8-414e-90c9-625843a452cb`, account
  `1db3e0bb-4a3a-4b98-9769-925d284ed9e4`.
- Tuple route: source `8f41f2cfd`, managed
  `a0afbf49-c783-4803-9320-8498def61e38`, account
  `3124539f-22bf-4e33-95ff-b24b7816e737`.
- Revert: `d05c9433c`, integrated as `ecc3605c7`. Root deployment removes account
  bindings before removing the managed entrypoint, preserving the existing route.
- [Curated traces](screen-upgrade-boundary-traces.json) contain client measurements,
  request IDs, matched safe account/managed spans, run times and load averages.
  They exclude credentials, query strings, SDP and captured media. Cross-isolate
  epochs remain diagnostic metadata only; no derived stage attribution uses them.

The subsequent local-verification experiment removed the managed service call
for already-authorized viewers and was retained only after the matched client
cohort demonstrated the improvement. It does not establish which Cloudflare
internal mechanism caused the earlier residual.
