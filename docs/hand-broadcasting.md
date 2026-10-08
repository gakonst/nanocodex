# Broadcasting a Hand

Open a connected screen and choose **Stream RTMP** in the web or native Apple
screen viewer. Enter the complete RTMP/RTMPS publish URL (including the stream
key), select a quality preset, and start. The destination is cleared from the
form after submission. Stop the stream with **Stop stream**; closing the preview
leaves it running. One broadcast can run per published Hand surface.

The publisher runs on the Hand. The account broker relays authenticated control
messages and sanitized status; it does not carry the outgoing media. RTMPS
verifies the destination certificate. Stream keys are not persisted in account
state, viewer state, or diagnostics. FFmpeg receives the destination locally;
local process inspection by the machine owner can see its arguments/environment.

## Playback links for VLC and other players

An updated Rust Hand also exposes **Playback links** in the web and Apple screen
viewer. Choose 720p or 1080p and an expiry, then create and copy the URL. In VLC,
use **Open Network Stream** and paste that URL. It is an HLS playlist, so an
HLS-capable player can open it without Nanocodex authentication. Other players
must support HLS with H.264/AAC; a WebRTC signaling URL is not a media URL.

Anyone holding the link can watch the screen and hear available system audio.
It does not grant keyboard, mouse or account access. The URL is shown only once;
keep it private. The active-link list contains status and expiry, and **Stop**
revokes playback. Repeating an unconfirmed create uses the same operation ID;
if its URL was lost, stop that link and create another.

The CLI exposes the same operations:

```sh
nanocodex2 hand stream create MACHINE_ID --expires-in 3600 --preset 720p
nanocodex2 hand stream list
nanocodex2 hand stream stop LINK_ID
```

Use `--surface SURFACE_ID` when a Hand publishes more than one screen. The create
command returns JSON containing the playback URL. Avoid putting that output in
shared logs. Expiry can be 60 seconds through eight hours. One broadcast can run
on a Hand at a time, including RTMP; up to four playback links can be active per
account on separate Hands.

The Hand makes two-second MPEG-TS segments and uploads them to a dedicated
Worker Durable Object. Only the latest six segments are kept in memory; this is
live playback, not a recording. The Hand keeps a temporary rolling window so an evicted playback server
can refill its buffer while the publisher remains connected. Revocation or expiry denies subsequent
playlist and segment reads and stops uploading; a player's already-buffered
media may finish playing. Stopping the Hand or losing its publisher authority
also stops encoding. HLS adds player buffering and is intended for watching;
use WebRTC for interactive screen control.

The managed Worker must deploy the `ScreenPlayback` binding and migration before
an updated Hand advertises `playback: true`. Clients hide playback creation for
older Hands. Server uploads use a separate scoped token and cannot control the
screen. Link hashes and lifecycle metadata are durable; media and plaintext
view tokens are not. Public view tokens use a query parameter; both serving
Workers enable query-string redaction in Cloudflare logs and traces. Application
logs record only the route, never the playback URL. FFmpeg and screen/audio
permissions are the same as RTMP.

## Quality

All presets use H.264, preserve aspect ratio, and never enlarge a smaller source.
Desktop system output is encoded as AAC at 48 kHz stereo. Microphones are never
selected as a fallback. Paired iPhone MJPEG streams carry video only.

| Preset | Maximum dimensions | Output rate | Video target | Keyframe interval |
| --- | --- | --- | --- | --- |
| Source | 3840 × 2160 | 60 fps | 24 Mbps | 2 seconds |
| 1080p | 1920 × 1080 | 60 fps | 8 Mbps | 2 seconds |
| 720p | 1280 × 720 | 60 fps | 4.5 Mbps | 2 seconds |
| Twitch | 1920 × 1080 | 60 fps | 6 Mbps | 2 seconds |
| X | 1920 × 1080 | 30 fps | 9 Mbps | 3 seconds |

Output rate is an encoder target; repeating frames cannot add detail or motion
to a slower capture source. A VM's physical desktop and a paired phone's existing
capture are the upper bound on source detail. Mac publishers use VideoToolbox;
Linux and Windows broadcast encoding currently uses software H.264. Achievable
resolution/rate depends on capture, CPU/GPU, and uplink capacity.

The native Mac paths use ScreenCaptureKit for system audio and broadcast video.
Rust Mac previews also use ScreenCaptureKit, avoiding AVFoundation screen-input
stalls observed during verification. Broadcast capture is separate from the
interactive preview's resolution and encoding settings. Encoded Rust VM sources
can feed a separate broadcast without opening a viewer on the host display.

