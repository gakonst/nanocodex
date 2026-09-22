# Fullscreen and captured remote control

The web viewer provides a Fullscreen button. On a desktop with a fine pointer,
Take control requests pointer lock and fullscreen from the same click, hides
local cursor and controls, and focuses the remote keyboard. Escape releases
held input and the control lease; the remote screen remains open. Touch devices
retain gestures, and unsupported fullscreen uses an expanded viewport.

A negotiated `relativePointer` capability enables native relative movement on
Waymote and Windows. Relative deltas use the reliable ordered control channel;
coalescing them like absolute positions would lose movement. Clicks and wheel
events omit coordinates so they operate at the native cursor without warping.
Older publishers use a bounded virtual cursor and existing absolute movement.
Input remains lease/generation/sequence checked. OS pointer acceleration still
applies; this is not a claim of raw hardware mouse input.

## Published runtime verification

Account version `976d52cc-01c3-42bc-96b5-d4df43c4f68b`, asset
`/assets/index-C_Rj1kLP.js`, was loaded through an authenticated loopback proxy
into headed Chromium on a separate Xvfb display. No app logic or browser
fullscreen/pointer-lock APIs were mocked. The viewer used the real account
signaling and publisher media/input channels.

The Omarchy test verified fullscreen viewing independently, then Take control,
actual pointer lock, hidden local cursor, focused remote keyboard, native
relative messages without absolute coordinates, and Escape sending releaseAll
followed by lease release. The dialog remained open and returned to Watching.
The same session decoded 3840×2160 at 60 fps, with 813 decoded frames, zero
video drops/loss, and 692 audio packets with zero loss. Sanitized events and
counters are in [fullscreen-control-measurements.json](fullscreen-control-measurements.json).

The same published fullscreen, native relative movement, and Escape-release
journey also passed against the Windows guest. The initial attempt exposed a
missing host-local NAT route: LAN clients could reach the advertised publisher,
but a viewer on the Omarchy host could not. A narrowly scoped host-local UDP
rule for the Windows publisher port range fixed that route; the successful
repeat used the unmodified published viewer. Windows media counters are
included in the same measurements file.

Safari on the local Mac also entered fullscreen and captured the pointer.
Escape was not independently verified there because the UI automation bridge
could not reliably target Safari's separate fullscreen window. A minimal
headless Chromium page rejected pointer lock, so the full interaction test
used headed Chromium instead. Phone fullscreen/gesture behavior was not
measured in this run.

Validation includes all 168 account tests (47 remote-session tests), full web
build/typecheck/docs checks, Go race tests for relative protocol validation and
Waymote encoding, and native Windows relative input tests. Audio and encoder
latency measurements are documented separately in this directory.
