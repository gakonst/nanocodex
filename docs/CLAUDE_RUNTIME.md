# Claude-native runtime

`nanocodex-claude` implements a separate Messages-based backend behind the common `nanocodex-agent` lifecycle. Tool registration is explicit; it never imports the OpenAI tool catalog or Claude Code credentials. Embeddings supply authentication and host-authorized capabilities.

## Native CLI instructions and project context

The shipped CLI (`ncl --claude` or `--harness claude`) composes original
coding instructions for its installed tools. The model calls `exec` and `wait`;
inside Code Mode, file tools retain their native names while shell and agent
tools use the shared Codex names, schemas and handlers. Optional capabilities must follow the actual catalog.
Instructions do not make unavailable browser, account, planning or worktree
capabilities available. See [the capability matrix](CLAUDE_TOOL_MATRIX.md).

Without `--instructions`, the CLI loads root project context through
`ClaudeProjectContext`: `AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md`, and
`.claude/CLAUDE.md`, local imports and applicable `.claude/rules` files.
Excerpts carry provenance and truncation/diagnostic information, and are
JSON-encoded as project reference data. The loader limits each source to 32 KiB,
total source text to 128 KiB, and imported files to 64, with depth and discovery
bounds. Symlinks and imports escaping the workspace are rejected; relative
parent imports are permitted when they remain inside that boundary. This does not
replace OS isolation against a concurrent hostile process.

The library's `load_for_path` supports root-to-leaf context and path-scoped
rules inside an explicitly authorized workspace. The CLI installs the
`ProjectContext` extension for explicit lazy loading. Workspace file operations
also attach current applicable context to their results. Search
loads guidance for its requested path, not every discovered descendant. Context
is guidance returned with a result, not an authorization gate before mutation;
read the relevant file/context before making an edit.
Home configuration and ancestors outside the workspace are not implicitly read.

`ClaudeSkills` discovers `.agents/skills/*/SKILL.md` and
`.claude/skills/*/SKILL.md` with bounded scanning, metadata validation and explicit
diagnostics. Claude receives catalog metadata, then `Skill` loads the selected
body and substitutes arguments as text. The host fixes invocation provenance:
model-disabled skills are omitted from model discovery and cannot be invoked by
a model supplying different JSON. `allowed-tools` is metadata, not a permission
grant. Native skills can request `context: fork`, a named agent, a Claude model
and background execution; they start a real clean child through inherited
profile/permission checks. They do not copy the caller's transcript. Portable
`ClaudeSkills::execute` refuses fork execution without an embedding's child
executor. Dynamic shell interpolation and skill-defined hooks remain unsupported.
Bounded `.claude/settings.json` and `.claude/settings.local.json` `skillOverrides`
control visibility/invocation, with local precedence; an override cannot lift
frontmatter restrictions or turn Model provenance into User provenance.
See [profiles and forked skills](claude-agent-profiles.md).

`--instructions TEXT` replaces the CLI startup instructions, including startup
project excerpts, skill catalog and optional delegation guidance. It does not
disable installed `Skill`/`ProjectContext` tools or the context attached to later
workspace file results.
Read-deny/ask policies disable automatic project/rule/skill loading and attached
file-result context; aggregate Skill and ProjectContext calls are denied because
their imports can touch paths beyond the requested file. Embeddings can select
`ClaudeWorkspaceFiles::execute_output_with_context(name, input, false)` to skip
context reads while retaining the requested file operation. Its existing
`execute_output` API includes context by default.
The replacement propagates to children. Otherwise, cross-family children resolve
their own family's defaults and native tool guidance; Codex children keep the
Codex builder's standard instructions. Capabilities remain controlled by tool
configuration, not by prompt text. Snapshot/durable restoration retains its
provider-native instruction state. Library embeddings still supply their own
instructions; this discovery is CLI-only.

