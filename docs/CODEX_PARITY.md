# Codex parity ledger

This ledger records the review of all 1,715 commits in three contiguous
exclusive local-checkout ranges

```text
openai/codex@35eaf3ffb0bf2001486c68c47a3d946b34d16634
    ..openai/codex@50ea8fd411422b3f7bc906bcde6c1c4432019a2e
    ..openai/codex@13bc770eaf0ad8548776bde59c3d6e5316406279
```

The review used the clean local Codex checkout at the range head. The earlier
`35eaf3ff..7ada37a1` range contains 555 commits. Its first 37 remain expanded
below; the following 279 are classified individually in
[`codex-parity/8431dc59-3418498f.md`](codex-parity/8431dc59-3418498f.md), and
the final seven are classified in
[`codex-parity/3418498f-be2e4afc.md`](codex-parity/3418498f-be2e4afc.md). The
latest 232 are classified in
[`codex-parity/be2e4afc-7ada37a1.md`](codex-parity/be2e4afc-7ada37a1.md).

The middle `7ada37a1..50ea8fd4` range contains 802 commits. Every commit is
classified individually in eight ordered appendices:
[`part 01`](codex-parity/7ada37a1-50ea8fd4-part01.md),
[`part 02`](codex-parity/7ada37a1-50ea8fd4-part02.md),
[`part 03`](codex-parity/7ada37a1-50ea8fd4-part03.md),
[`part 04`](codex-parity/7ada37a1-50ea8fd4-part04.md),
[`part 05`](codex-parity/7ada37a1-50ea8fd4-part05.md),
[`part 06`](codex-parity/7ada37a1-50ea8fd4-part06.md),
[`part 07`](codex-parity/7ada37a1-50ea8fd4-part07.md), and
[`part 08`](codex-parity/7ada37a1-50ea8fd4-part08.md). Mechanical validation
proves exact ordinal and SHA coverage for all 802 commits.

The latest `50ea8fd4..13bc770e` range contains 358 commits. Every commit is
classified individually in five ordered appendices:
[`part 09`](codex-parity/50ea8fd4-d58d0e58-part09.md),
[`part 10`](codex-parity/50ea8fd4-d58d0e58-part10.md),
[`part 11`](codex-parity/50ea8fd4-d58d0e58-part11.md),
[`part 12`](codex-parity/50ea8fd4-d58d0e58-part12.md), and
[`part 13`](codex-parity/d58d0e58-13bc770e-part13.md). Mechanical validation
proves exact ordinal and SHA coverage for all 358 commits. The global reviewed
checkpoint is now
`openai/codex@13bc770eaf0ad8548776bde59c3d6e5316406279`.

The classifications mean:

- `port`: the Nanocodex-relevant invariant is implemented and has concrete
  code and regression evidence below. A mixed commit may still contain
  Codex-only app-server or provider plumbing; that excluded portion is named.
- `evaluate`: the change is relevant, but the current tree does not contain
  enough direct regression or benchmark evidence to call it adopted.
- `defer`: the change is relevant and intentionally postponed.
- `out-of-scope`: the change belongs to a surface Nanocodex deliberately does
  not own, or to an implementation pipeline it does not have.

Classification is not implementation by analogy. A `port` row must link to the
concrete evidence below; `evaluate`, `defer`, and `out-of-scope` are not parity
claims.

| Classification | Count |
| --- | ---: |
| `port` | 109 |
| `evaluate` | 205 |
| `defer` | 77 |
| `out-of-scope` | 1,324 |
| Total | 1,715 |

## First range: `35eaf3ff..8431dc59`

