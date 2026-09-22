# Screen admission follow-up — 2026-09-16

## Change

Screen discovery (`GET /hands/screens`), initial viewer admission (`GET
/hands/view`, WebSocket), and TURN preparation (`POST /hands/ice`) can reuse the
existing credential-bound 120-second managed access snapshot. The native apps
share the in-memory snapshot with their managed Agent requests. Discovery issues
it on the existing response, without a separate authentication request.

Publishing and lease renewal continue to authenticate against current account
state. A viewer renews every ten seconds and the broker retains its thirty-second
lease expiry. Reusing a snapshot never extends its expiry. Changing the login/key
or origin prevents reuse; signing-key rotation invalidates it. Account changes
can affect initial admission after the snapshot expires, while the next live
renewal still checks current authority. Surface existence, generation, ownership
and capability checks remain at admission.

If an initial viewer upgrade receives an explicit pre-admission snapshot
rejection, the native client discards that cached token and retries once with the
live credential. It does not retry ordinary authorization failures or replay any
post-admission messages. Publishers never use this retry path.

## Measurement boundaries

The native URLSession probe opens actual authenticated WebSockets through the
public account origin. Its fixture publishes only temporary screen metadata; it
does not capture a screen or send input. Fresh viewer samples are separate from
full WebRTC/first-frame samples. The real Mac fixture captures three sequential
viewers and checks decoded frames plus acquire/release without sending input.

Before this change, four traced native host sockets took 1,086–1,551 ms to first
`ready`. Managed Worker execution took 364–431 ms, including 190–261 ms in live
authentication and 170–181 ms in account routing. URLSession reported
995–1,209 ms between request completion and first response byte. The first
connection also incurred a 338 ms TCP/TLS setup; subsequent connections took
60–73 ms. TCP totals include TLS and must not be added to the TLS subspan.

These spans are nested, not additive. Worker rows were matched to the sequential
fixture by timestamp and order. At this baseline stage, the remainder outside the measured managed
Worker was unassigned. Account proxy tail wall time can include WebSocket
lifetime work, so it is not a measurement of service-binding overhead.

The Mac had substantial concurrent build load (one-minute load average
131–157 on ten cores) during these initial runs. Client scheduling, encoding and
network variation limit total-latency comparisons. Authentication `Server-Timing`
and Worker stage logs provide the direct component comparison.

## Before-deployment video baselines

| Journey | Signaling ready, ms | First decoded frame, ms |
| --- | --- | --- |
| Mac → same Mac, WebRTC | 623 / 1,192 / 1,343 | 1,487 / 1,758 / 2,109 |
| Mac → Linux native host, frames-v1 | 1,084 / 1,236 / 1,174 | 1,346 / 1,449 / 1,423 |
| Mac → disposable Linux VM, frames-v1 | 1,178 / 1,104 / 1,257 | 1,616 / 1,516 / 1,530 |

Times start at screen selection and exclude catalog lookup. Catalog took
759 ms for the Linux host and 1,166 ms for the VM in these individual fixtures.
Both Linux publishers currently use bounded JPEG frame relay. These runs verify
WAN delivery of real decoded frames; they do not measure mobile or WAN WebRTC.
The disposable VM was deleted by its owning benchmark after its viewers closed.

The first WAN fixture exposed a measurement bug: frame-relay sessions reached
Watching with a decoded image but never populated `first_frame`, which only
covered WebRTC. The diagnostic path now records signaling readiness and the first
JPEG decode as well. Initial timeouts were missing diagnostics, not failed screen
connections. No frame images are retained in the report.

See [curated component traces](screen-admission-measurements.json). The account
proxy additionally logs the exact `NANOCODEX_BACKEND.fetch` wait, correlated by the
response request ID, without wrapping the response or recording queries, tokens,
SDP or image data. Its deployed measurements appear below.

## Validation

- Managed access policy: eleven tests passed, including token expiry, credential
  binding, publisher/renewal exclusion and preserving the upgraded socket.
- Managed TypeScript check passed after building its workspace protocol package.
- Native Remote package: 58 tests, eleven opt-in tests skipped, zero failures.
- InboxCore managed access: three tests passed, including account isolation and
  explicit handshake-rejection invalidation.
