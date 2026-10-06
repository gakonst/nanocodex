# Installed upstream CUA runtime

Native Nanocodex, Nanocodex2, local Hands, and the JavaScript desktop runtime
provision OpenAI's CUA provider automatically on macOS. Windows upstream native
CUA is currently unsupported without a verified no-Codex helper contract; its
automatic installation/launch path is disabled before side effects. They expose
its actual MCP catalog, including descriptions, schemas, metadata, and visibility.
Production Code Mode remains QuickJS; the provider uses its own bundled Node.

The managed **macOS** launcher enables upstream `browser,computer` surfaces and
TinySky. Its own native-message relay replaces the official Chrome app-server
proxy. The signed CUA and Sky components remain unchanged; no official Codex CLI
or app server runs. Hosted browser CDP and other Hands' screen providers remain
separate paths. Setup regenerates immutable Nanocodex host assets independently
of the cached signed bundle; a launcher update does not require `--refresh`.

```sh
nanocodex2 computer setup           # provision once or verify/reuse the cache
nanocodex2 computer setup --refresh # check OpenAI's feed and update changed components
# Both commands are also available as nanocodex computer setup.
```

`nanocodex setup` is the guided, resumable path. On macOS it installs the dormant
Hand first, starts CUA preparation in the background, then signs in and connects
the Hand. CLI account sign-in also connects the installed Hand automatically;
CUA download completion is not a prerequisite. `setup --skip-account` prepares
an unsigned-in Mac for later login. On macOS setup registers the Nanocodex native-message bridge for supported
installed browsers. It does not install extensions, open browsers or modify
profiles; extension-backed APIs require a compatible enabled extension.
CUA uses the direct MCP host without the official Codex binary or desktop GUI.
The trusted host owns app-access consent; protected-target and OS permission
checks remain native. See [direct MCP host](direct-mcp-host.md).

The macOS updater range-fetches only signed CUA/Node components plus signature
metadata and the tiny signed main executable used only for attestation. The
official `codex` executable, Chrome plugin, Electron, `app.asar`, and frameworks
are excluded. A small Nanocodex MCP/lifecycle host handles explicit native-app
consent and the helper's narrow policy reads. No model, Codex thread, sign-in
state, or general app server is involved. Node, node_repl, and Sky are still
upstream binary dependencies; this is not a Sky-only distribution.

A running Hand retains its provider generation. Installing the new direct host
or reloading a TUI does not replace that configuration in the shared daemon.
Restart the Hand to activate it; existing JavaScript scopes and debugger
attachments do not survive. Old cached bundles containing Codex are not reused
as new direct-host generations or modified beneath running processes.

## Timeout ownership and cancellation

`timeout_ms` belongs to the upstream provider. Nanocodex forwards it unchanged and
awaits the provider result; it does not subtract queueing or startup time, supply
a default execution timeout, or abandon a call based on that argument. The macOS
direct MCP host likewise delegates tool execution timeouts to the upstream
provider while keeping trusted startup and native readiness deadlines. This applies to standalone and managed
bridges. An unresponsive tool remains governed by upstream timeout handling or
caller cancellation.

For direct MCP processes, the Rust adapter gives each provider startup a trusted
120-second cumulative deadline covering initialization and complete catalog
discovery. This applies both to `ComputerTools::connect` and to each conversation's
new provider process. It does not consume or derive from `timeout_ms`, and ends
before tool execution starts. Startup expiry discards that owned transport; a
conversation interrupted during startup requires explicit reset. The managed
macOS launcher also retains its separate phase deadlines.

Genuine caller cancellation discards only the affected conversation's owned
transport and marks its session interrupted. Other conversations retain their
sessions. A successful explicit `js_reset` is required before continuing; a failed
reset leaves the session interrupted. Follow reset with a fresh
observation of the intended surface. Closing the transport or resetting the
session is not proof that upstream/native input stopped. Effects may be uncertain;
never automatically replay that input.

## Browser selection

