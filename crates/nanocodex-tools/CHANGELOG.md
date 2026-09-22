# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.4](https://github.com/gakonst/nanocodex/releases/tag/v0.6.4) - 2026-09-19

### Bug Fixes

- [ci] Validate Codex parity imports and bounded declarations ([#415](https://github.com/gakonst/nanocodex/issues/415))
- Preserve native view_image normalization in Code Mode
- Reject malformed image output and recover poisoned sessions
- Fix compaction and image recovery across durable replay ([#375](https://github.com/gakonst/nanocodex/issues/375))

### Features

- [hands] Unify computer publishers and reduce startup latency

### Miscellaneous Tasks

- Prepare release 0.6.4 with minified QuickJS fix
- Prepare corrected release 0.6.3 ([#418](https://github.com/gakonst/nanocodex/issues/418))
- Prepare release 0.6.2 ([#416](https://github.com/gakonst/nanocodex/issues/416))

### Other

- Expose the consumed Codex memories API over existing scoped storage ([#412](https://github.com/gakonst/nanocodex/issues/412))
- Align shared Codex tool contracts and schema rendering ([#407](https://github.com/gakonst/nanocodex/issues/407))
- Merge pull request [#382](https://github.com/gakonst/nanocodex/issues/382) from gakonst/fix/simulator-process-reaping
- Reap shell children even when yielded sessions are not polled
- Merge origin/master into feat/mobile-project-threads
- Merge pull request [#371](https://github.com/gakonst/nanocodex/issues/371) from gakonst/fix/hosted-image-output
- Merge master and align image preparation and replay with codex-rs
- Merge master and retain its shared Vault link routing fix
- Merge origin/master into Windows Hand installer

### Performance

- [hands] Separate attachment DNS and TCP timings
- [hands] Trace attachment connection stages

### Styling

- Align Code Mode execution behavior while retaining QuickJS ([#409](https://github.com/gakonst/nanocodex/issues/409))
- Preserve the Codex web tool schema and command batches ([#408](https://github.com/gakonst/nanocodex/issues/408))

## [0.6.1](https://github.com/gakonst/nanocodex/releases/tag/v0.6.1) - 2026-09-15

### Bug Fixes

- [voice] Ship verified native runtime in 0.6.1 ([#333](https://github.com/gakonst/nanocodex/issues/333))

## [0.6.0](https://github.com/gakonst/nanocodex/releases/tag/v0.6.0) - 2026-09-15

### Rust API migration

Read the [0.5 → 0.6 Rust API changelog and migration guide](https://github.com/gakonst/nanocodex/blob/v0.6.0/docs/MIGRATING_0_6.md) before upgrading.

- **Breaking:** turn usage and snapshots are optional; session IDs are strings; prompt wrappers now target `PromptRequest`.
- **Breaking:** `hosted` tool APIs move to `embedded`; Code Mode execution/wait returns `Result`; protocol literals gain asynchronous fields.
- **Behavior:** default model/reasoning changes to Astra/low; resumed sessions use current instructions and tools; billing-uncertainty metrics and generic MCP resource helpers are removed.
- **Optional SDK layers:** durable execution with caller-owned storage, reusable subagent orchestration, and a managed backend. Browser, egress, VM, and voice leave experimental paths; computer and evals retain the label.

### Bug Fixes

- [apple] Preserve repository tree in native playtest update
- [apple] Preserve reading position and playtest navigation edge cases
- [tools] Stop evicting live shell processes at an arbitrary session cap
- [tools] Scope patch validation to native targets
- [tools] Trust decoded web search responses
- [tools] Reject aliased apply patch targets
- [tools] Dispatch late-discovered embedded tools
- [tools] Validate catalog fence lease pins
- [tools] Canonicalize numeric schema keys identically
- [tools] Preserve attachment catalog fence details
- [tools] Align hosted catalog digest domain
- [tools] Decouple attachment observation
- [tools] Keep hidden embedded tools off model specs
- [tools] Keep catalog validation at recipe composition
- [tools] Finish embedded API rename
- [tools] Keep image diagnostics out of TUI stderr
- [tools] Retain shells across turn cancellation
- [js] Complete browser tool runtime
- [ci] Satisfy remaining Rust 1.98 lints
- [js] Expose typed Code Mode tool results
- Preserve SDK warmup behavior
- [web] Complete deferred Mercator MPP flows
- [tempo] Use mpp-rs paid MCP wrapper
- [mcp] Harden OAuth token refresh

### Documentation

- [tools] Defer workspace replication
- [tools] Qualify MCP API link

### Features

- Add parallel Sky-compatible computer use ([#315](https://github.com/gakonst/nanocodex/issues/315))
- [remote] Stream and control connected device screens
- Align Astra defaults and Code Mode with Codex ([#275](https://github.com/gakonst/nanocodex/issues/275))
- [managed] Add scoped VM hand factories
- [nanocodex2] Trace VM hand activity
- [managed] Add brain and hands workspace fabric
- [nanocodex2] Ship instant managed TUI
- [tools] Align MCP naming OAuth and typed output
- [tools] Attach immutable recipes over websocket
- [bindings] Align hosted Code Mode tools
- [web] Move Git and browser workspaces to Cloudflare ([#186](https://github.com/gakonst/nanocodex/issues/186))
- [tempo] Enable built-in paid Mercator MCP
- [wasm] Add deferred paid MCP support

### Miscellaneous Tasks

- Release nanocodex 0.6.0 ([#330](https://github.com/gakonst/nanocodex/issues/330))

### Other

- Merge pull request [#252](https://github.com/gakonst/nanocodex/issues/252) from gakonst/feat/host-vm-pools
- Merge remote-tracking branch 'origin/master' into codex/pr243-managed-wallet
- Merge remote-tracking branch 'origin/master' into codex/cloud-accounts-playground
- Merge remote-tracking branch 'origin/master' into codex/cloud-accounts-playground
- Merge master into durable runtime
- Merge pull request [#193](https://github.com/gakonst/nanocodex/issues/193) from gakonst/feat/viem-v3-js-api
- Merge pull request [#191](https://github.com/gakonst/nanocodex/issues/191) from gakonst/feat/browser-sandbox-hardening
- Merge pull request [#179](https://github.com/gakonst/nanocodex/issues/179) from gakonst/perf/minimize-agent-overhead
- Merge pull request [#178](https://github.com/gakonst/nanocodex/issues/178) from gakonst/fix/hosted-tool-search-dispatch
- Merge pull request [#171](https://github.com/gakonst/nanocodex/issues/171) from gakonst/feat/wasm-mcp-mercator
- Merge remote-tracking branch 'origin/master' into agent/eval-cluster-dashboard

### Performance

- [tools] Cache discovered embedded definitions
- [browser] Defer MCP catalog discovery
- Reduce agent startup overhead

### Refactor

- [durability] Persist bounded execution records across hosts
- [mcp] Make tool search the sole discovery surface
- [tools] Simplify reverse attachment protocol
- [tools] Separate embedded execution placement
- [tools] Make selection catalogs deterministic
- [subagents] Extract reusable native and wasm extension

### Testing

- [tools] Fence reconnect detach race

## [0.5.0](https://github.com/gakonst/nanocodex/releases/tag/v0.5.0) - 2026-08-12

### Bug Fixes

- [tui] Handle terminal input as shell output
- [events] Preserve structured results universally
- [events] Retain structured nested tool results

### Miscellaneous Tasks

- [release] Refresh 0.5.0 changelogs
- [release] Prepare 0.5.0

### Other

- Merge pull request [#169](https://github.com/gakonst/nanocodex/issues/169) from gakonst/release/0.5.0
- Merge pull request [#167](https://github.com/gakonst/nanocodex/issues/167) from clabby/cl/structured-events
- :broom:

## [0.4.0](https://github.com/gakonst/nanocodex/releases/tag/v0.4.0) - 2026-08-11

### Bug Fixes

- [http] Initialize rustls at client boundaries
- [browser] Keep deferred schema lookup private
- [tools] Restore stock Codex code mode parity
- Close remaining Codex wire parity gaps
- [tools] Keep tool search visible in code mode
- [tools] Align Code Mode tool contracts
- [agent] Dispatch unnamespaced hosted tools
- [tls] Standardize rustls on ring
- [vm] Harden cache and session lifecycle

### Features

- [browser] Add pixel-calibrated captures
- [cli] Enable browser tools by default
- [tools] Align current Codex parity
- [wasm] Support CSP-safe direct host tools
- [cli] Integrate deferred browser tooling
- [tools] Expose the ambient sensitive environment
- [vm] Add retained VM-backed workspace tools

### Miscellaneous Tasks

- [release] Refresh 0.4.0 changelogs
- [release] Prepare 0.4.0

### Other

- Merge pull request [#160](https://github.com/gakonst/nanocodex/issues/160) from gakonst/release/v0.4.0
- Merge pull request [#124](https://github.com/gakonst/nanocodex/issues/124) from gakonst/fix/codex-parity-current
- Merge pull request [#95](https://github.com/gakonst/nanocodex/issues/95) from gakonst/agent/pr61-code-mode
- Merge pull request [#75](https://github.com/gakonst/nanocodex/issues/75) from gakonst/feat/wasm-host-transport
- Merge pull request [#86](https://github.com/gakonst/nanocodex/issues/86) from gakonst/fix/ring-only-rustls
- Merge pull request [#78](https://github.com/gakonst/nanocodex/issues/78) from gakonst/agent/browser-tui-integration
- Merge pull request [#59](https://github.com/gakonst/nanocodex/issues/59) from cjustice/feat/ambient-sensitive-environment
- Merge pull request [#58](https://github.com/gakonst/nanocodex/issues/58) from gakonst/refactor/09-eval

### Refactor

- Trim Codex parity implementation

## [0.3.0](https://github.com/gakonst/nanocodex/releases/tag/v0.3.0) - 2026-07-28

### Bug Fixes

- [code-mode] Schedule timers on the host worker
- [mcp] Bound stdio stderr buffering
- [mcp] Own and bound stdio subprocesses
- [mcp] Stop cross-origin credential redirects
- [mcp] Send a deterministic HTTP user agent

### Documentation

- Finalize the PR 50 public API guide

### Miscellaneous Tasks

- [release] Refresh 0.3.0 changelogs
- [release] Prepare 0.3.0

### Other

- Merge pull request [#50](https://github.com/gakonst/nanocodex/issues/50) from gakonst/refactor/05-observability

### Performance

- Gate PR 50 hot paths

### Refactor

- [tools] Decompose runtime ownership
- Align agent lifecycle with Codex
- Isolate platform runtime boundaries
- Stabilize public SDK surface
- Consolidate tools and MCP

### Testing

- [mcp] Verify read-only parallel dispatch
- [tools] Serialize traced runtime integration tests

## [0.2.0](https://github.com/gakonst/nanocodex/releases/tag/v0.2.0) - 2026-07-26

### Bug Fixes

- Match Codex tool behavior
- [cli] Bound MPP egress concurrency

### Features

- Align code mode with Codex
- [mcp] Prewarm deferred default servers
- [cli] Route OpenAI through Tempo MPP
- [agent] Resume sessions from durable snapshots ([#13](https://github.com/gakonst/nanocodex/issues/13))
- [code-mode] Stream nested tool lifecycles
- [tools] Track nested call start offsets

### Miscellaneous Tasks

- [release] Prepare 0.2.0
- Raise Rust baseline to 1.97

### Other

- Merge pull request [#27](https://github.com/gakonst/nanocodex/issues/27) from gakonst/fix/mpp-egress-resource-bounds
- Merge pull request [#2](https://github.com/gakonst/nanocodex/issues/2) from gakonst/feat/mpp-integration

### Testing

- Synchronize code cell termination output ([#23](https://github.com/gakonst/nanocodex/issues/23))

## [0.1.1](https://github.com/gakonst/nanocodex/releases/tag/v0.1.1) - 2026-07-23

### Bug Fixes

- [wasm] Scope host tools to agent sessions
- [shell] Serialize session input and process interrupts
- [code-mode] Preserve tool results across yields
- [observability] Retain yielded tool lineage
- [tools] Preserve live shell session ids
- [ci] Support Windows shell tooling

### Features

- Expose VM-ready standard tools
- [tools] Align the WASM host runtime contract
- Expose VM-ready standard tools
- [tools] Allow replacing workspace tools
- [tools] Embed QuickJS code mode
- [agent] Refine task execution guidance
- Add ChatGPT subscription authentication
- [observability] Export full-fidelity agent traces
- [agent] Add controllable conversation lifecycle
- [observability] Add end-to-end OTLP tracing
- [tools] Reuse persistent Node code-mode host
- Add MCP observability and release automation
- Add embedded web and MCP integrations
- Add embedded Python and WASM bindings

### Miscellaneous Tasks

- [release] Prepare 0.1.1
- [eval] Remove benchmark-specific tuning
- [release] Prepare 0.1.0
- [release] Refresh 0.1.0 changelogs
- [release] Add per-crate changelogs
- [release] Automate publishing and native updates

### Other

- Merge pull request [#8](https://github.com/gakonst/nanocodex/issues/8) from gakonst/agent/embedded-quickjs-code-mode

### Performance

- [tui] Optimize long-session rendering and interaction
- [tools] Share code mode history snapshots
- [shell] Share process drain grace deadline
- [tools] Align nested shell yield deadlines
- [tools] Prewarm code mode node host

### Refactor

- [tools] Return typed handler results
- Rename project to nanocodex

### Testing

- [tools] Cover custom tools in code mode

<!-- generated by git-cliff -->
