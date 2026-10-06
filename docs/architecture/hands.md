# Hands

`nanocodex2 hand` is the headless machine runner. Install it once under the OS service manager; the CLI and app observe it. The installer/controller is not its lifetime owner: an installed Hand must remain independently owned after setup, an updater, or every observing CLI session exits. The native `nanocodex hand` controller uses a per-user LaunchAgent on macOS, systemd on Linux, and a per-user Task Scheduler job on Windows. If installation, start permission, or the account-scoped connection is unavailable, the CLI prints an actionable warning and continues remote work. `NANOCODEX_DISABLE_HAND=1` skips the local Hand check.

```text
OS -> Hand daemon <--- outbound WebSocket ---> AccountHostedTools <- agent
       |-- host tools
       `-- VM factory helper -> VMs published as separate mountable Hands
```

On macOS, CLI Hand setup also installs a standalone menu-bar companion. It
shows local service status, verified account sign-in, connected Hands and
Start/Stop/Restart controls without requiring the Nanocodex desktop app. A fresh
installation keeps the companion running while signed out; its sign-in action
opens the CLI account flow in Terminal. Network or permission failures remain
separate from signed-out state, and screen-only connections are labeled separately
from native Hands. Its separate login LaunchAgent observes the existing Hand; it
never creates another Hand identity. Quit Hand stops the local service before
closing the menu. `nanocodex hand menu-bar` installs, repairs, or reopens the icon;
`nanocodex hand menu-status` returns the read-only status snapshot used by the menu.
See the [menu-bar companion](../../macos/HandMenuBar/README.md) for build and lifecycle
details.

The broker durably claims each call before sending it once. The daemon owns execution; a socket carries requests and replies. Each admitted source call has one durable transport command ID. The living Hand keeps its running task or immutable terminal receipt until the broker records the result and acknowledges it.

- Offline before dispatch: not started.
- Result recorded: return that result.
- Connection lost after dispatch: reconnecting to the same runtime queries the original command ID and recovers running work or its retained result within the original deadline.
- Runtime replaced, journal proof missing, or deadline reached: outcome unknown, reported as a local tool failure; the command is never automatically rerun.

The publisher lock protects the host identity. Closing the last client leaves the daemon running. The OS service manager owns startup, restart, and shutdown. Current native and compatible Node transports use WebSocket control ping/pong to detect a broken peer; ordinary Hands have no liveness lease. The broker also replies to legacy JSON pings so older publishers can remain connected during upgrades. Standard browser/Node WebSocket APIs without control ping support use platform close/error events and command deadlines. Provisioned VM authorization has its own expiry and validation. The daemon leaves a live VM factory running through connection outages. Rejected credentials stop the daemon; log in again and restart the service.

Reattaching the same authenticated runtime and immutable catalog retains its ownership epoch (`lease_id`, `runtime_generation`) while physical connection IDs change. Recovery queries never execute commands. Lost acknowledgements replay the same receipt. The broker's SQLite ledger survives owner restarts; the Hand journal survives socket reconnects within one living daemon process. A daemon crash loses that execution proof and does not permit automatic shell replay.

On macOS, the standalone daemon prevents idle system sleep by default using `/usr/bin/caffeinate -i -w <daemon PID>`. The assertion starts after exclusive publisher ownership and state opening, survives reconnects and client disconnects, and ends when the daemon shuts down. It does not keep the display awake or bypass lid-close sleep. If the helper cannot start, the daemon logs a warning and continues without sleep inhibition. Linux and Windows do not acquire this assertion.

Set `NANOCODEX_HAND_KEEP_AWAKE=0` in the standalone service environment to opt out (for launchd, use its plist `EnvironmentVariables` dictionary and reload the service when convenient). An environment variable in an observing terminal does not change an already running service. The macOS app’s `keepMacAwake` preference controls its own ProcessInfo assertion separately; it does not configure the standalone daemon.

## Install

The supported local path is `nanocodex setup`, which performs the shared SMS
login, platform CUA provisioning, and idempotent `nanocodex hand install`. Use
`nanocodex hand status`, `start`, `stop`, or `restart` on all three desktop
platforms. A remote Linux host can be enrolled without interactive auth using
`nanocodex hand install --target user@host [--port PORT]`.

The installed Rust updater runs hourly as a per-user LaunchAgent, systemd timer,
or Task Scheduler job. It stages verified matching CLI/Hand bundles and never
silently restarts an installed Hand. An explicit `hand restart` activates a staged
pair; `hand start` may activate it when the owner is not already loaded. Windows background runs also refresh the verified upstream
CUA payload.

The older machine-wide helper remains available for explicitly managed macOS or
Linux deployments. First run `nanocodex2 login` as the machine owner, then:

```sh
sudo python3 scripts/install-hand-service.py --user "$USER" --binary /path/to/nanocodex2
```

The installer creates one machine-wide launchd service on macOS or systemd service on Linux, running as that non-root user. It uses the user's saved account login and existing `vm.json` configuration. It neither copies credentials into the service definition nor requires a terminal or app to stay open. Host tools work without a GUI; desktop capture needs the platform's GUI session and permissions.

For a custom login, pass `--managed-url https://your-server` and `--account-file /absolute/path/to/nanocodex-account.json` to the installer. These select the existing login without copying its secret.

