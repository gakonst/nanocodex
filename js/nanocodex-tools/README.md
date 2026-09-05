# nanocodex-tools

Reusable, platform-neutral JavaScript tools for Nanocodex agents: the common
tool router and Code Mode runtime, attachment and hosted-tool protocols,
durable-memory parsing and ranking, artifact and dataset tools, persistent
workspace adaptation, a bounded Just Bash runtime, Git/GitHub compatibility
commands, repository materialization, and workspace-backed SSH composition.
Session-search parsing, retrieval policy, and bounded model-visible projections
are exposed through the dedicated `nanocodex-tools/session` entrypoint.

Hosts own persistence, network policy, credentials, and socket transports and
inject those capabilities through the package's narrow interfaces.

`nanocodex` owns the Rust/WASM agent runtime and composes WASM, workspace, and
MCP adapters around these capabilities. It imports and reexports this package's
JS-only host capabilities; `nanocodex-tools` never imports `nanocodex`.

Cloudflare Workers may supply Durable Object persistence and WebSocket
registries to the hosted-tools core, but Cloudflare bindings, account authority,
Connect grants, and storage schemas remain owned by those Workers.
