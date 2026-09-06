# Codex runtime alignment

Reference: [openai/codex ac192cd7937b0d73edc6dffe009940ae53782dd4](https://github.com/openai/codex/tree/ac192cd7937b0d73edc6dffe009940ae53782dd4/codex-rs), fetched September 6, 2026. The implementation follows this revision; it does not depend on an unpinned checkout at build time.

## Model and request policy

- New SDK, Python, native CLI, managed, and account conversations default to `gpt-6-astra` with `low` reasoning. Existing managed agents retain their settings. Sponsored homepage sessions retain their explicitly selected Luna policy.
- Model selection in Rust builders resolves the catalog's default effort: Astra/Sol `low`, Terra/Luna `medium`. An explicitly selected effort wins regardless of builder order.
- Astra's developer prompt comes from `models-manager/models.json`, with only the Nanocodex identity substituted. Explicit replacement and additive instructions remain supported. The shared GPT-5.6 prompt already matches the upstream template.
- Responses Lite requests keep `parallel_tool_calls: false`, encrypted reasoning, and no default reasoning summary. Stable session cache keys and immutable instruction/tool prefixes survive follow-on turns, reconnects, replay, and context resets.
- `client_metadata["x-codex-turn-metadata"]` carries session/thread/turn identity, request kind, and effective `tool_namespaces_info`. The removed `code_mode_tool_names` inventory is no longer sent. Experimental context requests additionally carry `agent_name`, `window_id`, `window_number`, `context_window_id`. History ingestion is local; requests do not ask the provider to ingest history.

## Experimental context windows

`OpenAi::builder(auth).experimental_context(true)` is the default. Astra uses experimental context windows when its host supplies workspace storage, including API-key and Business accounts. Other models and hosts without storage retain provider compaction. Callers can explicitly opt out with `.experimental_context(false)`.

Native and capable WASM runtimes expose `new_context`, `get_context_remaining`, `context_history` and `context_notes`. The reset lifecycle and tool schemas follow Codex, but Nanocodex owns their storage rather than calling Codex's subscription-gated hosted backend. Astra reserves the `history` and `notes` namespaces for its hosted schemas, so file-backed tools use `context_history` and `context_notes` with ordinary JSON arguments. History uses exact retained item IDs from list/search results rather than provider-injected short markers. `new_context` and the history/notes namespaces are direct-only; `get_context_remaining` follows normal native tool exposure and the existing direct dispatch policy for Rust-local embedded tools.

Native and Node hosts use the workspace filesystem. Browser and Cloudflare hosts accept `contextStorage: Workspace`, defaulting to their supplied filesystem. Managed agents pass their existing durable `/brain` workspace, backed by R2. There are no new bindings, migrations, provider credentials, or history/notes relay routes. Context files live under `.nanocodex/context`, scoped to session and agent. Archives contain exact serialized history; note files contain text and timestamps. These files must travel with the workspace when moving a durable session between hosts.

`context_history` recovers the current agent's earlier context windows and its live retained history. `context_notes` stores progress across resets. Existing managed `find_session` and `read_session` remain the way to search other completed conversations; they exclude the active session and cannot substitute for mid-turn context recovery. This change does not alter those tools or the existing subagent lifecycle.

At 6,144 remaining tokens, the runtime injects the catalog reminder. At exhaustion it asks the model to save notes and request a reset. The fallback buffer is 16,384 tokens, capped at 95% of the configured context window. A reset installs fresh environment and context guidance plus at most 64,000 tokens of client developer instructions. It does not request a summary or carry previous user/tool history into the new window.

Tools, shell sessions, and Code Mode storage remain alive across a context reset. The runtime writes an immutable archive before discarding old context, then commits its reference and the successor identity as a retained execution step. A failed archive write keeps the old window intact; a lost acknowledgement replays the saved transition. Uncommitted archives are not exposed by history tools. Successful replayed reset calls are recognized from their tool results. Window identity and archive references are restored from retained context on reload. `context_history.read_item` recovers image and opaque tool-output parts as typed content, alongside the requested text range.

## Tool compatibility and host boundaries

Native and embedded hosts default to Code Mode. Node, browser Worker, and QuickJS hosts now share the native `exec`/`wait` schemas and typed tool declarations. Cells yield by timeout or `yield_control()`, stream notifications, resume without duplicating output, and preserve original nested-call IDs. Session cancellation terminates their work; a host restart invalidates old cell IDs without replaying side effects. MCP images/audio and owned timers work across all three evaluators. Existing custom Rust hosts opt into resumable cells explicitly. The shell, patch, plan, and image contracts use the existing Codex-compatible implementations. Direct-only tools are now also fenced at nested dispatch. Native Code Mode uses QuickJS; its description names a JavaScript context instead of claiming V8.

Unsupported custom hosts retain provider compaction. Browser hosts may explicitly select CSP-compatible direct tools. Embedded cells retain ownership of unawaited nested tool calls until completion or cancellation; they do not orphan host-side effects when guest evaluation ends.

The existing subagent extension and SDK contracts remain unchanged: numeric IDs, `AgentTask`, `start_agent(s)`, structured results, messaging, and lifecycle controls keep their current behavior. Codex's new named collaboration lifecycle is deferred; this PR does not add named task paths, configurable conversation forks, encrypted inter-agent messaging, or the new follow-up/mailbox protocol. Context-window restoration checks the owning agent so an existing SDK fork cannot restore its parent's window identity.

Codex-only capabilities such as its sandbox approval service, skills discovery, installation/plugin management, Ultra automatic delegation, and app-server configuration are not implemented by copying their tool declarations. Embeddings continue to own their actual capabilities.

## Invalid discovery-schema recovery

This work incorporates [PR #274](https://github.com/gakonst/nanocodex/pull/274). Context resets do not supersede that fix: the next turn can encounter the same rejected schema before a reset, and compatibility transports still retain summarized history.

The transport resolves an `invalid_function_parameters` path against the exact failed request. Before checkpointing failure, the agent removes only matching definitions from saved discovery outputs, including namespace children, clears continuation, and advances the history revision. It preserves the original error and does not silently rerun the failed turn. Tests cover durable reload, corrected rediscovery, checkpoint loss, HTTP/SSE/WebSocket errors, and unrelated history.

## Validation

Focused Rust tests exercise archive-write failure, reset acknowledgement loss and replay, persistent window and item identity, pre-turn reminder delivery, retained tools/storage/cache, and the imported discovery repair. Transport and agent integration suites exercise ordinary compaction and reconnect/recovery behavior. JS contract, package, type, and runtime checks use generated WASM from the worktree. Cell checks cover the Node evaluator, real browser Worker module, QuickJS, and the WASM agent transport, including session fencing, termination, output budgets, and observer ownership. Python binding tests cover default model/effort, explicit overrides, lifecycle, snapshots, and costs. The WASM context scenario uses real workspace files and fixture provider responses to exercise notes, exact history and media recovery, Code Mode storage and cache retention through reset, and durable shutdown/reopen. Browser-host integration checks activation with and without workspace storage. Existing Node/browser WASM tests continue to exercise structured SDK and model-created children, messaging, cancellation, and host-family isolation.

The long-history benchmark passes the existing 64 MiB limit. Immutable tool-namespace metadata is cached once per profile. Cold recovery decodes payloads directly into shared allocations and restores checkpoint sharing lost during serialization. Stored state, replay receipts, and their format are unchanged.

Redundant getter, pointer-identity, metadata-construction, and private presentation unit tests were removed. Their behavior is checked at these boundaries:

| Behavior | Retained evidence |
| --- | --- |
| Astra defaults and readable Code Mode output | Real managed browser journey; JS/Python binding requests cover model-specific effort and explicit overrides. |
| Context tools, exact history/media recovery, window metadata, and cache identity | One WASM conversation executes tools, resets context, and reopens durable state using real files and fixture Responses. |
| Cold checkpoint recovery | Existing native standalone-checkpoint reopen test, release-WASM long-history memory benchmark, and real managed reconnect journey. |

Reset acknowledgement-loss and discovery-schema repair tests remain because they exercise failure and recovery boundaries that successful live turns cannot establish.

On September 6, after removing the proposed collaboration lifecycle and simplifying relay validation, the canonical worktree `pnpm dev` stack was exercised through the account browser UI and local Wrangler Workers. The existing development SMS fixture and normal ChatGPT connection flow provided account-session authorization; Astra, private egress, managed turns, and durable storage were real. Conversation `56f7952d-849a-8726-8570-70c65240a8bf` defaulted to Astra/low. The unchanged subagent protocol returned 323, and Code Mode wrote/read `astra-core-6sep-indigo` followed by LF in `/brain/alignment-core-e2e.txt`. A second turn recovered the file and answer after reconnect and a browser reload during a live 20-second yielding cell. Both required turns completed without retries or errors, and provider usage reported cache hits on ten follow-on model calls.

Two follow-on turns confirmed the marker's exact length and final LF; all four turns completed. An extra attempt to use `apply_patch` confirmed that tool is unavailable on the managed brain host; the file itself was already correct. Terminal cards displayed readable Code Mode output. Application console and Worker/socket logs were inspected, and the final reload had no application-origin warnings or errors. No provider secrets were used in browser tooling. The Vite development document has no CSP header, so this run does not establish production CSP behavior or real SMS delivery.

The file-backed experimental flow was then exercised on the same Business account in conversation `9dbd5cb5-03ce-8fd4-b32c-98a30291bdf4`. Astra wrote a progress note, called `new_context`, recovered the original user message using `context_history.search_contents` and `read_item`, loaded the live Code Mode value, and read `/brain/context-recovery.txt`. The recovery phrase was absent from the note, so recovering it required the exact archived history. The turn completed without retries or errors, with cache hits before and after reset. An earlier probe exposed Astra's reserved `history` schema; the local namespaces described above resolve that provider rejection.

After the runtime was reconstructed and the browser reconnected, a second turn listed and read the saved note, appended a line, searched for it, and recovered it using negative line indices. It listed the archived windows and original user items, read the original request, and recovered the exact workspace file including its trailing LF. Both turns completed without retries or errors. The second turn reported 176,256 cached input tokens across ten model calls. Inspecting these results also exposed an empty live-window snapshot; direct history calls now receive the current conversation, and the existing WASM reset/reopen scenario checks live-window search alongside archive recovery.
