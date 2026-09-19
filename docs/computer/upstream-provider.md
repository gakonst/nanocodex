# Use an unmodified installed CUA provider

Nanocodex's native and JavaScript attachments can connect to an external CUA MCP
command. They discover the provider's own tools, descriptions, schemas, metadata,
and output schemas. Tools hidden by `_meta.ui.visibility` stay out of the model's
catalog. No bundled-companion arguments are added to an external command.

This mode delegates CUA to the provider's runtime and Sky implementation. Nanocodex
Code Mode continues to use QuickJS. A provider can use its own Node runtime
without changing the Code Mode engine.

## Prepare a local copy

```sh
python3 scripts/install-upstream-cua.py \
  --source-app /Applications/ChatGPT.app \
  --destination "$HOME/Nanocodex/upstream-cua"
```

The installer copies the app's `cua_node` tree without editing it, verifies all
file hashes and symlink targets, verifies the signed Sky app on macOS, and writes
a launcher plus a copy manifest. Existing copies with different bytes are
rejected. Binaries remain local; they are not vendored or redistributed by this
repository. The launcher explicitly configures module and trusted-code paths to
that copied package tree. It does not change OS grants or approval behavior.

Legacy Sky-only bundles are rejected. Use the current unified ChatGPT desktop
app, which also contains Codex. The copy manifest records the runtime manifest
and CUA, CUA REPL, and Sky package versions. Keep these packages together; do not
spoof a newer native protocol version or mix a client with an older Sky service.

Use the launcher explicitly:

```sh
NANOCODEX_COMPUTER="$HOME/Nanocodex/upstream-cua/cua-provider" \
NANOCODEX_COMPUTER_TRANSPORT=mcp nanocodex2
```

Or attach it from JavaScript:

```js
const computer = await connectComputerTools({
  executable: "/absolute/path/upstream-cua/cua-provider",
  transport: "mcp",
});
```

Use the discovered declarations rather than assuming the provider has a fixed
pair of tools. In the inspected newer provider, `js`, `js_add_node_module_dir`,
and `js_reset` are model-visible; `turn_ended` is a hidden host lifecycle hook.

## September 19, 2026 probe

The current official desktop release was verified from OpenAI's production
appcast: ChatGPT 26.915.31945, build 9922. Its bundle includes CUA 0.2.5,
Sky 0.7.1, Node 24.21.0, and signed Codex CLI 0.155.0-alpha.9.2. The copied CUA
runtime's 2,448 file and symlink entries matched the installed original; macOS
code signature verification passed.

The actual Nanocodex JavaScript attachment executed `await cua.getState()`
through this unmodified provider with the ChatGPT GUI closed. Native inventory
returned 41 apps and no inventory errors. The obsolete standalone Codex app was
archived and its old application path now resolves to the current ChatGPT app,
so native helper discovery cannot select that stale installation.

The provider exposes `js`, `js_add_node_module_dir`, and `js_reset` to the model;
`turn_ended` is a hidden host lifecycle hook. Screenshot and input operations
also require the provider's MCP form elicitation channel. An embedding host must
supply a real authorization decision; a successful inventory call alone does
not establish that these operations work. OS permissions and provider app
policies remain enforced. This local probe does not mean the parity branch has
been deployed to every Nanocodex host.