| # | Codex commit | Classification | Decision |
| ---: | --- | --- | --- |
| 1 | `312caf176a8f` Seed realtime V3 sessions with initial text items | `port` | `P31`: the experimental Realtime library accepts bounded typed initial items, rejects them for direct V2 sessions, and serializes exact role-bearing Frameless WebRTC bootstrap messages. Codex app-server request plumbing remains out-of-scope. |
| 2 | `643de86a190a` Add audio output support to dynamic tools and code mode | `defer` | Preserve the existing model-visible Code Mode audio shape, but do not claim the commit's full dynamic-tool, app-server, history, analytics, and model-modality support. The supported model contract remains text/image, so broader audio input/output stays deferred. |
| 3 | `0fb559f0f6e2` Support legacy views for paginated thread history | `out-of-scope` | This is app-server projection, resume, and pagination behavior. It is distinct from row 22: Nanocodex consumes legacy Codex rollouts and deliberately rejects a canonical paginated rollout rather than implementing app-server views. |
| 4 | `9dc372fbafb1` Avoid cloning thread data when rendering transcripts | `evaluate` | Nanocodex uses shared `Arc` transcript entries, but there is no allocation regression proving that resume-to-transcript construction avoids the clones removed by this Codex change. Profile the retained resume path before claiming adoption. |
| 5 | `3dd3c5d08ac8` Use the Markdown collector as the streaming source of truth | `port` | `P1`: Nanocodex keeps one canonical Markdown source, mutates it during streaming, and seals exact terminal source; parity tests and frame benchmarks cover the path. |
| 6 | `78fd2f2b2840` Start side conversations without replaying inherited turns | `port` | `P2`: `/btw` forks inherited model context while constructing a fresh, isolated side-pane transcript. No app-server `excludeTurns` API is imported. |
| 7 | `4d7a5c7c7394` Avoid liveness races when starting side conversations | `port` | `P3`: the side pane is selectable immediately, the direct `agent.fork()` result is authoritative, and generation IDs prevent stale open/failure updates from mutating a reopened pane. Codex app-server metadata reads remain out-of-scope. |
| 8 | `54994582b189` Avoid cloning buffered TUI history lines | `evaluate` | The Nanocodex Ratatui consumer has a different demand-rendered transcript pipeline. Add an allocation profile for queued history insertion before changing ownership solely to resemble Codex. |
| 9 | `3e2f79727a4e` Avoid retaining decoded MCP images in history cells | `evaluate` | The Nanocodex TUI currently summarizes MCP tool results instead of intentionally owning a decoded-image history cell, but there is no retained-memory regression proving the boundary. Validate representative MCP image traces before calling this a port. |
| 10 | `aa982319c264` Speed up TUI Markdown layout | `evaluate` | Nanocodex has its own Markdown/table renderer and representative frame benchmarks. Codex's bulk table shrinking, flattened styled-line reuse, and forward hyperlink scan have not been differentially benchmarked here. |
| 11 | `74bfbda9b587` Keep incremental rendering with visualization context | `out-of-scope` | Nanocodex does not expose Codex's inline visualization-context resolver, so its directive-sensitive fallback pipeline is absent. |
| 12 | `854a82dbfda6` Track TUI command completion separately from output | `port` | `P4`: `ToolCall` establishes running state and only `ToolResult` establishes terminal state; cancellation and continued shell tests cover the lifecycle. App-server output-delta plumbing is not imported. |
| 13 | `d0516cfe4ba0` Avoid buffering replay-irrelevant thread notifications | `out-of-scope` | Nanocodex has no app-server thread-notification replay buffer or approval/realtime state machine. Contractual typed events are consumed directly by each library client. |
| 14 | `6a54efb76bf5` Cache finalized Markdown history rendering | `port` | `P5`: finalized Markdown is width-cached, invalidated on source/width changes, parity-tested against fresh rendering, and covered by focused frame benchmarks. Visualization-specific invalidation is absent with the visualization surface. |
| 15 | `c86b1be3cdbe` Avoid cloning file changes in TUI diff rendering | `evaluate` | The Nanocodex patch renderer has a 16-file frame benchmark, but no allocation measurement demonstrates the consume/borrow optimization in this commit. |
| 16 | `7844386e3de0` Backfill completion items only for the active exec turn | `out-of-scope` | Codex's headless exec consumes a shared app-server event stream and performs `thread/read` backfill. Nanocodex agents own independent typed event streams and do not perform completion backfill requests. |
| 17 | `5a208c1fc353` Persist names for paginated threads | `out-of-scope` | Paginated app-server thread state, naming, search, and compatibility indexes are not owned by the library SDK. |
| 18 | `a97ae65362e8` Remeasure dynamic cells in the transcript overlay | `evaluate` | Nanocodex invalidates mutable transcript heights and avoids the outer Markdown height cache, but it lacks a focused regression for a committed dynamic cell growing after insertion. Add that test before treating the Codex overlay fix as adopted. |
| 19 | `678157acaa81` Avoid redundant TUI subagent metadata requests | `out-of-scope` | This optimizes Codex app-server thread/status reads and its generic subagent navigator. Nanocodex uses direct cloneable agent handles and application-owned `/btw`; it issues none of these metadata requests. |
| 20 | `bf3c1972b7d0` Migrate legacy exec policy allow rules | `out-of-scope` | Nanocodex intentionally has no approval or exec-policy subsystem and therefore no legacy policy migration. |
| 21 | `2deed3fb9c00` Preserve zsh tied PATH exports in shell snapshots | `out-of-scope` | Nanocodex does not capture or restore Codex shell snapshots. It starts tools from an explicit sanitized process environment. |
| 22 | `86102db5a1a7` Reject unsupported history modes when loading rollouts | `port` | `P6`: the first canonical session metadata record accepts legacy mode and returns a typed unsupported error for paginated mode; copied later metadata is not promoted to canonical state. |
| 23 | `221a34102929` Remove unused Rust helpers | `out-of-scope` | This is repository-local dead-code and dependency cleanup across Codex packages, not a portable runtime invariant. Nanocodex cleanup is governed by its own crate graph and lint gates. |
| 24 | `2244d11a1d9e` Track inline visualization directives during streaming | `out-of-scope` | The inline visualization directive state machine is absent together with the visualization-context resolver from row 11. |
| 25 | `ada5a79ddf51` Avoid cloning deferred TUI lifecycle payloads | `out-of-scope` | The changed paths are Codex app-server replay, approval, elicitation, and interrupt queues. Nanocodex does not carry that queueing architecture. |
| 26 | `eceb3eeaf3a6` Cache TUI flex heights across frame passes | `evaluate` | Nanocodex caches transcript entry and total heights and benchmarks complete frames, but it has no evidence that sizing, drawing, and cursor placement repeat the same flex measurement in its layout. Profile first. |
| 27 | `2661d8577ee1` Parallelize TUI bootstrap requests | `out-of-scope` | `model/list`, `configRequirements/read`, hooks, and the global app-server config queue are provider/app-server startup surfaces Nanocodex does not expose. |
| 28 | `20440a0833c4` Render streamed command output through preview iterators | `evaluate` | Nanocodex has cached, viewport-oriented transcript rendering, but it does not implement Codex's aggregated-output preview iterator contract. Compare representative long command traces before introducing it. |
| 29 | `ef6b597f416e` Keep streamed command output bounded in the TUI | `out-of-scope` | Nanocodex does not feed live shell output deltas into the TUI; its tool runtime bounds capture while producing the eventual typed result. That existing runtime bound is not a port of Codex's live-preview buffer. |
| 30 | `1e20272fa5a4` Avoid cloning thread history for token usage replay | `out-of-scope` | This changes app-server resume/fork response construction and persisted token-usage replay. Nanocodex reports usage on owned turns and has no equivalent reconstruction request. |
| 31 | `f944456d81f3` Animate Max and Ultra reasoning effort changes | `out-of-scope` | This is a Codex TUI cosmetic animation. The accepted Nanocodex TUI lifecycle is not being rewritten for parity, and reasoning effort remains an ordinary typed turn policy. |
| 32 | `28aacbb9d9e4` Avoid cloning hyperlink text during TUI rendering | `evaluate` | Semantic link copy is benchmarked in Nanocodex, but borrowed `Line` conversion has not been allocation-profiled against the current renderer. |
| 33 | `b6de5b524cdc` Use app-server skill metadata directly in the TUI | `out-of-scope` | Skills and app-server metadata are explicit Nanocodex non-goals. |
| 34 | `5c18cc0acc37` Clear stale Guardian reviews when turns end | `out-of-scope` | Guardian review and approval status are not Nanocodex lifecycle state. |
| 35 | `9a7e823e5be3` Extend second-based latency histogram buckets | `evaluate` | Codex added 12, 15, 20, 30, 60, and 120 second boundaries. Nanocodex currently emits tracing data and lets the embedding subscriber/exporter own metric aggregation; evaluate these buckets with the planned metrics consumer rather than silently adding a second observability policy. |
| 36 | `7e51abbbd122` Avoid rendering generated images twice | `port` | `P7`: generated-image output tells the model the image is already displayed, and the image-generation test asserts the hint. Codex provider/feature availability plumbing remains out-of-scope. |
| 37 | `8431dc590a5b` Stop retrying turns with invalid tool images | `port` | `P8`: invalid-image failures become a typed terminal Responses error without rewriting tool history or issuing an image-replacement retry. |

