# Remote screen responsiveness

This change covers the web, iOS, and macOS viewers and their shared signaling service.

## Connection setup

The host and viewer can request TURN credentials simultaneously. The managed service now shares in-flight credential generation for the same account and TURN key within a Worker isolate. Each caller receives its own no-store response. Work is bounded to 256 concurrent account/key pairs, failures leave no cache entry, and a subsequent connection can immediately retry. This removes duplicate upstream work; it is not a measured internet startup-latency guarantee.

Apple viewers send their SDP answer before draining early trickle candidates, then apply those candidates in order. Native canvases display an arriving video track while input channels finish opening. Control still requires the existing connection and exclusive lease.

## Pointer responsiveness

The browser sends the first motion sample immediately instead of waiting for a 4 ms batching timer. Sustained bursts still coalesce in 4 ms windows, retaining relative distance and flushing before reliable input boundaries. Under transport backpressure, one replaceable absolute-position slot retains the final mouse position and sends it when the queue drains. Click/key/scroll/release boundaries and lease changes discard old pending positions, so stale motion cannot replay after a later action.

## Publisher playback hint

The Rust publisher now negotiates the same RTP playout-delay extension as the Go publisher: minimum 0 ms, maximum 100 ms, in the extension's 10 ms wire units. This is a receiver playback preference, not a cap on total capture-to-display latency. It is attached only to video streams which negotiated it, using that viewer's extension ID and a cloned packet. Audio, viewers that omit the extension, default NACK/RTCP behavior, and other viewers' packet headers remain unaffected.

## What the measurements mean

Rates use differences between successive WebRTC counter reports, rather than lifetime averages. Missing measurements are shown as unavailable. Replaced streams, reset counters, and retired connections must not leak old samples into a new session.

- **Connection ready** is the time from the current connection attempt to usable transport and control channels.
- **First decoded frame** on Apple is the first frame handed to a WebRTC renderer. It does not include the display's presentation delay.
- **First frame** on the web uses the video's frame callback where available; older browsers can only report decoded readiness.
- **Decoded FPS** counts newly decoded frames over the sample interval. It is not a capture rate or a configured target.
- **Network RTT** comes from the selected ICE candidate pair. It excludes input injection, capture, encoding, decoding and display scheduling.
- **Receive buffer** divides the interval's jitter-buffer delay by the number of emitted frames.
- **Decode time** divides interval decode time by newly decoded frames.

Neither RTT nor receive-buffer/decode time is input-to-photon latency. End-to-end latency needs a controlled input whose visible response is timestamped or an external camera/photodiode experiment. Keep local loopback results separate from physical-device and WAN measurements.

The counter definitions follow [W3C WebRTC statistics](https://www.w3.org/TR/webrtc-stats/). Receiver buffering remains a best-effort preference under the [WebRTC specification](https://www.w3.org/TR/webrtc/#dom-rtcrtpreceiver-jitterbuffertarget), allowing the transport to handle jitter and loss.

## Validation

The accompanying tests cover concurrent TURN requests, account/key isolation, independent response bodies, failure/retry cleanup, and bounded in-flight capacity. Client tests cover counter deltas and control lifecycle behavior. Runtime results and remaining limitations are recorded with this change's final validation evidence.

### Verified checks

- Browser: 140 focused session/input/stats tests pass, as does the full account TypeScript check after building its workspace dependencies.
- Rust publisher: 57 tests pass, including supported and legacy real WebRTC peers, different extension IDs, one-byte/two-byte RTP wire formats, audio exclusion, and packet isolation. Formatting checks pass.
- Managed service: 60/60 TURN and real Durable Object signaling tests pass; TypeScript check passes after building the repository's protocol/tools/WASM/evaluator prerequisites.
- Apple package: full macOS Swift run completed 127 tests with 15 gated live-device/account cases skipped and no failures. A subsequent 14-test focused run passed, including real encoded/decoded WebRTC media and input, stats lifecycle, and an offer with 32 early candidates. The shared package also builds for arm64 and x86_64 iOS Simulator.
- An additional existing agent adapter test fails on both the changed branch and the original service implementation: the reconnect-fencing assertion expects `unavailable` but receives `cancelled`. This is recorded separately from the passing streaming tests.

The native synthetic 320 × 240 viewer fixture recorded 15.16 ms connection readiness and 65.16 ms to its first decoded frame. A separate peer test recorded 416.2 ms startup-to-first-decoded-frame and, over a one-second interval, 20.43 decoded FPS, 2.51 ms decode/frame and 0.018 ms jitter-buffer residence. These fixtures feed static frames with 33 ms capture pacing; they establish transport and instrumentation behavior, not 60 FPS throughput, internet performance, or before/after improvement. Physical iPhone and WAN/TURN performance have not been measured in this change.

### Browser runtime check

`node js/account/scripts/remote-performance-fixture.mjs` serves the production Screen component and RemoteBrowserSession with a real local RTCPeerConnection and the existing synthetic publisher. It does not open physical capture devices or use account credentials. Open the printed localhost URL, select Stats, take control, send synthetic text, and release control.

In Brave on macOS, CUA verified decoded video at the fixture's 5 FPS target, 960 × 540 VP8, zero dropped frames in the observed interval, 0.4–0.5 ms decode time, and approximately 1–3 ms selected-path RTT. First-frame time on that run was 818 ms; it is a single local correctness sample, not a before/after comparison. Text input reached the synthetic host and releasing control returned the viewer to Watching. The fixture's unused return-audio sink reported a browser autoplay restriction; microphone/audio playback were outside this check. A separate delayed-channel fixture visibly rendered video while its input channels were withheld for 12 seconds and Take control remained disabled.

No production deployment or physical-device installation is part of this validation.
