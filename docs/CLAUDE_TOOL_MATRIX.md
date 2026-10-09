# Claude-native tool implementation matrix

This inventory distinguishes portable library adapters from the native CLI host.
Tool names alone do not establish Claude Code parity. See the [runtime guide](CLAUDE_RUNTIME.md)
for context/recovery and the [managed guide](CLAUDE_MANAGED.md) for that separate surface.

Claude uses native Messages definitions and results. The CLI shares Nanocodex's
canonical subagent tools and Computer Use provider. The shared QuickJS engine
always exposes native capabilities through `exec` and `wait`.
See the [runtime tool inventory](TOOL_RUNTIMES.md) for both catalogs and startup
flags. The standalone `nanocodex-claude-tools` crate remains independent of the
OpenAI runtime; native CLI adapters own the shared host integration.

The native CLI's [Bash input schema](../bin/nanocodex/src/config/claude/bash.input_schema.json)
retains the [pinned Opus 5 compatibility schema](https://github.com/Continuum-AI-Corp/OrcaPromptVault/blob/33ce5a020cfcb5fe747d40d0a89e84743fabdd40/Claude-Code/claude-code-opus-5-tools.json); host limits and mode-specific timeout
validation belong in execution. The native CLI journey checks the transmitted
schema against that fixture. This host does not claim complete Claude Code
product or tool parity.

## Capability boundaries

| Capability | Implementation and limits |
| --- | --- |
| Text files | `ClaudeWorkspaceFiles` implements bounded `Read`, `Write`, `Edit`, `Glob`, and `Grep`. Ambiguous edits fail before mutation; workspace paths and symlinks are checked. These checks are not OS confinement of other tools or protection against every concurrent filesystem race. |
| Search | Glob uses `globset` wildcards/classes/alternation. Grep supports content/files/count, context, pagination, case control and a bounded file-type map. Rust regex semantics and traversal/output limits differ from ripgrep; unsupported options fail explicitly. |
| Prompt images | Ordered text and data/local images become native Messages blocks, prepared for the model's native resolution. Local bytes freeze before execution and survive durable replay. An image Claude cannot use, such as a remote URL, an OpenAI file ID, an undecodable image or a local file on WASM, is replaced by a note to the model; audio fails explicitly. |
| Media and notebooks | `execute_output` preserves supported image blocks, PDF rasters and notebook images. PDF reading needs host `pdfinfo`/`pdftoppm`; missing helpers, encrypted PDFs, invalid ranges and excess data fail explicitly. The CLI installs bounded `NotebookEdit`; text-only APIs cannot represent media. |
| Bash | Native retained jobs support explicit background execution, `TaskOutput`/`TaskStop`, bounded capture and descendant cleanup. Eligible foreground timeouts promote to background; sleep-start commands and disabled-background sessions retain cancellation. Background completion can enter the owner idle queue. Foreground cwd is retained only inside the workspace; background jobs pin it. Jobs/cwd/environment do not restore from SQLite. No Bash PTY parameter or OS sandbox is supplied. The portable adapter requires an injected executor. |
| Task board | Bounded session `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, and `TodoWrite`; shared durability restores content and next-ID watermark with committed receipts. This is distinct from Bash jobs, agents and cron. |
| Agents and messaging | `spawn_agent`, `send_agent_message`, `list_agents`, `wait_agent`, `interrupt_agent`, `close_agent` and `submit_result` use the same schemas and handlers as Codex. Fresh children, family routing, result contracts, tree authorization and lifecycle operations belong to the shared registry. Native `Agent`/`SendMessage`/`ListAgents`/`CloseAgent`/`SubmitResult` aliases are not installed. |
| Permission rules | Explicit `--claude-permissions`/`--permission-mode` supply deny > ask > allow, conservative Bash/path matching, exact interactive approval, final post-hook input checks and persisted rules. Modes include manual/default, acceptEdits, plan, dontAsk and full-access/bypassPermissions. No auto classifier or complete Claude Code permission-mode equivalence; this is admission, not OS isolation. |
| Questions and plan mode | Interactive pending `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`; only explicit approval leaves planning. The persisted guard blocks new model mutations, shell/MCP/agents and unknown capabilities before hooks. Inspection/context/task support passes configured hooks. Trusted hooks and previously admitted work retain their effects. Headless sessions omit interaction tools but retain restored planning guards. |
| Worktrees | Native `EnterWorktree` creates an owned branch/tree from the exact repository root and persists workspace transitions. File, shell, context, hooks/checkpoints and new children resolve it; existing jobs/children keep pins. `ExitWorktree` defaults KEEP. Explicit cleanup rejects dirty/untracked/ignored files, new commits, changed identities and active pins. External paths are not adopted; uncertain Git effects require inspection. |
| Skills and context | `Skill` and `ProjectContext` load bounded project guidance/imports/rules. Host-fixed model/user provenance and `skillOverrides` admission apply. Native `context: fork` starts a clean child with optional profile/model/background; the portable adapter refuses hostless fork execution. No ambient general home/ancestor context discovery, plugin installation, dynamic shell interpolation, skill-defined hooks or frontmatter permission grants. `/loop` has its explicit bounded maintenance-file lookup. |
| MCP and discovery | Caller-owned `ClaudeMcpProvider` preserves exact native schemas, error state, structured data, metadata and supported ordered media. CLI discovery/search/resources/wait use the authorized transport. Catalog refresh occurs at request/discovery boundaries. Only successful discovery receipts admit deferred calls; execution rechecks current availability and exact schema before hooks/remote effects. Unsupported media fails explicitly. |
| Web | `--web-search` installs native `WebSearch` via auxiliary server-search Messages calls and `WebFetch` via bounded public HTTPS capture/summarization. Fetch rejects credentials, proxies, private/reserved addresses, unsupported content and excess redirects; hops are revalidated. No complete domain-approval UX. Auxiliary inference has its own cost/failure boundary. |
| Hooks | Explicit synchronous `--claude-hooks PATH` supports three tool events plus `SessionStart`, `UserPromptSubmit`, `Stop`, `StopFailure`, `PreCompact`, `PostCompact`, `SubagentStart`, `SubagentStop`, `SessionEnd`. Committed replay runs no hooks; unknown lifecycle outcomes are fenced rather than retried. Tool-only policies create no lifecycle effects. No automatic hook discovery, async/prompt/agent hooks, hook approval UI or unlisted events. See [hook configuration](claude-command-hooks.md). |
| Scheduling and `/loop` | Interactive owner TUI installs session-local `CronCreate`, `CronList`, `CronDelete`, `ScheduleWakeup` unless cron is disabled. Persisted numeric cron/time zones, deterministic jitter, seven-day recurring expiry, idle dispatch and owner fencing are supplied. Native `/loop` handles fixed cadence or self-paced iterations, fresh bounded maintenance/Model skill loading, and one permission-gated 20-minute fallback. Reopen skips missed recurring fires and drops elapsed one-shots/dynamic wakeups. Claim-before-dispatch can lose a firing; no daemon or exactly-once guarantee. Children/headless omit scheduling. |
| Monitor | Interactive owner scheduling installs command and WebSocket sources with 200 ms event batching and composer-aware idle delivery. Commands honor Bash/file rules; sockets additionally require `--web-search` and WebFetch domain rules. Public addresses are checked/pinned; exact private origins need repeatable `--claude-monitor-ws-origin`. No ambient environment authorization. Jobs pin workspaces and retain session-owned output/stop status; overflow ends them. No process/socket/event recovery after exit. |
| Workflow | Root-only `--claude-workflows` explicitly enables private JavaScript orchestration through real registry children. Literal metadata validates before execution; helpers support agent/parallel/pipeline/phase. Limits: 15 agent calls, 4 concurrent, 5 minutes, 512 KiB script, 64 KiB result. Runs/children retain workspace pins; resume cannot retarget. Same-session terminal resume reuses confirmed matching results and fences uncertain calls. Read/Edit restrictions conservatively cover script loading/persistence; spawn_agent deny/ask rules cover the whole Workflow. spawn_agent allow rules grant no Workflow authority. No direct filesystem/network/process bridge; runs/cache are process-local. |
| Conditional tools | Product-specific tools such as `Artifact`, `DesignSync`, `EndConversation`, `PowerShell`, `PushNotification`, `RemoteTrigger`, `ReportFindings`, `SendFeedback`, and `ShareOnboardingGuide` are absent. `SessionEnd` handles ordinary shutdown. `LSP` and `SubagentHandback` are also unavailable. |
| Platform tools | Versioned server search/fetch/tool-search/code-execution definitions and native replay require explicit provider opt-in/support. Synthetic protocol tests do not establish live admission or billing. |
| User functions | Caller native handlers retain invocation identity, ordered errors, structured event data and cancellation/replay boundaries. CLI Code Mode wraps these handlers after permissions and hooks are attached. |

## Resume and rewind

`resume --claude [SESSION_ID]` discovers/reopens default SQLite journals with saved
model/workspace metadata; custom durability stores use explicit state-ID recovery.
Conversation and committed receipts restore; local processes and child registries
are not reconstructed. `rewind SESSION_ID --checkpoint TURN_ID --restore` defaults
to native files. `--mode conversation` creates a new settled journal before the
selected user turn; `--mode files-and-conversation` additionally restores recorded
native file before-images. Preview is explicit; pending sources, unknown history,
conflicts and stale owners fail closed. Source history remains recoverable and
old admitted tool IDs remain fenced. Bash, hooks, MCP and other external effects
are not undone or replayed. File restoration and branch publication are not one
atomic transaction; publication failure reports the file result for inspection.

Remaining application differences are complete permission-mode equivalence,
Bash PTY/agent-view support, unlisted hook/plugin features, paged
Claude transcript storage, and the conditional product/managed operations.
No full Claude Code parity is claimed.

## Evidence and references

Run `scripts/test-claude-native-parity.sh` for actual CLI/public-library journeys
with loopback Messages/SSE/MCP, synthetic authentication and real local effects.
Continuation coverage includes `claude_workflow`, `claude_skills`, `claude_hooks`,
`claude_checkpoints`, `claude_scheduler_monitor`, public `lifecycle_hooks` and
`checkpoint_branch`. Inspect commands, inputs, provider requests, terminal output,
receipts and outcomes under ignored `output/`; schema/compilation alone is not E2E
evidence. The runtime guide gives focused commands. Historical published results
do not validate later source edits. Live subscription observations, managed
admission and these synthetic host journeys are separate acceptance boundaries.

Compatibility references: [tools](https://code.claude.com/docs/en/tools-reference),
[permissions](https://code.claude.com/docs/en/permissions),
[CLI](https://code.claude.com/docs/en/cli-reference),
[checkpointing](https://code.claude.com/docs/en/checkpointing),
[scheduling](https://code.claude.com/docs/en/scheduled-tasks),
[skills](https://code.claude.com/docs/en/skills),
[subagents](https://code.claude.com/docs/en/sub-agents), and
[hooks](https://code.claude.com/docs/en/hooks).