## Port evidence

### P1 — canonical streaming Markdown source

[`MarkdownContent`](../bin/nanocodex/src/tui/transcript.rs) owns one raw
`source`. `append` and `append_reasoning` mutate that source, while `finalize`
installs the exact terminal message and disables streaming healing. The tests
`streaming_plain_markdown_append_matches_a_fresh_parse` and
`assistant_markdown_is_healed_and_rendered_while_streaming` compare incremental
and canonical rendering. The
[`tui_markdown/healed_streaming_frame/120x40` benchmark](../bin/nanocodex/benches/tui_render.rs)
covers the changed-frame cost.

### P2 — inherited model context, fresh side transcript

[`App::begin_btw`](../bin/nanocodex/src/tui/app.rs) creates a new empty
`Conversation`; [`open_btw`](../bin/nanocodex/src/tui/mod.rs) obtains the model
branch through `agent.fork()` and routes its events only to that pane. The test
`btw_conversation_isolated_and_focus_toggles` covers independent UI state. The
[fork record](../benchmarks/fork_results.md) additionally records a real PTY
trial where `/btw` could read inherited model context without leaking branch
activity back into the root.

### P3 — side-pane liveness follows the fork result

`begin_btw` focuses and renders the pane before the asynchronous fork finishes.
`open_btw` treats the direct library fork result as authoritative instead of
waiting for a second lifecycle notification. The tests
`btw_renders_as_a_side_by_side_focused_pane` in
[`view.rs`](../bin/nanocodex/src/tui/view.rs) and
`stale_btw_updates_do_not_reach_a_reopened_pane` in
[`app.rs`](../bin/nanocodex/src/tui/app.rs) cover immediate availability and
generation-scoped stale updates.

