# Background native computer use

The intended behavior is simultaneous use of the same desktop: human input stays
with the user's foreground application while the agent captures and operates an
explicitly bound background application. A second workspace, restored cursor,
or focus-then-restore sequence does not meet this contract.

## Regressions driving this work

Verified historical reports from September 2026 (these are reported failures,
not fresh reproductions):

- Session `01a0b0bd-67d4-7df3-879f-246e83567ebd`, turn
  `01a0b142-72da-7390-b301-274c7084bcca`: concurrent focus changes caused browser
  input to land in Terminal. Actual event destination is the primary oracle.
- Session `01a0ae34-4b45-7afe-bf0e-632183de716e`, turn
  `01a0ae41-2ab4-7e81-b7bb-9de9a310eabd`: unreadable 639×359 and stale frames.
  Capture must prove current target contents after input, not only produce bytes.
- Same session, turn `01a0ae61-c03e-7c13-85fa-e0214c9c6411`: controller ownership
  prevented confirming input release. Cancellation must release agent-owned
  keys/buttons without releasing a human's physical holds.
- Same session, turn `01a0ae81-2805-77d2-b9ac-4e94beaf0956`: collector tests passed
  while the actual Wayland publisher was disconnected and the feed black.
  Installed public-tool testing is required in addition to helper tests.

## Relevant Sky evidence

The sibling Sky research tree's original symbol inventory contains
`SyntheticAppFocusEnforcer`, separate real/synthetic activation state,
`VirtualCursor`, PID event posting and window-local event coordinates. Its actual
`captureScreenshotWithSkyLight` decompilation uses selected window IDs and
`ignoreGlobalClipShape`. The old rebuild's mandatory foreground activation does
not reproduce that architecture. The exact private event-field recipe is from
current CUA source, not independently recovered Sky code.

