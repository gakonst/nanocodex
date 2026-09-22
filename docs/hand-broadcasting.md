# Broadcasting a Hand to RTMP

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

Recorded private-ingest results on 2026-09-17:

| Path | Received output | End A/V skew | Result |
| --- | --- | --- | --- |
| Rust Mac actual display + system audio | 3840×1600, 60 fps | 9 ms | Full decode |
| Rust maximum-quality fixture | 3840×2160, 60 fps | 2 ms | Full decode |
| Rust X preset | 1920×1080, 30 fps, 3-second GOP | 4 ms | Full decode |
| Rust encoded VM-source fixture | 640×360, 60 fps | 12 ms | Full decode |
| Rust RTMPS with trusted test CA | 640×360, 60 fps | 17 ms | Full decode |
| Swift actual display + system audio | 1920×800, 60 fps | 12 ms | Full decode |
| Swift maximum-quality fixture | 3840×2160, 60 fps | 22 ms | Full decode |
| Go publisher on Linux | 1920×1080, 60 fps | 5 ms | Full decode |
| Go Linux after ingest outage | 1920×1080, 60 fps | 13 ms | Full decode |

Rust, Swift and Go recovery tests passed. Rust native preview also decoded 60
H.264 frames in isolated headless Chromium. Protocol/client tests, package type
checks, focused Rust tests, the Swift package suite, and the full Linux Go race
suite passed. The actual Windows capture, physical paired phone, and live
Wayland capture were not exercised in this run. No public Twitch/X broadcast or
production deployment was performed; platform ingest still requires the user's
stream endpoint and account eligibility. Private native recordings stay local
and are not part of the published test report.
