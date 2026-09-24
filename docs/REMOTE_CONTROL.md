# Interactive Hands

Interactive Hands connect macOS, paired iPhone, factory-spawned Linux VMs,
Cloudflare sandbox desktops, and existing Linux servers to the managed account.
Viewers are the native Apple clients and the account browser. A connected shell
Hand and a published screen are separate capabilities; verify both before
claiming that a machine supports files, processes, and desktop control.

## Connections

The product diagram uses illustrative mount names. `environment` returns the
actual mounts available to the current agent; `workdir` selects the execution
Hand. Code Mode runs in the managed service, while native commands run on the
selected Hand. Screen tools select their exact machine and publication instead
of inferring a target from `workdir`.

Desktop, mobile, and browser clients read the same account-owned durable agents
and resume their event streams. Signing in selects the account; a connected
device publishes its available capabilities through an outbound authenticated
connection. Normal Hand operation requires no inbound listener, port forwarding,
or VPN. A device may share its host workspace, offer an isolated VM factory, or
provide both. VM allocations keep their own workspaces and identities.

| Connection | Current implementation | Required live check |
| --- | --- | --- |
| Mac host | Automatic account-wide shell Hand and screen sharing while Nanocodex runs | File/process roundtrip, video/input, relaunch, window close |
| Mac-hosted VM | Factory-managed Linux VM with its own workspace and desktop | Retained files, video/input, generation change and viewer recovery after restart |
| Cloudflare Hand | Retained sandbox workspace and `frames-v1` desktop | Workspace roundtrip, rendered frames/input, sleep/resume |
| Native Linux Hand | `hand --workspace PATH` publishes an explicit workspace and native tools over an outbound account connection | File/process roundtrip, socket reconnect, process restart with retained identity |
| SSH Linux server | Vault-bound SSH for native commands; `server_hand` installs a dedicated desktop container | Reachable SSH target, Docker access, enrollment, video/input, reconnect |
| Browser viewer | Screens in Connect and agent terminals | Discovery, video/input, tab background/resume, host restart |
| iPhone viewer/host | Shared native viewer; hosting uses a paired Mac bridge | Physical-device viewer and paired-host journey; simulator builds alone do not establish this |
| Windows host | Native Rust Hand; GDI/FFmpeg H.264 WebRTC, native JPEG fallback when encoder startup fails | Interactive-session capture/input, reconnect, bundled encoder and fallback on a physical Windows host |
| Service connections | Account credential broker with per-agent and per-connection grants | Connected inventory and a read-only request to each granted service |

Mac-hosted factory VMs in this implementation are Linux guests. Linux server
desktops use a dedicated retained workspace; they do not expose every server
project automatically. SSH filesystem mounts and desktop workspaces are distinct.
SSH is an optional installation path for a machine that is not running a Hand.
That bootstrap requires the matching vault identity, pinned server fingerprint,
reachable SSH host/port, and Docker access for managed desktop setup. The current
cloud SSH broker needs a public SSH target; that limitation does not apply to a
Hand started directly on the device, whose normal connection is outbound.

## Transport and ownership

WebRTC video and data channels are the default. Cloudflare sandboxes explicitly
advertise `transport: "frames-v1"` and use authenticated WebSocket JPEG frames
and input. VNC is not required. Wayland remains the Linux compositor/input backend.

- `apple/NanocodexRemote` owns native WebRTC, ScreenCaptureKit capture, Quartz
  input, paired-phone capture/input, and the sharing UI. Apple apps consume this
  package without importing one another's source. One capture source serves all
  viewers of a surface; each peer owns its own track and encoder.
- `crates/nanocodex-remote` owns the shared Rust publisher used by native CLI,
  VM, Docker, and current Linux Hand images. Platform adapters provide encoded
  H.264 packets or external byte streams, plus optional speaker PCM. WebRTC
  forwards those encoded packets without decoding and encoding them again.
