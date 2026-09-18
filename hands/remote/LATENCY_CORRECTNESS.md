# Remote input and stream audit

This audit covers the Go companion and pinned Waymote source
`69f585e6a1dfb84a3ac8135f1a0536aa9d75f6c5`. It does not establish the
configuration or behavior of an already running host. No live input or process
restart is part of the verification below.

## Text delivery

Agent `type` becomes a single `remoteInput` text action; interactive key events
use HID press/release records. Text uses Waymote's input-method protocol by
default. XTEST and wtype are separate opt-in paths. An external typing command
failure must never fall through to another backend: a prefix may already have
been delivered. These paths are distinct from interactive key delivery.

The Go pipe writer now sends each text header and UTF-8 payload in one write,
at most 4016 bytes (within Linux's 4096-byte PIPE_BUF). Previously the header
could succeed and the payload fail. A later release record could then be
interpreted as text payload. Any failed or short write now closes the input pipe
and latches the failure; subsequent text/key/release calls cannot append to an
ambiguous prefix. Invalid UTF-8 is rejected before reaching the daemon. This
hardens the protocol; it does not prove a reported missing-character incident
had this cause.

The pinned upstream `RemoteInput.sendText` silently ignores text when its input
method is inactive and commits using the current input-method done serial.
There is no application-level delivery acknowledgement. Go splits only beyond
4000 UTF-8 bytes, so the multi-commit path does not explain partial loss inside a
shorter command by itself. The configured backend and an isolated terminal
read-back reproduction are still needed to distinguish IME handling from virtual
keyboard/XTEST delivery. Sleeps, automatic retries, or a global fallback change
would not establish correctness.

The HID map also now accepts PrintScreen, ScrollLock, Pause, Application, Power,
and F13–F24. Tests assert their exact evdev codes and both key edges. This is
protocol coverage; frontend emission, compositor bindings and application
behavior still require separate validation. It is not an all-keys live proof.

## Capture and transport pressure

The pinned upstream `VideoEncoder.zig` already owns a three-slot raw buffer pool
and replaces the pending frame on overload. Encoding happens off the Wayland
input loop. The Go wrapper removes FFmpeg `-re`, disables NVENC lookahead, and
uses encoder-reported access-unit boundaries by default. Preserve these existing
protections. The optional Annex-B scanner needs the next AUD (or EOF) to emit a
frame; it is not the preferred low-latency path.

Remaining boundaries requiring measurement or protocol changes:

- `h264Forwarder.read` waits synchronously for every RTP write; the capture
  callback ignores `WriteRTP` errors. A slow downstream write can backpressure
  the encoder pipe. Actual Pion sink blocking and cross-viewer impact have not
  been measured here. Instrument frame/write durations and errors before
  introducing bounded per-viewer queues. Dropping arbitrary encoded frames or
  packets would break H.264 reference dependencies; recovery must find/request
  an IDR with parameter sets.
- C1 framing carries frame sizes, not capture timestamps. RTP timestamps use
  forwarding time, so queued frames drained after a stall cannot be assigned
  their original capture time. A future timing extension needs a source
  monotonic clock and restart epoch; rawvideo frame-count PTS is insufficient
  when raw frames are skipped.
- JPEG fallback replies and signaling share a 128-message output queue and
  socket writer with a 10-second write timeout. One in-flight screenshot limits
  capture work but does not bound already queued image bytes or age. Bound
  outstanding replies through write completion and prioritize control between
  writes. That alone cannot preempt an image write already in progress.
- RTCP is drained without an application PLI/FIR response. The configured
  30-frame GOP at 60 fps is not a guaranteed wall-time recovery bound under
  stalls. Add a coalesced force-IDR protocol rather than restarting capture on
  each request.

## Bounded synthetic benchmark

On Darwin arm64, Apple M1 Max, one run of 1000 iterations per case:

| Operation | Time/op | Bytes allocated/op | Allocations/op |
| --- | ---: | ---: | ---: |
| Serialize 4096-byte text | 1.095 µs | 4224 | 3 |
| Frame/packetize 4 KiB | 3.841 µs | 19777 | 20 |
| Frame/packetize 64 KiB | 64.573 µs | 370177 | 138 |
| Frame/packetize 125000 bytes | 112.828 µs | 675662 | 243 |
| Frame/packetize 1 MiB | 939.936 µs | 6570761 | 1823 |

125000 bytes is the average frame size implied by 60 Mbit/s at 60 fps, not a
claim about actual encoded frame sizes. The packetization benchmark constructs
a new forwarder per iteration and uses in-memory synthetic encoded bytes and a
no-op RTP sink. Darwin emits 508-byte C1 payload records, while Linux uses
4092-byte payload records; these are not directly comparable platform results.
It includes neither an encoder nor real decoded content.

Run with:

```sh
go test -run '^$' -bench 'Benchmark(TextRecordSerialization|FramedH264Packetization)' -benchtime=1000x -count=1 -timeout=30s
```

After adding explicit backend isolation, rerunning text serialization with both
backend environment flags initially enabled passed at 1.181 µs/op (same
allocations). The benchmark clears both flags before exercising serialization.

Verification: `go test ./...`, `go test -race ./... -timeout=120s`,
`go vet ./...`, and Linux amd64/arm64 CGO-disabled cross-builds passed on the
Mac. Opt-in live desktop integration tests were not enabled. Linux pipe timing,
compositor behavior and application read-back were not exercised.

These numbers establish only local CPU costs. They are not before/after results
or measured capture-to-display improvements. End-to-end work needs correlated
input receipt, application response/capture, encode completion, send/receive,
decode, and actual presentation timestamps, plus p50/p95/p99 under controlled
network loss and writer stalls. Run that in an isolated desktop before claiming
nearly native latency or complete keyboard correctness.
