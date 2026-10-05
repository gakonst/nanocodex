# xAI tool provenance and host boundaries

Contracts adapted from the official [xai-org/grok-build](https://github.com/xai-org/grok-build) source checkout at revision `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`, SOURCE_REV `559751fdcec02d413e4c57c8832ab275e4f44980`. See `THIRD-PARTY-LICENSES/xai-grok.txt` (Apache-2.0).

Authoritative source paths:

- `crates/codegen/xai-grok-tools/src/implementations/grok_build/{read_file,search_replace,list_dir,grep,bash,web_fetch,web_search,task,task_output,kill_task}` and `send_subagent_message.rs`.
- `crates/codegen/xai-grok-tools/src/implementations/opencode/{write,glob}/mod.rs` supplies the explicit `write` and `glob` surfaces; these are not renamed Claude tools.
- `crates/common/xai-tool-types/src/{task,grep}.rs` defines task IDs, wait semantics and grep arguments.
- `crates/codegen/xai-grok-tools/src/implementations/{search_tool,use_tool}` and `crates/codegen/xai-grok-mcp/src/tool_name.rs` define MCP discovery, inline invocation and `server__tool` qualification.

This is an independent bounded adaptation, not a claim of complete upstream application parity. There is no Claude Messages conversion or Claude crate dependency. Read is UTF-8 text-only (1 MiB maximum); binary/PDF/image extraction and gitignore policy are not advertised. File traversal excludes symlinks, scans at most 10,000 entries and 16 MiB of searchable text, and caps rendered output. Atomic file replacement preserves existing regular-file permissions; new files start private (0600 on Unix). Concurrent hostile filesystem mutation requires host OS isolation. Grep uses Rust regex and supports the advertised context, case, multiline, output and pagination options; ripgrep-specific file type aliases are not advertised. MCP discovery uses deterministic keyword ranking instead of upstream BM25 and fetches a fresh caller-owned catalog on every call. File-delegated MCP input is not installed.

The foreground `run_terminal_cmd` schema deliberately excludes background and auto-background modes until a caller installs its own task-aware shell provider. Native `AuthorizedShell` clears the environment, authorizes each exact command, bounds captured output and deadlines, and terminates its Unix process group. It is an executor, not an OS sandbox; a host must approve command effects and provide isolation where required. No default executor, network transport, login or server discovery is installed.

Browser tools carry the host's real catalog through `XaiHostTools`; no invented upstream browser function is advertised. `XaiWeb` exposes only provider-selected `web_fetch`/`web_search`, with URL/DNS/redirect/response limits enforced by that provider. MCP and task providers retain native error flags, media, structured results and metadata. All effects receive actual session/turn/call identities. Task providers explicitly implement checkpoint/restore and own durable reconciliation, ownership checks, cancellation and child execution; checkpoint state must be persisted by the embedding alongside its own state.

`default-features = false` supplies the portable host contracts, MCP/web/task adapters and shell callback facade on native or WASM. The `native` feature adds filesystem and explicitly authorized Unix subprocess execution. Merely linking this crate installs no tool.
