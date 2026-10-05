# nanocodex-claude-tools

Standalone Claude-native, host-authorized capability adapters. No OpenAI API,
OpenAI tool runtime, agent, model transport, OAuth or ambient executor dependency.

For a local coding agent, the `nanocodex` facade's `Backend::claude(api_key)`
assembles the supported tools automatically. This crate exposes the individual
adapters for custom hosts; callers of the facade do not need to register them.

Modules: `bash`, `host`, `notebook`, `tasks`, `web`, `workspace_files`.
Primary adapter/capability types are also exported at the crate root. Filesystem
and notebook execution are native-only; portable contracts and the session task
board do not require the OpenAI runtime or a native process implementation.

Embeddings explicitly implement `ClaudeHost`, `SandboxBashExecutor`, approved
web capabilities and/or `ClaudeMcpProvider`. Host/MCP outputs are native
`ToolOutput` values (text/images, `is_error`, structured results and metadata).
`HostContext` carries model/session/turn/call identities and the output budget,
not Responses history or serialized `input_*` items. Hosts own permissions,
bounded capture, cancellation, durability, actual lifecycle and reconciliation.
The MCP contract intentionally requires a caller implementation rather than a
`DynamicToolProvider` alias; query its current definitions at request boundaries.

Enable `nanocodex-claude`'s `tools` feature for the native builder adapters;
`workspace-files` is a compatibility alias. The tools crate itself has no
runtime features to install implicitly. `mcp` is a no-op compatibility switch;
the dependency-light, caller-owned contract is always available.

See [runtime migration and limits](../../docs/CLAUDE_RUNTIME.md) and
[tool matrix](../../docs/CLAUDE_TOOL_MATRIX.md). The split does not establish
production wiring, lifecycle equivalence or full Claude Code parity.

Synthetic tests only; no live provider or credentials are needed:

```sh
cargo test --locked -p nanocodex-claude-tools --all-features
cargo test --locked -p nanocodex-claude --no-default-features --features tools
```

The Claude backend integration suite includes actual loopback Messages/SSE
continuations for host errors/media and reopened task checkpoints. Its caller
MCP journey exercises this native contract over loopback JSON-RPC HTTP; it does
not claim automatic dynamic-catalog builder wiring or production MCP services.