## Requirements and lifecycle

- Install FFmpeg on desktop Hands (Windows can use the bundled FFmpeg). The
  native Mac app also finds its bundled helper or Homebrew FFmpeg.
- Linux needs an explicit PulseAudio playback monitor. Server/Cloudflare images
  now include PulseAudio and create a private playback sink when necessary.
- Screen/system-audio OS permission must already be granted to the Hand.
- Broadcasts reconnect after transient ingest failure with bounded retries and
  buffers. Stop/shutdown cancels and reaps encoder children. Viewer disconnection
  does not stop a broadcast; unsharing or loss of publisher authority does.
- Old Hands do not advertise broadcast support and do not show the stream button.

Publish the managed broker before updated Hand binaries, then the web/native UI.
The catalog adds optional `broadcast: true`. Viewers send `broadcast` messages
with a request ID, action (`start`, `stop`, `status`), and, only for start, URL and
preset. The broker supplies the exact viewer/surface identity and fences host
replacement, lease expiry, stale correlation and cross-host replies. Results
contain only fixed states/errors and bounded media metadata. `stopping` is a
valid transient state while native Apple encoders drain.

## Verification

`scripts/test-hand-rtmp.py` starts a private loopback ingest, runs the **actual
product publisher test**, records FLV, and uses FFprobe plus full FFmpeg decoding
to check codecs, dimensions, frame rate, monotonic timestamps, keyframe interval,
and ending audio/video skew. `--outage-after 3 --outage-duration 2` kills and
restarts ingest without restarting the publisher. `--tls` adds an ephemeral TLS
proxy and trusts its test CA only in the publisher subprocess.

Compile before starting the receiver (its admission timeout is bounded):

```sh
cargo test -p nanocodex2-bin --bin nanocodex2 screen_broadcast
python3 scripts/test-hand-rtmp.py --output target/rtmp-validation/rust \
  --min-duration 7 --min-fps 55 --require-audio -- \
  cargo test -p nanocodex2-bin --bin nanocodex2 \
  screen_broadcast::tests::local_rtmp_sink -- --ignored --nocapture

swift test --package-path apple/NanocodexRemote
python3 scripts/test-hand-rtmp.py --output target/rtmp-validation/apple \
  --min-duration 7 --min-fps 55 --require-audio -- \
  swift test --package-path apple/NanocodexRemote --skip-build \
  --filter RemoteBroadcastTests/testLoopbackPublisher

cd hands/remote && go test -race ./...
```

For Rust fixtures, `NANOCODEX_RTMP_TEST_SIZE=3840x2160` tests source quality,
`NANOCODEX_RTMP_TEST_PRESET=x` tests the platform preset, and
`NANOCODEX_RTMP_TEST_ENCODED=1` exercises the VM encoded-source path. Native
capture is opt-in via `screen_native::broadcast_live_tests::local_rtmp_native`.
Go's `TestBroadcastRTMP` uses the production supervisor with synthetic media;
`NANOCODEX_RTMP_TEST_DESKTOP=1` switches it to real Wayland/Pulse capture.

Playback links are covered by a real workerd journey with synthetic accounts and
FFmpeg-generated H.264/AAC media. It verifies decoding, account isolation,
concurrent creation, expiry, revocation during creation, and recovery after both
DO eviction and a whole-runtime restart with a continuously running synthetic
uploader. A real Hand stops encoding when its publisher connection is lost:

```sh
pnpm --filter nanocodex-managed-service exec node --test test/screen-playback-journey.test.mjs
```

The fixture can also run the actual CLI or Apple HTTP client. It creates an
isolated account and passes its temporary credentials directly to the child
process; its saved transcript redacts tokens. Build the selected client first:

```sh
node js/managed/test/screen-playback-fixture.mjs -- \
  node js/managed/test/screen-playback-cli.mjs /absolute/path/to/nanocodex2
node js/managed/test/screen-playback-fixture.mjs -- \
  swift test --skip-build --package-path apple/NanocodexRemote --filter RemotePlaybackLinkTests
```

Native encoder checks use the production broadcast controller and FFmpeg against
a local HTTP upload sink. The server journey and native encoder checks use
synthetic media; actual desktop permission and capture still require a live Hand
verification after installation. Keep evidence in ignored `output/`.
