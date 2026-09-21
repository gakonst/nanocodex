# Installed upstream CUA runtime

Native Nanocodex, Nanocodex2, local Hands, and the JavaScript desktop runtime
provision OpenAI's CUA provider automatically on macOS and Windows. They expose
its actual MCP catalog, including descriptions, schemas, metadata, and visibility.
Production Code Mode remains QuickJS; the provider uses its own bundled Node.

Browser-enabled launchers set upstream `BROWSER_USE_TINYSKY_ENABLED=1`, matching
the official desktop host. This exposes `Tab.ax`, which upstream `cua.getTab()`
and `cua.createBrowserTab()` use to return accessibility state. Existing managed
copies need `computer setup --refresh` after upgrading to regenerate the launcher.

```sh
nanocodex2 computer setup           # provision once or verify/reuse the cache
nanocodex2 computer setup --refresh # check/download the current upstream release
# Both commands are also available as nanocodex computer setup.
```

The native installers refresh the upstream runtime. Direct binary/source installs
provision it on first CUA use. Older published CLIs that lack this command keep
working with the installer, but require an updated Nanocodex release before this
behavior becomes available. Installation does not launch the ChatGPT GUI or sign
in to a ChatGPT account. OS permissions and provider access policies still apply.

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

The adapters advertise no MCP elicitation capability. Unsupported provider
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
