# Tool catalogs by runtime

These are Nanocodex's own native CLI catalogs, not a claim about the tool
catalog shipped by Anthropic's Claude Code application. Availability depends on
startup flags, configured providers, permissions and the session's role. MCP
server tools are discovered dynamically and cannot be enumerated as a fixed list.

| Capability | Codex CLI | Claude CLI |
| --- | --- | --- |
| Model-facing orchestration | `exec`, `wait` | `exec`, `wait` |
| Files, shell and media | `exec_command`, `write_stdin`, `apply_patch`, `view_image` | `Bash`, `Read`, `Write`, `Edit`, `Glob`, `Grep`, `NotebookEdit` |
| Retained work | Shell session IDs through `write_stdin`; agent IDs through `wait_agent` | `TaskOutput`, `TaskStop` for shell jobs, monitors and workflows; agent IDs through `wait_agent` |
| Planning and task state | `update_plan` | `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TodoWrite` |
| Subagents | `spawn_agent`, `send_agent_message`, `list_agents`, `wait_agent`, `interrupt_agent`, `close_agent`, `submit_result` | The exact same seven tools, schemas and shared handlers |
| Computer and browser use | Discovered `mcp__cua_repl__*` tools | The same discovered `mcp__cua_repl__*` tools and provider |
| Web | `web__run` | `WebSearch`, `WebFetch` |
| Image generation | `image_gen__imagegen` | No native image-generation tool; `Read` handles supported existing media |
| MCP discovery | `tool_search` | `ToolSearch`, `WaitForMcpServers` |
| MCP resources | Through configured server capabilities | `ListMcpResourcesTool`, `ReadMcpResourceTool` |
| Project context | Host-loaded context; file inspection through shell tools | `Skill`, `ProjectContext` |
| Interactive questions and planning | No corresponding native tool in this catalog | `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode` |
| Git workspaces | Shell tools | `EnterWorktree`, `ExitWorktree` |
| Scheduling | Shell or configured providers | `CronCreate`, `CronDelete`, `CronList`, `ScheduleWakeup`, `Monitor` |
| Opt-in workflows | Code Mode and subagents | `Workflow` |
| Hosted memory (`--memory`) | `find_sessions`, `read_session`, `memories__list`, `memories__read`, `memories__search`, `memories__add_ad_hoc_note` | Not supported by the native CLI |

Both harnesses expose only `exec` and `wait` to the model. All other names in
the table are capabilities called through `tools` inside Code Mode, including
MCP discovery. Workspace tools, web, image generation, subagents and memory can
be disabled or depend on explicit configuration. Computer/browser integrations
contribute their discovered provider tools.

Claude's nested catalog installs file tools, Bash, retained-task access, task
state, project context and worktree tools. MCP requires configured servers; web
requires web access; agent tools require subagents. `submit_result` is for child
results. Interactive questions and plan transitions require an interactive UI.
Scheduling and monitors belong to the interactive owner; `Workflow` requires
`--claude-workflows` and subagents. See the [Claude capability matrix](CLAUDE_TOOL_MATRIX.md)
for exact behavior and limitations. Names in the same row are related
capabilities, not interchangeable schemas or equivalent guarantees.

Claude permission files and saved policies naming removed agent tools must be
migrated to the canonical names. They fail explicitly instead of silently losing
their restrictions. `spawn_agent(role)` selectors match the shared tool's role.

Sibling messages and threaded replies work across Claude and Codex children in
the same task tree. `send_agent_message` uses the shared agent IDs; replies use
the received message ID as `in_reply_to`. Harness choice does not change tree
authorization or message routing.

## Claude Code Mode

Start the native CLI with `ncl --claude` (or pass `--claude` to
`ncl run`). Claude receives only the `exec` and `wait` model
tools. Send `exec` a JSON object with a `code` string; inside it use the same
JavaScript helpers and QuickJS runtime as Codex:

```javascript
const results = await Promise.all([
  tools.Read({ file_path: "README.md" }),
  tools.Glob({ pattern: "src/**/*.rs" }),
]);
results.forEach(text);
```

Native callbacks retain their permissions and hooks. Shared subagent tools
return the same JSON objects as Codex. Other native tool results use
`content`, `isError` and `structuredContent`; images can be forwarded with
`image(result.content[i])`. Shared subagent tools keep their canonical names,
and CUA remains available through `tools.mcp__cua_repl__js(...)` and the rest of
the discovered provider catalog. The CUA JavaScript realm is separate from the
outer Code Mode cell. Invoke CUA with the provider's documented arguments.

Cells and `store` values are process-local. A restart preserves committed
conversation/tool receipts and durable task state, but cannot resume an old
JavaScript cell. Reconcile any uncertain external effects before starting new
work. Tool schemas are fixed for an admitted cell: after discovery changes a
tool's schema, start a new cell to use the refreshed definition.

CUA discovery uses the same `NANOCODEX_COMPUTER` configuration on both runtimes.
`off` disables it; `--workspace-tools false` skips local CUA discovery. The
provider determines the exact names and schemas (commonly `js`, `js_reset` and
module-directory helpers under `mcp__cua_repl__`). Installation and OS permission
grants remain host operations. Hidden provider lifecycle hooks are not tools
available to the model.

## JavaScript and managed hosts

The JavaScript SDK accepts caller-supplied nested catalogs, so it has no universal
application tool list. Roots and children use the shared Code Mode runtime
regardless of harness. Hosts supply an isolated evaluator; native tool handlers
remain behind `exec` and `wait`.

The managed Claude adapter supplies `Bash`, `Read`, `Write`, `Edit`, optionally
`BashOutput`, plus authorized application tools. Configured MCP adds
`MCPToolSearch` and `MCPExecute`; connector/Hand providers add `ToolSearch` and
`ToolExecute`. Shared subagent operations are installed by the task-tree runtime.
This catalog differs from the native CLI: for example, managed file tools work
under `/brain`, and `BashOutput` uses retained Hand shell sessions.

## Sources

- [Codex built-in selection](../crates/nanocodex-oai-tools/src/runtime/selection.rs)
  and [standard schemas](../crates/nanocodex-oai-tools/src/standard.rs)
- [Subagent tools](../crates/nanocodex-subagents/src/tools.rs)
- [Claude CLI assembly](../bin/nanocodex/src/config/claude.rs)
- [Claude MCP tools](../bin/nanocodex/src/config/claude/mcp.rs)
- [Claude JS host](../js/nanocodex/runtime/claude-host.mjs)
- [Managed Claude adapter](../js/managed/src/claude-tools.ts)

## Computer activity presentation

CUA calls are presented as compact computer activity with action titles,
screenshot indicators, and short failure diagnostics. Expand an activity to
inspect its calls and provider output. Code Mode wrappers remain orchestration;
their independently emitted output stays available in the transcript.

This presentation does not rewrite CUA observations. The upstream provider
chooses full accessibility trees or diffs; request `{ disableDiffing: true }`
when a complete current tree is needed. Both Claude and Codex use this same
provider behavior.
