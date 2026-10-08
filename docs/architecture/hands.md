# Hands

`nanocodex2 hand` is the headless machine runner. Install it once under the OS service manager; the CLI and app observe it. The installer/controller is not its lifetime owner: an installed Hand must remain independently owned after setup, an updater, or every observing CLI session exits. The native `nanocodex hand` controller uses a per-user LaunchAgent on macOS, systemd on Linux, and a per-user Task Scheduler job on Windows. If installation, start permission, or the account-scoped connection is unavailable, the CLI prints an actionable warning and continues remote work. `NANOCODEX_DISABLE_HAND=1` skips the local Hand check.

`nanocodex2`, `run`, and `attach` connect to this persistent daemon through
private local IPC. Each client owns only an observing lease; closing it leaves
the daemon and its processes running. A terminal's current directory is
descriptive request context, independent of publisher identity. Changing
projects does not publish or reconnect a Hand. An unavailable service produces
a warning instead of a replacement workspace publisher.

```text
OS -> Hand daemon <--- outbound WebSocket ---> AccountHostedTools <- agent
       |-- host tools
       `-- VM factory helper -> VMs published as separate mountable Hands
```

On macOS, CLI Hand setup also installs a standalone menu-bar companion. It
shows local service status, verified account sign-in, known connected and disconnected Hands and
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

The account-only `GET /v1/account/hands/inventory` combines live and retained
account registrations. It excludes thread tool hosts and Connect-scoped
routes and returns only names, IDs, kind and connection state. Discovery has a
four-second deadline. Unavailable account sources retain known identities as
unknown and mark the inventory incomplete; account-owned offline devices remain
retained. The obsolete workspace registry and its overflow marker are removed
when an account broker starts. The response retains the legacy
`known_account_and_workspace` coverage token for released clients, while its
data contains only account Hands. Existing `/v1/account/hands` consumers retain
their live-only contract; screen advertisements remain a separate source.

Both `nanocodex hand` and `nanocodex2 hand` expose `list`, `forget <id>`, and
`prune`. Forget removes account routing, retaining runtime tombstones; it refuses
connected or uncertain Hands unless `--force` is supplied. Prune removes only
Hands confirmed offline and rechecks them before removal. The matching SDK
methods are `client.hand.list()`, `client.hand.forget(id, { force: true })`, and
`client.hand.prune()`. Mutations require direct account `agents:write` and
`tools:use`; Connect grants cannot remove Hands.

Run `pnpm --filter nanocodex-managed-service test:hand-inventory` for the real
HTTP/WebSocket journey covering legacy-index retirement, account isolation,
authorization, offline retention, and migration rejection of thread-scoped
native catalogs. Each run writes its request and catalog evidence under ignored
`output/hand-inventory-journey/`.

### One account Hand per computer

Physical computers attach only to the account broker at
`/v1/account/tool-host`; that one Hand provides shell, code, screen and CUA to
every thread. Thread-scoped workspace Hands are retired. A thread tool host
(`/v1/agents/:id/tool-host`) that publishes machine metadata or a canonical
machine primitive (`exec_command`, `write_stdin`, `preview`,
`native_secure_input`, `validate_app`, `mcp__cua_repl__*`) is refused before
publication with `catalog_contract_mismatch` carrying
`hand_migration_required`. Thread tool hosts may still publish other tools
(SDK `toolsTarget(agentId)` MCP/app tools). Leased VM attachments remain
thread-scoped: their fixed `vm-host:<allocation>:<epoch>` route is injected by
the Worker only after verifying the server-issued lease grant, so machine kind
or metadata alone never qualifies.

Routes retained from older services are not deleted. They leave discovery,
inventory, file reads, secure input and new namespace routing even while
connected. The exact runtime that still owns a durable namespace process or an
unsettled admitted call may reconnect solely to finish that work: `write_stdin`
reaches it through the process's runtime-pinned key, and no other runtime or
reused machine ID matches. Once that work settles, the runtime's reconnect is
refused like any new thread-scoped catalog. The desktop runtime publishes only
its account Hand and drops saved thread-scoped Hand records.

Run `pnpm --filter nanocodex-managed-service test:hand-retirement` for the
upgrade journey: an older service build admits a thread-scoped Hand and a
process, the candidate restarts on the same durable storage, hides that Hand,
finishes the pinned process, refuses a fresh thread-scoped native publisher and
admits a non-native thread catalog. Set `NANOCODEX_RETIREMENT_BASELINE=<ref>`
to choose the older build and `NANOCODEX_RETIREMENT_CANDIDATE=<ref>` to replay
another revision. This upgrade fixture requires Git history containing the
baseline commit `f8a2b451dabba74cff003d6ec54d32cb06fd113b`; shallow checkouts
must fetch that commit before running it. Evidence is written under ignored
`output/hand-retirement-journey/`.

The broker durably claims each call before sending it once. The daemon owns execution; a socket carries requests and replies. Each admitted source call has one durable transport command ID. The living Hand keeps its running task or immutable terminal receipt until the broker records the result and acknowledges it.

- Offline before dispatch: not started.
- Result recorded: return that result.
- Connection lost after dispatch: reconnecting to the same runtime queries the original command ID and recovers running work or its retained result within the original deadline.
- Runtime replaced, journal proof missing, or deadline reached: outcome unknown, reported as a local tool failure; the command is never automatically rerun.

The publisher lock protects the host identity. Closing the last client leaves the daemon running. The OS service manager owns startup, restart, and shutdown. Current native and compatible Node transports use WebSocket control ping/pong to detect a broken peer; ordinary Hands have no liveness lease. The broker also replies to legacy JSON pings so older publishers can remain connected during upgrades. Standard browser/Node WebSocket APIs without control ping support use platform close/error events and command deadlines. Provisioned VM authorization has its own expiry and validation. The daemon leaves a live VM factory running through connection outages. Rejected credentials stop the daemon; log in again and restart the service.

Reattaching the same authenticated runtime and immutable catalog retains its ownership epoch (`lease_id`, `runtime_generation`) while physical connection IDs change. Recovery queries never execute commands. Lost acknowledgements replay the same receipt. The broker's SQLite ledger survives owner restarts; the Hand journal survives socket reconnects within one living daemon process. A daemon crash loses that execution proof and does not permit automatic shell replay.

Native process sessions retain their exact broker route in the thread's durable namespace storage. Reconstructing the cloud runtime restores that route without inventory discovery. Current authorization and the original daemon runtime are still checked; an identical machine name or path cannot redirect saved stdin to a replacement. Process IDs created by older server versions without a persisted route cannot be recovered after their cloud runtime is lost.

The real SQLite/WebSocket/PTY recovery journey runs with `pnpm --filter nanocodex-managed-service test:native-process-recovery`. It reconstructs the namespace and provider, verifies continued input to the admitted process, and rejects revoked authority, another session, and a replacement runtime.

On macOS, the standalone daemon prevents idle system sleep by default using `/usr/bin/caffeinate -i -w <daemon PID>`. The assertion starts after exclusive publisher ownership and state opening, survives reconnects and client disconnects, and ends when the daemon shuts down. It does not keep the display awake or bypass lid-close sleep. If the helper cannot start, the daemon logs a warning and continues without sleep inhibition. Linux and Windows do not acquire this assertion.

Use **Keep Mac Awake** in the standalone Hand menu, or `nanocodex hand keep-awake on|off`.
Omit the value to inspect JSON containing the saved `configured` preference,
effective `enabled` value and `active` daemon assertion (null if unconfirmed).
The setting defaults to on, persists in `~/.nanocodex/hand-keep-awake.json`, and
applies within one second without restarting the service or interrupting work.
The daemon owns its watcher through reconnects and releases the assertion on
shutdown. Screen locking and display sleep work normally; keeping awake uses
more battery. This does not unlock the Mac or permit interaction with protected
login screens.

The existing `NANOCODEX_HAND_KEEP_AWAKE=0` service-environment opt-out remains
an override. The menu shows it and disables its checkbox; remove that override
and restart the service to resume control through the persistent setting. An
environment variable in an observing terminal does not change a running service.

Run `node bin/nanocodex/tests/hand_keep_awake_e2e.mjs /absolute/path/to/nanocodex`
on a running updated macOS Hand to verify off/on, persisted settings, unchanged
owner PID, and the actual macOS idle-system assertion. The journey restores the
original preference and never locks, stops, or restarts the Hand.

The macOS app’s `keepMacAwake` preference controls its own ProcessInfo assertion separately; it does not configure the standalone daemon.

## Install

The supported local path is `nanocodex setup`, which performs the shared SMS
login, platform CUA provisioning, and idempotent `nanocodex hand install`. Use
`nanocodex hand status`, `start`, `stop`, or `restart` on all three desktop
platforms. A remote Linux host can be enrolled without interactive auth using
`nanocodex hand install --target user@host [--port PORT]`.

On macOS and Linux, the installer also supports logged-out preparation with
`nanocodex hand install --prepare`. It preserves an existing installation's
account configuration. The account login flow activates the prepared service
using the exact saved login through `nanocodex hand connect`; an unrelated
ambient API key does not select its account.

Linux prepares the service and starts `nanocodex-hand-components.service` with
`systemctl start --no-block`. This root-owned oneshot installs and validates
desktop dependencies, has a fifteen-minute startup bound, and remains active
after completion. The non-root Hand service requires that unit and waits for
`/opt/nanocodex/account.env`; logged-out preparation creates no account credential
or publisher. Sign-in installs that root-owned mode-0600 environment file,
starts the Hand, and waits for account and controllable screen catalogs.

A fresh local sudo installation uses the invoking non-root user's UID; retained
installations keep their existing service owner. The dedicated `nanocodex` user
is used when no non-root sudo invoker is available. State remains under
`/srv/nanocodex` with private permissions. Same-owner clients validate the
root-owned installation metadata and reuse this state and private IPC across
changes to `HOME`; other users do not gain access. Closing observers or repeating
unchanged setup does not restart the publisher.

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

On macOS the Hand needs Screen & System Audio Recording (live screen) and
Accessibility (mouse and keyboard input). Installation requests both together as
soon as the activated owner is verified connected, instead of at first screen or
input use: `nanocodex hand install`, `nanocodex setup`, `nanocodex hand connect`
(also run after a successful CLI/TUI login), and the first-launch
`hand install --if-missing` that activates a new owner. A dormant
`hand install --prepare` service has no running process and asks nothing until a
login activates it. An existing owner returned by `--if-missing`, ordinary
daemon startup, status and workspace attachment never request consent.

The request runs in the capturing process, with its PID verified against launchd
and the local socket peer; macOS shows its own dialogs and grants access only
after user consent. Already-allowed permissions are not requested again. A
pending or failed request never undoes the installed, connected service: setup
reports that it is waiting for permissions rather than complete, with the
recovery command. `nanocodex hand permissions` repeats the request and reports
pending and granted permissions separately; `--open-settings` opens the matching
privacy panes. Restart the Hand after granting access. Unsigned or ad-hoc signed
local builds can require consent again when their executable identity changes.
`nanocodex setup --skip-computer` skips only the optional upstream Computer Use
app, which is a separately signed macOS app with its own consent on first use;
the Hand's own permissions are still requested. `--skip-hand` requests none.


For a custom login, pass `--managed-url https://your-server` and `--account-file /absolute/path/to/nanocodex-account.json` to the installer. These select the existing login without copying its secret.

