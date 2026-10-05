# Claude-native runtime

`nanocodex-claude` implements a separate Messages-based backend behind the common
`nanocodex-agent` lifecycle. The low-level builder accepts explicit tool adapters.
For a local coding agent, use `nanocodex::Backend::claude(api_key)` or Node's
`Backend.claude({ apiKey })`: the SDK installs its supported native tools against
the selected workspace. See the [Rust guide](../crates/nanocodex/README.md) and
[JavaScript guide](CLAUDE_JAVASCRIPT.md) for the default catalogs and customization.
Credentials remain explicit, and the Claude model receives Claude-native tool
names and definitions.

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

## Tool crate migration

The former `nanocodex-tools` monolith is split by provider. Claude integrations
must depend on `nanocodex-claude-tools`, not `nanocodex-oai-tools` (the renamed
OpenAI runtime). Enable `nanocodex-claude`'s `tools` feature; `workspace-files`
remains an alias for existing callers. Imports now use the clean modules
`bash`, `host`, `notebook`, `tasks`, `web`, and `workspace_files`, with primary
adapter and capability types also exported at the crate root. For example,
`nanocodex_tools::claude_host::ClaudeHost` becomes
`nanocodex_claude_tools::host::ClaudeHost` and
`nanocodex_tools::ClaudeWorkspaceFiles` becomes
`nanocodex_claude_tools::ClaudeWorkspaceFiles`.

The standalone tools crate has no OpenAI API/tools or agent dependency.
`HostContext` carries the actual model, session, turn, call and output budget,
without a Responses history. Host outputs use native text/image blocks,
`is_error`, and optional structured data/metadata; `ClaudeBuilder::host_tools`
encodes these directly as Claude results. Hosts must migrate their old shared
`ToolContext`/Responses content DTOs instead of passing `input_*` wire items.
Unsupported media produces an explicit error, never a silently truncated block.
Portable capability contracts, tasks, Bash and web adapters are available on
WASM. Task-board registration and checkpoint recovery use the same Rust
implementation on native and WASM targets. Filesystem and notebook execution
in the Rust tools crate remains native-only; the Node backend supplies its local
filesystem and process handlers.

`ClaudeMcp` intentionally requires a caller implementation of
`ClaudeMcpProvider`, returning `McpToolDefinition` schemas and native
`ToolOutput` results. The old OpenAI `DynamicToolProvider` bridge is not retained
as an alias or wrapper. The embedding may reuse its own authorized MCP service,
but it owns connections, OAuth, discovery, validation, capture limits and
lifecycle. Read `definitions()` at each request boundary; schema changes and
removals are live, duplicate/non-MCP/malformed entries fail closed, and racing
removals or provider errors remain failures. Result blocks, structured data and
metadata survive adapter dispatch. This interface does not automatically wire
a dynamic MCP catalog into `ClaudeBuilder` or implement MCP resources/waiting.
The split establishes package and protocol boundaries, not production host
wiring, lifecycle equivalence or full Claude Code parity.

## Shared durability

Enable the `claude` feature of `nanocodex-durability`, import `DurableAgentExt`, and attach the same `DurableSession` with `.durability(state).await?.build()?`. The [Claude adapter](../crates/nanocodex-durability/src/claude.rs) uses the existing store, owner fencing, operation admission, continuation, effect receipt and terminal-result machinery. Without this attachment, the builder remains an in-memory agent. See the [durability setup](../crates/nanocodex-durability/README.md) for construction and store selection.

Checkpoints retain provider-native conversation blocks, signed/opaque content, admitted tool IDs, context/compaction state, discovery state, container identity, recovery notices and an attached task board. An unfinished operation also retains its original Messages request template, catalog and execution settings. Reopening with a different model, system prompt, token limit or tool catalog does not silently change that admitted request. An unavailable handler cannot execute; already committed tool receipts can replay without that handler.

Completed model and tool effects replay from stored receipts rather than issuing another provider request or invoking the handler. Repeating a completed request ID returns its terminal receipt without rewinding the current conversation. Unfinished effects with no committed receipt follow the shared store's **at-least-once** recovery policy: a crash after an external effect but before receipt commit can repeat that effect. This is not universal exactly-once execution. Hosts must use the stable session, turn and call identities to deduplicate or reconcile external operations where necessary.

A detached client does not discard an accepted operation. Store failures leave work recoverable instead of acknowledging a false terminal result; owner fencing prevents a replaced owner from dispatching a late response. Missing task-board recovery leaves the pending operation available for a correctly configured host to resume.

These checkpoints currently encode whole provider-native JSON snapshots and continuations. They do not implement the paged transcript/storage optimization of the OpenAI path; payload growth remains an operational limit to assess for long sessions.

## Context and compaction

The active estimate starts from the latest reported input, cache-read, cache-write and output usage. Newly queued text and tool receipts add a UTF-16 text estimate until the next provider response supplies an updated usage anchor. The configured automatic window is a trigger, not a guarantee that the preserved payload fits that size. The current reserve remains 20k output plus 13k headroom for supported coding models.

