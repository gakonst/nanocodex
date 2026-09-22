# Installed upstream CUA runtime

Native Nanocodex, Nanocodex2, local Hands, and the JavaScript desktop runtime
provision OpenAI's CUA provider automatically on macOS and Windows. They expose
its actual MCP catalog, including descriptions, schemas, metadata, and visibility.
Production Code Mode remains QuickJS; the provider uses its own bundled Node.

Browser-enabled launchers set upstream `BROWSER_USE_TINYSKY_ENABLED=1`, matching
the official desktop host. This exposes `Tab.ax`, which upstream `cua.getTab()`
and `cua.createBrowserTab()` use to return accessibility state. Mac setup regenerates Nanocodex host assets independently of the cached signed
bundle; a launcher update does not require `--refresh`.

```sh
nanocodex2 computer setup           # provision once or verify/reuse the cache
nanocodex2 computer setup --refresh # check/download the current upstream release
# Both commands are also available as nanocodex computer setup.
```

Direct binary/source installs provision the runtime on first CUA use. Installation
does not sign in to an account. On macOS, CUA starts an isolated official app server
without the desktop GUI and delegates application access and confirmation
handling to the existing upstream permission policy. Nanocodex adds no prompts. Build 9922 is currently supported;
setup rejects other builds before publication. Official sign-in and OS permissions
still apply. See [managed macOS host](official-app-server-bridge.md).

A running Hand retains its provider launch configuration. Updating the installed
launcher or reloading a TUI does not replace that configuration in the shared
Hand daemon. Restart the Hand after upgrading from the older direct provider
launcher to the managed macOS bridge, then rediscover its CUA contract. Existing
CUA JavaScript bindings and browser debugger attachments do not survive this
restart. Calls already admitted to the old connection are never retargeted.

If native app access reports `nodeRepl.createElicitation is unavailable because
the MCP client does not support form elicitation`, check the **active process
chain**, not just `provider.json`. The managed path is Hand → native host →
official Codex app server → CUA provider. The server handles confirmations under
its effective permission policy without starting the desktop GUI. A provider launched directly by an older Hand bypasses that path. The
outer Nanocodex adapter intentionally advertises no elicitation capability;
adding it there does not establish a working permission UI.

## Timeout ownership and cancellation