### P4 — command output is not command completion

[`Conversation::on_tool_call`](../bin/nanocodex/src/tui/app.rs) creates running
tool state. Only `on_tool_result` supplies completed, failed, or cancelled
state, and continued shell sessions remain running while their result still
contains a session ID. The tests
`terminal_transport_resolves_the_original_command_without_leaking` and
`cancelled_turn_is_terminal_without_rendering_a_generic_error` cover continued
and interrupted commands.

### P5 — finalized Markdown render cache

[`MarkdownContent::with_rendered`](../bin/nanocodex/src/tui/transcript.rs)
retains a `RenderedText` for the current width and invalidates it when the
source or width changes. The tests
`long_styled_markdown_line_uses_parity_checked_cached_rows` and
`finalized_assistant_renders_markdown_and_reflows_tables_by_width` compare the
cached path with fresh rendering. The
[`tui_markdown/finalize_and_first_frame/120x40` benchmark](../bin/nanocodex/benches/tui_render.rs)
keeps the public frame path measured.

### P6 — canonical rollout history-mode validation

[`materialize_rollout`](../crates/nanocodex-agent/src/rollout/load.rs) validates
`history_mode` only on the first canonical `session_meta`: missing or `legacy`
is accepted, other strings return `io::ErrorKind::Unsupported`, and later
copied metadata cannot become the canonical workspace record. The unit test
`rejects_rollouts_with_paginated_history` exercises the unsupported path.

### P7 — generated images are not rendered twice

[`image_output_hint`](../crates/nanocodex-tools/src/image_generation/mod.rs)
tells the model that the generated image is already displayed and need not be
repeated as Markdown or a file link. The test
`generation_uses_codex_images_request_and_persists_result` in
[`image_generation/tests.rs`](../crates/nanocodex-tools/src/image_generation/tests.rs)
asserts that model-visible hint alongside the saved artifact.

### P8 — invalid tool images fail terminally

The streamed Responses boundary maps `invalid_image` to
[`ResponsesError::InvalidImageRequest`](../crates/nanocodex-oai-api/src/transport/error.rs).
The integration test `prepares_images_and_stops_on_invalid_image_requests` in
[`model/tools/mod.rs`](../crates/nanocodex-agent/tests/it/model/tools/mod.rs)
serves one invalid-image failure and asserts the typed terminal error. There is
no history mutation or fallback request that substitutes `Invalid image`.

### P9 — borrowed Responses payloads

[`ResponseCreate`](../crates/nanocodex-oai-api/src/responses/request.rs) borrows
stable request-profile, configuration, history, and input state.
[`EncodedRequest`](../crates/nanocodex-oai-api/src/transport/wire.rs) retains
the serialized frame once for replayable Tower attempts. The
[`tower_responses`](../crates/nanocodex-oai-api/benches/tower_responses.rs)
benchmarks measure construction, serialization, and retry cloning separately.