Use `sudo systemctl stop/start nanocodex-hand` on Linux. On macOS, use `sudo launchctl bootout system/com.nanocodex.hand` to stop and `sudo launchctl bootstrap system /Library/LaunchDaemons/com.nanocodex.hand.plist` to start. Remove/disable the OS service to prevent future boot startup. Windows service installation is not provided by this helper.

The Linux SSH bootstrap installs the same single daemon with its VM recipe. Older separate factory services require an explicit installation/migration decision; an updater never removes or restarts them. Publishers send `capabilities: ["turn_metadata"]`; a retained command journal is advertised with `command_recovery: true` and a stable `runtime_id`. Recovery exchanges command status or retained receipts, and does not provide process persistence across daemon crashes.

## Coordinated updates and independent lifetime

The OS owner is distinct from the user's CLI version store. Launchd owns the
macOS Hand, Task Scheduler owns the Windows login task, and systemd owns the
Linux Hand. A controller command can inspect or explicitly change that owner;
it must not replace it with a foreground child. Closing the controller, CLI or
transport connection does not shut down the installed publisher. Login/boot
startup follows the installed native definition and permissions, not a terminal
session; verify logout/reboot behavior separately on the target OS.

Every selector uses the same verified-pair activation policy: stable/latest
`update`, `update VERSION`, `--nightly`, `--branch`, `--pr`, and local `--path` with its
`--hand-binary`. With an installed (even stopped) or loaded owner, these commands
stage the coherent CLI/Hand bundle by default. `update --apply` alone still
stages; neither it nor a background timer is permission to restart the owner.
The old CLI stays selected until an explicitly authorized handover. Without an
installed/loaded owner a verified pair may activate as CLI-only state, without
installing or starting a Hand.

`update --apply --restart-hand`, `hand restart`, or `hand start` for an unloaded
owner explicitly requests activation. The transaction verifies the candidate,
preserves rollback state, hands over the existing service, and commits the CLI
only after the selected worker's readiness is verified. Pending state and
recovery evidence remain on failure/ambiguity; `hand recover` reconciles an
interrupted transaction rather than blindly starting another publisher.

On Linux, the user controller does not own `/opt/nanocodex` or systemd. The
privileged Hand updater freezes the candidate under the root-owned release tree
and binds recovery to the transaction and candidate digest. It preserves the
service user, account/configuration and prior service state. A separately owned
VM factory and existing guests are not update targets: no factory restart,
guest termination or unit migration is implied by a Hand update. An active
factory must remain pinned to its verified running executable even if its unit
refers to the mutable `current` alias; an ambiguous/inactive alias fails closed.
Privilege/manager failures must be reported, not worked around with a new
foreground Hand.

See `bin/nanocodex/tests/UPDATE_E2E.md` for disposable public-CLI journeys and
separate native-service acceptance. Version probes, preserved plist text and
injected recovery journals do not prove independent OS-service lifetime,
screen readiness, or successful native service rollback.

## Execution mounts

`environment` lists authorized Hand roots; `workdir` selects where each command
runs. A mount already represents its advertised workspace: `/laptop/src` means
`src` beneath that workspace. `mount` provisions a named sandbox when native
builds, tests, or process sessions require one. `/brain` provides durable shared
scratch and the embedded shell without a native Hand.