- `hands/remote` remains the Go companion for deployed Wayland/Pion hosts and
  the Mac app's paired-device tunnel and signed Xcode runner. Current Linux
  images install the Rust publisher under the same `nanocodex-remote` executable
  name; the filename alone does not establish which implementation is running.
- The managed Worker and existing account Durable Object own account
  authorization, discovery, signaling, and short-lived Cloudflare TURN
  credentials. Video/input use the direct peer connection, with TURN as the
  optional relay. There is no SFU. Live video stays off the agent transcript;
  an agent's requested screenshots return through the normal tool-result path.
- Discrete input uses an ordered reliable channel. Absolute motion uses an
  unordered channel without retransmits. Clicks carry their own coordinates and
  fence older motion. Control is exclusive, expires after ten seconds without
  renewal, and releases keys/buttons on disconnect, focus loss, or revocation.
- Signaling is fenced to the account, host connection generation, and surface.
  A live socket cannot renew authorization itself: the client makes a freshly
  authenticated HTTP request. Existing Connect grants do not grant screen access.

## Agent control and human takeover

Agents use a Hand's attached `cua_repl` provider through Code Mode. Call
`tools.mcp__cua_repl__js({workdir:"/laptop"})` to discover its contract, then add
the provider arguments alongside `workdir` on each invocation. Nanocodex strips
only `workdir` and forwards the remaining arguments unchanged. There is no
`select_computer` or global desktop selection. Calls to different Hands can run
concurrently with `Promise.all`; JS and reset calls to the same Hand are ordered.
A cell pins each captured Hand connection, so reconnecting does not retarget an
admitted call. A new cell discovers replacement connections.

A signed upstream CUA provider is preferred when one is attached. A Hand with
only a controllable screen is exposed through the same workdir-routed CUA entry
point using its native `observe`, `click`, `type`, `key`, `scroll`, and `drag`
action schema. This gives Linux VMs and Cloudflare desktop sandboxes real CUA
without pretending they implement OpenAI's JavaScript provider contract; the
initial workdir-only call returns the exact contract selected for that Hand.

Each surface also advertises its account-owned `screen_*` tool through
`tool_search`, including individual windows when no unique desktop exists. Code Mode callers
use `image(result)` to display returned screenshots. Coordinates are normalized
across the whole image; keyboard actions use USB HID usages and optional
modifiers. An observation is bounded to 1280 pixels on its longest edge.

Agent and human input share the same host control lease and input backend.
Taking control in a viewer cancels pending agent input and releases held keys
and buttons. Agents receive `busy` while a human controls the screen; they may
still observe it. Releasing control allows agent input again. Already submitted
XCTest phone gestures must finish, but queued gestures are cancelled.

Agent calls use the existing authenticated signaling socket. They have a short
deadline, are bound to the exact host connection and publication generation,
and are never automatically replayed after a lost acknowledgement. A failed or
interrupted call requires another observation before deciding whether to send
more input. Hosts without the agent capability flag remain viewable and do not
advertise an unsupported tool. Connect-scoped agents receive no screen tools.

Linux uses `grim` to observe the existing compositor without opening a second
Waymote input session. Some distribution builds advertise JPEG in their help
while disabling it at compile time; the companion captures PNG and converts
only agent observations to bounded JPEGs. Native Mac and paired-phone hosts
encode the latest captured frame on demand.

