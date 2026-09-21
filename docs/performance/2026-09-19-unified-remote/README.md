# Isolated publisher streaming and control checks

This is a synthetic loopback test, not a live game latency measurement. Container
`nanocodex-unified-perf-20260919` has 2 CPUs and 4 GiB, no host mounts or physical
input devices, and only a loopback port mapping. See `container.json` and
`versions.txt`. The amd64 image runs under emulation on an arm64 Mac.

The first real Waymote run failed `ProcessMonitoringUnavailable` during encoder
startup (`go-waymote-failure.json`). The fallback replaces Waymote with a 16-byte
input-protocol consumer and C1-framed, software-H264 source. It excludes the
compositor, native injection and screen capture. Both publishers use that same
source and their production WebRTC/input-lease paths. Rust accepts the same
`wayland-host` command-line interface; no benchmark-only Rust adapter is used.

Requested settings: 1280×720 at 60 fps, software libx264, 6000 kbps ceiling,
ultrafast/zerolatency, GOP 30, 100 kbit VBV. Static alternating black/white frames
compress far below the bitrate ceiling. A generated 440 Hz tone enters a private
Pulse sink and reaches the browser over the publisher's Opus track. No physical
microphone is opened. The relay uses a synthetic loopback-only credential.

Timing starts immediately before an input DataChannel send and ends when
`requestVideoFrameCallback` sees the corresponding decoded pixel transition.
`expected_display_ms` separately records Chrome's scheduled display timestamp.
Neither is a photon measurement. Thirty sequential transitions follow a warmup.
First-frame timing starts with viewer JavaScript, before signaling and negotiation.

## Recorded checks before the microphone fix

| Run | Samples | Median / p95 input-to-decode | First frame | Received fps | Dropped / lost video |
| --- | ---: | ---: | ---: | ---: | ---: |
| `go-final-result.json` | 30 | 95.4 / 137.9 ms | 2256.4 ms | 60 | 0 / 0 |
| `rust-controls-result.json` | 30 | 59.9 / 78.7 ms | 2410.7 ms | 60 | 0 / 0 |

Both passed simultaneous left/right holds for 12 seconds across renewals,
independent left release while right remained held, 200 relative moves while
held, and release-all clearing buttons plus a modifier. Both received nonzero
speaker audio energy with zero reported audio packet loss. These are short,
sequential runs on a shared host; background builds differed. **Do not interpret
the numbers as a causal production speedup.**

Rust binary for those runs: commit `c48fbf1ca591952363aec8ac6049ea5abd56fa8b`,
SHA256 `cb89ff993447a17000eefc5401cb3ee468d2c35d33d3026465bf38d0169c1776`.
Managed `hand` builder image:
`sha256:925c6fd0d98af9d831a306ab6050adac54f70d5392583874db0860e1f55c6c0f`.
The Go executable SHA256 is in `versions.txt`.

`rust-final-first-mic-failure.json`, `rust-mic-debug-result.json` and
`rust-mic-verbose-result.json` retain microphone failures: opt-in acknowledged,
then an unsolicited stop before explicit mute. One run delivered no PCM; two
contained only a brief portion of the generated tone. They do not pass audio
continuity. The current harness rejects such unsolicited stops even if a sample
contains some tone energy.