`timeout_ms` belongs to the upstream provider. Nanocodex forwards it unchanged and
awaits the provider result; it does not subtract queueing or startup time, supply
a default execution timeout, or abandon a call based on that argument. The macOS
app-server bridge likewise delegates tool execution timeouts to the official app
server ([configured MCP tool timeout](official-app-server-bridge.md#transport),
upstream default 300 seconds) while keeping
trusted connection and startup deadlines. This applies to standalone and managed
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

OpenAI's browser selector accepts exact discovered browser IDs and lowercase
family aliases such as `brave`. The display name `Brave Browser` is not an
accepted alias in the pinned provider, despite its browser instructions saying
to pass a browser name. Codex-rs forwards JavaScript unchanged and does not
normalize this string. Nanocodex includes a separate selection note alongside
workdir-only discovery; the provider's tool definitions and call arguments
remain unchanged.

Use a known browser ID from current provider state. For an unambiguous request
for Brave, `cua.createBrowserTab('brave', url, options)` works directly. If
multiple browser instances or profiles could match, use the provider's browser
inventory to select the requested instance before creating a tab. Never guess a
numeric ID or retry a failed creation against another browser automatically.

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

On macOS, setup uses the official architecture-specific desktop DMG URLs from
[upstream's installer](https://github.com/openai/codex/blob/36430b36881cf5c289cb48e671cfc9e8b542ae7b/codex-rs/cli/src/desktop_app/mac.rs).
First use can reuse a compatible installed ChatGPT/Codex app. Explicit refresh
always downloads the current official release. The complete app is copied into
Nanocodex's cache and verified before and after copying against Apple's signature
chain, OpenAI team `2DC432GLL2`, and bundle identity `com.openai.codex`. This retains
the signed Codex host, Node, node_repl, CUA packages, and Sky service together.
The user's existing app is never replaced.

On Windows, setup obtains Store product `9PLM9XGG6VKS` through winget, as identified
by [upstream's Windows installer](https://github.com/openai/codex/blob/36430b36881cf5c289cb48e671cfc9e8b542ae7b/codex-rs/cli/src/desktop_app/windows.rs).
This installs the official ChatGPT/Codex desktop package for the current Windows
user and accepts the standard Store/package installation agreements. Setup checks
its Store signature, package family, and health. Store-owned executables cannot
be launched directly by an unpackaged Hand, so setup copies the complete CUA tree,
native host executables, and notices into a private cache. Every copied file is
compared with its Store source using SHA-256. The matching bundled Node handles
long Windows paths; no extra Node or Python installation is needed. Windows
requires Microsoft App Installer/winget and Store access for initial download.

Windows setup also writes a Nanocodex-owned host script outside the verified
OpenAI resources tree. It starts the packaged `WindowsHelperTransport` and signed
helper through the upstream native-pipe integration, then starts the official
MCP provider with that pipe. `CODEX_CLI_PATH` and the provider sandbox remain in
place. The host forwards authentic turn metadata and the SDK's
`requestComputerUseApproval` messages between the native helper and official
provider. This bridge carries upstream protocol messages; it supplies no consent
UI, permission cache, or approval decision. The Nanocodex MCP client does not
support host elicitation, so an upstream request that requires it fails explicitly.
Timeout, cancellation, disconnect, reset, and turn completion close pending
requests and the native helper. Each receipt retains its own host script so
replacing the selected runtime does not overwrite a running host.


Linux and Linux VM/container guests require an explicitly configured upstream
MCP provider. No custom CUA runtime, background-input plugin, or legacy fallback
is bundled. Automatic `computer setup` currently supports macOS and Windows;
without a provider, guests report CUA unavailable. Remote screen streaming is
a separate feature and does not imply an installed CUA provider.

## Selection and updates

The cache lives under `${NANOCODEX_DIR:-$HOME/.nanocodex}/runtimes/openai-cua`
(`USERPROFILE` is the Windows fallback). Version directories are immutable after
installation. Only a complete verified runtime is selected; a failed download or
copy preserves the previous selection. Old versions remain available to running
processes. Cached corruption produces an actionable error rather than silently
selecting a different backend. Run setup with `--refresh` to repair it.

The native and JS desktop hosts select the managed MCP provider automatically.
An explicit `NANOCODEX_COMPUTER` still wins; `off`, `none`, or `0` disables CUA and
its automatic download. Custom external MCP commands continue to use
`NANOCODEX_COMPUTER_TRANSPORT=mcp`. No versions are spoofed and no provider binaries
are committed to this repository or redistributed in Nanocodex release assets.

The older `scripts/install-upstream-cua.py` remains an explicit development-only
copy helper. Normal installations use the shared native provisioning command.

## Provider permissions and validation

Application policy, OS permissions, and provider-supplied approval flows belong
to the official OpenAI runtime. Nanocodex does not display consent forms, remember
application permissions, or expose an embedding callback that makes approval
decisions. Installing a provider does not grant consent.

The adapters advertise no MCP elicitation capability. On macOS, the official app server handles provider confirmations using its
existing permission policy. The managed bridge declines unresolved interactive
requests for its own thread without showing a prompt or launching the GUI. Direct provider-to-adapter requests use the following MCP behavior. Unsupported provider
requests, including `elicitation/create` and `openai/elicitation/create`, receive
a JSON-RPC method-not-found error (`-32601`), never an approval response. Operations
that require this host capability can therefore fail; discovery or a successful
operation does not establish support for every upstream permission flow. See
[native Hand computer access](native-hand-consent.md).

Validation covers installer invocation/opt-outs, exact command and environment
forwarding, failed refresh recovery, corrupt cache detection, and desktop first
start. A real macOS download and the Windows Store installation were exercised,
and both installed providers returned `js`, `js_add_node_module_dir`, `js_reset`,
and hidden `turn_ended` through MCP. Catalog discovery is not a claim of completed
approval UI or a full screen/input acceptance test.


The Windows native-pipe contract was verified against Store build 26.915.4065.0
and Codex Desktop 9922. A separate protocol probe returned app inventory,
forwarded a Calculator approval form, preserved a deliberate denial, and completed
`turn_ended`. That probe supplies its own decline-only response handler; it does
not represent the Nanocodex adapter, which rejects unsupported host requests.
It establishes transport and denial handling, not human approval UI, screen
capture, or input acceptance. The diagnostic fixture is
`crates/experimental/nanocodex-computer/tests/windows-sky/live-probe.mjs` (place it
beside the host script and run with the verified bundled Node on Windows). Its
responses to every elicitation are declines. Transport fixtures run with
`node --test crates/experimental/nanocodex-computer/tests/windows-sky/host.test.mjs`.

## Linux native host

A configured Linux provider must launch the native Sky service outside the model
sandbox so it can reach the desktop X server. `linux_sky_host.mjs` hosts the
unchanged `@oai/sky/service` in a disposable desktop-user process. Its trusted
proxy uses the upstream NodeREPL `nativePipe` bridge; ordinary model JavaScript
keeps the Codex sandbox and has no nativePipe capability. MCP tool definitions,
descriptions and results still come from the official provider.

For an already installed, compatible upstream Linux runtime, create a separate
host installation from this checkout:

```sh
python3 scripts/install-linux-sky-host.py \
  --runtime /path/to/cua_node \
  --codex-cli /path/to/codex \
  --destination "$HOME/.local/share/nanocodex/sky-host-version"
```

Set `NANOCODEX_COMPUTER` to the printed launcher path and
`NANOCODEX_COMPUTER_TRANSPORT=mcp`. Run the Hand/provider as the desktop user with
its real DISPLAY and session bus. Keep the host modules outside model-writable
workspaces. A system administrator can install the same modules in a protected
system directory and wrap the launcher with the desktop-session environment.
The script does not obtain or authenticate an upstream Linux distribution;
automatic `computer setup` remains limited to macOS and Windows.

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
Use the package's matching `resources/codex` and complete `resources/cua_node`
together, and verify the package signature against the fingerprint published in
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
default). Browser control remains the separate official browser surface.

Transport tests: `node --test crates/experimental/nanocodex-computer/tests/linux-sky/host.test.mjs`.
Live verification used the unmodified Linux service: inventory, a GTK X11 test
window screenshot, exact text plus Enter received by that app, and reconnect after
turn completion. The model process retained NoNewPrivs/Seccomp and had no nativePipe.