The Linux companion reads Waymote's native Annex-B H.264 pipe, bounds each
access unit, and packetizes it into WebRTC RTP using the encoder's 60 Hz clock.
There is no second decode/encode pass. This also works with libkrun TSI, whose
[documented networking limitations](https://github.com/libkrun/libkrun#known-limitations)
exclude listening on guest UDP sockets. The original loopback RTP hop produced
a connected control channel but no video in a real VM; the pipe fixes that. The current desktop profile is 1600×900,
60 fps, 6 Mbps. Those are configuration targets, not measured latency guarantees.

The Mac viewer uses Control–Command–F for fullscreen and Command–Shift–Escape
to release input. Linux microphone return audio exposes
**Nanocodex_Remote_Microphone**; select it in the remote application's voice-input
settings. Muting preserves that selection. Unsupported hosts do not advertise
microphone input.

## Apple setup

Build the Apple projects normally. The Mac app's build phase builds and signs
`nanocodex-remote` into `Contents/Helpers`; this requires Go 1.26 in addition to
Xcode and the existing managed runtime/Node build prerequisites. Both clients
expose Screens; the account browser exposes Screens from Connect and the agent
terminal.

For Mac hosting, screen sharing starts automatically after sign-in once macOS
Screen Recording permission is available. Accessibility/input permission is
for control. Allow Local Network access when connecting to a Hand on the same
network. Sharing remains visible in the main Mac toolbar after the picker
closes. Stop sharing and account changes revoke viewers and release input.
The selected display and Mac machine identity persist across relaunches. Closing
the window keeps the shell Hand and screen available. Quitting stops sharing;
reopening restores it. **Stop sharing** persists an opt-out; re-enable **Share
this Mac's screen automatically** in Settings. The automatic supervisor also
restores capture after system interruptions and display changes.
The signed app installed in `/Applications` defaults to opening at login through
macOS Login Items. Settings shows the actual OS registration state and preserves
later opt-outs. Development, isolated test, and ad-hoc builds do not register.
While sharing is requested, a temporary signaling outage retains the display
capture and retries with capped backoff. Recovery drops old viewers and releases
input; viewers must acquire control again. Stop sharing, account changes, and
permission or display failures end that publication. Automatic sharing waits for
permission and an available display before starting a fresh publication.

For iPhone hosting:

1. Pair and trust the iPhone with the Mac, enable Developer Mode, and configure
   Apple development signing in Xcode.
2. Build the `WebDriverAgentRunner` scheme from Appium WebDriverAgent using
   `build-for-testing` for that device. The output includes a `.xctestrun` file
   beside the signed runner application. Keep both together. The bridge currently
   accepts the standalone WebDriverAgentRunner format, not a combined test plan.
3. In the Mac app's Screens panel, find the paired iPhone, choose that trusted
   `.xctestrun`, and click Share iPhone. The app starts the companion and runner;
   separate terminal processes are unnecessary.
4. View/control the shared iPhone from another authenticated client. Stop sharing
   closes the tunnels and requests WDA shutdown. The companion also stops when
   its owning app's stdin closes, including after an app crash.

The bridge binds only `127.0.0.1:18100` and `127.0.0.1:19100`, on both the Mac and
runner configuration. It refuses occupied ports. Account/provider credentials
are excluded from the runner's environment. A manual developer bridge is also
available as `nanocodex-remote phone-tunnel --udid DEVICE`.

This is a paired developer-device workflow, not system-wide touch injection from
an ordinary App Store application. It does not require iPhone Mirroring. XCTest
submits complete drag gestures on release, so iPhone dragging does not yet have
the Mac backend's continuous feedback. Screen rotation ends sharing and requires
sharing the new geometry. The current phone stream is MJPEG from the runner,
converted to a WebRTC video track on the Mac.

## Cloudflare relay

The managed Worker accepts `NANOCODEX_TURN_KEY_ID` and
`NANOCODEX_TURN_API_TOKEN`. It generates one-hour credentials through Cloudflare
Realtime TURN through its `generate-ice-servers` endpoint and caches them briefly
per account. The API token stays on the Worker. Each new viewer fetches current
credentials; a long-running host does not retain its startup credentials for
later viewers. Linux fetches them asynchronously so existing input is not
blocked by a new viewer joining. Without both settings, the endpoint returns
Cloudflare STUN only.

Hosts refresh credentials and restart ICE every twenty minutes; viewers fetch
current credentials before answering each offer. This stays within the one-hour
credential lifetime even when the server returns a ten-minute-old cached value.
The existing video tracks and control channels survive renewal. Hosts send new
ICE candidates after their corresponding offers, and unanswered host offers
expire after twenty-five seconds.

## VM setup and lifecycle

`hands/remote/image/Dockerfile` builds a pinned labwc/Waymote desktop and the Rust
publisher. Build it from the repository root with:

```sh
docker build -t nanocodex-remote-desktop:development -f hands/remote/image/Dockerfile .
```

For a factory, materialize the image as a raw ext4 root using the existing
[`VmImageBuilder`](VM.md#preparing-immutable-images), then pass that immutable
root to the normal factory command:

```sh
nanocodex2 host --factory-name desktop-hands \
  --vm-template /path/to/desktop.ext4 \
  --vm-guest-runtime /path/to/nanocodex-vm-guest \
  --state-dir /path/to/private-factory-state --vm-workspace /workspace
```

Use a guest ELF built for the image architecture. On Apple Silicon, sign the
host executable with `nanocodex-vm.entitlements` after every Rust rebuild, and
place libkrunfw in a `firmware/` directory beside the guest runtime, or provide
`--vm-firmware` for another location outside the system loader path. The
factory automatically clones a private writable root for each allocation.
Updating a template affects future allocations; retained VM roots are preserved.

For local Portless testing only, the guest needs the public Portless CA and an
`/etc/hosts` entry for the canonical development hostname; `.localhost` wildcard
resolution on macOS is not inherited by Linux. Under TSI that entry points to
`127.0.0.1`. These development settings do not belong in production images.

## Linux servers and vault SSH setup

For native access without inbound SSH or a VM, configure the CLI with the same
account credential, then run on the machine itself:

```sh
nanocodex2 hand --workspace /path/to/workspace
```

`hand --workspace PATH` uses the CLI's account authentication and publishes the selected
workspace and its native execution tools over the
account's outbound connection. It persists the machine identity, reconnects
after socket loss, and handles Ctrl-C/SIGTERM. A single-instance state lock
prevents two processes from publishing the same identity. A different workspace
requires its own `--state-dir`. Credentials are excluded from native command
environments. This command provides native files/processes; desktop capture and
VM factories remain separate capabilities.

`nanocodex-remote server-host` starts a headless labwc desktop directly on Linux,
without a nested VM. It accepts `--url`, `--credential-file`, `--machine-id`,
`--name`, `--workspace`, and optional `--desktop-config`. Credential rotation and
signaling reconnect preserve the compositor. Clearing the credential stops its
owned desktop. The existing desktop image contains this mode and runs as an
unprivileged user by default.

The managed `server_hand` tool supports `list`, `connect`, and `disconnect` for
full account authority. Connect uses an exact vault SSH reference, its configured
host/user/port, and pinned host fingerprint. It checks Linux/Docker access,
enrolls a machine-scoped publisher, transfers that credential over SSH stdin,
and starts a dedicated container with no exposed ports, dropped capabilities,
and a retained workspace. It serializes setup per server and revokes a failed
installation. Disconnect revokes access before stopping the owned container.
The SSH private key stays in the encrypted broker vault.

In the account Vault, choose **Create in vault**, enter the target and its trusted
host fingerprint, and copy the resulting public key into that server's
`authorized_keys`. Existing PEM keys can also be uploaded. Generated private keys
never enter the browser response; repeating creation under an existing reference
returns a conflict instead of silently rotating it. Then ask the managed agent
to connect that SSH identity as a Hand. Current broker networking requires a
publicly reachable target; private/VPN-only SSH destinations are not supported.

The broker supports RSA/ECDSA host keys and prefers RSA when the server offers
both. Use the server's trusted RSA fingerprint in that case (for example, read
`ssh-keygen -lf /etc/ssh/ssh_host_rsa_key.pub` through an already trusted session).
Entering an ECDSA or Ed25519 fingerprint does not change the negotiated algorithm.
An ECDSA-only host works; an Ed25519-only host is currently unsupported. This
restriction does not apply to native Hands connecting outbound without SSH.

The operator must publish `hands/remote/image/Dockerfile` for the server's
architecture and configure `NANOCODEX_HAND_IMAGE` with its immutable
`registry/path@sha256:...` reference. No image is pulled from an agent-supplied
URL. The server user needs Docker access. Its desktop workspace is retained
under the SSH user's state directory; it is separate from other server projects.
`published` means discovery succeeded, so a viewer or screen-tool check is still
required before claiming rendered video and working input.

Machine enrollment uses `/v1/account/hand-hosts/:id` (PUT/DELETE) and scoped
`/v1/hand-hosts/:owner/:id/hands/{host,ice,renew}` endpoints. Publisher tokens cannot
view other Hands or read account data, and are rechecked during HTTP renewal.
Account, publication generation, control-lease, and input-sequence fencing apply
to both Linux and VM desktops.

## Viewer recovery

The shared viewer now retains selection during iOS backgrounding, releases
control and discards unsent text, and refreshes the publication generation when
resuming. Recovery uses a 90-second window with backoff capped at eight seconds;
a missing VM publication no longer exhausts all retries in seven seconds.
Stalled reconnect attempts time out after ten seconds within that window.
Disconnected canvases hide the old decoded frame. Remote Screens is also
available inside iPhone conversations.

Native viewer startup opens authenticated signaling and fetches ICE credentials
concurrently. The initial offer reuses that credential request; later offers
still refresh credentials. Cancellation and publication generation checks fence
queued signals. Native and browser discovery show a loading state while the
initial catalog request is pending. Actual decoded-frame benchmarks retain phase
timestamps separately from app build and unit-test timing.

When a factory's cloud connection is replaced, it delivers the new scoped
desktop credential before waiting for the tools socket to reconnect. This lets
the retained desktop republish during that independent handshake. Cancellation,
allocation identity, and server-side lease checks remain in force. Transport
diagnostics report reset categories without peer text or credentials.

## Cloudflare sandbox desktops

The managed sandbox image and lifecycle now include the Linux desktop publisher.
`NANOCODEX_SANDBOX_DESKTOPS=true` enables it after the retained workspace and peer
mounts are prepared. The mount receives a separate revocable publisher identity,
reuses its credential after sleep, and restarts the process on the next trusted
workspace preparation. Removing the sandbox revokes its publication before
stopping the container. Idle sleep remains governed by the Sandbox SDK.

The SDK image currently uses Ubuntu 22.04, which has no labwc package. Its custom
image bundles the Debian desktop executables and their own loader/libraries,
leaving the SDK's system libraries intact. Waymote is cross-compiled natively so
Apple Silicon builds do not depend on Rosetta compiling Zig-generated helpers.

Cloudflare's [outbound policy](https://developers.cloudflare.com/sandbox/guides/outbound-traffic/)
blocks raw WebRTC networking with `enableInternet=false`. Sandboxes therefore
publish explicit `frames-v1` surfaces through an intercepted HTTPS destination.
The trusted mount determines the exact account/publisher route, and the broker
still verifies its scoped bearer credential. The network policy stays enabled.

Frames are requested on demand, at most once per 100 ms, with one request in
flight per viewer. JPEG payloads are bounded to 700,000 base64 characters and
1280 pixels per dimension. Web/native clients validate dimensions before
allocation and clear stale images and control on disconnect. Input uses the same
exclusive host lease, generation and sequence checks as WebRTC. These are paced
screen updates, not the VM's 60 fps video transport.

Device inventory is available at `GET /v1/account/hands`; screen viewers use
`GET /v1/account/hands/screens`. These remain separate inventories.

References: [Cloudflare Realtime](https://developers.cloudflare.com/realtime/),
[Waymote](https://github.com/rockorager/waymote),
[Appium WebDriverAgent](https://github.com/appium/WebDriverAgent).