### P10 — copy-on-write typed history

[`ResponseHistory`](../crates/nanocodex-oai-api/src/responses/request.rs) stores
immutable shared segments and a copy-on-write tail. Compaction and fork
snapshots share committed prefixes; suffix replacement copies only the
affected boundary. Unit regressions cover cross-segment iteration and prefix
sharing, and
[`fork_history`](../crates/nanocodex-oai-api/benches/fork_history.rs) compares
representative checkpoint sizes.

### P11 — authoritative compaction installation

[`ManagedSessionState::install_compaction`](../crates/nanocodex-oai-api/src/session/state.rs)
is the single typed history replacement. The agent's pre-turn and mid-turn
entry points in
[`model/run/state.rs`](../crates/nanocodex-agent/src/model/run/state.rs) only
supply the appropriate retained context. Session and agent compaction tests
cover atomic replacement, failed-operation rollback, manual compaction, and
continuation ordering.

### P12 and P27 — detached subprocesses and tree cleanup

[`spawn_pipes`](../crates/nanocodex-tools/src/shell/process.rs) gives
non-interactive children null stdin. The same module owns process-group
termination on Unix and descendant termination on Windows; Code Mode and shell
cancellation retain the guard until output drains. The established local MCP
stdio transport reuses that guard and reaps its child on close. Shell/process,
agent-cancellation, and `dropping_mcp_terminates_stdio_descendants` regressions
cover timeout, cancellation, continued sessions, and descendant cleanup.
Codex's exact Windows job-object implementation is not treated as an API
requirement.

### P13 — stable response item IDs

[`assign_missing_response_item_ids`](../crates/nanocodex-oai-api/src/session/context.rs)
assigns client IDs once and preserves server IDs. History construction,
compaction, resume, and request serialization all pass through that invariant.
The tests `history_assigns_ids_once_and_preserves_them_across_checkpoints` and
`request_serialization_matches_codex_item_id_policy_without_mutating_history`
cover retention and provider-facing filtering.

### P14 — rejected turns do not close the TUI

[`start_turn`](../bin/nanocodex/src/tui/mod.rs) converts prompt-admission
failure into `TurnTraceRejected` and `TurnFinished` updates and returns control
to the worker loop. `rejected_turns_do_not_stop_the_tui_worker` submits two
consecutive rejected turns and proves the worker remains available.

### P15 — missing checkpoint replay

The typed retry policy in
[`tower/middleware/retry.rs`](../crates/nanocodex-oai-api/src/tower/middleware/retry.rs)
recognizes `previous_response_not_found`, removes the continuation checkpoint,
and immediately retries the owned attempt with complete authoritative history.
`missing_stored_checkpoint_replays_local_history_once` and
`active_boundary_fork_sends_tool_and_steer_delta_then_replays_on_checkpoint_miss`
cover ordinary and forked sessions.

### P16 — provider-owned post-response accounting

[`ContextManager`](../crates/nanocodex-oai-api/src/session/context.rs) updates
its active token count from completed provider usage and estimates only
unreported pending context. The model loop does not perform an additional
whole-history estimate after sampling. Context and compaction regressions cover
threshold crossings and missing-usage fallback.

### P17 — bounded syntax highlighting

[`highlighted_code_lines`](../bin/nanocodex/src/tui/markdown.rs) falls back to
plain rendering when any source line exceeds 4 KiB.
`skips_highlighting_for_oversized_source_lines` covers exact content and style,
while
[`tui_markdown/syntax_fallback_oversized_line_1m`](../bin/nanocodex/benches/tui_render.rs)
keeps the pathological retained-trace shape measured.

### P18 — shared request construction

The request profiles and immutable prefixes in
[`responses/request.rs`](../crates/nanocodex-oai-api/src/responses/request.rs)
are shared, and generation/compaction builders borrow configuration and
history. The request construction benchmarks distinguish full-history,
incremental, and serialized retry costs so clone reductions remain measurable.

### P19 — compaction time in turn profiles

[`RunStats`](../crates/nanocodex-agent/src/model/telemetry.rs) accumulates
`compaction_duration_ns` on both success and failure while retaining it as a
subset of aggregate model time. The public typed
[`RunMetrics`](../crates/nanocodex-oai-api/src/events/data.rs) and raw JSONL
carry the same value. The automatic-compaction integration regression decodes
and compares both projections.

### P20 — MCP HTTP user agent

