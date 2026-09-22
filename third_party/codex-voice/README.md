# Native voice

macOS packages use `webrtc-host`, a Rust helper built on LiveKit's pinned
`libwebrtc` bindings. Google libWebRTC owns Opus, adaptive NetEq playout,
CoreAudio device callbacks, echo cancellation, noise suppression and gain control.
There is no Swift helper, GStreamer pipeline or CPAL render queue in
this path. External synthesized PCM uses a bounded pipe ingress into this same
libWebRTC mixer, before its reverse echo processing and CoreAudio output. The bounded control protocol and same-build isolation remain shared
with the other platforms; provider credentials never enter the helper.

`pnpm build:voice-native` stages this engine on macOS. Both development and
release packages statically link libWebRTC and import only system frameworks.
`libwebrtc.json` identifies the engine; licenses include the inventory from the
exact linked native archive. Cargo.lock pins the Rust bindings and native build
package. Linux and Windows retain the upstream helper described below.


Ported from openai/codex `818f1cca8ccf8899f0f4d59336baebaccf358eed` (2026-09-10).
Apache-2.0 source and upstream unit tests are retained. Audio processing, RTP,
playout, mute epochs, bounded control protocol, and session actor follow upstream.
Nanocodex adapts package discovery, binary names, and owned child-process I/O.
The speaker format refresh fix from upstream
`7ef70f95d5c07976f3c992e413a02a6db238b0d0` (2026-09-11) is also included:
Bluetooth microphone activation can change the output sample rate, so speaker
restarts requery the device and rebuild render/playback conversion.
Nanocodex uses a 160 ms RTP jitter buffer (upstream: 60 ms) to absorb short
network scheduling bursts. A continuous-tone test exercises paced device callbacks
and 100 ms packet bursts, measuring audible coverage and playback gaps.
Transport test fixtures bind loopback to avoid local VPN/VM interface routing;
production binding is unchanged. The standalone
workspace keeps native build dependencies outside the public API and WASM graph.

Build the helper with `pnpm build:voice-native`. The native library does not link
GStreamer into the embedding process. Runtime libraries and required plugins live
beside the helper in `nanocodex-resources/voice`; microphone access begins only
after negotiation. The parent never sends provider credentials to the helper.


The legacy GStreamer builder can stage a prepared runtime with
`--runtime <prepared-runtime> --release`; receipts must be marked `publicRelease`
and match the selected target and source manifest. GNU Linux helper relocation
requires `patchelf`. The helper and embedding must use the same
`STABLE_GIT_COMMIT` (both default to `dev` in local builds). Keep the entire
`nanocodex-resources/voice` directory next to the embedding executable.
`NANOCODEX_VOICE_PACKAGE` can name an alternate package root containing that tree.

Stable, nightly, and PR distribution jobs build `nanocodex-voice-<target>.tar.gz`
with `scripts/build-voice-release.py` and checksum it alongside the executables.
On macOS this builds the Rust libWebRTC package. Other targets use the pinned
upstream native build tools, verify the GStreamer source archives, and seal a
public-release runtime receipt. Homebrew development libraries are not used for
release packages. CI extracts the final archive and
tests the relocated helper before publishing it. The helper and both CLIs share
the release's `STABLE_GIT_COMMIT`.

The installer and updater place this archive's files inside the selected version
directory before activation. Cached runtimes are checked file by file; missing
or corrupt resources trigger repair. On first voice use, managed stable installations
created by older updaters fetch the matching checksummed runtime automatically.
Custom development packages remain caller-owned. Releases predating the runtime asset remain
installable. An advertised runtime without a matching checksum or downloadable
asset fails installation, retaining the previously active version.

Validation:

```sh
cargo test --locked --manifest-path third_party/codex-voice/Cargo.toml -p nanocodex-webrtc-voice-host
cargo test --locked -p nanocodex-voice-native
pnpm build:voice-native
NANOCODEX_TEST_VOICE_RUNTIME="$PWD/target/debug/nanocodex-resources/voice" cargo test --locked -p nanocodex-voice-native --test packaged -- --ignored
```

