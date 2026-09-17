# Windows encoded-frame latency

Measured 2026-09-17 on the existing KVM Windows 11 guest (4 vCPU, 8 GiB, VirtIO display; no GPU passthrough), native 1920×1080 at 60 Hz with a 20 Mbps software H.264 ceiling and system audio enabled. The same published Chromium viewer and wired LAN path were used in every reported run.

A temporary Windows Forms window switches between solid red and blue on Space. After an explicit focusing click, the browser sends 20 actual leased WebRTC key-down/up pairs, separated by 150 ms. Each sample measures from data-channel send to requestVideoFrameCallback expectedDisplayTime for the first decoded color change. Pixels are sampled away from the captured cursor. This includes guest input/UI, capture, encode, network, decoder buffering, and scheduled browser display; it is not a photodiode or physical display measurement.

| Native encoder forwarding | Median | p95 |
| --- | ---: | ---: |
| Annex-B delimiter lookahead | 170.8 ms | 205.3 ms |
| Exact encoded packets | 151.5 ms | 183.0 ms |
| Exact packets, final rebuilt payload | 154.8 ms | 184.3 ms |

All reported runs had zero video packet loss and zero dropped frames. These short local samples show approximately one 60 Hz frame less latency, not a universal latency guarantee. A separate 12-second stream decoded 716 frames (59.65 fps) at 1920×1080 without drops/loss. Native SendInput dispatch itself previously measured 0.18–0.35 ms.

FFmpeg tees packet-size metadata before matching H.264 bytes; bounded readers forward complete packets without waiting for the next capture. The shared native Rust path applies to Windows, macOS, and Linux. The legacy Annex-B parser remains supported, and `NANOCODEX_SCREEN_FRAME_BOUNDARIES=annexb` restores delimiter forwarding. Each native capture owns a fresh FFmpeg child and pipe; it does not splice restarted encoders into an inherited pipe.

Nine shared video/framing tests passed on macOS and Windows, including the actual bundled FFmpeg delivering one encoded frame while raw input remains open. Malformed/truncated metadata and oversized lengths fail closed. Tests also cover split input, repeated framing headers, and Annex-B fallback. A separate local macOS check confirmed libx264 and VideoToolbox packet lengths matched their Annex-B output. Actual macOS/Linux desktop latency was not measured in this Windows comparison.

Sanitized individual samples and counters are in [windows-encoded-frame-latency.json](windows-encoded-frame-latency.json).
