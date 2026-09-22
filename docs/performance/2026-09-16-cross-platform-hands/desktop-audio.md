# Desktop audio and streaming quality

The Wayland Go publisher and native Rust publisher send desktop output as a
stereo Opus WebRTC track: 48 kHz, 128 kbit/s, 20 ms packets. Audio shares the
existing authenticated peer and authorization lifecycle. Linux resolves the
actual playback sink monitor; Windows uses native WASAPI render loopback. Neither
falls back to a microphone. Frames-v1 and the macOS native publisher currently
remain video-only.

The web viewer retains both tracks in one MediaStream. Enable sound is a user
gesture so browser autoplay rules can unlock playback; a denied sound request
falls back to muted video. Closing the viewer detaches both tracks. Audio-source
failure does not disconnect video. The playback endpoint is selected at publisher
startup; changing the default endpoint requires restarting the publisher.

The Go Ogg reader checks page checksums, preserves individual packet boundaries
across page aggregation/continuation, and bounds every packet. Actual PipeWire
capture can aggregate two Opus packets even with a 20 ms page-duration setting;
a regression test covers this observed behavior. Rust consumes bounded stereo
PCM directly and encodes Opus; the Windows producer has a bounded queue and a
cancellation-aware lifetime.

## Measured sound, not merely a negotiated track

On Omarchy, a four-second 997 Hz WAV was played through the ordinary desktop
playback sink. An actual Chromium viewer received the live publisher through
production authorization/signaling. Its Web Audio analyser measured maximum RMS
0.10858 and the expected nearest FFT bin at 996.09 Hz. WebRTC received 319 audio
packets with zero loss and nonzero decoded audio energy. Audio and video tracks
were live; 377 video frames decoded during the measurement. No desktop audio
recording or credentials are retained in the evidence file.

This first measurement used the actual viewer component with an account-context
fixture and a local authenticated proxy. A separate published-app measurement
is recorded after deployment. Measurements are individual local wired-network
samples, not a guarantee for cellular or WAN connections.

## Quality configuration

`NANOCODEX_SCREEN_BITRATE_KBPS` accepts 1000 through 100000; the default stays 6000.
The Wayland publisher passes this bitrate to its existing video encoder. Windows
also accepts `NANOCODEX_SCREEN_MAX_DIMENSION` from 1280 through 7680, default 1280,
keeps the display aspect ratio, and never upscales. Its H.264 level accounts for
resolution, frame rate, and bitrate.

The Omarchy desktop was configured separately for 3840 by 2160 at 60 Hz, scale 2,
with NVIDIA NVENC and 60000 kbit/s. A 30-second 40 Mbit/s motion test decoded 1800
frames with zero dropped frames, loss, or freezes. At 60 Mbit/s, the subsequent
30-second motion test decoded 1758 frames, reported 61 fps, and again had zero
loss, drops, or freezes; average quantization parameter fell from 16.5 to 11.3.
These are separate live runs, not a same-frame image-quality comparison.

## Checks

Focused Rust screen/audio tests, Go race tests including a real FFmpeg Opus
encoder, browser viewer tests, full web typechecking, and the production build
cover the changed paths. The full account test run also exercises the existing
450-entry IndexedDB recovery test. That test exceeded its prior 10-second CI
budget and took 7.92 seconds locally; its budget is now 30 seconds with its
pagination assertions preserved. Existing documentation spelling and Rust
item-order lint failures were repaired without changing runtime behavior.

## Published verification

Account version `a6d8b881-ca3f-490a-9826-a1f20ee26b2a` serves
`index-CVenutuc.js`, verified byte-for-byte against the built artifact (SHA-256
`1777fbe9e21f234722fb4ffda1c9e80bed00d5c286ae40a80ca3c75de691df69`).
The full published `/connect` app, with no component fixture, then repeated the
997 Hz desktop tone test: RMS 0.10857, FFT 996.09 Hz, 327 audio packets and zero
loss. The actual video element reported 3840 by 2160 while its MediaStream held
both live tracks. It decoded 317 video frames with no packet loss and kept one
viewer connection. This explicitly checks the visible video element, since the
previous viewer replaced its entire stream when each new track arrived. Existing
open browser tabs need reloading to receive the updated track handling.

Windows separately measured 713 frames over 12 seconds (59.38 fps), zero lost or
dropped frames, and 1 ms RTT at 1920 by 1080 with a 20 Mbit/s ceiling (4.36 Mbit/s
actual traffic during that motion sample).

The final published Windows viewer then played a system WAV after the connection
was ready. Web Audio measured RMS 0.10238 and nonzero decoded energy. The actual
video element stayed 1920 by 1080 with both tracks live and sound enabled. Over
approximately 74 seconds including the playback wait, it kept one viewer socket,
received 3,625 Opus packets with zero loss, and decoded 4,339 video frames with
zero packet loss. A separate simultaneous motion/audio probe reported 59.65 fps,
716 frames over 12 seconds, zero drops or loss, and RMS 0.11028.

The same shared Rust audio tests pass on native Linux and actual Windows GNU.
The Windows native suite has eight passing tests. The final web suite has 165
passing tests, including 44 viewer tests; all nine focused Rust screen/audio tests
and the Rust CLI Clippy check pass. The Go suite passes with the race detector.
