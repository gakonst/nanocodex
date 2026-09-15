# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.1](https://github.com/gakonst/nanocodex/releases/tag/v0.6.1) - 2026-09-15

### Miscellaneous Tasks

- [release] Synchronize workspace packages to 0.6.1

## [0.6.0](https://github.com/gakonst/nanocodex/releases/tag/v0.6.0) - 2026-09-15

### Rust API migration

Read the [0.5 → 0.6 Rust API changelog and migration guide](https://github.com/gakonst/nanocodex/blob/v0.6.0/docs/MIGRATING_0_6.md) before upgrading.

- **Breaking:** turn usage and snapshots are optional; session IDs are strings; prompt wrappers now target `PromptRequest`.
- **Breaking:** `hosted` tool APIs move to `embedded`; Code Mode execution/wait returns `Result`; protocol literals gain asynchronous fields.
- **Behavior:** default model/reasoning changes to Astra/low; resumed sessions use current instructions and tools; billing-uncertainty metrics and generic MCP resource helpers are removed.
- **Optional SDK layers:** durable execution with caller-owned storage, reusable subagent orchestration, and a managed backend. Browser, egress, VM, and voice leave experimental paths; computer and evals retain the label.

### Bug Fixes

- [transport] Let quiet reasoning wait without restarting
- [durability] Checkpoint current execution and retire settled effects
- [voice] Improve WebRTC startup and transcript handling
- [apple] Preserve repository tree in native playtest update
- [apple] Preserve reading position and playtest navigation edge cases
- [durability] Preserve encrypted provider item identities
- [durability] Recover with retained model requests
- [astra] Use model-specific prompts across runtimes
- [agent] Retain provider session routing across branches
- [agent] Replay unstored forks on fresh transports
- [durability] Remove billing uncertainty state
- [durability] Settle uncertain effects safely
- [oai] Distinguish claimed and committed terminals
- [oai] Close terminal publication races lock-free
- [oai] Enforce contiguous terminal event streams
- [voice] Use Codex realtime model
- [durability] Terminally commit failed turns
- [oai] Acknowledge realtime constructor width
- [auth] Repair access-only durable credentials
- [ci] Unblock current toolchain checks
- [js] Expose typed Code Mode tool results
- [auth] Derive subscription store defaults
- [eval] Align prompt cache identity with Codex ([#180](https://github.com/gakonst/nanocodex/issues/180))
- Preserve SDK warmup behavior

### Features

- [voice] Align native, browser, and Swift sessions with Codex
- Align Astra defaults and Code Mode with Codex ([#275](https://github.com/gakonst/nanocodex/issues/275))
- [managed] Add scoped VM hand factories
- Complete GPT-6 Astra integration
- Prepare GPT-6 Astra support
- [tools] Align MCP naming OAuth and typed output
- [agent] Align Responses context and rollout replay
- [voice] Match Codex realtime call controls
- [subagents] Configure spawned model and thinking
- [voice] Recover realtime sideband sessions
- [agent] Journal durable prompts and steps
- [auth] Add Rust-owned ChatGPT subscriptions
- [auth] Support persistent ChatGPT access tokens

### Miscellaneous Tasks

- Prepare release 0.6.0
- [ci] Satisfy workspace formatting and Rust 1.97 checks

### Other

- Merge pull request [#252](https://github.com/gakonst/nanocodex/issues/252) from gakonst/feat/host-vm-pools
- Merge pull request [#251](https://github.com/gakonst/nanocodex/issues/251) from gakonst/feat/gpt-6-astra-readiness
- Merge pull request [#226](https://github.com/gakonst/nanocodex/issues/226) from gakonst/fix/btw-unstored-fork
- Merge pull request [#217](https://github.com/gakonst/nanocodex/issues/217) from gakonst/refactor/durability-total-state
- Merge pull request [#181](https://github.com/gakonst/nanocodex/issues/181) from gakonst/feat/durable-runtime
- Merge master into durable runtime
- Merge pull request [#190](https://github.com/gakonst/nanocodex/issues/190) from gakonst/feat/rust-owned-chatgpt-subscription
- Merge remote-tracking branch 'origin/master' into wrapup/pr-171
- Merge pull request [#175](https://github.com/gakonst/nanocodex/issues/175) from gakonst/fix/persistent-chatgpt-access-tokens
- Merge remote-tracking branch 'origin/master' into agent/eval-cluster-dashboard

### Performance

- Reduce agent startup overhead

### Refactor

- [oai] Keep event publication lock-free
- [oai] Publish portable agent events

## [0.5.0](https://github.com/gakonst/nanocodex/releases/tag/v0.5.0) - 2026-08-12

### Bug Fixes

- [tui] Handle terminal input as shell output
- [events] Preserve structured results universally
- [events] Retain structured nested tool results
- [oai] Drop notifications orphaned by compaction

### Miscellaneous Tasks

- [release] Refresh 0.5.0 changelogs
- [release] Prepare 0.5.0

### Other

- Merge pull request [#169](https://github.com/gakonst/nanocodex/issues/169) from gakonst/release/0.5.0
- Merge pull request [#167](https://github.com/gakonst/nanocodex/issues/167) from clabby/cl/structured-events
- :broom:
- Merge pull request [#168](https://github.com/gakonst/nanocodex/issues/168) from clabby/cl/fix-orphaned-notifs

## [0.4.0](https://github.com/gakonst/nanocodex/releases/tag/v0.4.0) - 2026-08-11

### Bug Fixes

- [http] Initialize rustls at client boundaries
- [oai] Allow long silent response generations
- [eval] Preserve same-role benchmark messages
- [eval] Recover cleanly from worker infrastructure failures
- [oai] Recover forbidden websocket handshakes
- [oai] Preserve code mode notifications in replay
- Close remaining Codex wire parity gaps
- [oai] Account for usage-uncertain attempts
- [tools] Align Code Mode tool contracts
- [tls] Standardize rustls on ring
- Preserve Codex rollout model compatibility
- [ci] Stabilize observability tests

### Features

- [eval] Add benchmark adapter foundation
- [tools] Align current Codex parity
- [model] Support Terra and routed OpenAI model IDs
- [voice] Add Codex realtime parity
- Support Luna
- Close Codex realtime parity gaps
- Match Codex realtime steering
- Add reusable realtime voice sessions
- [vm] Add retained VM-backed workspace tools

### Miscellaneous Tasks

- [release] Refresh 0.4.0 changelogs
- [release] Prepare 0.4.0

### Other

- Merge pull request [#160](https://github.com/gakonst/nanocodex/issues/160) from gakonst/release/v0.4.0
- Merge pull request [#142](https://github.com/gakonst/nanocodex/issues/142) from gakonst/feat/eval-adapter-foundation
- Merge pull request [#139](https://github.com/gakonst/nanocodex/issues/139) from gakonst/fix/websocket-403-fallback
- Merge pull request [#124](https://github.com/gakonst/nanocodex/issues/124) from gakonst/fix/codex-parity-current
- Merge pull request [#121](https://github.com/gakonst/nanocodex/issues/121) from Slokh/kartik/upstream-contributions
- Merge pull request [#97](https://github.com/gakonst/nanocodex/issues/97) from gakonst/agent/pr61-tower-accounting
- Merge pull request [#95](https://github.com/gakonst/nanocodex/issues/95) from gakonst/agent/pr61-code-mode
- Merge pull request [#86](https://github.com/gakonst/nanocodex/issues/86) from gakonst/fix/ring-only-rustls
- Merge pull request [#82](https://github.com/gakonst/nanocodex/issues/82) from gakonst/feat/realtime-codex-parity
- Merge pull request [#80](https://github.com/gakonst/nanocodex/issues/80) from clabby/cl/luna
- Merge pull request [#77](https://github.com/gakonst/nanocodex/issues/77) from gakonst/feat/realtime-voice
- Merge pull request [#58](https://github.com/gakonst/nanocodex/issues/58) from gakonst/refactor/09-eval

### Performance

- Harden realtime voice audio paths

### Refactor

- [eval] Simplify durable benchmark ownership
- Trim Codex parity implementation
- Fix the model for each thread

### Testing

- Synchronize realtime response queue

## [0.3.0](https://github.com/gakonst/nanocodex/releases/tag/v0.3.0) - 2026-07-28

### Bug Fixes

- [ci] Satisfy strict Clippy checks
- [oai] Reject empty continuation checkpoints
- [oai] Bound WebSocket pump backlog
- [oai] Ignore non-assistant final messages
- [oai] Preserve stable item ids in ephemeral requests
- [api] Restore refactored consumer builds

### Documentation

- Finalize the PR 50 public API guide
- [oai] Prepare package changelog

### Features

- Stabilize observability and USD cost

### Miscellaneous Tasks

- [release] Refresh 0.3.0 changelogs
- [release] Prepare 0.3.0

### Other

- Merge pull request [#50](https://github.com/gakonst/nanocodex/issues/50) from gakonst/refactor/05-observability

### Performance

- Gate PR 50 hot paths

### Refactor

- [api] Stabilize Tower and lifecycle boundaries
- [oai] Contain agent-only session internals
- [agent] Decompose model lifecycle
- Align agent lifecycle with Codex
- Isolate platform runtime boundaries
- Stabilize public SDK surface
- Extract owned agent lifecycle
- Consolidate tools and MCP
- Consolidate the OpenAI Responses API

### Styling

- [oai] Format final-message regression
- [oai] Format item id policy

### Testing

- [oai] Compare tool search arguments semantically

<!-- generated by git-cliff -->