Use `sudo systemctl stop/start nanocodex-hand` on Linux. On macOS, use `sudo launchctl bootout system/com.nanocodex.hand` to stop and `sudo launchctl bootstrap system /Library/LaunchDaemons/com.nanocodex.hand.plist` to start. Remove/disable the OS service to prevent future boot startup. Windows service installation is not provided by this helper.

When explicitly migrating a machine that already has a separately managed VM
factory, set `NANOCODEX_EXTERNAL_VM_FACTORY` in the Hand service environment to
that existing provider's name. The daemon advertises this reference and reports
`factory.status: "external"`; it neither starts nor stops that factory or its
VMs. This reference takes precedence over a local VM recipe. It is not evidence
of live capacity: the broker still validates the provider and allocation. Fresh
installations normally let the daemon own its configured factory helper.

The Linux SSH bootstrap installs the same single daemon with its VM recipe. Older separate factory services require an explicit installation/migration decision; an updater never removes or restarts them. Publishers send `capabilities: ["turn_metadata"]`; a retained command journal is advertised with `command_recovery: true` and a stable `runtime_id`. Recovery exchanges command status or retained receipts, and does not provide process persistence across daemon crashes.

Run the Linux installer journey against the built CLI pair:

```sh
docker build -t nanocodex-installer-journey scripts/fixtures/linux-installer
sudo python3 scripts/tests/linux-hand-installer.py --bin-dir target/debug \
  --image nanocodex-installer-journey --output output/linux-hand-installer
```