[`resolve_http_headers`](../crates/nanocodex-tools/src/mcp/client.rs) installs
`nanocodex-mcp-client/<version>` in both the HTTP client's defaults and RMCP's
request headers; explicit caller configuration wins. The focused header test
covers default and override behavior.

### P21 — complete errors, separate retry advice

[`ResponsesServiceError`](../crates/nanocodex-oai-api/src/tower/service_error.rs)
retains the full typed source, failure phase, attempt, and connection
generation. Retry class and optional server delay are separate advice fields,
so scheduling metadata never replaces provider detail. Retry tests cover
server delay, exhaustion, terminal errors, and checkpoint recovery.

### P22 — nonblocking Ratatui interruption

The terminal loop sends a cancellation command and redraws; the independent
[`AgentWorker`](../bin/nanocodex/src/tui/mod.rs) awaits agent cancellation.
`second_escape_sends_cancel_for_the_focused_turn` verifies that input handling
only queues the command rather than awaiting lifecycle work.

### P23 — forks use the active typed history

[`prepare_checkpoint`](../crates/nanocodex-agent/src/model/run/mod.rs) captures
committed typed history, active continuation policy, stable prefix, and opaque
provider checkpoint together. Fork tests cover healthy incremental
continuation and missing-checkpoint full replay without exposing a history
mode or response ID to the caller.

### P24 — bounded Responses Lite Code Mode metadata

[`ToolRuntime::model_contract`](../crates/nanocodex-tools/src/runtime/execution.rs)
builds the direct tool prefix and deterministic nested-name map from the same
registry snapshot. Request serialization emits the structured
`x-codex-turn-metadata` compatibility header, including MCP namespaces, while
the WebSocket Responses Lite marker remains bounded. Unit and mock-server
warmup regressions decode and assert the metadata.

### P25 — idempotent OpenTelemetry shutdown

[`ObservabilityGuard::shutdown`](../crates/nanocodex-observability/src/lib.rs)
takes ownership of its provider before flushing and closing it. The combined
formatting/OTLP regression calls shutdown twice, then drops the guard, while
asserting one successful export.

### P26 — focus does not replace input

Focus events in [`UiModel`](../bin/nanocodex/src/tui/mod.rs) update terminal
focus, notification, resize, and redraw state without touching the composer.
`focus_gain_redraws_and_clears_an_unfocused_completion_notification` now also
asserts that an unfinished draft and cursor survive the round trip.

### P28 — stable ten-second Code Mode yield

The model-visible exec description, wait schema, parser, and runtime in
[`code_mode`](../crates/nanocodex-tools/src/code_mode/mod.rs) agree on the
ten-second default across platforms. Code Mode timing/parser tests cover the
default and explicit override. Codex's experimental 30-second buffered-exec
feature is intentionally outside the narrow runtime.

### P29 — MCP credentials stay on their origin

Both Streamable HTTP and OAuth MCP clients use
[`same_origin_redirect_policy`](../crates/nanocodex-tools/src/mcp/mod.rs).
Redirects retain custom secret headers only on the original origin and stop
before a cross-origin target. The two-listener regression
`oauth_headers_do_not_follow_cross_origin_redirects` proves that a custom API
key never reaches the redirected server.

### P30 — bounded local MCP messages

[`McpStdioTransport`](../crates/nanocodex-tools/src/mcp/stdio.rs) decodes the
established MCP JSONL stream with RMCP's compatibility codec while enforcing an
8 MiB frame bound. `stdio_message_reader_rejects_an_oversized_frame` exercises
the exact boundary without allocating a production-sized fixture. The newer
MCP discovery protocol, pagination, and dual lifecycle mode from the upstream
commit are not imported.

### P31 — bounded Frameless initial items

[`RealtimeSessionBuilder`](../crates/nanocodex-oai-api/src/realtime.rs) exposes
owned role-bearing startup items and applies Codex's V3-only, 128-item, and
8,192-estimated-token limits before transport work. The ChatGPT WebRTC call
serializes developer/user text as `input_text`, assistant text as
`output_text`, and omits the field when empty. Focused wire and policy tests
cover the public contract; the experimental voice lifecycle forwards the same
items without introducing app-server protocol types.

### P40 — serialized, recoverable MCP OAuth refresh

