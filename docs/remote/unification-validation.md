# Unified remote implementation: validation and rollout

This branch integrates the streaming/backend, native input, iPhone controller and
WoW addon work. Runtime checks below have different scopes; a passing synthetic
fixture is not a live WoW or native mouse demonstration.

## Implemented

- `nanocodex-remote` owns Rust publication, peer preparation, framing, leases and
  microphone lifecycle. Native CLI and standalone Linux Wayland/desktop/server
  commands delegate to it. Both Docker definitions select that publisher.
- Native viewer input tracks buttons independently, flushes movement before
  discrete events, renews held input, and releases on focus loss. Native
  fullscreen includes visible Take/Release Control and exit controls.
  Control–Command–F toggles fullscreen; Command–Shift–Escape releases capture.
- Speaker mute and microphone opt-in are independent. Microphone permission and
  capture wait for a matching host acknowledgement. Revoke, disconnect and
  audio interruption stop capture. Linux supplies a machine-scoped virtual input;
  macOS/Windows host microphone sinks remain unavailable.
- The iPhone controller sends physical gamepad identities, axes and triggers.
  It no longer invents spell/action bindings. Actual WoW bindings remain a
  separate in-game validation gate.
- `examples/wow` contains the addon and companion bridge, isolated from transport
  implementation. Asset provenance and runtime setup are documented there.

The Go implementation remains for rollback/comparison and the paired-phone
companion. Apple screen publication remains native Swift. This is not a claim
that every platform now runs one capture implementation.

## Evidence

| Check | Result and scope |
| --- | --- |
| Shared Rust core | 51 tests passed on Mac and Linux; isolated Pulse hardware fixture is separately exercised |
| Linux host/platform | 32 tests passed, 1 hardware-dependent test ignored; CLI check and release build passed |
| Linux virtual microphone component | 3 tests passed against an isolated Pulse server, including generated PCM, device lifetime and ownership cleanup |
| Native Swift | 62 focused tests passed; Mac Release and iOS Simulator builds passed |
| Installed Mac app | Final signed Release app (`ff363e09`) installed in `/Applications/Nanocodex.app`, with a rollback bundle; signature, matching helper executable and running process verified; previous bundle retained in AppBackups |
| Web remote control | 62 existing focused tests passed |
| Addon/bridge | 135 application and 77 transport Python tests; 4 Lua addon tests plus autostart/chord suites passed |
| Addon carrier integration | Actual Lua/Python codecs and journals exercised with mocked WoW/backend; no live game reply claimed |
| Managed image | Rust `hand` builder target passed; final complete managed/server images have not been deployed |
| Streaming/control fixture | Go and Rust received 720p video and audible-energy Opus. Both passed 12-second simultaneous mouse holds across renewals, independent release, 200 relative moves and release-all |
| Full microphone WebRTC fixture | Clean final binary passed: continuous 660 Hz input across renewals, zero PCM before opt-in/after mute; outgoing speaker tone and held-input checks also passed |

The full WebRTC fixture found a PulseAudio null-sink buffering stall. A controlled
`norewinds=1` experiment fixed it without increasing the 100 ms write timeout.
The implementation applies that Pulse-specific setting only when the server
identifies as PulseAudio; PipeWire retains its native scheduling. Permission
renewals also now preserve in-flight sink work; revoke/expiry still cancel it. A Go race-instrumented baseline exposed a snapshot timing failure; it is
not reported as a clean full race suite. An attempted simulator UI test hung;
iOS compilation is not a phone interaction result.

Exact synthetic workloads, binary identities, raw successful and failed results
are in [the performance directory](../performance/2026-09-19-unified-remote/README.md).
The real Waymote run failed encoder process monitoring under amd64 emulation.
The fallback replaces capture/input injection with a wire-compatible synthetic
source; it measures publisher/control/encode/browser decode only. Short runs
under shared host load do not establish a production speedup.

## Remaining live gates

- Test native mouse holds, drag/camera motion, modifier chords, fullscreen/focus
  transitions and release against the exact live WoW host. Omarchy's available
  CUA provider failed to connect to the desktop (X11/authorization); no alternate
  input path was used to bypass that boundary.
- Roll out and check the Rust host on the actual game machine, including its
  encoder, controller device and microphone routing. Production host services
  and Cloudflare images have not been replaced by this branch.
- iPhone is offline. Verify physical controls, background/reconnect release and
  WoW's current bindings after streaming validation.
- Install/reload the addon in WoW and demonstrate send, incremental/final reply,
  reconnect and opt-out. Current addon evidence uses mocked game/backend APIs.
- Confirm physical speaker output and microphone input on the native clients.
  No physical microphone was opened for the synthetic tests.