The relocation test starts the physical helper from a path containing spaces,
initializes its private runtime, gathers a real audio SDP offer, and closes it.
It also checks duplicate initialization and build mismatch rejection. Device
processing tests use synthetic audio; these checks do not replace a live
microphone/speaker conversation against the authenticated Realtime service.

The Rust engine test sends continuous audio through a real Opus peer connection
and NetEq, checking decoded frames for gaps. The ignored `platform_devices` test
opens the local devices and checks initial mute and repeated capture restarts.

On macOS, suppressing a response disables the receiver track while keeping the
CoreAudio device running. Switching between hardware and synthetic playout on
speech interruptions disrupts full-duplex microphone capture. The managed CLI
holds microphone capture muted until the backend reports that the session is ready;
a user mute during startup remains in effect after that transition.

For local device regression checks, install BlackHole and run the ignored tests
serially (`-- --include-ignored --test-threads=1`). The capture test selects the
virtual microphone explicitly and measures continuity while repeatedly suppressing
playback. The playout test selects the virtual speaker explicitly and verifies
continuous output, silence during suppression, and continuous output after resume.
These tests do not change the system default devices. They test audio devices and
Opus locally; they do not establish subjective quality of a live provider call.

## External PCM on macOS

`RealtimeWebrtcSessionHandle` exposes async `begin_pcm_stream(generation, sample_rate)`,
`write_pcm(generation, Vec<i16>)`, `drain_pcm(generation)` and `cancel_pcm(generation)`.
Samples are signed mono PCM at 16, 24 or 48 kHz; each write is 1–960 samples.
Serialize writes and use strictly increasing nonzero generations for each session.
The host resamples 10 ms frames with libWebRTC's sinc resampler, retains at most
200 ms in its native queue, and backpressures without blocking the control actor.
The existing eight-command client queue also bounds queued pipe requests. A stuck
write or drain times out after five seconds. Cancel invalidates buffered and partial
PCM, and a stale write, drain or cancel cannot modify a newer stream.

`set_speaker_suppressed(true)` disables provider receiver tracks separately from
external PCM. It does not mute the microphone or restart the ADM. External audio
enters the existing factory's mixer; that mixed output goes through libWebRTC's
normal reverse APM reference and the same device output used by ChatGPT. No local
Opus loopback or second audio device is created. Keep microphone controls independent
so the server can detect speech during external playback.

Drain flushes a partial 10 ms frame and the resampler tail, waits for native mixer
consumption, then allows 100 ms of hardware grace. This is **not a DAC completion
receipt**: platform/Bluetooth buffers may be longer, and samples already submitted
to hardware cannot be recalled by cancel. This patch does not claim a measured
physical-speaker echo-rejection result; synthetic mixer tests verify ingress and
fencing. Legacy GNU Linux/MSVC GStreamer helpers explicitly return Unsupported for
external PCM; this path currently ships in the macOS libWebRTC package only.

`vendor/webrtc-sys` is the pinned 0.3.45 crate with a small factory-mixer patch,
not a different native WebRTC archive. See its `NANOCODEX.md` for patch boundaries.
Build and stage the actual Mac package with:

```sh
CARGO_NET_OFFLINE=true python3 scripts/build-voice-native.py --output target/local-elevenlabs
NANOCODEX_VOICE_PACKAGE="$PWD/target/local-elevenlabs" target/local-elevenlabs/nanocodex
```

The application executable must be built separately into that package directory;
the staging command places the helper/resources there. Set the same
`STABLE_GIT_COMMIT` for application and helper, or leave both unset for `dev`.
Keep the full `nanocodex-resources/voice` directory with the executable. Offline
builds require the pinned Rust crates and native archive already cached.

Device-free regression checks for the new path:

```sh
cargo test --offline --locked -p nanocodex-voice-native
cargo test --offline --locked --manifest-path third_party/codex-voice/Cargo.toml -p nanocodex-webrtc-voice-host pcm::tests
```
