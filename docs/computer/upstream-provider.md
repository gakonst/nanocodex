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

`--source-app /Applications/Codex.app` also works for older runtime bundles. A
bundle without `@oai/cua-repl` exposes its original Node REPL with the installed
Sky package rather than inventing a CUA facade that bundle does not contain.

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

The ChatGPT bundle used CUA 0.2.4, Sky 0.6.32 and Node 24.20.0. All 4,273 regular
files and 11 symlinks matched the original. Native Rust discovery and the actual
JavaScript attachment both connected successfully. The JavaScript attachment
executed `await cua.getState()` through the unmodified provider. Its native
inventory returned error -10005: `codex app-server exited before returning a
response`. A successful outer MCP result was not treated as a successful native
observation.

The installed Codex bundle used Sky 0.6.6 and Node 24.14.0, without the newer CUA
facade package. All 3,612 regular files and seven symlinks matched its original.
Its real Sky `list_apps()` call failed with a native-pipe startup timeout.

These results establish the external-provider route, not working screenshot or
input control. The original provider still depends on its native service,
app-server compatibility and existing OS grants. The experiment did not replace
the active default provider or alter installed OpenAI applications, account
credentials, or permissions. The app-server versions differed between the two
installations; that is a diagnostic observation, not a proven cause of failure.
