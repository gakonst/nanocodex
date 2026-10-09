# Native Hand recording

The Hand owns workflow recording: native observation, private evidence storage,
recording lifecycle and access controls run locally. Computer Use providers and
model connections are not required. The brain can read the resulting evidence
to prepare skills; this API does not itself infer workflows or authorize replay.

Capture is off until an explicit `start`. Recordings continue when control
clients disconnect. Restarting a Hand retains committed evidence and marks an
unfinished session interrupted; it does not restart capture. Capture never
records keyboard values, clipboard contents or window titles.

The current local recording indicator is CLI `status` and fixed lifecycle/capture
notices in the Hand's stderr logs. There is no GUI, tray or screen-overlay
indicator. Notices do not include application metadata or evidence paths.

## Local controls

An installed native screen Hand hosts owner-local recording controls. The
recording root is its state directory's `recordings` directory. Standalone
`screen-host` stores recordings in the persistent Hand config directory, scoped
by endpoint and machine identity. A separate local
recorder can be run without account authentication:

```sh
nanocodex2 hand-recording --serve --state-dir /private/path/recordings
nanocodex2 hand-recording --state-dir /private/path/recordings '{"operation":"sources"}'
```

For the private X11 desktop, pass its existing private runtime:

```sh
nanocodex2 hand-recording --serve --state-dir /private/path/recordings --desktop-runtime /private/path/desktop
```

Use the returned foreground `app_id` or `window_id` to select the recording
scope. These are live native identities, not permanent application names. For
example, substitute the observed window ID below:

```json
{"operation":"start","scope":{"windows":["x11:12345"],"capture_frames":false}}
```

Every subsequent control uses the returned recording ID. Local IPC is a
private Unix socket with owner checks. It is independent of account authorization and does not expose a network listener.
Windows recording is unavailable until owner-private storage ACLs can be verified;
recording requests fail before creating files. Native screen access remains available.

## Remote controls

Native screen discovery advertises recording only when its recorder is
available. Use the existing workdir-scoped CUA tool with the native screen
contract:

```json
{"action":"recording","operation":"status"}
```

These calls use the same authenticated screen transport and surface generation
as existing screen actions. Recording controls do not acquire a keyboard/mouse
input lease. An interrupted response may have committed a mutation: query status
or list before retrying, particularly after `start`.

| Operation | Fields | Result |
| --- | --- | --- |
| `sources` | none | Current native foreground IDs and capture capabilities |
| `start` | `scope`, optional `limits` | New recording ID and state |
| `pause`, `resume`, `stop` | `id` | Persisted lifecycle state |
| `status` | optional `id` | Current or selected state and capture diagnostics |
| `list` | optional `cursor`, `limit` ≤ 50 | Recording metadata and `next_cursor` |
| `read`, `export` | `id`, optional `cursor`, `limit` ≤ 200 | Ordered evidence and `next_cursor` |
| `frame` | `id`, `sha256`, optional byte `offset`, `length` ≤ 375000 | Bounded base64 content chunk |
| `delete` | `id` | Removal of a stopped/interrupted recording and its frames |

`scope` has `apps`, `windows`, `exclude_apps` (at most 32 native IDs each) and
`capture_frames` (default false). At least one app or window must be allowlisted;
app exclusions take precedence. IDs are validated, never interpreted as paths.
These scopes identify current native processes/windows, not persistent app names;
PID or window-ID reuse is not guarded by a process-generation token.

Default limits are one hour including pauses, 10,000 events and 64 MiB per
recording. Custom `limits` may only lower these bounds. The store additionally
caps recordings at 100 and total bytes at 512 MiB. Stopped and interrupted
recordings expire seven days after their final lifecycle transition; pruning runs
while the service is active and when storage reopens. Hitting a limit stops
recording with an explicit reason. Export is a paginated evidence manifest; retrieve its referenced frames
separately. It is not a rendered video.

## Native evidence and privacy

Evidence includes ordered focus changes, pointer locations associated with
meaningful events, mouse-button transitions, scrolling where available, optional
frames and suppression reasons. Native input may originate from a person or an
injected action; the recorder does not label that distinction as verified human
provenance. Recorded content is untrusted evidence, never executable instructions.

Capture checks the scope before persisting evidence. Frames require a verified
foreground, native sensitive-input checks and window-specific capture. Unknown
sensitivity, secure input, a changed foreground or unavailable native context
suppresses frames. A non-password control does not establish that other visible
content is free of secrets; only enable retained frames for approved work.

Linux X11 uses its own bounded connection, native XI2 mouse events and XRes
process attribution. It selects the Hand's private display explicitly when a
runtime is supplied. Pixel capture verifies the selected window is on-screen
and unobscured. Its optional AT-SPI helper is read-only and does not collect text
values. Wayland global observation is unavailable; an Xwayland connection is not
accepted as proof of the compositor's foreground.

The macOS and Windows observer adapters use native foreground and sensitive-input APIs.
The Windows adapter is not exposed through recording while private storage is
unavailable. Mouse polling on these platforms can miss transitions between samples; unsupported scroll and
frame capabilities are reported explicitly. Runtime permissions are preserved,
not bypassed. Check the returned capability and capture diagnostics on the actual
Hand before relying on a workflow trace.

Local lifecycle messages identify recording state without echoing captured
content. This release provides CLI controls and status diagnostics; it does not
add a menu-bar indicator or a recording review UI. Local pause/stop remain available if the network is disconnected.
Recordings reside in private Hand storage with ordered timestamps and
content-addressed frame files. JSON state updates are durable atomic replacements.
This favors bounded workflow evidence over high-rate telemetry: repeated
full-manifest writes and quota scans can become expensive near the event ceiling.

## Validation

The real CLI/native desktop journey uses synthetic data and preserves its trace
in ignored `output/hand-recording-e2e`:

```sh
cargo build -p nanocodex-bin --bin nanocodex
python3 scripts/tests/hand-recording-e2e.py /absolute/path/to/nanocodex2
```

It requires Xvfb, openbox, xterm and xdotool and creates an isolated desktop. It
exercises controls, actual native input, scope exclusions, pause fencing,
export bounds, rapid pause/resume input exclusion, crash recovery and deletion.
Platform-specific build checks do not establish native permission behavior or live capture on another OS.

A second synthetic journey checks real GTK accessibility roles, window-sized
JPEG capture, password-field suppression and recovery:

```sh
python3 scripts/tests/hand-recording-gtk-e2e.py /absolute/path/to/nanocodex2
```

It also requires GTK3 introspection, python3-dbus, dbus-daemon and AT-SPI2. It
starts its own session and accessibility buses and never uses the ambient desktop.
