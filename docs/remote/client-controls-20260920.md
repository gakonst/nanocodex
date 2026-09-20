# Remote client controls, 2026-09-20

This follow-up closes concrete web/native input and audio gaps in the shared
publisher migration. The phone controller UI is deliberately paused; its draft
is not included in this change.

## Web viewer

The production `Screen` component tracks left, right and middle buttons
independently using the DOM `buttons` bitset. Chord transitions can arrive as
`pointermove`, without another `pointerdown`; see the
[Pointer Events specification](https://www.w3.org/TR/pointerevents/#chorded-button-interactions).
Pointer capture and a gesture-scoped document fallback preserve independent
releases outside the picture, including Chromium dropping capture after the
first release in a chord. The fallback ends on cancellation or focus/lease loss. Unsupported extra
buttons do not become left clicks. Touch gestures retain their separate state.

Mouse movement is batched for at most 4 ms. Absolute motion keeps the newest
position; relative batches preserve total displacement and stay within wire
bounds. Buttons, keys and wheel events flush preceding motion. Release,
revocation and teardown discard unsent motion. The existing reliable channel
carries relative movement and transitions; disposable absolute motion uses the
unordered channel. No video/jitter-buffer tuning is bundled into this change.

Take Control and Lock Mouse are separate actions. Fullscreen retains the toolbar
and a persistent capture-release hint. Escape releases pointer lock;
Command–Shift–Escape releases control; Control–Command–F toggles fullscreen.
Exiting fullscreen releases control. Late pointer-lock completions cannot
recapture an inactive session. Remote scrolling consumes wheel events only over
an actively controlled picture, without also scrolling the web app.

Microphone forwarding requires the host's microphone capability and a negotiated
return-audio transceiver. Enable requests carry the current lease generation and
a unique request ID. A matching affirmative host acknowledgement is required
before asking for browser capture. Permission, sender replacement and teardown
are fenced against stale asynchronous completion. Mute, release, revoke,
disconnect, background/suspend and device interruption stop local tracks.
Reconnect does not silently restart recording. Speaker mute remains independent.
Hosts without return-audio support and the JPEG fallback do not expose a mic.

## Shared/native path

See [native handoff and host release validation](../performance/2026-09-20-native-input-handoff.md).
Native WebRTC input enters one bounded mailbox before main-actor scheduling.
Adjacent absolute positions may replace older positions; reliable transitions and
relative displacement remain ordered. Overflow closes the peer rather than
silently losing input. The Rust runtime requires successful native release before
acknowledging cleanup or granting a subsequent owner.

## Verification scope

Commands for the web controls:

```sh
node --experimental-strip-types --test js/account/src/handRemote.test.ts js/account/src/handRemoteInput.test.ts
js/account/node_modules/.bin/tsc -p js/account/tsconfig.remote-controls.json
node js/account/scripts/remote-controls-smoke.mjs
```

The focused typecheck covers the production viewer and protocol tests. Full
account typechecking also passes. It initially reproduced four existing TS7006
errors in `HostedToolsDemo.tsx` and `monsterWorldAgent.worker.ts` on the detached
base, `74232979b`; explicit `unknown` input and `ToolContext` annotations fix
those two handler signatures without changing runtime behavior.

Browser protocol mocks and real loopback WebRTC have distinct scopes in the
smoke fixture. Neither is a live WoW, hardware-microphone or internet latency
benchmark. New code in this branch is not proof of installation on every host
or a production web deployment. macOS/Windows host return-microphone sinks
remain unavailable; Linux has the tested virtual input from the preceding merge.

Follow-up results: **111 web protocol/input tests passed**, and the focused
production-viewer typecheck passed. The Chromium fixture passed **13 interaction
and audio cases**, including both mouse chord orders outside the picture,
cancellation, unrelated pointer IDs, Shift+W with repeated keydown, local toolbar
keyboard, wheel consumption, pointer lock, fullscreen exit, and focus release.
The audio case negotiated real local WebRTC peers and received a generated
440 Hz microphone input with nonzero decoded audio energy (52 RTP packets,
4,192 bytes, energy 0.02358 in the recorded run). Mute and control release stopped
and detached the captured tracks. No physical microphone was opened. Browser
video/audio layout was inspected in windowed and fullscreen screenshots.
