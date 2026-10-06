# Claude-native tool implementation matrix

This inventory distinguishes portable library adapters from the native CLI host.
Tool names alone do not establish Claude Code parity. See the [runtime guide](CLAUDE_RUNTIME.md)
for context/recovery and the [managed guide](CLAUDE_MANAGED.md) for that separate surface.

Claude uses native Messages definitions and results. The CLI does not advertise
Responses `exec_command`, `apply_patch`, `web__run`, `exec`, `wait`, or `tool_search`
as Claude-native tools. Host implementations can reuse private process,
transport, task-registry and JavaScript services. The standalone
`nanocodex-claude-tools` crate has no OpenAI dependency.

## Pinned capture inventory

The [OrcaPromptVault comparison](https://github.com/Continuum-AI-Corp/OrcaPromptVault/tree/33ce5a020cfcb5fe747d40d0a89e84743fabdd40/Claude-Code)
is pinned to `33ce5a020cfcb5fe747d40d0a89e84743fabdd40`. Its September captures
contain conditional product catalogs, not one universal tool protocol:

| Capture | Names | Differences from the 35-name interactive set |
| --- | ---: | --- |
| Opus 5 interactive | 35 | None |
| Fable 5.1 interactive | 35 | None |
| Fable 5.1 print / Agent SDK | 29 | Omits `Artifact`, `AskUserQuestion`, `EndConversation`, `EnterPlanMode`, `ExitPlanMode`, `SendFeedback` |
| Opus 4.8 interactive | 33 | Omits `EndConversation`, `SendFeedback` |

The native host implements **26 of these 35 names**, including explicitly
opted-in `Workflow`. Availability depends on the host flags, UI and session role
below. This count measures implemented capabilities with synthetic transport
journeys; it does not establish identical vendor options, proprietary behavior,
live authentication, provider admission or billing.

| Captured group | Complete names |
| --- | --- |
| Files and shell | `Bash`, `Edit`, `Glob`, `Grep`, `NotebookEdit`, `Read`, `Write` |
| Agent lifecycle | `Agent`, `ListAgents`, `SendMessage`, `TaskOutput`, `TaskStop` |
| Interaction | `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode` |
| Context and web | `Skill`, `WebFetch`, `WebSearch` |
| Workspace and scheduling | `EnterWorktree`, `ExitWorktree`, `CronCreate`, `CronDelete`, `CronList`, `ScheduleWakeup`, `Monitor` |
| Opt-in orchestration | `Workflow` |
| Nine conditional product names absent from this host | `Artifact`, `DesignSync`, `EndConversation`, `PowerShell`, `PushNotification`, `RemoteTrigger`, `ReportFindings`, `SendFeedback`, `ShareOnboardingGuide` |

`TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TodoWrite`, `ToolSearch`, MCP
resource/discovery tools, and extensions `ProjectContext`, `ListAgentProfiles`,
`CloseAgent` and `SubmitResult` do not increase that captured-name coverage.
`LSP` and `SubagentHandback` are absent and do not occur in these pinned catalogs.
Anthropic server tools use a separate versioned API. Capture model names do not
establish current provider availability or launch status. Instructions are
independently authored; captured vendor prompts and identity are not bundled.

## Capability boundaries

| Capability | Implementation and limits |
| --- | --- |
| Text files | `ClaudeWorkspaceFiles` implements bounded `Read`, `Write`, `Edit`, `Glob`, and `Grep`. Ambiguous edits fail before mutation; workspace paths and symlinks are checked. These checks are not OS confinement of other tools or protection against every concurrent filesystem race. |
| Search | Glob uses `globset` wildcards/classes/alternation. Grep supports content/files/count, context, pagination, case control and a bounded file-type map. Rust regex semantics and traversal/output limits differ from ripgrep; unsupported options fail explicitly. |
| Prompt images | Ordered text and HTTPS/data/local images become native Messages blocks. Local bytes freeze before execution and survive durable replay. Opaque provider file IDs and audio fail explicitly. WASM supports URLs/data URLs rather than local files. |
| Media and notebooks | `execute_output` preserves supported image blocks, PDF rasters and notebook images. PDF reading needs host `pdfinfo`/`pdftoppm`; missing helpers, encrypted PDFs, invalid ranges and excess data fail explicitly. The CLI installs bounded `NotebookEdit`; text-only APIs cannot represent media. |
| Bash | Native retained jobs support explicit background execution, `TaskOutput`/`TaskStop`, bounded capture and descendant cleanup. Eligible foreground timeouts promote to background; sleep-start commands and disabled-background sessions retain cancellation. Background completion can enter the owner idle queue. Foreground cwd is retained only inside the workspace; background jobs pin it. Jobs/cwd/environment do not restore from SQLite. No Bash PTY parameter or OS sandbox is supplied. The portable adapter requires an injected executor. |
| Task board | Bounded session `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, and `TodoWrite`; shared durability restores content and next-ID watermark with committed receipts. This is distinct from Bash jobs, agents and cron. |
| Agents and messaging | With subagents enabled, native `Agent`, `ListAgents`, `SendMessage`, `TaskOutput`/`TaskStop` use the authorized registry; `CloseAgent` and child-only `SubmitResult` are extensions. General-purpose children start fresh and support family routing. Native `fork` copies the completed pre-batch conversation with independent identities/effects. Named project profiles and `isolation: worktree` use immutable inherited restrictions and owned cleanup. Named teams remain unavailable. See [profiles and forked skills](claude-agent-profiles.md). |
| Permission rules | Explicit `--claude-permissions`/`--permission-mode` supply deny > ask > allow, conservative Bash/path matching, exact interactive approval, final post-hook input checks and persisted rules. Modes include manual/default, acceptEdits, plan, dontAsk and full-access/bypassPermissions. No auto classifier or complete Claude Code permission-mode equivalence; this is admission, not OS isolation. |
| Questions and plan mode | Interactive pending `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`; only explicit approval leaves planning. The persisted guard blocks new model mutations, shell/MCP/agents and unknown capabilities before hooks. Inspection/context/task support passes configured hooks. Trusted hooks and previously admitted work retain their effects. Headless sessions omit interaction tools but retain restored planning guards. |
| Worktrees | Native `EnterWorktree` creates an owned branch/tree from the exact repository root and persists workspace transitions. File, shell, context, hooks/checkpoints and new children resolve it; existing jobs/children keep pins. `ExitWorktree` defaults KEEP. Explicit cleanup rejects dirty/untracked/ignored files, new commits, changed identities and active pins. External paths are not adopted; uncertain Git effects require inspection. Agent isolation creates a separate child tree without moving the parent. |
| Skills and context | `Skill` and `ProjectContext` load bounded project guidance/imports/rules. Host-fixed model/user provenance and `skillOverrides` admission apply. Native `context: fork` starts a clean child with optional profile/model/background; the portable adapter refuses hostless fork execution. No ambient general home/ancestor context discovery, plugin installation, dynamic shell interpolation, skill-defined hooks or frontmatter permission grants. `/loop` has its explicit bounded maintenance-file lookup. |
| MCP and discovery | Caller-owned `ClaudeMcpProvider` preserves exact native schemas, error state, structured data, metadata and supported ordered media. CLI discovery/search/resources/wait use the authorized transport. Catalog refresh occurs at request/discovery boundaries. Only successful discovery receipts admit deferred calls; execution rechecks current availability and exact schema before hooks/remote effects. Unsupported media fails explicitly. |
| Web | `--web-search` installs native `WebSearch` via auxiliary server-search Messages calls and `WebFetch` via bounded public HTTPS capture/summarization. Fetch rejects credentials, proxies, private/reserved addresses, unsupported content and excess redirects; hops are revalidated. No complete domain-approval UX. Auxiliary inference has its own cost/failure boundary. |
| Hooks | Explicit synchronous `--claude-hooks PATH` supports three tool events plus `SessionStart`, `UserPromptSubmit`, `Stop`, `StopFailure`, `PreCompact`, `PostCompact`, `SubagentStart`, `SubagentStop`, `SessionEnd`. Committed replay runs no hooks; unknown lifecycle outcomes are fenced rather than retried. Tool-only policies create no lifecycle effects. No automatic hook discovery, async/prompt/agent hooks, hook approval UI or unlisted events. See [hook configuration](claude-command-hooks.md). |
| Scheduling and `/loop` | Interactive owner TUI installs session-local `CronCreate`, `CronList`, `CronDelete`, `ScheduleWakeup` unless cron is disabled. Persisted numeric cron/time zones, deterministic jitter, seven-day recurring expiry, idle dispatch and owner fencing are supplied. Native `/loop` handles fixed cadence or self-paced iterations, fresh bounded maintenance/Model skill loading, and one permission-gated 20-minute fallback. Reopen skips missed recurring fires and drops elapsed one-shots/dynamic wakeups. Claim-before-dispatch can lose a firing; no daemon or exactly-once guarantee. Children/headless omit scheduling. |
| Monitor | Interactive owner scheduling installs command and WebSocket sources with 200 ms event batching and composer-aware idle delivery. Commands honor Bash/file rules; sockets additionally require `--web-search` and WebFetch domain rules. Public addresses are checked/pinned; exact private origins need repeatable `--claude-monitor-ws-origin`. No ambient environment authorization. Jobs pin workspaces and retain session-owned output/stop status; overflow ends them. No process/socket/event recovery after exit. |
| Workflow | Root-only `--claude-workflows` explicitly enables private JavaScript orchestration through real registry children. Literal metadata validates before execution; helpers support agent/parallel/pipeline/phase. Limits: 15 agent calls, 4 concurrent, 5 minutes, 512 KiB script, 64 KiB result. Runs/children retain workspace pins; resume cannot retarget. Same-session terminal resume reuses confirmed matching results and fences uncertain calls. Read/Edit restrictions conservatively cover script loading/persistence; Agent deny/ask rules cover the whole Workflow. Agent allow rules grant no Workflow authority. No direct filesystem/network/process bridge; runs/cache are process-local. |
| Conditional tools | The nine product names above remain absent. `EndConversation` is permanent product closure in the capture, not ordinary shutdown; `SessionEnd` does not implement it. `LSP` and `SubagentHandback` are also unavailable. |
| Platform tools | Versioned server search/fetch/tool-search/code-execution definitions and native replay require explicit provider opt-in/support. Synthetic protocol tests do not establish live admission or billing. |
| User functions | Caller native handlers retain invocation identity, ordered errors, structured event data and cancellation/replay boundaries. No Codex definitions are implicitly installed. |

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
named teams, Bash PTY/agent-view support, unlisted hook/plugin features, paged
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