The behavior is intentionally narrower than Claude Code's documented
[memory hierarchy](https://code.claude.com/docs/en/memory) and
[skills system](https://code.claude.com/docs/en/skills). No full Claude Code
prompt, context-management or application parity is claimed.

Reproduce the CLI context journeys with
`cargo test --locked -p nanocodex-bin --test harness_routing native_cli_ -- --nocapture`.
They run the actual executable against loopback Messages/Responses providers,
inspect transmitted project data and exact caller overrides, read a lazy skill
through native tools, verify a file-write effect, exercise mixed-family children,
and reject symlink/special-file context. Request transcripts, CLI JSONL and
commands are retained under ignored `output/harness-routing/`. These synthetic
providers establish runtime boundaries, not live-model instruction-following.

The modular design follows the capability, context and verification distinctions
in the [OrcaRouter discussion](https://x.com/OrcaRouter/status/2105364729422344549)
and its [versioned prompt archive](https://github.com/Continuum-AI-Corp/OrcaPromptVault/tree/33ce5a020cfcb5fe747d40d0a89e84743fabdd40/Claude-Code).
The instructions here are independently authored for Nanocodex's actual tools;
no captured vendor prompt, product identity or environment is bundled. Captured
prompt differences are research context, not evidence of improved model performance.

## Prompt images and documents

The shared `Prompt` API preserves ordered text and image inputs as native
Messages blocks. Base64 data URLs are supported. Native `LocalImage` inputs are
read with a bounded regular-file check and frozen into bytes before execution.
Local-image receipts survive durable reopen and do not reread a changed or
deleted file after commit. WASM callers use data URLs. Local filesystem images
require a native host. Image detail hints are not sent as a Claude field. Inline
and local images are prepared before acceptance like tool-result images. They
are decoded, converted to PNG unless they are PNG, JPEG, or WebP, labelled by
their bytes, and reduced to the model's native resolution, the size the Messages
API would otherwise reduce them to. That is a 2576 px long edge and 4,784 visual
tokens of 28×28 px on Claude 4.7 and later models, and 1568 px and 1,568 tokens
on other models. A prepared image must fit 5 MiB, so a larger encoding is shrunk
further. Originals of up to 64 MiB are read. An image Claude cannot use is
replaced in place by a short note that tells the model why, and the rest of the
prompt is sent. Such images include an opaque OpenAI file ID, an unreadable or
undecodable image, one that cannot fit 5 MiB, a remote image URL, and a local
image on WASM. Audio prompts fail explicitly before HTTP. Limits are 100 content
items, 20 images, and 20 MiB of combined prepared media per prompt. Durable
steers are journaled in this prepared form. A resumed durable operation prepares
its prompt and steers for the model its continuation names, even when the agent
reopens with another model. An operation that has no continuation yet runs on
the reopened agent's model. Inline `UserInput::File` documents become native
`document` blocks: base64 `application/pdf` (requiring `%PDF-` magic bytes) or
UTF-8 `text/plain`. Each document is bounded to 10 MiB, with at most five
documents; optional filenames are validated and used as titles. The managed Rust
HTTP client preserves these inline files for Claude prompts and steering, while
retaining the explicit GPT document-input rejection.

Reproduce the public API and SQLite media journeys with
`cargo test -p nanocodex-durability --features claude,sqlite --test claude_prompt_media -- --nocapture`.
They exercise native request ordering, notes for unusable images, audio and
limit rejection, queued image freezing and replay after local file changes, and
steers of a turn resumed by an agent reopened with another model. These are
transport/storage checks, not a measurement of model vision quality.

Base64 images in client tool results are decoded with bounded memory and
prepared the same way, for the native resolution of the model the turn's
requests name and within 5 MiB, before the result joins request history. Native
sizes stay within the 3000 px long edge the direct Messages API enforces once a
request carries more than twenty images, and preparing each image when it first
arrives keeps earlier request bytes stable as the conversation grows. An image
that cannot be decoded or reduced becomes a text omission inside the same
result, which keeps its success status. Host tool images are validated first, so
an invalid host image, or one over 5 MiB, fails the call. A URL source becomes
the same note as a remote prompt image, and file sources pass through unchanged.
Durable tool receipts keep the handler's original output, and a replayed receipt
is prepared the same way. The low-level `ClaudeClient` sends caller-supplied
images as given. Reproduce these journeys with
`cargo test -p nanocodex-claude --test agent_loop tool_images` and
`cargo test -p nanocodex-durability --features claude,sqlite --test claude image_receipt`.

## Native harness composition

The facade's `Harness::builder().register(family, recipe).build()` is a reusable
host router. A recipe accepts `HarnessRequest` and constructs a concrete native
builder, returning `(Nanocodex, AgentEvents)`. Concrete service and builder types
remain generic until that lifecycle boundary; Messages transcripts and tool
definitions stay native. See the [complete reusable example](../crates/nanocodex/README.md#reusable-native-harnesses).

`HarnessModel::Codex(Model)` and `HarnessModel::Claude(ClaudeModel)` identify
family-scoped choices. `Harness::start(model)` uses the selected model's effort
default; `start_with(SpawnOptions)` supports an explicit family, model and effort.
Child overrides resolve against the live parent. Omitted overrides inherit its
current settings; an explicitly different family uses its own defaults. Family
and model mismatches and unsupported effort fail before recipe invocation.
Registering a family authorizes construction through that recipe; it does not
discover credentials or grant host tools.

Every concrete recipe installs `request.spawn_factory` and uses its own
`.tools_factory(...)`. Codex's factory returns `Tools`; Claude's returns
`ClaudeTools`, whose callbacks receive native `ClaudeToolInvocation` identities
and private host context. A weak `AgentHandle` belongs to its invoking runtime,
so tools can share one `nanocodex-subagents::Registry` across both families while
retaining the correct parent and session. The host supplies any native callback
bridge to those authorized lifecycle capabilities. Mixed-family children start
clean conversations. `fork` remains native to the owning backend and does not
translate history into another family.

One implementation of the shared nanocodex `Tool` contract serves both
families: install it in Codex's `Tools` and pass the same value to
`ClaudeTools::shared_tool`. Claude receives a function definition with the
tool's output schema appended to its description, and the tool receives the
invocation's identities and host context but no Responses history.

The registry can unload idle children at its residency limit. Rehydration sends
the family's in-memory `ChildSnapshot` to the current construction recipe;
the recipe reattaches authentication, host context and freshly authorized tools,
restores native state and preserves session identity, model and effort. Weak
owner handles and the routed factory refuse spawning and restoration after
owner shutdown. These snapshots are ephemeral residency state; process-restart
recovery requires the durability attachment below.

The [public library acceptance journey](../crates/nanocodex/tests/it/harness.rs)
uses actual localhost Responses HTTP and Messages SSE transports with synthetic
provider output and authentication. It runs real Code Mode calls to the shared
registry, forces idle Claude eviction at `set_max_resident(1)`, resumes the child
with its native conversation and identity, verifies live parent defaults, and
checks that stopped-owner routing reaches neither recipe nor provider. Reproduce
with `cargo test -p nanocodex --all-features --test it harness:: -- --nocapture`;
inspect the request transcript under ignored `output/library-harness/`.
This acceptance boundary does not establish live provider admission, every
native tool, cross-family fork, or full Claude Code parity.

## Turn controls and response events

Fast mode is captured when a turn is accepted. `set_fast_mode` affects later
accepted turns; queued work keeps its admitted setting. Supported models receive
the native speed field and matching beta header. Unsupported models omit both.
This controls request encoding, not a promise of live provider eligibility or
latency.

`ModelCallCompleted` publishes response usage before client tools finish,
including cache-read/write details. Compaction calls are excluded from
active-response events; their usage still participates in the turn totals
described below. Each admitted steer is reported by an `InputAccepted` event of
kind `steer`. Like the prompt's event, it carries the prepared input the model
receives, with a note in place of each unusable image. Steering is acknowledged
with `RunSteered` when the runtime consumes the queued instruction at a tool or
terminal response boundary, rather than when it is submitted. Multiple
instructions retain their order as separate native messages. Cancelling a queued
ephemeral turn retires that turn without cancelling the currently active model
call or tool effect.

The public Messages/SSE journeys in
[`agent_loop.rs`](../crates/nanocodex-claude/tests/agent_loop.rs) exercise these
controls with held provider/tool boundaries. They verify runtime timing and
request content with synthetic providers; they do not measure live-model
compliance with steering.

## Tool crate migration

The former `nanocodex-tools` monolith is split by provider. Claude integrations
must depend on `nanocodex-claude-tools`, not `nanocodex-oai-tools` (the renamed
OpenAI runtime). Enable `nanocodex-claude`'s `tools` feature; `workspace-files`
remains an alias for existing callers. Imports now use the clean modules
`bash`, `context`, `host`, `media`, `notebook`, `skills`, `tasks`, `web`, and `workspace_files`, with primary
adapter and capability types also exported at the crate root. For example,
`nanocodex_tools::claude_host::ClaudeHost` becomes
`nanocodex_claude_tools::host::ClaudeHost` and
`nanocodex_tools::ClaudeWorkspaceFiles` becomes
`nanocodex_claude_tools::ClaudeWorkspaceFiles`.

The standalone tools crate has no OpenAI API/tools or agent dependency.
The `nanocodex-claude` runtime crate is the one intentional Claude-side consumer
of `nanocodex-oai-tools`: it enables only that crate's `image-processing`
feature (no Code Mode, MCP or tool runtime) for the shared bounded image
decoder. `scripts/check-crate-boundaries.sh` snapshots this edge and still
fails if `nanocodex-claude-tools` reaches OpenAI or agent crates transitively.
`HostContext` carries the actual model, session, turn, call and output budget,
without a Responses history. Host outputs use native text/image blocks,
`is_error`, and optional structured data/metadata; `ClaudeBuilder::host_tools`
encodes these directly as Claude results. Hosts must migrate their old shared
`ToolContext`/Responses content DTOs instead of passing `input_*` wire items.
Unsupported media produces an explicit error, never a silently truncated block.
Portable capability contracts, tasks, Bash and web adapters are available on
WASM; filesystem/notebook execution and the builder's native adapters remain
native-target-only.

`ClaudeMcp` intentionally requires a caller implementation of
`ClaudeMcpProvider`, returning `McpToolDefinition` schemas and native
`ToolOutput` results. The old OpenAI `DynamicToolProvider` bridge is not retained
as an alias or wrapper. The embedding may reuse its own authorized MCP service,
but it owns connections, OAuth, discovery, validation, capture limits and
lifecycle. Read `definitions()` at each request boundary; schema changes and
removals are live, duplicate/non-MCP/malformed entries fail closed, and racing
removals or provider errors remain failures. Result blocks, structured data and
metadata survive adapter dispatch. `ClaudeTools::dynamic_tools` supplies per-request catalog refresh for embeddings.
The shared MCP transport separately exposes native discovery, calls, resources
and startup status through `McpHandle`; it preserves the configured exposure and
authentication boundary. The CLI retains that service and attaches native MCP tools plus resource,
search and wait handlers. `ToolSearch` refreshes remote discovery and returns native `tool_reference`
receipts; configured MCP definitions retain their deferred flags. Undiscovered
calls are rejected. Each model continuation admits its refreshed catalog;
execution also requires a currently available handler and transport entry with
the exact admitted definition. A same-name schema replacement is refused before
hooks or remote execution; a later request may admit the new schema.
Remote changes are observed at explicit discovery, not continuously polled. Transport capabilities and synthetic protocol
journeys do not establish every CLI integration, OAuth flow or MCP feature.

## Native host operations

The CLI installs a task board, the shared `exec_command`/`write_stdin` process
runtime and, when enabled, a shared child-agent registry. Shell calls return the
same structured results as Codex directly inside Code Mode. `exec_command`
accepts `cmd`, optional `workdir`, shell/login settings, `tty`, `yield_time_ms`
and output limits according to the shared schema. An omitted `workdir` uses the
current session workspace. Each invocation selects its own working directory;
a command's `cd` or environment changes do not carry into later invocations.

A command still running after the requested wait returns a numeric `session_id`.
Use `write_stdin` with that ID to send input or poll with empty `chars`; inspect
`exit_code` to establish completion. The wait is a return interval, not a process
execution deadline. PTY sessions support interactive input, including explicit
Ctrl-C. Cancelling a Code Mode cell or model turn retains the shell session,
matching Codex; continue it through `write_stdin` rather than starting a duplicate
command. Output and polling limits follow the shared process runtime.

Retained shell sessions pin their workspace until `write_stdin` observes exit,
so worktree cleanup is refused while such a lease exists. `TaskOutput` and
`TaskStop` operate monitor and workflow jobs, not these shell sessions. Shell
sessions and child registry snapshots live in the process; SQLite receipt
persistence does not reconstruct them after process exit. The former native
Bash background flags, timeout promotion, cwd retention, and `BASH_*` timeout
settings do not apply to the shared shell tools.

The native CLI installs the exact same subagent handlers and input schemas as
Codex: `spawn_agent`, `send_agent_message`, `list_agents`, `wait_agent`,
`interrupt_agent`, `close_agent` and `submit_result`. The shared registry owns
fresh children, family/model selection, result contracts, message routing,
management checks and subtree shutdown. Claude-native agent aliases and profile
selection are not part of the CLI tool catalog. Inherited workspace and permission
restrictions still apply to Claude children.

Both native runtimes discover the same Computer Use provider with the same
`NANOCODEX_COMPUTER` configuration. Its nested tools retain the exact
`mcp__cua_repl__*` names, provider schemas and handlers. `--workspace-tools false`
skips local CUA discovery. Text, screenshots, structured results and provider
errors cross the native Messages adapter; hidden lifecycle tools stay hidden.

The CLI always uses the shared QuickJS Code Mode engine, independent of harness.
Claude sees only `exec` and `wait`; `exec` accepts `{ "code": "..." }`. Native tools,
shared subagent tools and CUA tools are callable on `tools` inside JavaScript.
Permissions, hooks and file checkpoints wrap each nested native invocation.
See the [tool inventory and usage](TOOL_RUNTIMES.md) for both runtime catalogs.

`--claude-hooks PATH` explicitly loads synchronous command hooks for
`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SessionStart`,
`UserPromptSubmit`, `Stop`, `StopFailure`, `PreCompact`, `PostCompact`,
`SubagentStart`, `SubagentStop`, and `SessionEnd`. Pre-tool hooks can deny or
rewrite validated input; post-hook failure retains completed effects. Prompt
and precompact gates fail closed. Stop permits one extra model round and refuses
repeated blocking; observational failures preserve completed content.
Lifecycle effects commit started intent before dispatch. Recovery with no
confirmed outcome reports uncertainty and does not rerun the hook; completed
request replay invokes no hooks. `handles_lifecycle` defaults false, and unmatched
native lifecycle events create no effect writes. `SessionEnd` is ordinary runtime
shutdown, not permanent `EndConversation` closure.
Hook processes have bounded output/deadlines and descendant cleanup. Hook-requested
`ask` fails closed because there is no hook approval UI. Automatic hook settings
discovery, asynchronous hooks, prompt/agent hook types and unlisted events remain
unsupported. Hooks are executable user-selected host configuration; project prose
grants no authority. See [command-hook configuration](claude-command-hooks.md)
for exact events, JSON, recovery behavior and acceptance commands.

Interactive sessions install pending `AskUserQuestion` prompts and plan entry/exit
through terminal/TUI input. Empty or invalid input does not approve anything;
only the literal approval action leaves plan mode. The guard restricts model workspace and external mutations while allowing
inspection and planning support tools.
It remains installed without a UI when restoring a planning session. Plan state
is written under `CODEX_HOME/claude/plan-mode` before publishing transitions in
memory. The guard denies new model calls to workspace mutations, `exec_command`/`write_stdin`, MCP and
agents before running hooks. Inspection, context/skill loading, task-board updates
and user interaction remain available and pass through configured hooks. Explicit
host command hooks may have their own effects; previously admitted work can
continue. Plan mode is a dispatch policy, not OS isolation or reversal of prior
effects.

`--claude-permissions PATH` explicitly loads a JSON object with a `permissions`
object containing `allow`, `ask`, `deny` string arrays and optional `defaultMode`.
`--permission-mode` selects `default`/`manual`, `acceptEdits`, `plan`, `dontAsk`,
or `full-access`/`bypassPermissions`. With no saved or explicit policy the CLI
retains full-access compatibility. Selecting a rules file defaults to manual
admission. Deny rules take precedence over ask, then allow; unsupported syntax
fails configuration. Rules support tool names, MCP server/tool patterns, bounded
Read/Edit path patterns, `exec_command` patterns, legacy `Bash` policy aliases,
spawn_agent role/Skill selectors and WebFetch domains.
Shell compounds require every simple command to match an allow rule; complex
shell syntax requires approval. File deny/ask rules conservatively cover shell
calls. Shell deny/ask rules also cover `write_stdin`, including polling, because
continuation input can execute arbitrary commands.
`acceptEdits` admits workspace edits except sensitive `.git`/`.claude` paths.

Policy and planning state persist per session. With no explicit flags resume
retains saved rules; selecting only a mode retains saved rule lists. Admission
checks rule denials before hooks and checks the final rewritten input again.
Interactive approvals apply to one exact call and final JSON input. Headless
approval requests fail closed; `dontAsk` denies instead of prompting. This is a
native admission subset, without Claude Code's auto classifier or OS isolation.
Trusted command hooks retain their own authority.

`EnterWorktree` creates a new owned Git worktree and `claude/NAME` branch under
`.claude/worktrees` from the exact current repository root. It does not adopt
existing paths or branches; external paths requiring approval are unsupported.
Session workspace resolution switches file/notebook tools, new shell invocations,
context/skills, hooks and file checkpoints. New children snapshot the current
workspace, while existing children and retained shell sessions retain their pinned roots.
Worktree state persists independently of the conversation journal. An uncertain
Git transition is fenced and requires inspection before continuation.
`ExitWorktree` defaults to keeping the worktree and branch. Explicit `cleanup:true`
requires the exact owned branch and common repository, no dirty/untracked/ignored
files, no new commits and no pinned shell, monitor or child contexts. Cleanup uses
nonforced Git removal. This lifecycle does not change the process-wide cwd or
confine arbitrary shell commands.

Interactive TUI sessions install `CronCreate`, `CronList`, `CronDelete` and
`ScheduleWakeup` unless `CLAUDE_CODE_DISABLE_CRON=1`. Five-field numeric cron
supports local or IANA time zones, ranges, lists and steps. At most 50 tasks are
retained; recurring tasks expire after seven days. Tasks run only when the CLI
is open and idle, through the normal session prompt lane. Due state is persisted
before dispatch, so a crash in that gap may lose a firing. Deterministic task-ID
jitter delays recurring fires by up to half the period, capped at 30 minutes;
one-shots at :00/:30 can fire up to 90 seconds early, never before creation.
Scheduler owner epochs fence older resumed UIs; there is no exactly-once delivery
guarantee. Reopen retains unexpired future cron tasks,
skips missed recurring intervals, and drops elapsed one-shots and dynamic wakeups.
A recurring task still open at expiry gets a final firing before removal.
`ScheduleWakeup` replaces a single pending wakeup, clamps delay to 60–3600 seconds,
and supports explicit stop; Escape cancels it. These tools are omitted from
headless and child catalogs and do not create an account cron or daemon.
The native `/loop` frontend accepts a task, a leading interval or a trailing
`every` interval. Fixed intervals become normal `CronCreate` requests, disclosing
minute rounding and the selected nearest uniform cron step. Bare/task-only loops
ask the model to select `ScheduleWakeup`; a host-only iteration token prevents
older completion from changing a newer loop. If an allowed iteration ends without
an explicit wakeup, one 20-minute fallback is installed; its completion does not
create another fallback. Ask/Deny policies do not grant automatic fallback.
At each maintenance dispatch, `.claude/loop.md` is read afresh from the current
workspace, then the trusted user home only if the project file is absent, with
symlink rejection and 25,000-byte truncation. Read Ask/Deny imports no contents.
Scheduled skills are checked and expanded with Model provenance, fresh metadata
and normal Skill policy; built-ins cannot be shadowed. Maintenance/skill text is
reference data and grants no permissions. Explicit Escape/stop cancels the loop.

The same interactive owner TUI installs `Monitor` for commands or WebSocket
sources. Commands start real Bash processes pinned to the current workspace.
Bounded events are batched for 200 ms and enter the serialized idle prompt queue
as untrusted external data; composer drafts suppress dispatch.
`TaskOutput` returns retained status/stdout/stderr; `TaskStop` cancels the process
group, including descendants. Jobs belong to their originating session. The
host retains at most 32 jobs, 100 stdout lines of at most 4096 bytes each, and
64 KiB each of stdout and stderr. Output or queue overflow stops the job; if the
queue cannot accept its terminal event, status remains available through
`TaskOutput`. The default timeout is five minutes; `timeout_ms` accepts one
second through one hour. `persistent:true` removes that deadline only for the
current CLI lifetime. No monitor process or pending event is restored after
exit. Headless/child catalogs omit Monitor, and disabling cron also disables it.
WebSocket input uses `ws: {url, protocols?}` and requires `--web-search` plus
normal WebFetch domain admission. Public destinations are validated and DNS-pinned;
private destinations require a matching repeatable explicit
`--claude-monitor-ws-origin ws://HOST:PORT` or `wss://HOST:PORT`. Origins cannot
include credentials, path, query or fragment. Ambient dotenv/environment values
grant no socket authority. Text, bounded binary frames, ping/close, cancellation
and timeout use the native socket transport; no socket is recovered after exit.

With `--web-search`, the CLI installs native `WebSearch` and `WebFetch` client
tools. Search uses an auxiliary Messages server-search request. Fetch captures
bounded HTML/text from public HTTPS destinations and summarizes it through an
auxiliary model request. The fetch host disables ambient proxies/credentials,
pins validated public addresses per redirect hop, and rejects local/reserved
addresses and unsupported responses. It does not implement Claude Code's domain
approval UI. Auxiliary calls have their own provider cost and failure boundary.

Reproduce shipped-host journeys with:

```sh
cargo test --locked -p nanocodex-bin --test claude_host -- --nocapture
cargo test --locked -p nanocodex-bin --test claude_scheduler_monitor --test claude_workflow --test claude_hooks --test claude_checkpoints -- --nocapture
cargo test --locked -p nanocodex-bin --test claude_skills --test claude_mcp --test claude_web --test claude_prompt_images -- --nocapture
python3 scripts/tests/claude-interaction-cli-journey.py --binary target/debug/nanocodex
python3 scripts/tests/claude-resume-cli-journey.py --binary target/debug/nanocodex
```

These invoke the executable over loopback Messages/SSE with synthetic provider
responses and real filesystem/process effects. Evidence is retained under
ignored `output/claude-host/`, `output/claude-native-cli/`, and the corresponding
`output/claude-*/` journey directories. The interaction journey uses real PTY
input to verify pending questions, stale-input rejection, explicit plan approval,
persistence failure, cancellation and hook enforcement. Resume uses independent
processes and both explicit selection and the picker. Only paths exercised
by the completed run count as validated; source presence is not a passing run.

## Native workflows

`--claude-workflows` explicitly enables root-only `Workflow` when subagents are
enabled. It is disabled by default and omitted from child catalogs. Inline,
named or workspace `scriptPath` workflows begin with pure literal
`export const meta = {name, description, phases?}`; metadata is validated before
runtime or child dispatch. An original private JavaScript helper environment
provides `agent`, `parallel`, `pipeline` and `phase` through the real authorized
registry, with no direct filesystem, network or process bridge.

Limits are 15 agent calls per run, four concurrent, five minutes, 512 KiB script
and 64 KiB result. Admission pins the workspace for scripts and all late-spawned
children; completed resumable children retain their own pins. Read/Edit
restrictions conservatively cover script loading and persistence; spawn_agent deny/ask
rules also cover the entire Workflow, including parameter-scoped spawn_agent rules.
spawn_agent allow rules do not grant Workflow authority. Runs start in
the background with `wf_` IDs for `TaskOutput`/`TaskStop`. Same-session terminal
`resumeFromRunId` reuses only confirmed matching prompt/options results; uncertain,
failed or pending calls remain fenced. Resume cannot retarget another workspace.
Runs and caches are process-local; SQLite receipts do not reconstruct them.

Reproduce the real QuickJS/registry/CLI journey with
`cargo test --locked -p nanocodex-bin --test claude_workflow -- --nocapture`.
Inspect actual provider requests, script/phase/output receipts, cancellation,
permission refusal, and late-child/retained-child worktree evidence under
`output/claude-workflow/`. Synthetic model output does not establish live
subscription admission or complete vendor orchestration semantics.

## Native session resume

Default CLI persistence uses `CODEX_HOME/claude/sessions.sqlite`.
`nanocodex resume --claude SESSION_ID` reopens a Claude journal;
`nanocodex resume --claude` presents the native session picker. Discovery is
read-only and does not acquire ownership. Selecting a session uses the normal
durable owner/fencing path, restores its saved model and canonical workspace,
and rejects a conflicting explicit `--cwd`. An explicit `--model` can change the
model for new work; unfinished admitted requests keep their frozen configuration.
Legacy sessions missing routing metadata require explicit `--cwd` or `--model`.
Unsupported, missing or oversized journals fail discovery instead of creating
replacement sessions. Custom `--local-durability` stores remain a separate
explicit recovery path and are not scanned by the default picker.

Resume restores native conversation and task receipts. Process-local shell sessions
and child runtimes are not reconstructed. Authentication and currently authorized
capabilities are reattached by the host.

## Native file checkpoints

The CLI records before-images for native `Edit`, `Write`, and `NotebookEdit`
after permission checks and input-rewriting pre-hooks. The journal lives under
`CODEX_HOME/claude/checkpoints` and associates each file effect with its session,
turn, call, and actual workspace. Failed or incomplete captures block unsafe
restoration. Shell, MCP and hook side effects are outside this file-restoration
scope. Conversation branching is selected separately below.

```sh
nanocodex rewind SESSION_ID
nanocodex rewind SESSION_ID --checkpoint TURN_ID
nanocodex rewind SESSION_ID --checkpoint TURN_ID --restore
```

The first commands show JSON previews. `--restore` requires an explicit turn ID
and restores that turn's native file edits and all later recorded edits. Existing
bytes and permissions are restored; newly created files are removed. Hash/mode
checks reject external modifications before restoration begins. Restoration is
not an atomic transaction across multiple files: an interruption or concurrent
change during writes leaves a persistent fence requiring inspection instead of
silently resuming or claiming success. Symlinks and special files are rejected.
Capture limits are 8 MiB per file, 512 calls and 32 MiB per session journal, and
256 MiB for the checkpoint store; reaching a limit can refuse a file operation.
Committed durable replay does not recapture or repeat the edit.

Reproduce the real CLI file-restoration journey with
`cargo test --locked -p nanocodex-bin --test claude_checkpoints -- --nocapture`.
It covers preview, conflicts, effective hook inputs, file creation/removal,
notebook restoration, file modes, bounded/special-file rejection and replay.
Use `--mode conversation` to branch the settled default SQLite journal before
the selected user turn, or `--mode files-and-conversation` to additionally restore
native file before-images from that turn and later turns. Default `--mode files`
retains the existing behavior. Previews list retained user boundaries, selected
discarded turns and applicable file changes without mutation. Unknown/expired
history, pending source operations, unsafe boundaries and changed source owners
are refused. A new UUID journal is returned with its resume command; the original
history remains recoverable. The branch retains admitted tool identities so
continuation cannot replay discarded effects under old IDs. Shell, hooks, MCP
and other external effects are neither undone nor replayed.

File restoration and new branch publication are separate operations. A failed
publication reports any restored file result and requires inspection before retry;
there is no cross-file/store atomicity claim. Reproduce selected previews,
first-turn/combined branching, original continuation, pending refusal and exact
old-ID fencing through the same `claude_checkpoints` target and
`cargo test --locked -p nanocodex-durability --features claude,sqlite --test checkpoint_branch`.
Evidence is retained under `output/claude-conversation-rewind/`.

## Shared durability

Enable the `claude` feature of `nanocodex-durability`, import `DurableAgentExt`, and attach the same `DurableSession` with `.durability(state).await?.build()?`. The [Claude adapter](../crates/nanocodex-durability/src/claude.rs) uses the existing store, owner fencing, operation admission, continuation, effect receipt and terminal-result machinery. Without this attachment, the builder remains an in-memory agent. See the [durability setup](../crates/nanocodex-durability/README.md) for construction and store selection.

Checkpoints retain provider-native conversation blocks, signed/opaque content, admitted tool IDs, context/compaction state, discovery state, container identity, recovery notices and an attached task board. An unfinished operation also retains its original Messages request template, catalog and execution settings. Reopening with a different model, system prompt, token limit or tool catalog does not silently change that admitted request. An unavailable handler cannot execute; already committed tool receipts can replay without that handler.

Completed model and tool effects replay from stored receipts rather than issuing another provider request or invoking the handler. Repeating a completed request ID returns its terminal receipt without rewinding the current conversation. Unfinished effects with no committed receipt follow the shared store's **at-least-once** recovery policy: a crash after an external effect but before receipt commit can repeat that effect. This is not universal exactly-once execution. Hosts must use the stable session, turn and call identities to deduplicate or reconcile external operations where necessary.

A detached client does not discard an accepted operation. Store failures leave work recoverable instead of acknowledging a false terminal result; owner fencing prevents a replaced owner from dispatching a late response. Missing task-board recovery leaves the pending operation available for a correctly configured host to resume.

These checkpoints currently encode whole provider-native JSON snapshots and continuations. They do not implement the paged transcript/storage optimization of the OpenAI path; payload growth remains an operational limit to assess for long sessions.

## Context and compaction

The active estimate starts from the latest reported input, cache-read, cache-write and output usage. Newly queued text and tool receipts add a UTF-16 text estimate until the next provider response supplies an updated usage anchor. The configured automatic window is a trigger, not a guarantee that the preserved payload fits that size. The current reserve remains 20k output plus 13k headroom for supported coding models.

Compaction summarizes the earlier prefix while preserving a pending assistant/tool round's tool calls, opaque fields and complete tool results. A summary changes the prefix that retained `thinking` and `redacted_thinking` blocks are bound to, so requests packed with a summary omit those blocks and any messages they leave empty, including requests from sessions restored from compacted checkpoints. Reasoning received after compaction is replayed on later turns: committing that response stores the packed history and clears the summary. See Claude's [preserved-thinking contract](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking).

Paused server-tool content retains calls and results across the whole current assistant turn, including earlier calls whose results arrive in a later pause; no client results are fabricated. If this is the first tool round, the original user task is the summary prefix. A prior summary participates in later compaction, including repeated manual compaction with no intervening turn.

The summary contract requests current task scope, latest user corrections,
authorization boundaries, verified work, unresolved actions with available IDs,
and concrete next steps. It preserves source distinctions and uncertainty.
Continuation frames the generated summary as lossy historical context, and a new
user message remains a separate later message rather than being folded into that
summary. Structured pending rounds and recovery notices remain authoritative
runtime evidence; summary prose does not grant permissions or prove completion.
Protocol journeys verify these boundaries, not a live model's semantic fidelity.

Summary requests retain the tool catalog for caching but set `tool_choice: {"type":"none"}` to prevent provider-side tool execution. A summary is validated before replacing context. Failed summaries retain the original state; a successful summary is checkpointed before continuation. Automatic summary usage contributes to the successful turn's usage totals. Rebuilt context receives an estimate for the summary, retained messages, system context and tools.

Automatic compaction suppresses an unchanged boundary after a failed continuation. New assistant rounds can make progress and trigger another summary during the same user turn. A bounded local refill policy allows two rapid summaries, then waits for three advancing assistant responses. This is an explicit local policy, not the CLI's exact breaker implementation. The model-call loop continues until completion, cancellation, an error or exhaustion of its `u32` ordinal.

Manual compaction cancels the active turn before taking the conversation lock and summarizes at the preserved receipt boundary. Its summary stream has a registered cancellation token, allowing shutdown to stop a stalled summary instead of waiting indefinitely. A summary interrupted before validation leaves the previous context intact.

## Prompt caching and discovery

Caching is opt-in through `automatic_cache(true)` or `cache_one_hour()`. The request builder also places a stable system-prefix breakpoint when the cache budget and caller policy permit it. Explicit caller system markers are preserved. A provider cache hit, minimum token eligibility, expiry and billing remain provider decisions.

Before authentication or HTTP, requests validate cache markers in tools → system → messages order: no more than four effective breakpoints, valid TTL/type, longer TTL before shorter TTL, and valid automatic/final-marker combinations. Thinking and empty text cannot carry direct markers. Tool-result cache controls and signed thinking metadata survive round-trip serialization.

Custom `ToolSearch` returns standard tool-result references. All registered definitions stay in a stable top-level catalog with their original deferred flags; discovery does not promote them into the eager prefix. When client and server search are enabled together, successful server search receipts can authorize a deferred client call in the same response. That availability is derived from matching configured server-search calls and retained references, so discarded references do not survive compaction as permanent privileges. Rejected discovery options and failed discovery post-hooks do not activate tools. After successful compaction, the execution-discovery set is intersected with authentic ToolSearch references still present in the retained suffix. Discarded references require fresh discovery; failed compaction leaves discovery unchanged. The implementation follows the public [custom tool-search protocol](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool#custom-tool-search-implementation) and [tool caching behavior](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching).

The live CLI uses additional request fields and message roles. The library uses public Messages representations rather than copying those fields. General policy follows [Anthropic's caching documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching); synthetic tests establish request behavior, while the separate live trace establishes observed CLI cache reuse.

## Tools and recovery

Client tool identities are admitted once per session and survive compaction. Reuse of an admitted ID fails before handlers execute. Sequential and queued parallel calls check cancellation before starting another handler; completed receipts survive cancellation or a failed follow-up. Interrupted work receives an explicit unknown-outcome receipt. With `.durability(...)`, the admitted IDs and completed receipts also survive reopen. Without it, this protection is in-process. Different tool IDs are not semantically deduplicated, and uncommitted effects retain the at-least-once recovery policy described above.

Completed provider-side tool receipts are checkpointed even when a valid response ends with an unsupported stop or cancellation. Ordinary failure or cancellation retires unresolved native server calls into bounded, unknown-outcome transcript data before terminal settlement. A fresh prompt can reconcile that evidence without implicitly resuming the failed server turn. Input cancelled before admission is not queued for a future turn. Healthy `pause_turn` continuations remain native; an unfinished durable operation whose store commit failed instead follows its frozen request and receipt recovery contract above. Interrupted server-tool streams also retain any observed container identity; recovery never invents a completed assistant or server-result block. Provider code-execution containers are retained whether their identity arrives in the initial message or final delta. Recovery notices are stored separately from summary text and are reinserted into packed context when absent, so a lossy summary cannot erase the unknown-outcome warning; the durable checkpoint preserves them across reopen. The embedding host supplies the durable store, authority and sandboxing. Dropping a blocking filesystem future does not guarantee the underlying operation stopped.

The optional workspace adapters support bounded UTF-8 files, exact edits, globset wildcard/class/alternation matching, scoped Rust regex search, notebooks and session-scoped tasks. Unsupported mutation options fail before changes. Edit expansion is checked before allocation; directory traversal bounds both queued and visited entries. Task mutations preserve the readability of bounded TaskGet/TaskList results and reject oversized updates atomically. `.tasks(board)` attaches a board whose tasks, dependencies, todos and next-ID watermark are checkpointed with tool receipts when durability is enabled. Recovery restores committed task mutations into a new board without calling the handler again; task-bearing durable sessions execute tool receipts sequentially to preserve board ordering. The board remains scoped to its session, not an account scheduler or shared cross-agent service.

`.host_tools(...)` installs only the explicitly enabled subset of eight `ClaudeHostTools` adapters: `Agent`, `TaskOutput`, `TaskStop`, `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`, `EnterWorktree` and `ExitWorktree`. They pass validated inputs and real session/turn/call identity to an injected `ClaudeHost`. The host must actually own child/task execution, pending user answers, plan approval and workspace transitions; the adapters supply no default implementation or synthetic acknowledgement. Background agents require installed output and stop capabilities.

Bash requires an injected sandbox executor; web tools require explicit provider/page-source capabilities. Nested WebSearch sends the server web search tool without a max_uses cap and follows provider pause_turn continuations until end_turn; a paused response with no content fails as no progress instead of being replayed. It returns the complete answer and every deduplicated source URL and title. Auxiliary WebFetch accepts multiline prompts and returns its whole summary with the source URL. Both results are subject only to the per-receipt history bound applied to every recorded tool result.

## Coverage and limits

The [SQLite integration suite](../crates/nanocodex-durability/tests/claude.rs) runs the public builder and localhost Messages/SSE journey through real store reopen. Its fault matrix learns the write boundaries of a model/tool/automatic-compaction operation, then fails every observed write both before commit and after commit with a lost acknowledgement. It checks frozen request configuration, terminal replay, committed-effect reuse and task-board reconstruction, while allowing an uncommitted external effect to run again. Other scenarios cover reasoning invalidation at compaction, retained opaque tool receipts, discovery/container recovery, sticky interruption notices, detached clients, owner fencing, cancellation during recovery, missing task boards and aborted admission/compaction callers.

Reproduce that coverage with `cargo test -p nanocodex-durability --features claude,sqlite --test claude`. The separate [host adapter tests](../crates/nanocodex-claude-tools/src/host_tests.rs) cover pending questions, host-owned background task identity/stop, and denied or unsupported input. The [native caller MCP journey](../crates/nanocodex-claude/tests/mcp_native.rs) uses actual loopback JSON-RPC HTTP to exercise intact input/context, live schema refresh/removal, error status, structured results, metadata and ordered media. The caller owns discovery/transport; the library journey does not establish the shipped CLI configuration or full MCP transport parity. The [builder-level host integration tests](../crates/nanocodex-claude/tests/host_tools.rs) also exercise real Messages continuations: answers stay pending until the host responds, host failures remain error results, structured results and metadata survive on tool events, image output becomes Claude image content, and unsupported audio returns an explicit error. These synthetic journeys establish boundary behavior, not a deployed product's host services or live provider parity.

The [canonical tools-feature checkpoint regression](../crates/nanocodex-claude/tests/tools_checkpoint.rs) runs with `cargo test -p nanocodex-claude --no-default-features --features tools --test tools_checkpoint`. It writes a provider-native task checkpoint to a temporary file through a synthetic host execution policy and reopens a fresh board/builder, continuing through actual Messages/SSE TaskGet and TaskCreate calls to check task content, next-ID watermark and sequential durable dispatch. Synthetic request/checkpoint transcripts are written to local ignored evidence. It does not depend on activating the `workspace-files` alias and does not replace the SQLite store/fencing integration suite.

The Claude backend and shared durability adapter compile for `wasm32-unknown-unknown`; provider streaming, auth futures and clock handling have WASM paths. The additive [JavaScript API](CLAUDE_JAVASCRIPT.md) exposes explicit host-owned authentication and tools through a separate `Nanoclaude` WASM handle while reusing the common JS lifecycle and shared durability store. The standalone SDK does not switch existing managed agents or install ambient host capabilities. The [managed integration](CLAUDE_MANAGED.md) adds a separate private account connection, native tool catalog and subscription-backed routing. Browser Claude runs in the current isolate rather than silently creating the Codex module Worker. Actual synthetic WASM execution evidence is recorded separately from compilation and prior live native subscription measurements.

The fallback estimate after an unknown/invalid response includes packed messages, system context and the tool catalog from the frozen request template until the next successful usage anchor. Invalid-response evidence is capped at 64 KiB with a truncation/unknown-effects marker; valid completed boundaries remain intact. Live interactive Claude Code measurements remain separate and do not establish every CLI tool, model, or exact prompt/compaction parity.

The Rust subscription manager supplies PKCE login, callback validation, persisted
token exchange/refresh and account continuity through a host-owned private secret
store and HTTP capability. Its defaults follow measured Claude Code 2.1.283
behavior. OAuth state is separate from agent checkpoints; the host reattaches the
authenticated client on reopen. A composed SQLite journey covers synthetic login,
tools, compaction, refresh, restart and logout. Historical native subscription,
PKCE and refresh observations are recorded in [authentication setup and measured
protocol](claude-authentication.md). Those observations do not establish live
admission of the continuation capabilities above. Natural expiry, managed live
admission and billing remain separate acceptance boundaries.

The native host exposes its capabilities through Code Mode. Conditional product
tools remain absent:
Artifact, DesignSync, EndConversation, PowerShell, PushNotification, RemoteTrigger,
ReportFindings, SendFeedback and ShareOnboardingGuide. Remaining differences include
complete permission-mode equivalence, named teams, proprietary agent-view support,
unlisted hook/plugin features, paged transcript storage and unsupported managed
operations in the [managed guide](CLAUDE_MANAGED.md). See [the tool matrix](CLAUDE_TOOL_MATRIX.md)
and [interactive compaction measurements](research/nanoclaude-auto-compaction-measured.md).
This is implemented capability coverage, not full proprietary behavior or new
live authentication/provider admission evidence. No full Claude Code parity is claimed.

## Strict Messages stream completion

The shared native/WASM SSE decoder accepts LF, CRLF and bare CR line endings,
including CRLF and UTF-8 codepoints split across HTTP chunks, and one leading
UTF-8 BOM. Bounds apply to individual SSE frames, not whole coalesced transport
chunks. Successful HTTP responses must have `text/event-stream` media type;
a non-SSE body is a body-free protocol error, not a misleading missing-terminal
error. Framing follows the [SSE standard](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation).

A complete, blank-line-terminated `message_stop`, final stop reason and closed
content blocks are required. EOF does not dispatch an unfinished event, promote
`message_delta` to completion or authorize partial tool calls. A terminal event
with an open block is reported separately as a protocol error. Accepted streams
are never automatically replayed after truncation. These rules retain
[Claude's Messages lifecycle](https://platform.claude.com/docs/en/build-with-claude/streaming).

`cargo test -p nanocodex-claude --all-features` covers byte-split framing, proper
terminals, unterminated/missing terminals and no dispatch/history/final success
from a complete-looking tool block on a truncated stream. These are synthetic
transport regressions, not proof of the historical production error's exact
cause or a live-provider deployment.
