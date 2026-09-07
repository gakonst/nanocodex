# Codex runtime alignment

Reference: [openai/codex ac192cd7937b0d73edc6dffe009940ae53782dd4](https://github.com/openai/codex/tree/ac192cd7937b0d73edc6dffe009940ae53782dd4/codex-rs), fetched September 6, 2026. The implementation follows this revision; it does not depend on an unpinned checkout at build time.

## Model and request policy

- New SDK, Python, native CLI, managed, and account conversations default to `gpt-6-astra` with `low` reasoning. Existing managed agents retain their settings. Sponsored homepage sessions retain their explicitly selected Luna policy.
- Model selection in Rust builders resolves the catalog's default effort: Astra/Sol `low`, Terra/Luna `medium`. An explicitly selected effort wins regardless of builder order.
- Astra's developer prompt comes from `models-manager/models.json`, with only the Nanocodex identity substituted. Explicit replacement and additive instructions remain supported. The shared GPT-5.6 prompt already matches the upstream template.
- Responses Lite requests keep `parallel_tool_calls: false`, encrypted reasoning, and no default reasoning summary. Stable session cache keys and immutable instruction/tool prefixes survive follow-on turns, reconnects, and replay.
- `client_metadata["x-codex-turn-metadata"]` carries session/thread/turn identity, request kind, and effective `tool_namespaces_info`. The removed `code_mode_tool_names` inventory is no longer sent.

## Tool compatibility and host boundaries

Native and embedded hosts default to Code Mode. Node, browser Worker, and QuickJS hosts now share the native `exec`/`wait` schemas and typed tool declarations. Cells yield by timeout or `yield_control()`, stream notifications, resume without duplicating output, and preserve original nested-call IDs. Session cancellation terminates their work; a host restart invalidates old cell IDs without replaying side effects. MCP images/audio and owned timers work across all three evaluators. Existing custom Rust hosts opt into resumable cells explicitly. The shell, patch, plan, and image contracts use the existing Codex-compatible implementations. Direct-only tools are now also fenced at nested dispatch. Native Code Mode uses QuickJS; its description names a JavaScript context instead of claiming V8.

Browser hosts may explicitly select CSP-compatible direct tools. Embedded cells retain ownership of unawaited nested tool calls until completion or cancellation; they do not orphan host-side effects when guest evaluation ends.

The existing subagent extension and SDK contracts remain unchanged: numeric IDs, `AgentTask`, `start_agent(s)`, structured results, messaging, and lifecycle controls keep their current behavior. Codex's new named collaboration lifecycle is deferred; this PR does not add named task paths, configurable conversation forks, encrypted inter-agent messaging, or the new follow-up/mailbox protocol.

Codex-only capabilities such as its sandbox approval service, skills discovery, installation/plugin management, Ultra automatic delegation, and app-server configuration are not implemented by copying their tool declarations. Embeddings continue to own their actual capabilities.

## Invalid discovery-schema recovery

This work incorporates [PR #274](https://github.com/gakonst/nanocodex/pull/274). Provider compaction remains the context-management policy. Experimental context windows and workspace history/notes are split into a follow-up PR.

The transport resolves an `invalid_function_parameters` path against the exact failed request. Before checkpointing failure, the agent removes only matching definitions from saved discovery outputs, including namespace children, clears continuation, and advances the history revision. It preserves the original error and does not silently rerun the failed turn. Tests cover durable reload, corrected rediscovery, checkpoint loss, HTTP/SSE/WebSocket errors, and unrelated history.

## Validation

Transport and agent integration suites exercise provider compaction, reconnect/recovery, and the imported discovery repair. JS contract, package, type, and runtime checks use generated WASM from the worktree. Cell checks cover the Node evaluator, real browser Worker module, QuickJS, and the WASM agent transport, including session fencing, termination, output budgets, and observer ownership. Python binding tests cover default model/effort, explicit overrides, lifecycle, snapshots, and costs. Existing Node/browser WASM tests continue to exercise structured SDK and model-created children, messaging, cancellation, and host-family isolation.

The long-history benchmark passes the existing 64 MiB limit. Immutable tool-namespace metadata is cached once per profile. Cold recovery decodes payloads directly into shared allocations and restores checkpoint sharing lost during serialization. Stored state, replay receipts, and their format are unchanged.

Redundant getter, pointer-identity, metadata-construction, and private presentation unit tests were removed. Their behavior is checked at these boundaries:

| Behavior | Retained evidence |
| --- | --- |
| Astra defaults and readable Code Mode output | Real managed browser journey; JS/Python binding requests cover model-specific effort and explicit overrides. |
| Cold checkpoint recovery | Existing native standalone-checkpoint reopen test, release-WASM long-history memory benchmark, and real managed reconnect journey. |

Discovery-schema repair tests remain because they exercise failure and recovery boundaries that successful live turns cannot establish.

Duplicate Python runtime/thread tests are covered by the installed-wheel benchmark, which exercises sequential turns, eight concurrent agents, independent sockets, construction costs, and shutdown. Its existing thread ceiling is unchanged.

Before the compaction split, on September 6, the canonical worktree `pnpm dev` stack was exercised through the account browser UI and local Wrangler Workers. The existing development SMS fixture and normal ChatGPT connection flow provided account-session authorization; Astra, private egress, managed turns, and durable storage were real. Conversation `56f7952d-849a-8726-8570-70c65240a8bf` defaulted to Astra/low. The unchanged subagent protocol returned 323, and Code Mode wrote/read `astra-core-6sep-indigo` followed by LF in `/brain/alignment-core-e2e.txt`. A second turn recovered the file and answer after reconnect and a browser reload during a live 20-second yielding cell. Both required turns completed without retries or errors, and provider usage reported cache hits on ten follow-on model calls.

Two follow-on turns confirmed the marker's exact length and final LF; all four turns completed. An extra attempt to use `apply_patch` confirmed that tool is unavailable on the managed brain host; the file itself was already correct. Terminal cards displayed readable Code Mode output. Application console and Worker/socket logs were inspected, and the final reload had no application-origin warnings or errors. No provider secrets were used in browser tooling. The Vite development document has no CSP header, so this run does not establish production CSP behavior or real SMS delivery.
