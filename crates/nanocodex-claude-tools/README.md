# nanocodex-claude-tools

Standalone Claude-native, host-authorized capability adapters. No OpenAI API,
OpenAI tool runtime, agent, model transport, OAuth or ambient executor dependency.

Modules: `bash`, `host`, `notebook`, `tasks`, `web`, `workspace_files`.
Primary adapter/capability types are also exported at the crate root. Filesystem
and notebook adapters use native IO; their validators, transforms, search engines,
and schemas also run in WASM through the portable snapshot planner. Neither the
portable engine nor the session task board needs a model runtime or process host.

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

## Async workspace file hosts

`portable_plan::schemas()` returns the canonical Read, Write, Edit, Glob, Grep,
and NotebookEdit definitions. `portable_plan::plan(Request)` consumes an
explicitly authorized snapshot and returns `Plan { output, mutations, reads }`.
The JavaScript WASM exports `claudeFileToolSchemas()` and
`claudeFileToolPlan(requestJson)` use JSON strings with these same contracts.

A request contains `root` (absolute workspace path), `name`, `input`, and `files`.
Each file has a workspace-relative `path`, optional UTF-8 `content`, byte `size`,
and optional `modified` timestamp in milliseconds since the Unix epoch. Missing
content represents an unreadable, binary, or oversized file. `directories`
contains directory paths, including empty search roots; file parents are inferred.
`visits` records the host's traversal count, including skipped entries.

For Grep, first send metadata with `prepare: true`. The engine validates the
regex and options and returns `reads` after applying Rust globset and ripgrep
file-type filters. Gather those files and execute a second request without
`prepare`. Preparation checks the complete filtered scan budget, even when a
later result limit might permit execution to stop early. Glob requires metadata
only and sorts newest first, then by path; missing timestamps sort last.

The host must enforce authorization and symlink isolation before reading, and
bound gathering to 10,000 visited entries, 1 MiB per file, and 128 MiB per search
(counting `min(size, 1 MiB + 1)` for each selected file, even when skipped).
The planner repeats these bounds. Apply no writes until planning succeeds.
Mutations contain `path`, `content`, and `before`; compare Edit/NotebookEdit
before-images immediately before committing through the host's atomic write
facility. Hosts requiring protection against concurrent writers must provide
transactional compare-and-write or serialize those writers.

Portable Read renders text and notebook cells/outputs through the native
formatters. Images and PDFs require a host media capability and fail explicitly
in the snapshot planner. Native `execute_output` retains image blocks and PDF
rendering. Notebook images likewise fail explicitly in the portable text result;
they are never silently discarded.