Cloudflare sandbox processes can write their own workspace and `/brain`, and
read peer sandbox workspaces through native mounts. Connected user Hands provide
execution placement; peer filesystem access requires a conforming native adapter.
Each call captures its Hand connection. Retained process sessions remain pinned
to that Hand; reconnecting never retargets admitted work. Subagents share this
mount policy while keeping model state private. Coordinate concurrent file writes.



## Tool-call latency and measurement

Native Unix pipe children wake their guarded reaper on `SIGCHLD` rather than
waiting for a fixed polling tick. Notifications never replace child identity
checks, process-group cleanup, or the retained terminal receipt. Portable PTYs,
non-Unix hosts and failed signal registration retain the autonomous polling
fallback; PTY-only embeddings still work on time-only Tokio runtimes. Node
Hands keep a bounded 64 KiB unread-output window in memory and spill larger
output to a private file; small commands avoid temporary-file setup/cleanup.
Reply budgets do not discard unread output, and UTF-8 decoding spans polls and
spills.

Fresh managed cells join retained-VM readiness and fresh account Hand discovery
concurrently, then capture one authorized, generation-pinned namespace. Both
branches are joined even on failure; current authority and verified VM routes
are rechecked at capture. Account `/snapshot` and `/invoke` build a synchronous
request-local catalog index once, rather than rebuilding every publisher's
catalog for each machine primitive. This index is not a persistent authority
cache: invocation still checks grants, leases, exact generation and the durable
call ledger before dispatch, and process routes retain their original owner.

Measure the boundaries separately:

- Client/tool-await elapsed time includes routing, transport and result delivery,
  but not the model's time deciding to call a tool.
- `wall_time_seconds` is a host-local wait/execution measurement, not complete
  tool-call latency. Subtracting it from client elapsed time does not establish
  one-way network latency.
- `namespace.prepare` includes fresh-cell preparation; its
  `namespace.host_readiness` and `namespace.account_discovery` sub-stages overlap
  and must not be summed as serial costs. A successful readiness phase can still
  exclude individual unavailable VMs. Each stage excludes the cost of recording
  its own diagnostic event; measure full public/client waits too. `/brain`
  bypasses Hand preparation.
- Correlated Hand observations split namespace routing, account input/ownership,
  broker admission/round trip/settlement, host scheduling/execution and result
  encoding. Preserve first-observed requests, warm calls and concurrent bursts
  separately; do not infer a server cold start from the first request.

Run the actual HTTP/WebSocket/workerd-SQLite shell journey with:

```sh
NANOCODEX_BENCHMARK_LABEL=local pnpm --filter nanocodex-managed-service test:hand-communication
```

The journey publishes source hashes, distributions, phase traces and ownership,
replay and restart evidence under `output/hand-communication-journey/`. It also
checks that shell calls finish while an external CUA request remains pending.
Only external identity and CUA are synthetic fixtures; loopback results are not
WAN or screen-action latency measurements.

Measure catalog scaling and the shipped fresh-cell preparation boundary with:

```sh
pnpm --filter nanocodex-managed-service test:hand-catalog-scaling
pnpm --filter nanocodex-managed-service test:hand-preparation
```

The catalog journey uses 1/8/24 real reverse publishers and native shells,
including stale-token denial, replay, process continuity and disconnected
identity. The preparation journey runs public managed turns through the real
Session and WASM Code Mode. Its explicit external-pool and snapshot delays
exercise dependency overlap, not production network latency. Both retain source
hashes, public transcripts, wire receipts and phase diagnostics under `output/`.

The native executable/public-WebSocket benchmark runs real pipe and PTY shells:

```sh
cargo test -p nanocodex-oai-tools --test it native_shell_call_latency_over_public_websocket -- --nocapture
```

Its wire receipts and per-sample timings are retained in
`output/native-shell-latency/`. Repeat baseline/candidate runs under comparable
load and retain outliers. Local source improvements require a separately
verified Hand build/update before being attributed to deployed machines.

## CUA selection and recovery

