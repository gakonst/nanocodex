# Interactive Hands

Interactive Hands connect macOS, paired iPhone, factory-spawned Linux VMs,
Cloudflare sandbox desktops, and existing Linux servers to the managed account.
Viewers are the native Apple clients and the account browser. A connected shell
Hand and a published screen are separate capabilities; verify both before
claiming that a machine supports files, processes, and desktop control.

## Architecture checklist

The product diagram uses illustrative mount names. `accountInfo` returns the
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
| Native Linux Hand | `native-hand` publishes an explicit workspace and native tools over an outbound account connection | File/process roundtrip, socket reconnect, process restart with retained identity |
| SSH Linux server | Vault-bound SSH for native commands; `server_hand` installs a dedicated desktop container | Reachable SSH target, Docker access, enrollment, video/input, reconnect |
| Browser viewer | Screens in Connect and agent terminals | Discovery, video/input, tab background/resume, host restart |
| iPhone viewer/host | Shared native viewer; hosting uses a paired Mac bridge | Physical-device journey; currently paused |
| Windows host | Planned | No native Windows host is claimed |
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
- `hands/remote` is the Go companion. On Linux it bridges Waymote capture/input to
  Pion WebRTC. On macOS it owns a paired-device tunnel and signed Xcode runner.
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

Each published surface advertises an account-owned `screen_*` tool through the
existing Hand registry. An agent discovers it with `tool_search`, observes the
screen, and sends click, text, key, scroll, or drag actions. Code Mode callers
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

Cloudflare Realtime was activated with explicit approval. The initial relay
evidence below used an isolated managed Worker. It does not establish a
production-duration relay soak or connectivity from a second physical network.

The real authenticated development Worker now issues Cloudflare credentials.
Live testing caught a Workers compatibility issue: its fetch implementation
requires `redirect: "manual"`; redirects and other non-success responses are
rejected without forwarding the provider credential. Provider errors return 503.

A native WebRTC test forced Cloudflare relay candidates, decoded video, exchanged
reliable input and disposable motion, and switched to newly minted credentials
and a new TURN allocation. Video and input continued after ICE restart. The test
passed in 1.696 seconds (`/tmp/nanocodex-cloudflare-relay-test.log`); this is test
runtime, not an end-to-end latency measurement. The earlier local Coturn renewal
test also passed.

## VM setup and lifecycle

`hands/remote/image/Dockerfile` builds a pinned labwc/Waymote desktop and the Go
companion. Build it from `hands/remote` with:

```sh
docker build -t nanocodex-remote-desktop:development -f image/Dockerfile .
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
provide `--vm-firmware` if libkrunfw is outside the system loader path. The
factory automatically clones a private writable root for each allocation.
Updating a template affects future allocations; retained VM roots are preserved.

For local Portless testing only, the guest needs the public Portless CA and an
`/etc/hosts` entry for the canonical development hostname; `.localhost` wildcard
resolution on macOS is not inherited by Linux. Under TSI that entry points to
`127.0.0.1`. These development settings do not belong in production images.

The image and authenticated desktop backend work in a real Wayland container.
Factory-spawned `nanocodex-vm` Hands now launch the companion when it is present
in their image; shell-only and explicitly offline images keep their existing behavior.
The real factory mounted the desktop in 1.345 seconds on the test Mac,
published it with its allocation credential, and retained its workspace across
agent turns and a host restart. This is one local startup sample.

The integration lives in the existing VM launch/lifetime owners, including
`vm_hand.rs` and `vm_host.rs`. It preserves each VM's private
root, workspace mounts, two-turn retention, and shutdown contract. The guest must
receive an allocation-scoped host credential, never a full account/provider key.
The current standalone `--credential-file` path was tested using an isolated
local development account; it must not be copied into factory guests as-is.

The user explicitly approved the scoped Rust launch/shutdown changes. Factory
startup now detects the desktop companion in the image and publishes with the
allocation credential. The guest retains labwc across signaling reconnects,
rotates its credential when the host lease changes, and exits before VM shutdown.
A real factory VM has now streamed decoded 1600×900 video to both the browser
and native Mac viewer. Browser text and raw key input created/read workspace
files, and pointer dragging moved its terminal window. Host shutdown disconnected
the old viewer; restarting retained the private root and its files. Network
publication retries independently of compositor readiness, so a signaling outage
does not block the shell attachment.

## Evidence and outstanding work (2026-09-08)

Fresh wrap-up evidence is retained in `/tmp/nanocodex-remote-wrapup-20260908/`.
The cloud agent executed on the actual Mac Hand, verified Darwin and its native
workspace, and wrote/read/removed an isolated marker. The native app's real
Hand journey passed across runtime restart, including a completed turn while
its window was closed. The native VM viewer decoded video, exercised input and
control reacquisition, and recovered after a 12-second factory outage with the
same machine identity and new publication generation. The desktop runtime's 33
tests also passed.

The updated identity-signed `/Applications/Nanocodex.app` registered successfully
with macOS Login Items. Its login, laptop Hand, automatic screen-sharing, and
keep-awake settings are enabled. Closing the window left both the native Hand
and controllable screen published; the browser decoded the live Mac afterward.
This verifies registration and background availability, not a computer reboot.
After the final app update while the Mac was locked, unlocking restored its
screen publication automatically without restarting the app. The production
browser reached Watching and rendered the live 1920×1080 display.

The deployed browser discovered Mac, VM, and Cloudflare screens. It rendered a
fresh Cloudflare desktop, created a marker through text/key input, recovered its
selected viewer after publisher restart, and created a second marker. Both
markers were independently read back; the disposable cloud agent and screen
were removed. `/tmp/nanocodex-web-wrapup-20260908.json` records the checks and
automation limits.

Read-only broker requests returned HTTP 200 for GitHub, Gmail, Drive, and X.
Calendar, Tasks, Contacts, Docs, Sheets, and Slides have retained connections but
their Google APIs returned `SERVICE_DISABLED`; the Google Cloud project needs
those APIs enabled. Slack is not connected. Provider inventory alone is not
proof that every provider API is usable. Sanitized receipts are under
`/tmp/nanocodex-live-connectors-20260908/`.

### Linux servers and vault SSH setup

For native access without inbound SSH or a VM, configure the CLI with the same
account credential, then run on the machine itself:

```sh
nanocodex2 native-hand --workspace /path/to/workspace
```

`native-hand` uses the CLI's account authentication and publishes the selected
workspace and its native execution tools over the
account's outbound connection. It persists the machine identity, reconnects
after socket loss, and handles Ctrl-C/SIGTERM. A single-instance state lock
prevents two processes from publishing the same identity. A different workspace
requires its own `--state-dir`. Credentials are excluded from native command
environments. This command provides native files/processes; desktop capture and
VM factories remain separate capabilities.

Real CLI checks passed on macOS and Linux with an isolated local account
service: file/process requests, forced socket reconnect, process restart with
the same catalog/UUID, credential filtering, and graceful shutdown. A separate
disposable Linux container also joined the production account: a real cloud
agent executed in its explicit native workdir, verified Linux, and wrote/read
a marker. Restarting the process retained its identity and workspace; a fresh
turn read the same marker. The agent, containers, volume, and publication were
removed afterward. The receipt is under
`/tmp/nanocodex-production-native-hand/f585d4b5-bff7-4210-8c00-dbf45fef1ff6/`.
This does not establish connectivity to an external SSH server.

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

Local evidence: both server and frame transports passed three consecutive real
Linux desktop lifecycle runs, including decoded JPEGs, terminal file creation,
credential rotation, signaling reconnect with the same compositor, subsequent
input, and revocation cleanup. Vault key generation/encrypted storage, actual SSH
stdin transport, setup failure cleanup, and account browser onboarding also pass
their focused tests. The release image is now published for Linux arm64 and
amd64, and production reports installation available. The vault currently has
no SSH targets, so external server installation remains unverified.

Both local desktop images were rebuilt with the updated Go daemon:
`nanocodex-server-hand:evidence` for Linux arm64 and
`nanocodex-cloudflare-hand:evidence` for Linux amd64. The managed preparation
script's generated Hand sources matched the daemon sources, and the daemon's
`server-host --help` startup check passed inside each rebuilt image. Build logs
are `/tmp/nanocodex-server-hand-image-build.log` and
`/tmp/nanocodex-cloudflare-hand-image-build.log`.

### Viewer recovery

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

### Cloudflare sandbox desktops

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

The real local Worker tests cover sandbox enrollment, intercepted publication,
transport isolation, bounded frames/input, and revocation. The production
Cloudflare journey passed on September 7: a fresh sandbox mounted its workspace,
rendered native frames, accepted input that created a file, resumed after viewer
suspension with control released, and returned an agent screenshot. Deleting the
test agent removed its screen. The receipt is retained under
`/tmp/nanocodex-production-cloudflare-evidence/c32e6837-542d-4a2a-8301-0313f422e455/`.

### Additional viewer evidence

The account browser now retains selection, clears stale video/input, and retries
within a 90-second recovery window. Chrome decoded and controlled a real VM,
then recovered about 18 seconds after its host was stopped and restarted. Separate
Chrome fixtures validated `frames-v1` JPEG rendering/input, tab hide/show, and a
12-second outage without creating ICE or WebRTC peers. The shared native package
also passes frame bounds/decoding and viewer recovery tests on macOS.

A fresh Mac-hosted factory VM also passed the native Swift viewer journey against
the live account. The test decoded video, changed the focused terminal through
remote input, stopped its factory for 12.8 seconds, and automatically recovered
in 17.6 seconds with the same selected screen and a new publication generation.
It verified that disconnect cleared the old track and control, then acquired a
new control lease and verified fresh input. Input to the first substantial
decoded frame transition measured 55–71 ms in these samples. The factory was
left running for the installed Mac app's check; evidence is retained in
`/tmp/nanocodex-mac-vm-evidence/native-swift.log`.

On September 8, the updated signed Release app passed a physical iPhone 17 Pro
screen-menu regression against the Mac host: four open/select/back/dismiss
cycles and a background/foreground recovery, with decoded Mac video and no new
crash reports. The earlier crash came from publishing viewer state while SwiftUI
was dismantling its UIKit canvas. Canvas detach now only releases renderer
references; the dashboard owns session closure. Screen rows also accept taps
across their full width. This check did not send input or restart the VM; the
physical-phone-to-VM chat/restart journey remains unverified. Evidence is in
`/tmp/nanocodex-iphone-screens-20260908/receipt.json`.

The installed Mac app includes the rebuilt command runtime, which retains remote
process sessions after uncertain polling failures and cancels completed poll
timers. Both initial and incremental signed builds pass bundle verification.
After relaunch, the app reconnected and shared its screen automatically with
login startup and the host Hand still enabled.

- A real managed agent discovered the factory VM's `screen_*` tool, received
  decodable screenshots through Code Mode, clicked its visible terminal, typed
  a command, and pressed Return. A later screen-only turn visibly listed the
  resulting `/workspace/agent-screen-control-evidence` file and the retained
  file created by the iOS viewer. No shell tool was used for these screen
  journeys. Native video continued while the agent worked.
- The native Mac viewer took human control of that VM. Agent observation still
  succeeded, but its one attempted text action returned `busy`; the marker was
  absent afterward. After release, agent input worked again. In a second live
  check, human takeover interrupted the third drag of a bounded four-drag
  sequence. The result was `cancelled`, and the agent never sent the fourth
  drag (`/tmp/nanocodex-remote-vm-agent-interrupt2.log`).
- Local VM screen-tool observations took 67–85 ms and click/text/Return actions
  with a resulting screenshot took 174–180 ms in the first successful journey
  (`/tmp/nanocodex-remote-vm-agent-screen-live2.log`). These are tool response
  samples, excluding model reasoning, not internet latency claims. The Linux
  gesture scheduler now keeps the requested gesture clock instead of rounding
  every step's delay up to a host tick; input delayed over 500 ms is cancelled.
- Eight Worker protocol tests pass across signaling and agent screen routing,
  including host replacement, cross-account rejection, result ownership, stale
  routes, image results, and unknown outcomes without replay. Five Swift
  protocol tests pass, including real JPEG encoding, image bounds, and clearing
  retained frames. Go race tests pass after the capture and scheduling fixes.
- Native Mac and iOS Simulator app builds pass. The Swift package's protocol tests
  and real WebRTC video/data-channel test pass.
- The Mac app's real Screens UI published its 2560×1440 display, and the account
  browser decoded the ScreenCaptureKit stream at 1920×1080. Sharing remained
  visible after closing the picker. The main toolbar stop action disconnected
  the browser and removed the Mac from the account catalog. This used the
  isolated app bundle with `NANOCODEX_DESKTOP_DATA` and `NANOCODEX_ENV_FILE`,
  leaving the normal app's runtime and saved account untouched.
- The current Mac test build uses a stable Apple Development signing identity.
  Ad-hoc signatures can change the identity macOS associates with permissions
  on every rebuild; use your team's development certificate for repeated TCC
  testing. Earlier enabled Settings entries retained an old ad-hoc code hash
  and did not authorize the signed build. After OS authentication, removing and
  re-adding the exact installed app in Accessibility and restarting Nanocodex
  applied the current grant. Future builds using the same signing identity
  retain a stable requirement.
  The UI now exposes Enable control for a shared display lacking input access,
  retains permission guidance across catalog refreshes, and remembers the host's
  selected display when reopening the picker.
- The normal `/Applications/Nanocodex.app` was rebuilt and installed with its
  existing development team/signing identity, preserving its saved account and
  workspace. Its native Screens UI decoded the owned VM, typed a terminal
  command, sent Return, and maximized the terminal with a remote double-click.
  The normal app also published its own Mac display with control enabled after
  the grant refresh. An account-authenticated native WebRTC viewer decoded that
  display, acquired control, typed into an empty native app composer, sent
  Backspace, and released control. UI inspection confirmed the exact resulting
  text; the unsent marker was then cleared. This passed on 2026-09-07 in 2.938
  seconds (`/tmp/nanocodex-native-mac-input-live.log`). VM viewing and control do
  not require this local Mac-host permission.
- Live native VM testing exposed a release/reacquire race: the old host release
  acknowledgement cancelled a new control request. Native viewers now serialize
  these exchanges, including cancellation before the original grant arrives.
  Six focused protocol regressions pass, and both immediate-retake cases passed
  against the running VM with subsequent decoded input transitions at 64–68 ms.
- Five signaling tests pass in the real local Durable Object runtime, including
  host replacement, stale generation, authorization expiry, and ownership of
  viewer closure. Managed and account TypeScript checks pass. Six account proxy
  tests previously passed. Two TURN endpoint contract tests pass, covering the
  documented request, credential cache expiry, and provider failure. These use a
  stubbed provider and do not constitute Cloudflare relay connectivity evidence.
  Nine VM pool tests also pass, including the public allocation-authenticated
  screen publication/renewal route and rejection after allocation release.
  Twenty-three Rust VM host lifecycle tests pass.
- A native account-authenticated VM viewer changed the focused test terminal
  through the WebRTC data channel and detected the resulting decoded-pixel
  transitions at 84 and 77 ms locally. The measurement excludes signaling setup
  and reports the first substantial visual change, not completion of an arbitrary
  application operation (`/tmp/nanocodex-remote-vm-latency-test.log`).
  The final packaged companion also passed this journey, with local samples of
  61 and 63 ms (`/tmp/nanocodex-remote-vm-latency-test-v6.log`). Its agent drag
  journey moved the visible terminal and released control; a requested 1500 ms
  drag returned its screenshot in 2.533 seconds, so that duration setting is
  best effort, not an end-to-end response deadline.
- The browser decoded the real 1600×900 Wayland stream, submitted text and raw
  keyboard input, created files in the desktop's workspace, released control,
  and reconnected after page reload. The retained files remained visible. The
  latest packaged companion also reconnects, decodes video, and accepts keyboard input. Browser Escape
  shortcut delivery remains unverified in the current automation environment;
  the visible Release control button works.
- The native iPhone Simulator UI signed into the real local account over HTTPS,
  decoded the Wayland desktop, took control, sent a shell command that created
  `/workspace/ios-native-control-evidence`, released control, relaunched, and
  reconnected. The test passed and its retained screenshots show decoded video
  before and after reconnect. Simulator tests require normal Xcode signing:
  disabling signing omits the app identity needed for Keychain storage. This
  validates the iOS viewer UI. The same journey also passed against the rebuilt
  companion that fetches credentials per viewer and renews ICE. A physical iPhone
  viewer on another network still needs evidence.
- The same iOS UI journey passed against the actual factory VM in 56.565 seconds
  (`apple/build-remote-evidence/RemoteVM-3.xcresult`), including the terminal
  command, keyboard visibility, release, app relaunch, and reconnection. An
  earlier attempt stalled while opening the Simulator keyboard and lost the
  session; the repeat completed without changing the input implementation.
- The physical iPhone 17 Pro (iOS 26.6) produced decoded WebRTC frames and accepted
  Home through a viewer data channel. Account-authenticated tests have passed
  discovery, control exclusivity, touch input opening Calculator's mode menu,
  Home input, handoff, and stop/disconnect using
  the owned bridge. Intermittent native ICE failures on this Mac's virtual/VPN
  interfaces were resolved for same-Mac viewers by enabling loopback candidates.
  Three consecutive full account/phone runs then passed, including two viewers
  and control handoff (19.99 s, 19.73 s, 18.44 s total test duration). Those
  durations are not input latency measurements. A separate decoded-video check
  measured Home input to the first substantial visible transition at 295, 296,
  and 305 ms in three local runs. It sampled frame luminance after Calculator
  settled and separately verified SpringBoard became active; it retained no
  screenshots. This is a small local sample, not an internet performance claim.
- The physical-phone account test exercises renewal accelerated to three
  seconds. Both native viewers retain video and the control lease across repeated
  renewals, followed by control handoff. A browser joined the same phone host,
  decoded 602×1310 video across three observed ICE renewals, and received the
  correct control-exclusivity response after renewal. Extended runs exposed a
  test fixture issue: Calculator restores its open mode menu after Home. The
  test now dismisses that existing menu through remote input and waits for the
  mode button to become hittable before opening it again. Two consecutive full
  runs passed after this correction, with Home-to-visible-transition samples of
  272 and 280 ms. These remain local measurements, not internet latency claims.
- The owned phone bridge starts against the physical device and releases its
  localhost listeners on stop. The signed runner configuration is copied
  privately; source artifacts remain unchanged.
- A real managed agent also controlled the physical iPhone through its published
  screen tool. It observed Calculator, tapped the mode button, and returned
  decodable 589×1280 screenshots. A native viewer took control; the agent's one
  Home attempt returned `busy`, and Calculator remained active. After release,
  agent Home input succeeded and an independent device query confirmed
  SpringBoard. Stopping sharing disconnected the viewer and closed both bridge
  listeners. `AccountPhoneAgentTests` passed in 105.340 seconds; that includes
  model reasoning and is not a latency sample. The evidence is private under
  `/tmp/nanocodex-phone-agent-evidence`, with the test log at
  `/tmp/nanocodex-phone-agent-test2.log`.
- Go race tests cover control fencing, input sequencing, runner configuration,
  and bounded H.264 pipe framing/packetization. The gated Wayland integration test exercised the
  real compositor, encoded video, and injected input. The full account/Wayland test also
  publishes a real host, observes changed ICE credentials, and creates a terminal
  file through the existing data channel after renewal while video continues.
  Run these tests with exclusive access to the desktop: two Waymote instances
  compete for the compositor input method. The application uses one capture per
  host and shares it across viewers.

The normal signed Mac app now passes real WebRTC video, text entry, and raw
Backspace input after its Accessibility grant was refreshed through System
Settings. The user also confirmed the phone viewer works. Mac-host pointer and
agent screen-tool checks, a physical iPhone viewer on a different network, an
external SSH server installation, and a production-duration Cloudflare relay
soak remain follow-ups; the local tests do not establish those results.
No latency numbers should be inferred from test duration or configured FPS.

For the gated Mac viewer test, first start sharing through the isolated Mac app,
focus its empty composer, and provide its published machine ID:

```sh
NANOCODEX_TEST_REMOTE_ENV=/path/to/local-account.env \
NANOCODEX_TEST_MAC_MACHINE_ID=the-published-machine-id \
swift test --package-path apple/NanocodexRemote --filter AccountMacTests
```

The environment file contains `NANOCODEX_MANAGED_URL` for the localhost service
and its local `NANOCODEX_API_KEY`. The input guard requires the app bundle ID
`xyz.paradigm.nanocodex.macos.remote-evidence`. After a non-skipped run, inspect
the composer for `WebRTC Mac input verified`: text entry plus a raw Backspace
must remove the trailing test character. The test does not inspect another
application's UI or count a skipped input step as completed input evidence.

References: [Cloudflare Realtime](https://developers.cloudflare.com/realtime/),
[Waymote](https://github.com/rockorager/waymote),
[Appium WebDriverAgent](https://github.com/appium/WebDriverAgent),
[WebRTC network defaults](https://webrtc.googlesource.com/src/%2B/5a7e6f8ed1c1313300fb6bb48d70e056202011ed/rtc_base/network.h),
[iPhone Mirroring requirements](https://support.apple.com/en-gb/120421).

Device inventory remains available at `GET /v1/account/hands`. Screen viewers
use `GET /v1/account/hands/screens`; the separate responses preserve the native
Hands inventory while screens are published, disconnected, or replaced.