Compaction summarizes the earlier prefix while preserving a pending assistant/tool round, including its signed thinking, opaque fields and complete tool results. Paused server-tool content retains the whole current assistant turn, including earlier calls whose results arrive in a later pause; no client results are fabricated. If this is the first tool round, the original user task is the summary prefix. A prior summary participates in later compaction, including repeated manual compaction with no intervening turn.

Summary requests retain the tool catalog for caching but set `tool_choice: {"type":"none"}` to prevent provider-side tool execution. A summary is validated before replacing context. Failed summaries retain the original state; a successful summary is checkpointed before continuation. Automatic summary usage contributes to the successful turn's usage totals. Rebuilt context receives an estimate for the summary, retained messages, system context and tools.

Automatic compaction suppresses an unchanged boundary after a failed continuation. New assistant rounds can make progress and trigger another summary during the same user turn. A bounded local refill policy allows two rapid summaries, then waits for three advancing assistant responses. This is an explicit local policy, not the CLI's exact breaker implementation. The model-call loop continues until completion, cancellation, an error or exhaustion of its `u32` ordinal.

Manual compaction cancels the active turn before taking the conversation lock and summarizes at the preserved receipt boundary. Its summary stream has a registered cancellation token, allowing shutdown to stop a stalled summary instead of waiting indefinitely. A summary interrupted before validation leaves the previous context intact.

## Prompt caching and discovery

Caching is opt-in through `automatic_cache(true)` or `cache_one_hour()`. The request builder also places a stable system-prefix breakpoint when the cache budget and caller policy permit it. Explicit caller system markers are preserved. A provider cache hit, minimum token eligibility, expiry and billing remain provider decisions.

Before authentication or HTTP, requests validate cache markers in tools → system → messages order: no more than four effective breakpoints, valid TTL/type, longer TTL before shorter TTL, and valid automatic/final-marker combinations. Thinking and empty text cannot carry direct markers. Tool-result cache controls and signed thinking metadata survive round-trip serialization.

