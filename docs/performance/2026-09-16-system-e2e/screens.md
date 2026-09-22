# Screen journeys: system E2E measurement

All measured journeys passed. First decoded frame after viewer connect was
**607 ms median for Linux native**, **778 ms for a Linux VM**, and **1,323 ms for
a temporary Mac WebRTC publisher**. Cached viewer admission still uses local
verification; its broker interval stayed 164–179 ms despite high client load.

This was a measurement-only pass from **23:26:00–23:27:01 UTC on 2026-09-16**.
No deployment, installation, application-source change or remote input occurred.
No new optimization is claimed from this cohort.

## User-visible journeys

Each frame journey fetched the catalog once, then connected, decoded a real
frame and closed three times. Samples two and three are explicit reconnects to
the same publication, not forced network-failure recovery. All viewers closed
successfully. The disposable VM was deleted afterward by the VM measurement task.

| Journey | Catalog | Connect → first decoded frame, three samples | Median | Initial catalog → first decoded frame |
| --- | ---: | --- | ---: | ---: |
| Mac viewer → Linux native, `frames-v1`, 1280×800 | 601 ms | 668 / 607 / 568 ms | **607 ms** | **1,269 ms** |
| Mac viewer → Linux VM, `frames-v1`, 1280×800 | 503 ms | 910 / 778 / 767 ms | **778 ms** | **1,413 ms** |
| Same-Mac native WebRTC fixture, 1920×800 | 215 ms | 1,471 / 954 / 1,323 ms | **1,323 ms** | **1,686 ms** |

The final column adds the sequential initial catalog and first viewer sample;
it is not a median and excludes test-runner launch. The Mac catalog is warm after
publisher setup, unlike the fresh Linux/VM catalog reads. Temporal server traces
show approximately 201–203 ms live authentication plus 172 ms broker routing for
Linux/VM discovery, versus zero observed auth I/O and 185 ms routing for the Mac
catalog. The frame test binary does not export response request IDs, so those
server matches are identified as temporal candidates in the evidence.

Starting the temporary Mac publisher took **2,439 ms** before discovery. Including
that one-time publication, catalog and its first viewer gives **4,125 ms** from
the fixture's host-start timer to first frame. Publication is not charged again
when a host is already sharing; its internal capture/admission split is not
separately instrumented by this fixed test binary.

### Where the frame time went

Linux/VM stages use the same client's monotonic event clock:

| Journey | Connect → signaling ready | Ready → decoded frame |
| --- | --- | --- |
| Linux native | 392 / 380 / 341 ms | 276 / 227 / 227 ms |
| Linux VM | 501 / 359 / 359 ms | 409 / 419 / 408 ms |

After-ready time includes host capture/encoding, frame transfer and client
decoding. Existing factory/native journals did not contain per-frame spans for
this window, so those three components cannot be separated. The VM has a visible
~180 ms larger post-ready interval than Linux native in this cohort; the cause
is unresolved.

Mac WebRTC has additional negotiation stages. Each row below sums to its actual
first-frame latency; the stages do not overlap:

| Sample | Connect → signaling ready | Ready → offer | Offer → answer | Answer → channels ready | Channels → decoded frame | Total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 358 ms | 204 ms | 8 ms | 310 ms | 591 ms | 1,471 ms |
| 2 | 340 ms | 212 ms | 4 ms | 200 ms | 198 ms | 954 ms |
| 3 | 337 ms | 434 ms | 8 ms | 395 ms | 149 ms | 1,323 ms |

Initial ICE configuration became ready at 45–53 ms and overlaps signaling;
it must not be added again. The peer reached connected at 874 / 753 / 1,173 ms,
just before channels were ready at 880 / 756 / 1,174 ms. The first sample's
591 ms channel-to-frame interval is the largest individually measured remaining
Mac stage; it does not, by itself, identify capture, encoder or renderer cost.

The Mac fixture also acquired/released its control lease in 102 / 153 / 105 ms
without sending keyboard or pointer input. Linux/VM viewers remained read-only.

## Cached and live admission, with exact request IDs

A separate metadata-only publisher fixture alternated three live and three
cached-authority viewer admissions, each preceded by a catalog read. It sent no
images or input. Every cached viewer logged `route: local_access`, and all live
viewers logged `route: managed`. The viewer sockets were fresh HTTP/1.1
connections. The client timer uses monotonic system uptime.