References:
- `../sky-re-1000502/archive/1000502/symbols/service.txt`
- `../sky-re-1000502/snapshots/2026-09-06/native-app-state/pipeline/pass6/100e2caf0.c`
- [CUA macOS input](https://github.com/trycua/cua/tree/main/libs/cua-driver/rust/crates/platform-macos/src/input)
- [CUA action evidence](https://github.com/trycua/cua/blob/main/libs/cua-driver/docs/action-support.md)

## Hot-path changes

- Text-only `getApp` and `getAXState` do not capture or write hidden images.
- `getScreenshot` refreshes window identity and geometry through a dedicated
  backend hook, without requiring a full accessibility-tree/text traversal.
- Cached app handles use a backend identity-validation hook; native backends can
  avoid enumerating every running application on each action.
- Native drag accepts an explicit mouse button and event-local modifiers, allowing
  Blender-style middle-button orbit and Shift+middle-button pan on capable
  backends. Unsupported backends must refuse before input rather than discard
  the options.

The live-screen `computer` service retains exclusive human-priority control.
Background app actions belong to `cua.getApp`, with exact app/window binding.
Do not remove screen ownership checks to simulate concurrency.

## Acceptance

Use owned foreground and background applications. Verify target state changed
exactly once; foreground app, focus and physical cursor did not move; the target
screenshot is fresh and readable. Cover typing while the human types, modified
and middle-button drags, menus/popups, stale windows/PIDs, resizing, cancellation,
reset and disconnect. Record action and capture latency separately, including
cold and warm captures. API success alone is not delivery evidence.

For Omarchy, final acceptance specifically requires native Wayland Blender while
WoW remains the human's foreground XWayland app, on the installed compositor.
macOS success, X11 tests, another compositor or an isolated desktop does not
establish that result. Track supported routes and test results explicitly.

## Implemented and verified (2026-09-17)

The branch `feat/background-cua` implements background app control separately
from the human-priority live-screen route. The normal screen service and its
ownership checks are unchanged.

macOS uses PID/window-local input and synthetic app focus. Its owned live AppKit
test verified clicks, scrolling, Shift+middle drags and Unicode typing through
MacDesktop while the foreground application stayed unchanged. It does not yet
establish compatibility with Chromium/Electron, arbitrary menus, or every app.
The original Sky private-input recipe was not completely recovered; do not claim
binary-equivalent Sky behavior. Public CGEventPostToPid is still used.

AppKit-active state alone does not make a window native-key: its first click can
be consumed for focus instead of reaching the control. The Mac backend now sends
exact-window key notifications before input without changing the WindowServer
front process. A fresh-window regression verifies the first production click and
subsequent text entry. Explicit PID lookup bypasses the cached NSWorkspace app
list. All four owned Mac tests pass with foreground/cursor preservation checks.

Install the Mac companion into a fresh version directory and switch the current
version atomically, retaining the previous version. During live deployment,
replacing the executable in an existing directory produced ScreenCaptureKit
capture timeouts despite positive permission preflight; the identical binary in
a fresh directory captured successfully. Existing CUA processes retain their
loaded version until renewed. This is observed deployment behavior, not proof of
the specific macOS caching mechanism.
 Native plain-text paste uses background
typing without changing the shared clipboard; Markdown and HTML paste are
refused. Use `typeText` or `setValue` for native app text.

Linux has an opt-in Hyprland app backend, independently bound to a window's
compositor stable ID, address, PID, process start time and executable. Executable
policy approval is separate from the window/session identity, so two Blender
processes do not silently collapse into the same handle. It attests compositor
socket peers, captures the exact toplevel, normalizes HiDPI images to logical
window coordinates, requires a fresh screenshot for coordinate input, and uses
fresh per-operation grants. Unknown/partial transport outcomes are not retried.
Input socket closure releases the lane's keys/buttons. App-state formatting now
supports an app interface on Linux without falsely labeling the OS as macOS.

The actual public MCP stdio API verified, in an owned isolated Hyprland session:

- Factory-default Blender cube translation through `cua.getApp`.
- Middle-button orbit and Shift+middle-button pan, checked against Blender's
  view rotation/location state.
- Foreground GTK typing during both transform commands and held Shift gestures.
- Unchanged primary cursor and foreground PID; lowercase primary text remained
  lowercase while the agent held its own Shift modifier.
- Exact-window screenshots and disconnect midway through a modified drag, with
  zero held buttons/keys/drag afterward.

A quiet run measured about 146 ms for click plus five keys, 279–283 ms for a
250 ms drag, and 400–491 ms for screenshot delivery. During a simultaneous Hand
build, the final run measured 239 ms, 301–303 ms and 827–966 ms respectively.
These are end-to-end experimental debug-build measurements, not a latency SLA.

Validation: 416 parity tests passed; the Linux backend's two input validation
tests passed; the standalone C++ drag-options parser tests passed. JS tests pass
with the opt-in native/browser fixtures skipped; the separate owned Mac live
test supplies background-input evidence. Rust transport tests exercise actual
companion sessions, cancellation, isolation and reset. The Linux Hand binary and
companion compile. See `.build/background-cua` and Omarchy's
`/srv/nanocodex/workspace/background-cua` for retained detailed logs.

Current Linux limits: native Wayland toplevels only; canonical US keyboard layout;
ASCII text with unsupported characters refused before any delivery; visual app
state rather than an accessibility tree; no global clipboard paste, XWayland
background input, or arbitrary subsurface/popup support. WoW may remain the human
foreground app, but this specific combination has **not yet been verified on the
installed user compositor**.

## Omarchy installed deployment

The background companion and ABI-matched plugin are installed under
`/opt/nanocodex/background-cua`. The Hand runs as `nanocodex` (UID 960) and
launches only the fixed companion as desktop user `gakonst` (UID 1000), through
a narrowly scoped sudo rule. The Hand service was restarted; the compositor
and existing applications were not restarted.

Activation supports both legacy `hyprctl keyword` configuration and the live
Lua configuration (`hl.config`). The systemd override is named
`zz-background-cua.conf` so it sorts after the existing `omarchy-screen.conf`
which otherwise disables the companion. Activation attests the desktop user's
sole compositor, sanitizes its environment, and serializes plugin activation.
Unknown loaded plugins or a changed compositor version refuse activation;
loaded modules are never automatically replaced or unloaded.

The installed public app interface captured the existing Blender window and
refused an input attempt with `primary_target_busy` while it was foreground.
A temporary owned GTK window on the actual user compositor then received
`background-live-proof` through `cua.getApp(...).typeText(...)`. The foreground
terminal PID remained 463962 in the immediate post-input check; a subsequent
exact-window screenshot showed the entered text. The transparent fixture used
an exact-class no-focus rule and was removed afterward. By the later cleanup
check the foreground was a different Blender PID, so that delayed check is not
an assertion of uninterrupted foreground stability. The first capture showed
the previous buffer; the next capture showed the updated text. Input completion
does not guarantee that an application has painted a new frame.

This establishes installed background input and capture on the real desktop.
The specific combination of agent Blender operations while the human plays
WoW remains unverified. Full Blender transforms/orbit/pan and concurrent primary
typing were verified in the isolated fixture described above.

The reviewed installation bundle remains at
`/srv/nanocodex/workspace/background-cua/omarchy-install`. To activate after a
fresh desktop login, the fixed companion invokes the ABI-checked activator.

Rollback: remove `zz-background-cua.conf` from the Hand's systemd drop-in directory
and `/etc/sudoers.d/nanocodex-background-cua`, daemon-reload and restart the Hand.
As `gakonst`, disable `plugin:cua:enabled` and call `cua:refresh`; the retired seat
resources remain until the compositor naturally exits. Do not unload/replace a
live input plugin or reboot the desktop merely to complete rollback.

## Concurrent window bindings and agent cursors

On a provider that exposes native window discovery, initialize CUA, discover the
window IDs, then retain a separate handle for each target:

```javascript
const windows = await cua.listWindows("Example App");
const first = await cua.getApp("Example App", { windowId: windows[0].windowId });
const second = await cua.getApp("Example App", { windowId: windows[1].windowId });
```

Choose windows from their observed titles and IDs. Never infer an ID or reuse an
old binding after its window closes. Native executable authorization remains
separate from the exact-window session identity. AX handles, screenshot geometry
and observation history must remain valid independently for each bound window.

Mac applications share keyboard focus across their own windows. A cooperative
cross-process lock covers each synthetic-focus/input transaction for a PID;
other applications use different locks. This lock is local to the application process. Captures and operations in other
applications do not need it. Two input sequences aimed at windows in one process
still require coordination because the application owns a single keyboard focus.
The background route refuses keyboard/pointer input into the human's foreground
process. Separate calls are separate transactions; inspect the resulting state
before depending on a sequence another agent could also modify.

Independent operations should be submitted together when their results do not
depend on each other:

```javascript
const frames = await Promise.all([
  first.getScreenshot({ emit: false }),
  second.getScreenshot({ emit: false }),
]);
```

Each exact-window worker preserves its own request order and observation state.
The JavaScript bridge admits requests without waiting for earlier replies, and
correlates each completion with its original promise. Native workers own their
platform state on their own main threads. On macOS, workers delegate screenshots
to concurrent ScreenCaptureKit requests in the parent process: multi-process
capture requests hung during live testing on macOS 26. The parent starts each
request asynchronously, validates its approved PID and window, and returns the
image to that worker for final geometry checks. Input workers remain independent
of capture completion. A stalled capture must not hold up another window. Request queues and worker counts are
bounded; saturation returns an error. Cancellation invalidates pending work and
allows already-held synthetic input to unwind before the worker exits. An
uncertain delivery is never automatically replayed. Native operation time counts
toward the caller’s execution timeout; only trusted human approval waits receive
timeout credit. Already admitted native work can progress during another call’s
approval wait. JavaScript continuations and timers wait until the clock resumes;
approval credit must never become unlimited JavaScript execution time.

The agent cursor is an independent visual indicator. It must never move, hide or
replace the human's hardware pointer. Its panels ignore mouse events, cannot
become key/main, and are ordered relative to their target windows. Cursor state
is per target; movement interpolation, click feedback and fading do not delay
input delivery. Native run-loop servicing continues while the companion waits
for the next request. Foreign window ordering and Spaces remain OS constraints
that require live verification; there is no unconditional floating overlay over
the human's foreground app.

Sky evidence supports per-controller virtual cursors, window-relative ordering,
main-thread view ownership, spring/scoot animation states and separate input
readiness from visual completion. It does not establish exact animation timings,
colors or artwork. Nanocodex's visual design and timings are independent choices.
Sky also explicitly composes virtual cursor windows into target screenshots;
Nanocodex's Mac exact-window screenshots currently remain clean target captures.
The human-facing desktop view displays the separate agent overlay.

The Hyprland candidate provides eight independent native Wayland lanes, with a
window decoration for each agent cursor. Decorations are clipped to their
window and participate in its normal stacking/occlusion. Each lane has its own
color, click pulse and idle fade. The runtime retains a connection per target
and discovers the plugin's advertised capacity; legacy plugins without a
capacity field retain the two-lane behavior. Only an explicit `lane_busy`
receipt permits trying the next lane. Unknown transport outcomes are never
retried. Windows that share a Wayland client cannot occupy competing lanes.

An isolated Hyprland 0.56.2 test verified eight simultaneous 800 ms drags in eight
GTK clients while a ninth foreground client received primary keyboard events.
Target destruction and transport cleanup released the corresponding input
state; cursor render artifacts showed eight indicators and their idle fade.
This is not a live WoW acceptance test. A changed compositor plugin must wait
for a fresh desktop session: existing process-lifetime Wayland seat callbacks
make hot replacement/unloading unsafe. Keep the installed plugin active until
that transition; staging a candidate is not activation.


A single-companion Linux acceptance run drove four native Wayland windows with
`Promise.all` through the public JavaScript tool. All four drag states overlapped;
completion times were 328–348 ms, with exact input receipts, unchanged foreground
focus and primary pointer, and concurrent exact-window captures. Disconnect
mid-drag released every reservation and held button within the 300 ms check.
This run used an isolated compositor; the candidate remains staged for a fresh
user desktop session. The installed process-lifetime plugin is not hot replaced.

On macOS, the release four-window workload (two application processes, one
companion and one `Promise.all` eval) completed in 8.815 seconds versus 29.890
seconds for the serial baseline. All 16 screenshot dimensions and four isolated
Unicode text receipts matched; sampled foreground and pointer were unchanged.
These workload timings supplement causal scheduler tests and do not prove that
every native operation overlapped. A separate render/lifecycle test observed two
colored arrow panels, idle fading, and no panels after reset; warm two-click time
was 174 ms. Freshness checks decoded 12 alternating red/blue captures correctly
with a 176 ms warm median. The strict pointer assertion in that separate run
failed during physical pointer movement, so it is not a pointer-isolation proof.

Approval waits continue polling and renewing previously admitted lanes. Ordinary
capture failures return to the child as screenshot errors, preserving optional
AX-only observations and queued work. Invalid capture bindings still revoke the
lane. Both cases have causal scheduler regressions.
