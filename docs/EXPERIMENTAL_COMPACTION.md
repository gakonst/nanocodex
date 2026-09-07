# Experimental compaction

This change builds on the Astra and Code Mode alignment in [PR #275](https://github.com/gakonst/nanocodex/pull/275). It contains the context-window lifecycle, workspace history/notes storage, host integration, and reset/recovery coverage split from that PR.

## Context windows and storage

`OpenAi::builder(auth).experimental_context(true)` is the default. Astra uses experimental context windows when its host supplies workspace storage, including API-key and Business accounts. Other models and hosts without storage retain provider compaction. Callers can explicitly opt out with `.experimental_context(false)`.

Native and capable WASM runtimes expose `new_context`, `get_context_remaining`, `context_history` and `context_notes`. The reset lifecycle and tool schemas follow Codex, but Nanocodex owns their storage rather than calling Codex's subscription-gated hosted backend. Astra reserves the `history` and `notes` namespaces for its hosted schemas, so file-backed tools use `context_history` and `context_notes` with ordinary JSON arguments. History uses exact retained item IDs from list/search results rather than provider-injected short markers. `new_context` and the history/notes namespaces are direct-only; `get_context_remaining` follows normal native tool exposure and the existing direct dispatch policy for Rust-local embedded tools.

Native and Node hosts use the workspace filesystem. Browser and Cloudflare hosts accept `contextStorage: Workspace`, defaulting to their supplied filesystem. Managed agents pass their existing durable `/brain` workspace, backed by R2. There are no new bindings, migrations, provider credentials, or history/notes relay routes. Context files live under `.nanocodex/context`, scoped to session and agent. Archives contain exact serialized history; note files contain text and timestamps. These files must travel with the workspace when moving a durable session between hosts.

`context_history` recovers the current agent's earlier context windows and its live retained history. `context_notes` stores progress across resets. Existing managed `find_session` and `read_session` remain the way to search other completed conversations; they exclude the active session and cannot substitute for mid-turn context recovery. This change does not alter those tools or the existing subagent lifecycle.

At 6,144 remaining tokens, the runtime injects the catalog reminder. At exhaustion it asks the model to save notes and request a reset. The fallback buffer is 16,384 tokens, capped at 95% of the configured context window. A reset installs fresh environment and context guidance plus at most 64,000 tokens of client developer instructions. It does not request a summary or carry previous user/tool history into the new window.

Tools, shell sessions, and Code Mode storage remain alive across a context reset. The runtime writes an immutable archive before discarding old context, then commits its reference and the successor identity as a retained execution step. A failed archive write keeps the old window intact; a lost acknowledgement replays the saved transition. Uncommitted archives are not exposed by history tools. Successful replayed reset calls are recognized from their tool results. Window identity and archive references are restored from retained context on reload. `context_history.read_item` recovers image and opaque tool-output parts as typed content, alongside the requested text range.

Experimental requests carry `agent_name`, `window_id`, `window_number`, and `context_window_id` in turn metadata. Session cache identity and the immutable instruction/tool prefix survive resets. History ingestion is local; requests do not ask the provider to ingest history. Restoring a context window checks its owning agent so a fork cannot restore its parent's identity.

The discovery-schema repair in #275 remains required: a subsequent turn can encounter the same rejected schema before a reset, and other models and hosts retain provider compaction.

## Validation

Focused Rust tests cover archive-write failure, reset acknowledgement loss and replay, persistent window and item identity, pre-turn reminders, and retained tools, Code Mode storage, and cache keys. The WASM reset/reopen scenario uses real workspace files and fixture provider responses to check notes, exact history and image recovery, live-window search, and durable reconstruction. Browser-host integration checks activation with and without workspace storage.

The implementation and tests are preserved from `3c597019725f6d58a7bcfc8e80432bea1d6460a9`. The following live evidence was collected before the split on September 6, 2026.

The file-backed experimental flow was then exercised on the same Business account in conversation `9dbd5cb5-03ce-8fd4-b32c-98a30291bdf4`. Astra wrote a progress note, called `new_context`, recovered the original user message using `context_history.search_contents` and `read_item`, loaded the live Code Mode value, and read `/brain/context-recovery.txt`. The recovery phrase was absent from the note, so recovering it required the exact archived history. The turn completed without retries or errors, with cache hits before and after reset. An earlier probe exposed Astra's reserved `history` schema; the local namespaces described above resolve that provider rejection.

After the runtime was reconstructed and the browser reconnected, a second turn listed and read the saved note, appended a line, searched for it, and recovered it using negative line indices. It listed the archived windows and original user items, read the original request, and recovered the exact workspace file including its trailing LF. Both turns completed without retries or errors. The second turn reported 176,256 cached input tokens across ten model calls. Inspecting these results also exposed an empty live-window snapshot; direct history calls now receive the current conversation, and the existing WASM reset/reopen scenario checks live-window search alongside archive recovery.
