# Remote stream architecture

Follow-up to [responsiveness and diagnostics](2026-09-22-remote-responsiveness/README.md).

## Removed work

Native FFmpeg capture previously read a complete encoded packet, serialized its size and bytes into a 64 KiB in-process pipe, read arbitrary chunks back out, and reassembled the packet. Capture now distinguishes raw/legacy byte streams from complete encoded packets. A pull-based packet stream transfers its owned bytes to WebRTC directly. It adds no application frame queue. FFmpeg still supplies packet-length metadata: pipe reads and H.264 start codes do not reliably identify complete frames, and waiting for the next access-unit delimiter delays the first frame.

The Wayland adapter already assembled complete packets at its actual process boundary. Its intermediate 256 KiB pipe, frame serialization, forwarding task and second parser are removed. Reference-counted encoded packets pass directly from the shared capture to WebRTC. If its bounded broadcast receiver lags, it waits for an IDR frame before resuming; arbitrary H.264 delta-frame dropping would break decoder dependencies. Atomic chunk framing remains at the Waymote process boundary, where encoder replacement can interrupt a write.

The publisher's input loop now consumes already-parsed WebRTC events as values. It no longer serializes them into synthetic WebSocket messages and parses them again. Broker signaling still cannot inject input/control/frame requests into a WebRTC session. Lease validation, input age limits and disconnect release remain in the same policy path.

Both browser and Apple JPEG fallback receivers replace their FIFO with one active decode and one newest waiting image. Superseded decoded images are released without publication. Every received frame still occupies its credit until a current image is displayed; then exactly that received batch is replenished. This bounds memory and producer admission while allowing the decoder to catch up. A six-frame burst during a blocked decode takes two decodes and one current-frame publication, instead of six decodes. This rule applies to independent JPEG images, not H.264.

Browser WebRTC attaches one MediaStream and updates its tracks in place, avoiding reloads when audio arrives or a video track changes. It sends an SDP answer before draining early ICE candidates, matching the Apple viewer's ordering. Candidate application remains serialized and fenced to the connection attempt.

The RTMP adapter uses the same packet reader for native, framed and legacy inputs, so internal NCH264 headers never enter FFmpeg as video bytes. Raw pixel and PCM consumers require a byte capture explicitly; an encoded source cannot silently enter those paths.

## Platform paths

| Consumer / publisher | Media path and constraints |
| --- | --- |
| Web viewer | Native browser WebRTC video; bounded latest-image JPEG fallback; stable MediaStream; control requires its existing lease. |
| iOS / macOS viewer | Shared NanocodexRemote WebRTC/Metal viewer and bounded latest-image JPEG fallback. Same protocol and credit behavior. |
| macOS app publisher | Shared ScreenCaptureKit / RTCVideoSource feeds WebRTC. Desktop RTMP capture remains separate because it requires a different resolution, BGRA format and stereo audio; deleting it would remove functionality. Phone broadcast already shares captured pixels. |
| Native Rust macOS publisher | AVFoundation + VideoToolbox H.264 → direct packets → WebRTC. Private Unix metadata socket separates FFmpeg packet lengths from native diagnostics. |
| Linux Wayland publisher | Waymote + x264 or NVENC → atomic process framing → shared complete packets → WebRTC. Initial/lag recovery waits for IDR. |
| Linux X11 publisher | X11/FFmpeg H.264 → direct packets → WebRTC. |
| Windows publisher | GDI/FFmpeg x264 H.264 → direct packets → WebRTC. Native JPEG fallback remains available if encoder startup fails. This is not a claim of DXGI or hardware encoding support. |
| VM / legacy source | External byte transport retains bounded NCH264F1/NCH264C1/Annex-B parsing. A real process boundary still needs framing. |

Current Linux images install the Rust publisher as `nanocodex-remote`. The Go companion remains necessary for deployed Wayland hosts and the Mac app's paired-iPhone tunnel/runner. Removing the entire companion would break active paths.

Windows Hand compilation was also blocked by an unrelated unconditional reference to the Unix-only terminal IPC server. Server creation/receipt is now Unix-gated; Windows keeps terminal IPC unavailable with an explicit `Unsupported` result. This preserves Windows Hand media support without claiming a Windows TUI IPC implementation. CI now runs the shared media and terminal IPC crates on Linux, macOS and Windows.

## Validation

- Browser: 151 focused session/input/stats tests and the full account TypeScript check pass. Coverage includes six-frame coalescing, windows 2/3/6, starvation prevention, exact credit refill, unexpected frames, deadline/reconnect behavior, stable track attachment, older-browser audio-first readiness and 32 early ICE candidates with a blocked candidate operation.
- Apple: 62 focused viewer/frame/control/recovery/performance tests pass, including native WebRTC loopback. ARM64 iOS Simulator package compilation passes. A blocked six-frame burst takes two decodes, publishes the latest frame and refills six credits.
- Rust: 62 media tests pass on macOS and Linux (one additional Linux live case is gated), including real FFmpeg first-packet delivery, blocked-producer cleanup, malformed/truncated packet rejection and F1/C1/Annex-B normalization. Native packet storage identity is preserved. The Linux host passes 19 Wayland tests, including lag-to-IDR recovery and real process-pipe lifecycle. Workspace Rust formatting and full macOS host compilation checks pass.
- Terminal IPC: 15 tests pass on both macOS and Linux; the crate and its new unsupported-platform test cross-compile for x86_64 Windows MSVC. [Windows CI at 52e2bc55](https://github.com/gakonst/nanocodex/actions/runs/35746398255/job/106809127084) passes the shared media/IPC suites, capture/input adapter tests, and full native Hand identity/lease/reconnect tests. This establishes Windows compilation and protocol behavior; physical desktop capture remains unmeasured.
- Browser runtime: CUA verified the production viewer on an animated 60 FPS synthetic WebRTC source in Brave on macOS. Observed intervals were 58.6–59.6 decoded FPS at 960×540 VP8, zero dropped frames, 0.9–1.1 ms decode, 8.8–10.9 ms receiver buffer residence, and 1–2 ms selected-path RTT. First presented frame was 137 ms in this run. A text event reached the synthetic host; release returned the viewer to Watching. The fixture's unused return-audio element encountered autoplay policy; this was not an audio playback test.

These are correctness checks and local loopback samples, not matched before/after throughput benchmarks, physical iPhone/Windows capture validation or WAN input-to-photon measurements. The six-frame test measures eliminated decoder work deterministically; it does not imply a 67% end-to-end latency reduction. Existing full-app Inbox UI and unrelated repository CI failures are tracked separately from streaming validation.