The journey owns a disposable privileged systemd container; sudo permits reading
privately owned build binaries. Only the account HTTP/WebSocket service is a
fixture. It checks logged-out preparation, activation, private credentials, two
same-owner observers, daemon survival and idempotent setup, retaining command,
service and account evidence in the selected output directory. This container
journey does not establish physical desktop capture or reboot behavior on a
user's machine.

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

An explicit `--path`, `--branch`, or `--pr` selection is preserved by background
updates while it is active or pending. Hourly checks still refresh the separate
upstream CUA components, but do not stage a release over that selection. Use
`update --auto enable` to resume automatic release selection, or select a release
explicitly with `update VERSION`. `update --auto status` reports a held selection.
Failed installs preserve the previous choice. The Hand restart contract is unchanged.

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

For a known selected user Hand, `exec_command` and workdir-scoped CUA capture
only that route. A locally online shell route needs no account inventory;
account-owned routes and CUA use a selected-machine lookup, including the
independently published screen for CUA. Captures extend the cell without
replacing its earlier generation bindings. `write_stdin` uses its retained
process owner and rechecks authority and generation directly, without inventory
preparation or rerouting a poll.

Full discovery remains for inventory, unresolved routes and retained-VM
preparation. That path joins retained-VM readiness and account Hand discovery
concurrently before capturing an authorized, generation-pinned namespace. Both
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
  `namespace.host_readiness` and `namespace.account_discovery` sub-stages on
  the full-discovery path overlap
  and must not be summed as serial costs. A successful readiness phase can still
  exclude individual unavailable VMs. Each stage excludes the cost of recording
  its own diagnostic event; measure full public/client waits too. `/brain`
  bypasses Hand preparation.
- `namespace.selected_lookup` measures the selected account Hand lookup;
  selected routes do not imply retained-VM preparation avoids full inventory.
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

A persistent dynamic CUA gateway can discover components that finish preparing
after daemon startup. Discover with only `workdir` before supplying provider
arguments. If components are still preparing, discovery can select the captured
native screen; without one it reports preparation and requires a new cell.
Once selected, the provider remains pinned for that cell, and an input failure
never triggers a retry on another backend.

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