- Live Mac baseline: three first decoded frames and control acquire/release passed.
- Frame diagnostics follow-up: thirteen viewer tests passed; three actual decoded
  WAN frames each passed on the Linux native host and a disposable Linux VM.
- Account proxy: nine tests passed, including forwarding snapshot/rejection
  headers, preserving response identity and omitting private query/auth data.

The final live measurements follow below; physical-device delivery is tracked
by the integration owner.

## Final deployed measurements

The final cohort ran 22:07:49–22:08:39 UTC against managed version
`dd307c9e-d2aa-4752-af53-ea1559716905` (source `3b51b5fa`) and account version
`c627a2a8-6dc6-475e-af81-22fecdc2bbb6` (built from `8b4077a4`). Account deployed
last; its root build and TypeScript checks passed. Container rollout was skipped
because this change only affects the Worker. The curated JSON separately retains
an intermediate cohort whose exact deployed source was not established.

Four alternating live/snapshot viewer pairs show live authentication at
189 / 196 / 196 / 191 ms, versus 0 / 0 / 0 / 0 ms with the snapshot. Worker clocks
advance at I/O boundaries: zero means no observed I/O wait at this resolution,
not zero CPU cost. Managed total execution was 360–378 ms live versus 161–192 ms
with the snapshot. This supports removal of approximately **190–200 ms of
repeated authentication wait**. It does not imply the full socket connection
becomes 200 ms faster in every sample: ready medians were 1,102 ms live and
1,041 ms with the snapshot, with substantial network and scheduling variation.

Seven of eight requests have exact request-ID matches across the native client,
account proxy and managed Worker (the tail missed the first account event):

| Nested component | Measured range | Median |
| --- | --- | --- |
| Account service-binding fetch wait | 689–1,083 ms | 860 ms |
| Same-request managed execution inside that wait | 161–378 ms | 192 ms |
| Account fetch wait minus managed execution | 482–922 ms | 555 ms |
| Client response wait minus account fetch wait | 112–230 ms | 125 ms |

The subtraction is across paired request IDs; the separate column medians must
not be subtracted. The dominant unexplained wait is now localized inside the
account-to-managed service call, outside managed handler execution. It can
include dispatch, transport and upgrade return handling; these measurements do
not isolate the platform mechanism. The next investigation should measure or
remove that boundary, rather than assume another database query accounts for it.
The exact fetch span excludes the subsequent open WebSocket lifetime.

### First decoded frames

| Journey | Baseline frames, ms | Final frames, ms | Median before → final |
| --- | --- | --- | --- |
| Mac → same Mac, WebRTC | 1,487 / 1,758 / 2,109 | 1,935 / 1,728 / 2,262 | 1,758 → 1,935 ms |
| Mac → Linux native, frames-v1 | 1,346 / 1,449 / 1,423 | 1,136 / 1,253 / 1,638 | 1,423 → 1,253 ms |
| Mac → Linux VM, frames-v1 | 1,616 / 1,516 / 1,530 | 1,182 / 1,130 / 1,620 | 1,530 → 1,182 ms |

These are three sequential decoded-frame samples per path, not a percentile
benchmark. Final Mac load averages were 93–99 on ten cores. The Linux reductions
are observed cohort differences, not precise causal estimates. Same-Mac first
frame did **not** improve in this run: signaling reached ready at 932–958 ms,
then negotiation and capture/encoding/decoding under load still consumed the
remaining time. Connected-to-first-frame was 215–640 ms. Initial ICE preparation
was 60 / 66 / 332 ms, versus 317 / 251 / 222 ms before.

Final catalog lookup was 222 ms for the Mac fixture (its publisher preparation
had already warmed shared authority), 521 ms for Linux native, and 534 ms for the
fresh VM. Catalog is excluded from all first-frame values. The two Linux fixtures
both decoded real 1280×800 frames over WAN relay. The VM remained alive until all
viewers closed, then its owning benchmark cleaned it up.

The real rejected-snapshot regression also passed against the final public
service: HTTP rejection headers reached the native client, and an initial
WebSocket rejection recovered with one live admission attempt. The final Mac
fixture passed three decoded frames and control acquire/release; WAN fixtures
sent no input. Physical iPhone performance remains unmeasured because UI
automation timed out while enabling automation mode; installation and launch
of the signed Release app are tracked by the release owner.
