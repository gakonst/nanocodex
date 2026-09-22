# Unified remote streaming

## Scope and integration baseline

The integration branch starts at upstream `365f5755a` and includes the three
isolated streaming changes `7b605ab7b`, `6ae422118`, and `67a789511`.
It reconciles the existing native mouse/fullscreen changes with the already
merged native gamepad protocol. The original working checkout stays intact.

The target is one Rust publisher/session implementation with platform adapters,
not a common API hiding two independent session implementations. Platform
adapters retain the native capture, encoder and input mechanisms. Wayland must
share the existing compositor rather than silently creating an Xvfb desktop.

## Required invariants

- Account authorization and control ownership are checked independently of media.
- One viewer owns input; lease generations reject stale packets after takeover.
- Left/right/middle buttons and keys have independent down/up state. A hold has
  no arbitrary click-duration timeout while its authorized owner is responsive.
- Relative movement and transitions are ordered, bounded, and lossless. Ordinary
  absolute hover motion may replace older motion without dropping transitions.
- Focus loss, disconnect, lease expiration, backgrounding and explicit release
  clear held input. Reconnect cannot revive an old hold or microphone session.
- New viewers, ICE requests, screenshots, encoder startup/shutdown and broadcast
  management cannot block established viewers' input processing.
- Capabilities describe devices that were successfully initialized. No gamepad
  grant without a working virtual controller; no microphone grant without a
  working return-audio sink.
- Desktop sound uses the playback monitor. Viewer microphone transmission is
  explicitly enabled, visibly indicated, muted by default and tied to ownership.
- Queues and buffers are bounded. Device errors are surfaced; a stopped audio
  device is not shown as working just because the video connection remains live.
- H.264 access-unit boundaries come from the encoder. Waymote's inherited pipe
  must recover from encoder replacement using atomic C1 records. Pipe reads are
  not frame boundaries.

## Work order

1. Reconcile input and backend protocols and provide the shared publisher.
2. Exercise native capture/encode/transport/audio/input together and compare
   streaming performance under matched conditions.
3. Verify the phone controller in the running game after the streaming checks.

Addon/bridge implementation and the phone controller UI proceed independently
of this critical path. The addon bridge is not a remote input transport.
The phone sends standard gamepad snapshots; game action hints must come from
verified WoW mappings and must not pretend to know custom user bindings.

## Performance evidence

Keep baseline and candidate results, including failures. Compare identical
resolution, refresh rate, encoder, bitrate, network, viewer and sound settings.
Report input-to-scheduled-presentation median/p95, first-frame time, decoded
frame rate, stalls, dropped frames, packet loss, and audio continuity. Browser
scheduled presentation is not optical glass-to-glass latency. Native app results
must be labeled separately from browser results.

A short loopback or parser microbenchmark does not establish live game latency.
Historical measurements in `docs/performance/2026-09-16-cross-platform-hands`
are context and are not current baseline measurements for this refactor.

## Completion evidence

| Path | Required runtime evidence |
| --- | --- |
| Native mouse | Long independent and simultaneous holds, drag/camera motion, release, focus changes, fullscreen transitions |
| Keyboard | Modifier chords, key holds/repeats, raw gameplay keys, focus/reconnect release |
| Video | First frame, sustained matched quality, reconnect, encoder replacement, a second viewer without input stalls |
| Speakers | Known tone through the remote playback device, received/played at the viewer, mute and reconnect |
| Microphone | Explicit enable, known synthetic audio at the remote virtual source, mute/release/disconnect stop delivery |
| Gamepad | Complete snapshots, analog axes/triggers, watchdog, background/reconnect, correct WoW actions |
| Addon | Send in WoW, incremental reply, final reply, reload/reconnection and explicit opt-out |

Until those paths are tested on the exact installed versions, passing unit
suites is component evidence, not a claim that the whole system works.

## Implemented paths and remaining rollout

`crates/nanocodex-remote` owns the Rust publisher, peer preparation, media framing,
input lease and microphone lifecycle. The native CLI delegates to it. Linux
Wayland capture/input and the standalone `wayland-host`, `desktop-host`, and
`server-host` commands use that same runtime. Both managed and standalone server
image definitions now build the Rust publisher. The Go source is retained for
comparison/rollback and the separate paired-phone companion; it is no longer the
publisher selected by those image definitions. The Apple screen-publishing UI and
phone capture remain native Swift consumers of the existing wire protocol.

The Mac viewer exposes Take/Release Control, Lock Mouse, microphone/speaker
controls and native fullscreen. Control–Command–F toggles fullscreen;
Command–Shift–Escape releases input. Capture belongs to the focused window and
releases on focus loss. This follows the window-owned relative-input behavior
in [SDL](https://wiki.libsdl.org/SDL3/SDL_SetWindowRelativeMouseMode) and the
separate capture/fullscreen escape controls described by
[Moonlight](https://github.com/moonlight-stream/moonlight-docs/wiki/Setup-Guide).
No Moonlight code was copied.

Linux microphone input is a stable virtual Pulse source named from the machine
identity. Select **Nanocodex_Remote_Microphone** once in the remote app's voice
input settings. Mute/revoke stops the PCM writer while preserving that selection;
publisher shutdown removes only its owned devices. The publisher does not explicitly change global defaults. PulseAudio can
automatically select the new input on a headless host whose only previous source
was a playback monitor; hosts with an existing input retain that selection. This does not claim that a particular game has selected the source.
Mac and Windows host microphone sinks are not implemented; unsupported hosts do
not advertise the microphone capability. iPhone audio defaults to the speaker,
and interruptions/device removal stop microphone capture without automatic resume.

The phone controller displays physical WoW gamepad button identities and sends
standard snapshots. It does not assign invented spells or actions. Current game
bindings still require an in-game check. The addon is an independent Lua/Python
consumer under `examples/wow`; its carrier and application tests use mocked game
APIs and are not a live WoW reply demonstration.