Custom `ToolSearch` returns standard tool-result references. All registered definitions stay in a stable top-level catalog with their original deferred flags; discovery does not promote them into the eager prefix. When client and server search are enabled together, successful server search receipts can authorize a deferred client call in the same response. That availability is derived from matching configured server-search calls and retained references, so discarded references do not survive compaction as permanent privileges. Rejected discovery options do not activate tools. After successful compaction, the execution-discovery set is intersected with authentic ToolSearch references still present in the retained suffix. Discarded references require fresh discovery; failed compaction leaves discovery unchanged. The implementation follows the public [custom tool-search protocol](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool#custom-tool-search-implementation) and [tool caching behavior](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching).

The live CLI uses additional request fields and message roles. The library uses public Messages representations rather than copying those fields. General policy follows [Anthropic's caching documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching); synthetic tests establish request behavior, while the separate live trace establishes observed CLI cache reuse.

## Tools and recovery

Client tool identities are admitted once per session and survive compaction. Reuse of an admitted ID fails before handlers execute. Sequential and queued parallel calls check cancellation before starting another handler; completed receipts survive cancellation or a failed follow-up. Interrupted work receives an explicit unknown-outcome receipt. With `.durability(...)`, the admitted IDs and completed receipts also survive reopen. Without it, this protection is in-process. Different tool IDs are not semantically deduplicated, and uncommitted effects retain the at-least-once recovery policy described above.

Completed provider-side tool receipts are checkpointed even when a valid response ends with an unsupported stop or cancellation. Ordinary failure or cancellation retires unresolved native server calls into bounded, unknown-outcome transcript data before terminal settlement. A fresh prompt can reconcile that evidence without implicitly resuming the failed server turn. Input cancelled before admission is not queued for a future turn. Healthy `pause_turn` continuations remain native; an unfinished durable operation whose store commit failed instead follows its frozen request and receipt recovery contract above. Interrupted server-tool streams also retain any observed container identity; recovery never invents a completed assistant or server-result block. Provider code-execution containers are retained whether their identity arrives in the initial message or final delta. Recovery notices are stored separately from summary text and are reinserted into packed context when absent, so a lossy summary cannot erase the unknown-outcome warning; the durable checkpoint preserves them across reopen. The embedding host supplies the durable store, authority and sandboxing. Dropping a blocking filesystem future does not guarantee the underlying operation stopped.

The optional workspace adapters support bounded UTF-8 files, exact edits, a simple Unicode glob subset, scoped regex search, notebooks and session-scoped tasks. Unsupported mutation options fail before changes. Edit expansion is checked before allocation; directory traversal bounds both queued and visited entries. Task mutations preserve the readability of bounded TaskGet/TaskList results and reject oversized updates atomically. `.tasks(board)` attaches a board whose tasks, dependencies, todos and next-ID watermark are checkpointed with tool receipts when durability is enabled. Recovery restores committed task mutations into a new board without calling the handler again; task-bearing durable sessions execute tool receipts sequentially to preserve board ordering. The board remains scoped to its session, not an account scheduler or shared cross-agent service.

`.host_tools(...)` installs only the explicitly enabled subset of eight `ClaudeHostTools` adapters: `Agent`, `TaskOutput`, `TaskStop`, `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`, `EnterWorktree` and `ExitWorktree`. They pass validated inputs and real session/turn/call identity to an injected `ClaudeHost`. The host must actually own child/task execution, pending user answers, plan approval and workspace transitions; the adapters supply no default implementation or synthetic acknowledgement. Background agents require installed output and stop capabilities.

Bash requires an injected sandbox executor; web tools require explicit provider/page-source capabilities. Nested WebSearch preserves bounded findings and complete, deduplicated source URLs across server pause/continuation responses. Auxiliary WebFetch accepts multiline prompts. WebSearch/WebFetch bound output while reserving source attribution; an oversized source set fails explicitly rather than silently dropping citations.

## Coverage and limits

The [SQLite integration suite](../crates/nanocodex-durability/tests/claude.rs) runs the public builder and localhost Messages/SSE journey through real store reopen. Its fault matrix learns the write boundaries of a model/tool/automatic-compaction operation, then fails every observed write both before commit and after commit with a lost acknowledgement. It checks frozen request configuration, terminal replay, committed-effect reuse and task-board reconstruction, while allowing an uncommitted external effect to run again. Other scenarios cover signed compaction suffixes, discovery/container recovery, sticky interruption notices, detached clients, owner fencing, cancellation during recovery, missing task boards and aborted admission/compaction callers.

Reproduce that coverage with `cargo test -p nanocodex-durability --features claude,sqlite --test claude`. The separate [host adapter tests](../crates/nanocodex-claude-tools/src/host_tests.rs) cover pending questions, host-owned background task identity/stop, and denied or unsupported input. The [native caller MCP journey](../crates/nanocodex-claude/tests/mcp_native.rs) uses actual loopback JSON-RPC HTTP to exercise intact input/context, live schema refresh/removal, error status, structured results, metadata and ordered media. The caller owns discovery/transport; this does not establish automatic dynamic-catalog wiring into the builder or full MCP transport parity. The [builder-level host integration tests](../crates/nanocodex-claude/tests/host_tools.rs) also exercise real Messages continuations: answers stay pending until the host responds, host failures remain error results, structured results and metadata survive on tool events, image output becomes Claude image content, and unsupported audio returns an explicit error. These synthetic journeys establish boundary behavior, not a deployed product's host services or live provider parity.

The [canonical tools-feature checkpoint regression](../crates/nanocodex-claude/tests/tools_checkpoint.rs) runs with `cargo test -p nanocodex-claude --no-default-features --features tools --test tools_checkpoint`. It writes a provider-native task checkpoint to a temporary file through a synthetic host execution policy and reopens a fresh board/builder, continuing through actual Messages/SSE TaskGet and TaskCreate calls to check task content, next-ID watermark and sequential durable dispatch. Synthetic request/checkpoint transcripts are written to local ignored evidence. It does not depend on activating the `workspace-files` alias and does not replace the SQLite store/fencing integration suite.

The Claude backend and shared durability adapter compile for `wasm32-unknown-unknown`; provider streaming, auth futures and clock handling have WASM paths. The additive [JavaScript API](CLAUDE_JAVASCRIPT.md) exposes explicit host-owned authentication and tools through a separate `Nanoclaude` WASM handle while reusing the common JS lifecycle and shared durability store. The standalone SDK does not switch existing managed agents or install ambient host capabilities. The [managed integration](CLAUDE_MANAGED.md) adds a separate private account connection, native tool catalog and subscription-backed routing. Browser Claude runs in the current isolate rather than silently creating the Codex module Worker. Actual synthetic WASM execution evidence is recorded separately from compilation and prior live native subscription measurements.

The fallback estimate after an unknown/invalid response includes packed messages, system context and the tool catalog from the frozen request template until the next successful usage anchor. Invalid-response evidence is capped at 64 KiB with a truncation/unknown-effects marker; valid completed boundaries remain intact. Live interactive Claude Code measurements remain separate and do not establish every CLI tool, model, or exact prompt/compaction parity.

The Rust subscription manager supplies PKCE login, callback validation, persisted token exchange/refresh and account continuity through a host-owned private secret store and HTTP capability. Its defaults follow measured Claude Code 2.1.283 behavior. OAuth state is separate from agent checkpoints; the authenticated client is reattached on reopen. A composed SQLite journey covers login, tools, compaction, refresh, restart and logout. Live native subscription admission was verified with the observed public compatibility profile, including tool use, cache hits, compaction and recall after a real SQLite reopen. A separate fresh native PKCE authorization and deliberately triggered real refresh also completed provider inference. Natural expiry, managed live admission and billing remain separate acceptance boundaries. See [authentication setup and measured protocol](claude-authentication.md).

Remaining gaps include complete host service wiring, background Bash/PTY sessions, multimodal/PDF Read, skill and MCP-resource surfaces, account scheduling/workflows, paged transcript storage, and the explicitly unsupported managed operations described in the [managed guide](CLAUDE_MANAGED.md). See [the tool matrix](CLAUDE_TOOL_MATRIX.md) for individual capabilities and [interactive compaction measurements](research/nanoclaude-auto-compaction-measured.md) for the observed reference behavior. No full Claude Code parity is claimed.
