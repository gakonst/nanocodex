# CUA provider contract

Upstream audited: `openai/codex` revision
`36430b36881cf5c289cb48e671cfc9e8b542ae7b`.

The upstream CUA surface is an MCP provider, not a built-in `computer` tool:

- `codex-rs/core/src/tools/handlers/mcp.rs` builds the tool specification from
  `ToolInfo` through `mcp_tool_to_responses_api_tool`.
- `codex-rs/tools/src/mcp_tool.rs` takes the description and input schema from
  the MCP tool declaration.
- `codex-rs/protocol/src/mcp.rs` recognizes `cua_repl` as a REPL-backed MCP server.
- `codex-rs/app-server/tests/suite/v2/guardian_v2.rs` exercises CUA `js`,
  `js_reset`, and `js_add_node_module_dir` as provider tools.

This checkout's bundled companion owns its API documentation and schemas. Its
fixture tests prove that our Rust and JavaScript adapters preserve its contract;
they do not prove that it implements every API of a separately installed OpenAI
CUA provider.

Native CLI and desktop registration now perform MCP initialize and paginated
`tools/list` before advertising the CUA pair. Provider descriptions are retained
verbatim. Session processes rediscover the catalog before dispatch and reject
catalog changes. External commands use the explicit MCP transport mode without
adding bundled-companion flags. Unsupported schemas fail with a generic MCP
registration instruction instead of being silently rewritten.

The managed namespace has no `computer` tool. Internal screen publishers are
also excluded from public discovery and resolution, so `screen_*` aliases cannot
expose the removed custom control contract. Its CUA entries are routing
wrappers with the supported argument schemas, not copies of a universal provider
API description. `select_computer` must run first: it returns the chosen
provider's exact descriptions and schemas, pins its admitted connection, and
rejects missing or incompatible declarations. The hosted broker and account
relay retain these original declarations before adding any public route aliases.

A screen publisher alone does not implement CUA. Screen-only Mac, Windows,
Linux and phone publications remain unsupported by this path. Their hardware,
video, viewer and internal input services remain intact. Generic MCP registration
is required for a provider with other schemas or additional tools, including
`js_add_node_module_dir`; this two-tool attachment does not advertise those.
