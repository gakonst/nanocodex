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

The bundled companion pins the installed CUA 0.2.5 declarations, documentation,
and source provenance. Factory comparisons and fixture tests cover defined API
behavior; they do not prove complete native-provider equivalence. The external
provider path instead executes the installed provider's own unmodified runtime;
see [the current release and live verification](upstream-provider.md).

Native CLI and desktop registration now perform MCP initialize and paginated
`tools/list` before advertising model-visible CUA tools. Provider descriptions are retained
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
video, viewer and internal input services remain intact. Both adapters discover the full provider catalog, including
`js_add_node_module_dir`. `_meta.ui.visibility` controls model exposure; the
hidden `turn_ended` lifecycle hook remains available only to trusted host code.

Both adapters support form elicitation through an optional host callback,
including Codex's `openai/elicitation/create` alias. They advertise that
capability only when a handler exists, preserve request and response metadata,
and cancel pending forms on timeout, cancellation, call completion, or closure.
The library callback does not automatically add a desktop or remote Hand
approval UI, and the adapters never manufacture persistent consent.