| Operation | Three client samples | Median | Observed server work |
| --- | --- | ---: | --- |
| Live catalog | 1,112 / 456 / 466 ms | 466 ms | Auth 192–207 ms + broker 161–170 ms |
| Cached catalog | 376 / 255 / 237 ms | 255 ms | Auth 0 observed I/O; broker 162–178 ms |
| Live viewer ready | 1,302 / 1,327 / 1,977 ms | 1,327 ms | Auth 182–204 ms + broker 178–184 ms; additional service/upgrade wait remains |
| Cached viewer ready | 893 / 514 / 557 ms | **557 ms** | Auth 0 observed I/O; direct broker 164–179 ms |

Cached socket setup before the request took **561 / 195 / 238 ms**, while
request-to-response wait took **327 / 313 / 314 ms**. TLS intervals of
162 / 117 / 207 ms are included in socket setup, not additional stages. This
locates much of the higher client elapsed time compared with the earlier
366 ms median cohort; it does not establish whether network conditions or
client scheduling caused the setup delay. The broker interval remained stable.

The Mac one-minute load was **267–309 on ten cores** during this pass, compared
with 18.9 in that earlier cohort. Unrelated builds were not terminated. These
three-sample results are not p95 estimates or a controlled before/after test.

## Renewal and rejected-authority recovery

After ten seconds, the fixture renewed its active viewer using live credentials:
**528 ms** through the HTTP response and socket `renewed` acknowledgement,
HTTP 200, with an extended expiration. Exact request ID
`4fc5c638-5b5a-49e1-91da-b60c16a80142` joins client and server: **198 ms auth +
163 ms broker = 361 ms managed handler work**. Renewal did not use a cached token.
This is a direct protocol renewal, not a measurement of the native app's timer
scheduler.

The existing native-client rejected-snapshot test passed. It injected an invalid
snapshot, observed the 401 rejection marker, and then admitted the viewer after
one live-credential retry. The two viewer server requests in that isolated run
window were `090791f1-96e9-444a-ab78-785ead9087b9` (401) and
`05905715-d296-49fc-818f-5b812b7b28b9` (101). The unchanged native test binary does
not export their response IDs, so this is temporal correlation. Its entire test
invocation took 5.8 seconds including runner startup, catalog lookup and the
separate ICE rejection check; that is not a recovery-latency measurement.
Natural 120-second token expiration and forced network-disconnect recovery were
not repeated in this pass.

## Current stack rank within these screen journeys

1. **Starting an absent Mac publisher: 2,439 ms, once.** Largest setup span, but
   its internal stages are not split in this fixture.
2. **Live viewer admission: 1,327 ms median.** The initial cached path is faster;
   live fallback still pays authority reads and additional service/upgrade wait.
3. **Mac media negotiation and first frame after admission: 614–1,113 ms.**
   Offer delivery, peer/channel establishment and initial frame are separately
   visible above. The largest first-sample stage is channels→frame 591 ms.
4. **Fresh catalog: 503–601 ms in Linux/VM journeys.** Approximately 200 ms live
   auth and 172 ms broker are visible; the rest is client/transport overhead.
5. **Cached admission/socket setup and frame transfer.** Direct broker remains
   164–179 ms, while fresh socket setup can dominate the client result. Once
   admitted, Linux-native frames take 227–276 ms and VM frames 408–419 ms.

These are different scopes, not additive portions of one request. No source or
configuration change is proposed by this measurement-only report.

## Provenance, limits and evidence

- Deployed source: `c0d049f68`; managed
  `e1877ce4-0170-4de6-9cf9-ccfc003b7ece`; account
  `83d1e7ce-0db2-4637-9de8-8e9fd5f6968f`. Root independently verified the
  deployment receipt before the cohort. Exact-ID metadata requests reached SJC.
- Frame client: unchanged **debug runtime test binary**, built
  `2026-09-16T21:53:04Z`, SHA256
  `ab1978395667349ee0be860efb431c30472b4c2e9e53aff8f81aa3221d524264`.
  It is not the installed Release UI. Its source revision was not embedded in
  the prior build receipt; this pass does not claim coverage of later client
  source changes. The metadata-only Swift probe was built for this pass.
- Paired physical iPhones were listed, but the iPhone17Pro read-only lock-state
  check timed out after five seconds. No physical UI automation or unlock
  request was attempted; no physical-phone latency is claimed.
- [Sanitized measurements and correlated traces](screens-traces.json) retain
  exact metadata request IDs, native event timelines, run times, host load,
  version provenance and safely filtered server events. They contain no
  credentials, SDP or captured media. Native frame server windows are labeled
  temporal candidates rather than exact frame-to-request joins.
- Worker clock intervals advance at I/O boundaries: zero is not zero CPU.
  Do not subtract epochs from different isolates or add overlapping intervals.
  See [Cloudflare's timer documentation](https://developers.cloudflare.com/workers/runtime-apis/performance/).