CUA discovery is scoped to the Hand's logical `workdir`. A workdir-only call
returns `backend: "upstream"` for a complete JS/reset pair from an online
publisher, or `backend: "native_screen"` for its published screen action API.
Retained offline upstream catalogs do not take precedence over a live screen.
An online upstream pair adds the `computer` capability even when the native
publisher omitted that label. Shell connectivity and screen connectivity remain
separate; capability labels alone do not prove that an input can be dispatched.

Each Code Mode cell keeps its captured provider. A missing provider fails before
input dispatch and directs the caller to rediscover the same Hand in a new
cell. A disconnected or replaced screen does not redirect an admitted action to
another screen or backend. Observe fresh state before issuing new input after
an uncertain result. Native scroll actions require `x`, `y`, `deltaX`, and
`deltaY`; a vertical scroll supplies `deltaX: 0`.

Run `pnpm --filter nanocodex-managed-service test:cua-routing` for the public
managed-turn journey through account discovery, Code Mode and the Hand
transports. Evidence is written under `output/cua-routing-journey/`; the external
model and native CUA/screen endpoints are synthetic.

## Native screen ownership

Screen startup, display allocation, capture-helper supervision and reconnects
belong to the shared Rust Hand lifecycle. The ordinary `nanocodex2 hand` and
installed device Hand call the same `NativeScreen`/`screen_supervisor` code.
No per-machine shell watchdog, JPEG fallback, socket-unlink script or factory
restart is part of screen recovery. On headless Linux the Hand starts its own
private Xvfb desktop, reserves an explicit display with a retained file lease
and normal X server PID lock, and repairs a lost owned socket in place. It never
unlinks another display or weakens display authentication. Native desktop live
media requires WebRTC H.264; agent-requested screenshots are separate.

The Linux installer supports Debian/Ubuntu (`apt-get`) and Arch/Omarchy
(`pacman`). It provisions FFmpeg, Xvfb, Openbox, XTerm and fonts, validates
`libx264`/`x11grab` and actually encodes a synthetic H.264 frame before service
startup. Arch installation uses the existing sync database: it never silently
refreshes it or performs a host-wide upgrade. A stale database or dependency
conflict requires an administrator-approved upgrade rather than an automatic
retry. Setup waits for the connected Hand and its controllable positive-size
video desktop, rejecting JPEG-frame catalogs as readiness. Catalog readiness is
not a substitute for decoding the published stream.

Published x86_64 Linux-GNU Hands embed pinned Waymote and Grim plus their ELF
loader/runtime-library closure. `scripts/build-linux-screen-helpers.sh` builds
that payload before the Hand binary; release/nightly Linux-GNU jobs set
`NANOCODEX_LINUX_SCREEN_BUNDLE`. Linux Hand and sandbox images build the same
payload natively for their architecture and verify it is embedded in the binary.
Only the single Hand executable is distributed.
Runtime extracts into an owner-private hash-addressed cache, rejects links and
special entries, checks the embedded manifest and all file digests before use,
and invokes the bundled loader without global loader-path configuration. A
developer build without this payload reports a Wayland packaging error; it does
not silently switch to a different desktop or JPEG live media.

Linux session discovery checks a private same-owner runtime directory, a live
same-owner compositor Unix socket and its peer credentials. With no valid
explicit hint it scans only that owner's standard `/run/user/<uid>` (and a
validated inherited runtime), never another user's session or `/proc` environment.
A sole live Wayland display is selected automatically; ambiguous displays require
an explicit choice. Child environments are configured without process-global
mutation. An explicit Wayland backend fails visibly if no usable session exists.
With no Wayland session, the Hand owns a private Xvfb desktop. The normal remote
installer's dedicated non-root service account intentionally does not acquire an
interactive user's compositor: run the Hand as that desktop owner when physical
capture is intended. OS permissions and compositor capture/input protocols remain
required; packaging does not bypass them. Native Wayland capture does not depend
on an `X0 -> X0_` alias; an optional upstream X11 CUA integration may have separate
requirements. Managed desktop/server images retain their explicitly provisioned,
architecture-specific helper executables via `NANOCODEX_WAYMOTE`/`NANOCODEX_GRIM`.
Those build-owned overrides are not a missing-bundle or capture-failure fallback.
Runtime performs no downloads, package installation or sudo.
