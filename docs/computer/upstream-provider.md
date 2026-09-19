# Installed upstream CUA runtime

Native Nanocodex, Nanocodex2, local Hands, and the JavaScript desktop runtime
provision OpenAI's CUA provider automatically on macOS and Windows. They expose
its actual MCP catalog, including descriptions, schemas, metadata, and visibility.
Production Code Mode remains QuickJS; the provider uses its own bundled Node.

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

Linux and Linux VM/container guests retain the existing Linux computer backend.
This implementation has no verified official Linux Sky distribution; it does not
try to run a macOS or Windows binary there. `computer setup` reports unsupported
on other platforms.

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

## Approval integration and validation

The adapters preserve upstream `elicitation/create` and
`openai/elicitation/create` forms. Desktop and remote-Hand form approval UI is
still separate work: installing a provider does not grant consent, and operations
requiring an unwired form remain unavailable. See the native and JavaScript
adapter READMEs for the embedding callback API.

Validation covers installer invocation/opt-outs, exact command and environment
forwarding, failed refresh recovery, corrupt cache detection, and desktop first
start. A real macOS download and the Windows Store installation were exercised,
and both installed providers returned `js`, `js_add_node_module_dir`, `js_reset`,
and hidden `turn_ended` through MCP. Catalog discovery is not a claim of completed
approval UI or a full screen/input acceptance test.
