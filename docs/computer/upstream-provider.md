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

## Host approval integration

The Rust and JavaScript libraries accept an embedding-owned form callback for
`elicitation/create` and `openai/elicitation/create`. Only a configured callback
advertises `elicitation.form`. It receives the original provider parameters and
metadata; the host must obtain the user's choice and dismiss its UI on cancellation.
The adapter never synthesizes acceptance or persistence. Without a callback,
server approval requests receive an MCP error.

The desktop app and remote Hand do not yet wire this callback to approval UI.
Discovery and noninteractive provider operations can work, but operations requiring
forms remain unavailable there. OS permissions and provider app policies still
apply. See the [JavaScript adapter](../../js/nanocodex-computer/README.md) and
[Rust adapter](../../crates/experimental/nanocodex-computer/README.md) for the callback API.

## Validation

The adapter tests use synthetic stdio providers to check arbitrary catalogs,
visibility, exact schemas, argument forwarding, and form cancellation. The bundled
companion tests continue to use this checkout's existing catalog. Optional installed
provider discovery uses only `initialize` and `tools/list`:

```sh
NANOCODEX_TEST_EXTERNAL_COMPUTER=/absolute/path/upstream-cua/cua-provider \
  cargo test --locked -p nanocodex-computer \
  installed_external_provider_discovery -- --ignored
```

This inventory check does not validate screenshots, input, host approval UI, or
deployment. Provider binaries and their local copy manifest are not tracked.