Read the provider's actual discovered browser declarations before selecting a
browser or tab. Setup prepares the Nanocodex bridge and registers manifests for
installed Chrome, Chrome Beta/Dev/Canary, Chromium, Brave, Edge and Arc apps.
Only the two supported OpenAI extension origins may connect. The relay enforces
agent request headers on session commands and reports that effective state only
when the extension advertises boolean support. The launcher disables ambient
browser telemetry/identity network requests with
`BROWSER_USE_DISABLE_AMBIENT_NETWORK=1`; no Codex credentials are fabricated.
See [browser request marking](direct-mcp-host.md#browser-request-marking). Existing official
Codex or other non-Nanocodex native-message registrations are preserved and
reported as conflicts; resolve ownership explicitly before rerunning setup.
Setup never installs an extension, opens or restarts a browser, or modifies a
profile. See [the bridge setup contract](direct-mcp-host.md#browser-and-computer-surfaces).

If a running provider reports Browser APIs disabled, its native app controls do
not provide background tab isolation. Use a supported background browser or an
isolated desktop; do not activate the user's browser as a fallback.

## Capability boundaries

Browser background tabs and native background apps are separate capabilities.
The extension owns browser tab groups, leases, handoff marks and tab cleanup;
the direct relay preserves the upstream protocol. Availability depends on the
installed extension's advertised capabilities. A successful catalog or synthetic
relay test does not verify live background screenshots, input, or handoff.
The extension supports `markHandoff` to retain a tab between turns; the pinned
API lists `requestManualHandoff` as a cloud-browser feature unavailable by default
on extension backends. A retained tab does not provide a private credential form.

The pinned macOS Sky API targets native apps by name, path or bundle ID. It does
not expose the old custom runtime's exact-window selection, independent input
lanes or per-window agent cursors. Background app launch and an app-scoped action
do not establish simultaneous human/agent input isolation. Linux and Windows
window APIs have different contracts; discover the selected provider's API.

Audio capture is not supported by the direct host: it does not forward the
upstream audio opt-in and declines recording consent. Locked-computer access,
persistent per-app approval and full managed-policy import are also unavailable.
These require explicit host integrations, not enabling a browser surface.

## Native app recovery

On macOS, the pinned provider's `cua.getApp` launches an app in the background
and includes an initial accessibility observation. It accepts an app name, path,
or bundle ID, not a native window ID. A running process alone does not guarantee
a responsive or usable app window.

If that initial observation fails with a provider timeout, or the caller cancels
the wait, reset the CUA session before continuing. Reset does not establish that
earlier upstream/native operations stopped; inspect fresh state before acting.
Use supported CUA to open the intended app normally from an observed launcher, such as its item in Finder, then select it again. In live Slack testing,
opening the installed app through Finder recovered a stalled initial snapshot;
subsequent background observations, search, channel navigation, and a fresh CUA
session succeeded without opening ChatGPT. This is a verified recovery, not proof
of the upstream stall's root cause or a reason to replay input automatically.

After a transient menu or window closes, `cgWindowNotFound` can refer to that
vanished window. Select the same app again and inspect its fresh state before
acting. This recovered Finder's desktop target in live testing. For window-based
input, use an actual app window. These recovery notes accompany workdir-only
discovery separately from the unchanged provider tool definitions.

## Distribution

On macOS, setup uses the official versioned, architecture-specific archives from
the desktop appcast. Setup probes the immutable archive with bounded HTTP ranges,
rebuilds a ZIP containing only the CUA Node runtime, signature anchor, and
signature metadata, and rejects archives that do not honor exact ranges. The
minimal bundle is verified against Apple's signature chain, OpenAI team
`2DC432GLL2`, bundle identity `com.openai.codex`, and every selected `files2`
SHA-256 seal. Compatibility is based on required signed components, not a hardcoded
desktop build. The user's installed desktop app is never read or replaced.

On Windows, setup and refresh return `unsupported` without launching PowerShell,
installing the Store app, copying companion executables, or selecting an old
managed receipt. The standalone installer and upstream native-host live probe
also refuse execution. The local Windows JavaScript transport can launch a
native helper directly, but the signed Windows helper binary/source is absent
from the available local package and the registered Windows Hand is offline.
Absence of a CLI call in JavaScript is not proof of the native policy contract.
See [Windows evidence and blocker](windows-upstream-helper-contract.md).
Generic explicitly configured external MCP providers remain separate trusted
host configuration; this restriction is not a ban on all Windows MCP servers.
The native screen fallback is unchanged.


Linux and Linux VM/container guests require an explicitly configured upstream
MCP provider. No custom CUA runtime, background-input plugin, or legacy fallback
is bundled. Automatic `computer setup` currently supports macOS; Windows upstream setup is
unsupported until the native no-Codex contract is verified;
without a provider, guests use their native controllable screen action contract
through the workdir-routed CUA entry point. This fallback does not emulate or
claim to install OpenAI's JavaScript provider.

## Selection and updates

The cache lives under `${NANOCODEX_DIR:-$HOME/.nanocodex}/runtimes/openai-cua`
(`USERPROFILE` is the Windows fallback). Version directories are immutable after
installation. Only a complete verified runtime with prepared bridge assets and successful
manifest registration is selected; a failed download or
copy preserves the previous selection. Old versions remain available to running
processes. New managed macOS receipts carry `dependency_contract` equal to
`nanocodex-direct-cua-v2`; Linux retains its separate computer-only
`nanocodex-native-no-codex-v1` contract. Automatic discovery rejects unmarked legacy
receipts or receipts exporting `CODEX_CLI_PATH`; installation regenerates the
Mac selection rather than running the old host. Windows automatic discovery
never reads its old receipt. Explicit external-provider commands remain trusted
owner configuration, not an assertion that they satisfy this dependency contract.
Cached corruption produces an actionable error rather than silently
selecting a different backend. Run setup with `--refresh` to repair it.

The native and JS desktop hosts select the managed MCP provider automatically.
An explicit `NANOCODEX_COMPUTER` still wins; `off`, `none`, or `0` disables CUA and
its automatic download. Custom external MCP commands continue to use
`NANOCODEX_COMPUTER_TRANSPORT=mcp`. No versions are spoofed and no provider binaries
are committed to this repository or redistributed in Nanocodex release assets.

The older `scripts/install-upstream-cua.py` copy-only launcher is retired and
fails before touching files. It did not provide the no-Codex lifecycle/policy
host or attestation context. Use the shared native provisioning command on
macOS, or the separate Linux Sky installer; Windows upstream support is currently
unavailable.

## Provider permissions and validation

The outer Rust/JavaScript adapters advertise no MCP elicitation and reject
unsupported incoming requests with method-not-found (`-32601`). On managed
macOS, the direct host is the upstream provider's internal MCP client. It
advertises form support and supplies empty native application-access consent
from trusted `NANOCODEX_CUA_APP_CONSENT=allow` policy (the managed launcher's
default). A trusted host can set `deny`; tool arguments cannot override it.
Audio, data-bearing forms, unknown connectors, and requests outside active JS
are not automatically approved. Native protected-target checks, OS permissions,
and caller authorization continue to apply. Known enforced local/MDM policy
sources fail closed pending a real policy integration; no enterprise Codex
identity is fabricated. See [native Hand computer access](native-hand-consent.md).

Validation covers transport fidelity, cancellation and cleanup, installer
migration, attestation, and isolated no-Codex macOS MCP lifecycle checks.
Arithmetic/catalog success alone is not proof of native observation/input.
Mac native UI acceptance remains pending an unlocked console. The modified
no-Codex Linux wrapper delivered a key to its uniquely observed synthetic X11
fixture; this does not establish native Wayland coverage or a production rollout.
Windows native acceptance remains unavailable, not inferred from transport tests.

Windows transport fixtures preserve framing, metadata, deliberate denial,
turn completion, disconnect cleanup and cross-client isolation. Those fixtures
use injectable transports, not real native screenshot/input. The previous
Codex-dependent Windows live probe is retired and fails before launch.

## Linux native host

A configured Linux provider must launch the native Sky service outside the model
JavaScript provider so it can reach the desktop X server. `linux_sky_host.mjs`
hosts the unchanged `@oai/sky/service` in a disposable desktop-user process.
Its trusted proxy uses upstream NodeREPL `nativePipe`; ordinary model JavaScript
has no nativePipe capability. Standalone upstream node_repl is **not** a
Codex-managed execution sandbox. The host passes only selected desktop/runtime
environment variables, scrubs inherited CLI/tokens/bootstrap overrides, disables
analytics, and fails closed on known enforced administrative policy. MCP tool
definitions, descriptions, metadata and results still come from the real provider.
Only `computer` is enabled; browser opt-ins fail before any child starts.

For an already installed, compatible upstream Linux runtime, create a separate
host installation from this checkout:

```sh
python3 scripts/install-linux-sky-host.py \
  --runtime /path/to/cua_node \
  --destination "$HOME/.local/share/nanocodex/sky-host-version"
```

Set `NANOCODEX_COMPUTER` to the printed launcher path and
`NANOCODEX_COMPUTER_TRANSPORT=mcp`. Run the Hand/provider as the desktop user with
its real DISPLAY and session bus. Keep the host modules outside model-writable
workspaces. A system administrator can install the same modules in a protected
system directory and wrap the launcher with the desktop-session environment.
The script does not obtain or authenticate an upstream Linux distribution;
automatic `computer setup` remains limited to macOS.

For a Linux VM, add `--register-managed` to publish the launcher selection under
`$NANOCODEX_DIR/runtimes/openai-cua/provider.json` (or
`$HOME/.nanocodex/runtimes/openai-cua/provider.json`). The guest runtime discovers
this receipt at Hand startup. Install it in the guest, not on the VM host, and
restart the guest Hand to refresh its tool catalog. Host and guest binaries must
use the same VM tool protocol; an older factory binary can advertise tools yet
fail when forwarding a call to a newer guest.

OpenAI's Linux distribution includes both `x86_64` and `aarch64` packages. The
26.915.31945 ARM64 package is published at
`https://persistent.oaistatic.com/codex-app-prod/linux/arch/26.915.31945/aarch64/chatgpt-bin-26.915.31945-1-aarch64.pkg.tar.zst`.
Use the complete matching `resources/cua_node`; do not copy or require
`resources/codex`. Verify the package signature against the fingerprint published in
the upstream installer. This version contains `sky_linux_arm64`; the x86 package
contains `sky_linux_x64`. A macOS ARM64 bundle is not a Linux ARM64 bundle.
Bundled Node and the native Linux Sky executable require glibc, so a bare Alpine
image needs a compatible userspace before this runtime can start. None of these
installation steps require changing Sky's input behavior.

The host serializes native calls, bounds frames and queues, and owns a private
Unix socket. Disconnect, cancellation, reset and turn completion reject queued
work, release tracked drags through upstream `drag_end`, and terminate the
service/helper process group after bounded cleanup. An in-flight input operation
can have partial effects before cancellation; cancellation is never a rollback.

The installed Linux Sky target controls X11/Xwayland windows. This transport does
not make native Wayland windows visible to that target. Application-level input
filters still apply (for example, xterm rejects synthetic SendEvent input by
default). The Linux host remains computer-only; the managed macOS browser bridge is a
separate implementation and does not add Linux browser support.

Transport tests: `node --test crates/experimental/nanocodex-computer/tests/linux-sky/host.test.mjs`.
The new no-Codex wrapper was verified with real upstream MCP catalog/metadata,
inventory, persistence/reset/turn-end, and a key delivered to a uniquely observed
owned X11 test window. Historical Codex-sandboxed GTK tests are not evidence of
this standalone kernel's sandbox or full native coverage. No global provider
selection or production Hand restart was performed for that Linux probe.