The null-sink-only fixture also showed PulseAudio automatically choosing the new
non-monitor virtual source. This follows [PulseAudio's source selection](https://github.com/pulseaudio/pulseaudio/blob/master/src/pulsecore/core.c).
It is permitted only for this fixture's previous monitor source; playback default
must remain unchanged. Production code does not call `set-default-*`.

Earlier `go-synthetic`, `go-c1` and `go-c1-controls` results are retained as
preliminary runs. The oldest source used Annex-B framing, GOP 60 and a larger VBV;
do not mix those settings with the table. `rust-initial-c1-controls-result.json`
records the loopback TLS-provider startup failure that led to the startup fix.

## Reproduction

Prepare an isolated container with the recorded resource limits, FFmpeg,
PulseAudio, labwc, Chrome, `aiohttp==3.14.3` and `playwright==1.63.0`. Copy this
directory to `/perf` and make `synthetic-waymote.py` executable. Run sequentially:

```sh
python3 /perf/harness.py --synthetic --controls --label go-new
python3 /perf/harness.py --publisher /usr/local/bin/nanocodex2-candidate --synthetic --controls --microphone --label rust-new
```

The optional microphone check negotiates sendrecv audio, generates a 660 Hz Web
Audio track, requires a generation/request-matched enable ACK, and samples the
host's stable virtual input before enable, during delivery and after mute. RTP
continues through mute so silence tests host gating. It also checks that the
received tone differs from the outgoing 440 Hz speaker tone. No `getUserMedia`
is used. Preserve failures and executable/harness hashes with each new run.

The harness cleans up its own process groups. Use `docker exec -d` for a detached
run; copy results out after completion. `run.sh` is a convenience launcher for an
already prepared fixture; it does not install dependencies or deploy a backend.

## Microphone diagnosis

`rust-audio-diagnostic-result.json` and its publisher log confirm a PCM write
timeout approximately 1.2 seconds after enable. A controlled configuration
experiment (`rust-audio-norewinds-experiment-result.json`) kept the binary and
workload unchanged and appended `norewinds=1` only to the owned Pulse null sink.
That run passed: 660 Hz amplitude 0.1994, RMS 0.1410, zero baseline/muted RMS,
no unsolicited stop across renewals, and all held-input checks. The temporary
`pactl` wrapper was removed after this experiment.

[PulseAudio's null-sink implementation](https://github.com/pulseaudio/pulseaudio/blob/master/src/modules/module-null-sink.c)
uses a two-second maximum rewind window by default and a 50 ms window with
`norewinds`. The final implementation selects this option for PulseAudio only;
[PipeWire documents different supported module options](https://docs.pipewire.org/page_pulse_module_null_sink.html).
An isolated test also verifies writing before a game opens the input, then
receiving PCM, mute/reopen lifetime and final device cleanup (3/3 tests passed).
The clean final binary run below uses the actual implementation, with both temporary
`pactl`/`pacat` wrappers absent.


## Final clean run

Both builds had completed before these runs. The same harness/source and container
resource limits were used, sequentially, with a generated speaker tone. Rust also
negotiates return audio; its microphone tone starts after the video timing phase.

| Run | Median / p95 input-to-decode (30 samples) | First frame | Received fps | Video dropped / lost |
| --- | ---: | ---: | ---: | ---: |
| `go-clean-result.json` | 57.7 / 80.8 ms | 2305.5 ms | 60 | 0 / 0 |
| `rust-complete-result.json` | 43.8 / 67.9 ms | 2573.2 ms | 59 | 0 / 0 |

Both held-input suites passed and both received nonzero speaker audio energy with
zero reported audio packet loss. Rust's full microphone check passed: baseline and
muted RMS 0, enabled RMS 0.14098, 660 Hz amplitude 0.19937, no unsolicited stop,
and continuous delivery across lease renewals. The previous monitor-only input
was automatically replaced by exactly the owned virtual input; playback selection
stayed unchanged. No physical microphone or game was involved.

Rust had lower observed input timing and a slower first frame in this single pair.
This short emulated fixture is insufficient to establish either effect in
production, particularly with real capture, motion-heavy video, network variation
or physical clients. There is no measured live WoW performance claim.

Final source: `ff363e09ac2463708489708785116ae8712ceef4`.
Rust binary SHA256: `fc11eaae1c14de7cd17edca2012913b1c3509a0117f8e45f1706e22758d5c12b`.
Builder image: `sha256:8b00ec6b7fb9b3686927627fd5923ce6c83ae2e171136e904487023da5a917cb`.
This final local builder changed only Cargo build parallelism from 2 to 8 jobs;
release profile, target, dependencies and runtime settings remained identical.
Harness SHA256: `ff0987a71ed4f84e16e3b1870687783cbd717b1522a81a5d0acc83e0f2d0b08d`.
The result JSONs also record executable and source hashes. Both harness exits were 0.
