# Codex runtime alignment

Reference: [openai/codex ac192cd7937b0d73edc6dffe009940ae53782dd4](https://github.com/openai/codex/tree/ac192cd7937b0d73edc6dffe009940ae53782dd4/codex-rs), fetched September 6, 2026. The implementation follows this revision; it does not depend on an unpinned checkout at build time.

## Model and request policy

- New SDK, Python, native CLI, managed, and account conversations default to `gpt-6-astra` with `low` reasoning. Existing managed agents retain their settings. Sponsored homepage sessions retain their explicitly selected Luna policy.
- Model selection in Rust builders resolves the catalog's default effort: Astra/Sol `low`, Terra/Luna `medium`. An explicitly selected effort wins regardless of builder order.
- Astra's developer prompt comes from `models-manager/models.json`, with only the Nanocodex identity substituted. Explicit replacement and additive instructions remain supported. The shared GPT-5.6 prompt already matches the upstream template.
- Responses Lite requests keep `parallel_tool_calls: false`, encrypted reasoning, and no default reasoning summary. Stable session cache keys and immutable instruction/tool prefixes survive follow-on turns, reconnects, replay, and context resets.
- `client_metadata["x-codex-turn-metadata"]` carries session/thread/turn identity, request kind, and effective `tool_namespaces_info`. The removed `code_mode_tool_names` inventory is no longer sent. Experimental context requests additionally carry `agent_name`, `window_id`, `window_number`, `context_window_id`, and `history_ingest_requested`.

## Experimental context windows

`OpenAi::builder(auth).experimental_context(true)` is the default. Activation follows upstream's Astra, standard Codex backend, and ChatGPT Plus/Pro/ProLite gates. API keys, Business/Enterprise/Free, other models, custom providers, and hosts without the backend capability retain provider compaction. Callers can explicitly opt out with `.experimental_context(false)`.

Native and capable WASM runtimes expose the upstream `new_context`, `get_context_remaining`, `history` and `notes` contracts. `new_context` and the history/notes namespaces are direct-only; `get_context_remaining` follows normal native tool exposure and the existing direct dispatch policy for Rust-local embedded tools. Notes/history requests bind the session and agent, forward account authentication, set output truncation and encrypted-argument headers, and preserve encrypted/multimodal results. Authentication and backend errors do not disclose credentials.

At 6,144 remaining tokens, the runtime injects the catalog reminder. At exhaustion it asks the model to save notes and request a reset. The fallback buffer is 16,384 tokens, capped at 95% of the configured context window. A reset installs fresh environment and context guidance plus at most 64,000 tokens of client developer instructions. It does not request a summary or carry previous user/tool history into the new window.

Tools, shell sessions, and Code Mode storage remain alive. Window transitions are retained execution steps, and the new identity is installed only after the step is persisted. A lost acknowledgement replays the saved transition. Successful replayed reset calls are recognized from their tool results. History ingestion preserves item IDs in retained requests so notes references remain valid after recovery. Window identity is restored from retained context on reload; the context marker includes a Nanocodex window-number line for this persistence boundary.

## Tool compatibility and host boundaries

Native and embedded hosts default to Code Mode. Node, browser Worker, and QuickJS hosts now share the native `exec`/`wait` schemas and typed tool declarations. Cells yield by timeout or `yield_control()`, stream notifications, resume without duplicating output, and preserve original nested-call IDs. Session cancellation terminates their work; a host restart invalidates old cell IDs without replaying side effects. MCP images/audio and owned timers work across all three evaluators. Existing custom Rust hosts opt into resumable cells explicitly. The shell, patch, plan, and image contracts use the existing Codex-compatible implementations. Direct-only tools are now also fenced at nested dispatch. Native Code Mode uses QuickJS; its description names a JavaScript context instead of claiming V8.

Node hosts authenticate history/notes through the existing Rust subscription owner. Browser and Cloudflare hosts use a narrow, credential-free host capability; the private broker or Vite subscription relay applies the subscription gate and injects authentication. Both the development relay and production relay container allow only the pinned history/notes operations and forward their truncation/encryption headers. Unsupported custom hosts retain provider compaction. Browser hosts may explicitly select CSP-compatible direct tools. Embedded cells retain ownership of unawaited nested tool calls until completion or cancellation; they do not orphan host-side effects when guest evaluation ends.

The existing subagent extension and SDK contracts remain unchanged: numeric IDs, `AgentTask`, `start_agent(s)`, structured results, messaging, and lifecycle controls keep their current behavior. Codex's new named collaboration lifecycle is deferred; this PR does not add named task paths, configurable conversation forks, encrypted inter-agent messaging, or the new follow-up/mailbox protocol. Context-window restoration checks the owning agent so an existing SDK fork cannot restore its parent's window identity.

Codex-only capabilities such as its sandbox approval service, skills discovery, installation/plugin management, Ultra automatic delegation, and app-server configuration are not implemented by copying their tool declarations. Embeddings continue to own their actual capabilities.

## Invalid discovery-schema recovery

This work incorporates [PR #274](https://github.com/gakonst/nanocodex/pull/274). Context resets do not supersede that fix: the next turn can encounter the same rejected schema before a reset, and compatibility transports still retain summarized history.

The transport resolves an `invalid_function_parameters` path against the exact failed request. Before checkpointing failure, the agent removes only matching definitions from saved discovery outputs, including namespace children, clears continuation, and advances the history revision. It preserves the original error and does not silently rerun the failed turn. Tests cover durable reload, corrected rediscovery, checkpoint loss, HTTP/SSE/WebSocket errors, and unrelated history.

## Validation

Focused Rust tests exercise context eligibility, native backend request headers and context binding, encrypted outputs, reset acknowledgement loss and replay, persistent window and item identity, pre-turn reminder delivery, retained tools/storage/cache, and the imported discovery repair. Transport and agent integration suites exercise ordinary compaction and reconnect/recovery behavior. JS contract, package, type, and runtime checks use generated release WASM from the worktree. Cell checks cover the Node evaluator, real browser Worker module, QuickJS, and the WASM agent transport, including session fencing, termination, output budgets, and observer ownership. Python binding tests cover default model/effort, explicit overrides, lifecycle, snapshots, and costs. Real WASM integration checks also cover subscription and host-managed context activation, notes calls, live Code Mode storage and cache retention through reset, and window identity restored after durable shutdown/reopen. Worker and relay protocol tests check subscription gates, exact routes, bound context, private authentication, encrypted output, and rejection without credential disclosure. Existing Node/browser WASM tests continue to exercise structured SDK and model-created children, messaging, cancellation, and host-family isolation.

The long-history benchmark passes the existing 64 MiB limit. Immutable tool-namespace metadata is cached once per profile. Cold recovery decodes payloads directly into shared allocations and restores checkpoint sharing lost during serialization. Stored state, replay receipts, and their format are unchanged.

Redundant getter, pointer-identity, metadata-construction, and private presentation unit tests were removed. Their behavior is checked at these boundaries:

| Behavior | Retained evidence |
| --- | --- |
| Astra defaults and readable Code Mode output | Real managed browser journey; JS/Python binding requests cover model-specific effort and explicit overrides. |
| Context tool exposure, encrypted media, window metadata, and stable cache identity | One WASM conversation executes tools, resets context, and reopens durable state against fixture Responses/history backends. |
| Cold checkpoint recovery | Existing native standalone-checkpoint reopen test, release-WASM long-history memory benchmark, and real managed reconnect journey. |

Reset acknowledgement-loss and discovery-schema repair tests remain because they exercise failure and recovery boundaries that successful live turns cannot establish.

On September 6, after removing the proposed collaboration lifecycle and simplifying relay validation, the canonical worktree `pnpm dev` stack was exercised through the account browser UI and local Wrangler Workers. The existing development SMS fixture and normal ChatGPT connection flow provided account-session authorization; Astra, private egress, managed turns, and durable storage were real. Conversation `56f7952d-849a-8726-8570-70c65240a8bf` defaulted to Astra/low. The unchanged subagent protocol returned 323, and Code Mode wrote/read `astra-core-6sep-indigo` followed by LF in `/brain/alignment-core-e2e.txt`. A second turn recovered the file and answer after reconnect and a browser reload during a live 20-second yielding cell. Both required turns completed without retries or errors, and provider usage reported cache hits on ten follow-on model calls.

Two follow-on turns confirmed the marker's exact length and final LF; all four turns completed. An extra attempt to use `apply_patch` confirmed that tool is unavailable on the managed brain host; the file itself was already correct. Terminal cards displayed readable Code Mode output. Application console and Worker/socket logs were inspected, and the final reload had no application-origin warnings or errors. No provider secrets were used in browser tooling. The Vite development document has no CSP header, so this run does not establish production CSP behavior or real SMS delivery.

The available live Codex credential is a Business subscription. It can exercise Astra and the compatibility path, but cannot provide live evidence for the subscription-gated experimental backend.
