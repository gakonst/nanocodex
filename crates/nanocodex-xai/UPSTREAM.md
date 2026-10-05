# Grok Build source adaptation

This crate adapts portable Responses conversation, stream, and compaction
policies from [SpaceXAI Grok Build](https://github.com/xai-org/grok-build),
licensed under Apache License 2.0. The upstream copyright and complete license
are retained in [THIRD-PARTY-LICENSES](THIRD-PARTY-LICENSES).

- Public repository commit: `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`.
- Upstream monorepo `SOURCE_REV`: `559751fdcec02d413e4c57c8832ab275e4f44980`.
- Copyright 2023–2026 SpaceXAI.

The following files contain modified adaptations. This notice and their file
headers identify changes under Apache License 2.0 section 4(b). Paths in the
upstream column are relative to that pinned source tree.

| Local file | Upstream source | Adaptation |
| --- | --- | --- |
| `src/conversation.rs` | `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs` and `sanitize_tool_arguments` in its parent `conversation.rs` | Native JSON replaces typed conversation conversion. Preserve reasoning normalization, function call/result identity, argument sanitization on replay, and hosted-tool separation. |
| `src/stream.rs` | `crates/codegen/xai-grok-sampler/src/stream/responses.rs` | Bounded incremental SSE decoding and strict terminal completion. Incomplete output does not dispatch callbacks; upstream's incomplete tool-call salvage is deliberately omitted. |
| `src/compaction.rs` | `crates/codegen/xai-chat-state/src/compaction_utils.rs`, `crates/codegen/xai-grok-agent/src/compaction.rs`, and `crates/common/xai-grok-compaction/src/code_compaction/` | Adapt summary preparation, pinned instructions, complete tool boundaries, and model thresholds to native JSON. Retain a recent conversation tail and stage replacement until a completed, smaller summary is available. This is not the entire upstream full-replace, two-pass, memory-flush, or overflow-fitting pipeline. |
| Shared xAI model catalog | `crates/codegen/xai-grok-models/default_models.json` | Expose pinned Grok 4.6/4.5 IDs and effort choices through Nanocodex family selectors. |

`src/lib.rs` and `src/lifecycle.rs` connect these policies to the Nanocodex
lifecycle. `src/durable.rs` defines the xAI execution-policy and native checkpoint
format; the `nanocodex-durability` adapter uses Nanocodex's fenced store. Host
adapters, subagent routing, and JavaScript/WASM bindings are Nanocodex integration
code. They do not convert xAI requests through Claude Messages or wrap an
installed Grok executable.

The complete current API, recovery boundaries, and reproducible validation
commands are documented in [NANOXAI.md](../../docs/NANOXAI.md). Registration of
host tools is explicit. The adaptation does not import the upstream TUI, ACP
server, subscription credential discovery, permission engine, OS sandbox,
plugins, session database, or complete recovery policies. Live account access
and provider behavior are not established by loopback protocol fixtures.

Upstream also contains separately licensed source ports and vendored code,
including OpenAI Codex and sst/opencode tool implementations. Its
[`THIRD-PARTY-NOTICES`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/THIRD-PARTY-NOTICES)
and
[`xai-grok-tools/THIRD_PARTY_NOTICES.md`](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md)
remain the provenance sources for those components. This crate's retained
Apache license covers the adapted first-party material listed above; it does
not relicense unrelated upstream components.