[`OAuthRuntime`](../crates/nanocodex-tools/src/mcp/oauth.rs) refreshes
known-expiring credentials before startup and every MCP operation. The
transaction in
[`oauth/refresh.rs`](../crates/nanocodex-tools/src/mcp/oauth/refresh.rs)
continues after caller cancellation, bounds lock and provider waits, locks
across the authoritative reread, provider exchange, and durable save, adopts a
refresh won by another runtime, preserves refresh tokens and scopes omitted by
the provider, and restores the prior in-memory credential if persistence
fails. Only a typed refresh-token rejection requires authorization; transient
failures remain ordinary retryable errors. The CLI store implements
Codex-compatible per-credential filesystem locks, allowing Codex and
Nanocodex processes sharing `CODEX_HOME` to serialize rotating-token
refreshes. The exact `rmcp 3.0.0` upgrade supplies one-shot server-401 refresh
and retry behavior.

Focused regressions cover omitted response fields, transient and rejected
refreshes, concurrent runtimes, caller cancellation, persistence rollback, and
a real HTTP 401 followed by token refresh and one authenticated retry. This
ports the refresh invariants from Codex `6962a2ecae` and the OAuth-relevant
portion of `a05bcda3db`; unrelated RMCP protocol and server surfaces remain
outside this claim.

### P41 — recoverable Frameless WebRTC sidebands

[`run_socket`](../crates/nanocodex-oai-api/src/realtime.rs) keeps the owned
WebRTC peer and media channels alive when a Frameless sideband ends
unexpectedly. It reconnects after Codex's capped 200 ms, 400 ms, 800 ms, ...,
5 s delays, resets the rapid-disconnect counter after 30 stable seconds, treats
HTTP 404 and 410 as an ended call, keeps microphone and playback traffic live
while reconnecting, and retries the one interrupted text or agent-output
command. A normal close remains terminal; V1 and direct V2 behavior is
unchanged.

The same port bounds the active transcript to its newest 8 KiB and bounds each
escaped Realtime delegation field to 4 KiB, retaining the start of the user
input and the end of the transcript. The focused regressions
`frameless_sideband_reconnects_without_ending_the_session`,
`interrupted_text_command_resolves_after_replay`,
`active_transcript_retains_a_bounded_suffix`,
`sideband_reconnect_delay_backs_off_and_caps`, and
`delegation_fields_keep_the_codex_bounded_edge` cover those invariants. This
adopts Codex `ecb8013dfa82120d11b02e3b68b7d3a3afd79d39` without importing its
app-server lifecycle.

### P42 — caller-owned Realtime calls and complete live control surface

[`RealtimeSessionBuilder::connect_with_sdp`](../crates/nanocodex-oai-api/src/realtime.rs)
creates a caller-owned V1 or V3 WebRTC call and returns the answer SDP directly
before its authenticated sideband finishes joining. The embedding owns the peer
and all media. [`OpenAi::attach_realtime_call`](../crates/nanocodex-oai-api/src/openai/mod.rs)
attaches to an already-negotiated V1 or V3 call through a configuration-free
builder: it performs no call-create request and sends no `session.update`.
Closing either external mode detaches the control socket without sending
Frameless `session.close`; owned WebSocket/WebRTC sessions retain their existing
close behavior.

The same transport port uses Codex's direct V3 `/v1/live` HTTP and WebSocket
routes, V1 Quicksilver/AVAS query contract, backend JSON versus direct multipart
call bodies, direct-V3 `session.started` readiness gate, V3 delegation
`ack_filler`, and versioned sideband call-ID routing.
The low-level session exposes Codex's role-bearing text and speakable-context
append operations, ignores empty speech, and applies the same 1,000-token bound
after backend/item prefixing. The high-level voice session forwards those two
bounded controls without introducing app-server request types.

Focused regressions cover direct V1/V3 endpoint and multipart shapes,
direct V3 readiness, SDP-before-sideband ordering, pending-join cancellation,
configuration-free existing-call attachment, reconnect/transcript continuity,
terminal 410 handling, external detach semantics, and output truncation.
[`realtime_external.rs`](../examples/realtime_external.rs) is the concrete
caller-owned SDP/existing-call consumer. This adopts Codex
`536f86e5cc9ec1ff38457d099bf320b9d08eeeba`; its JSON-RPC and app-server
notification plumbing remains out-of-scope.

## Reviewed baseline behavior

### Realtime voice delegation

The experimental owned-session Realtime boundary matches Codex's V1, V2, and
Frameless/V3 behavior: lifecycle developer context, bounded 5,300-token startup
context, typed-turn mirroring, transcript-tail flushing, current model/voice
catalogs, byte-identical backend instructions, exact tools and
acknowledgements, atomic steering, queued `response.create`, 200 ms bounded
agent updates, 500-byte Frameless appends, BEM commentary/speakable routing,
responses-as-items, V2 audio truncation on interruption, and recoverable
Frameless sidebands. Protocol/transport, transcription/text output,
client-managed handoffs, initial items, channel prefixes, startup context, and
tail flushing are explicit builder policies. Shutdown awaits the
transport/media lifecycle before the agent is stopped. Recent-work startup
context discovers the canonical `<CODEX_HOME>/sessions/YYYY/MM/DD` layout
through the rollout owner rather than scanning a nonexistent nested sessions
directory.

All Realtime orchestration remains in `nanocodex-voice`; the agent crate exposes
only protocol-neutral live-input, developer-message, and read-only session
context hooks. ChatGPT WebRTC uses native Rust WebRTC/Opus with a sideband
WebSocket, while the host owns device-attestation generation.

Five Realtime-adjacent entries in the complete later range are called out here
because they connect directly to `P41` and `P42`:

- `13dfaab4469eff5c5b929bb7e1cbc6bba5e0c1be` is `out-of-scope`: it narrates
  Codex approval, permission, patch-approval, elicitation, and app-owned input
  requests. Nanocodex deliberately has no approval subsystem or app-server
  request surface.
- `ecb8013dfa82120d11b02e3b68b7d3a3afd79d39` is `port`: `P41` owns recoverable
  V3 sidebands and the related transcript/delegation bounds.
- `536f86e5cc9ec1ff38457d099bf320b9d08eeeba` is `port`: `P42` owns caller-SDP
  call creation and configuration-free existing-call attachment without
  importing Codex's app-server protocol.
- `d44696065723a56b9de6538cd6348fcbe6c1542e` is `out-of-scope`: it adds an
  unused Codex TUI keymap schema field and Linux exec-server version-skew build
  plumbing, not a Realtime session invariant.
- `be6ebb1f6d4cf3bcd70c5c20be2677bb38dec860` is `out-of-scope`: it adds a
  trace span around Codex's app-server Realtime running-state query without
  changing session behavior.

### Responses Lite parallel-tool scheduling

For `gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna`, Nanocodex matches
Codex's Responses Lite request contract by sending `parallel_tool_calls: false`.
The client still accepts multi-call responses and schedules them through
Codex's read/write admission gate: explicitly safe calls may overlap, while an
unsafe call excludes every sibling.

The safe built-ins match Codex: `exec_command`, `write_stdin`, `view_image`,
provider-native `tool_search`, and web search. MCP tools opt in through
`annotations.readOnlyHint` or an explicit server-wide setting; every other
caller-defined tool is serial by default. Tool-result events follow actual
completion order, while committed model history remains in provider order.
Cancellation retains completed sibling outputs and synthesizes Codex-shaped
aborted outputs only for unfinished calls.

The focused regressions in
[`model/tools/parallel.rs`](../crates/nanocodex-agent/tests/it/model/tools/parallel.rs)
cover overlap, exclusion, event/history ordering, cancellation, and aggregate
work-versus-wall timing. The public provider panic regression in
[`model/tools/panic.rs`](../crates/nanocodex-agent/tests/it/model/tools/panic.rs)
also proves that a failed `aborted` output repairs the same model turn without
stopping the private driver.

## Open evaluation queue

The 155 `evaluate` rows are not parity claims. The 111 evaluations in the
latest range are enumerated in its eight appendices with the exact missing
evidence or decision. Representative earlier TUI evaluations should be
resolved only through the existing trace corpus and focused allocation/frame
benchmarks:

- ownership and retained allocations: rows 4, 8, 9, 15, and 32;
- Markdown and layout algorithms: rows 10, 18, 26, and 28;
- operator-owned metric histogram policy: row 35.

Additional earlier evaluations are named `E2` through `E11` in the
[range appendix](codex-parity/8431dc59-3418498f.md): platform-specific terminal
behavior, Codex-only live exec rendering, side-pane navigation, and exact item
start timing require their corresponding workload before adoption.

The two model-tool audio rows remain deferred until the supported Responses
model contract includes that modality; they are distinct from the experimental
Realtime voice transport. No app-server, provider abstraction, approval,
Guardian, exec-policy, shell-snapshot, plugin, skills, or generic multi-agent
surface is implied by completing this review.
