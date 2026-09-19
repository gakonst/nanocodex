# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.4](https://github.com/gakonst/nanocodex/releases/tag/v0.6.4) - 2026-09-19

### Bug Fixes

- [ci] Validate Codex parity imports and bounded declarations ([#415](https://github.com/gakonst/nanocodex/issues/415))
- Repair malformed stored tool images before replay
- Classify rejected images during connection warmup
- Reject malformed image output and recover poisoned sessions
- Fix compaction and image recovery across durable replay ([#375](https://github.com/gakonst/nanocodex/issues/375))
- Align hosted compaction retries and HTTPS fallback with codex-rs ([#370](https://github.com/gakonst/nanocodex/issues/370))
- [agent] Settle compaction failures and expose retry progress ([#363](https://github.com/gakonst/nanocodex/issues/363))

### Dependencies

- Align Codex compaction requests and summary recovery ([#410](https://github.com/gakonst/nanocodex/issues/410))

### Miscellaneous Tasks

- Prepare release 0.6.4 with minified QuickJS fix
- Prepare corrected release 0.6.3 ([#418](https://github.com/gakonst/nanocodex/issues/418))
- Prepare release 0.6.2 ([#416](https://github.com/gakonst/nanocodex/issues/416))

### Other

- Pin only consumed Codex runtime prompts and verify upstream fidelity ([#411](https://github.com/gakonst/nanocodex/issues/411))
- Merge origin/master into feat/mobile-project-threads
- Merge pull request [#371](https://github.com/gakonst/nanocodex/issues/371) from gakonst/fix/hosted-image-output
- Merge master and align image preparation and replay with codex-rs
- Merge remote-tracking branch 'origin/master' into fix/hosted-image-output
- Merge master and retain its shared Vault link routing fix
- Merge remote-tracking branch 'origin/master' into feat/hand-rtmp
- Merge remote-tracking branch 'origin/master' into feat/hand-rtmp

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

- [durability] Checkpoint recovered cancellations ([#301](https://github.com/gakonst/nanocodex/issues/301))
- [durability] Emit terminals only after durable settlement
- [durability] Checkpoint current execution and retire settled effects
- [agent] Allow const admission cancellation configuration
- [agent] Finish nested tools across yielded calls
- [apple] Preserve repository tree in native playtest update
- [apple] Preserve reading position and playtest navigation edge cases
- [durability] Preserve encrypted provider item identities
- [durability] Recover from authoritative operation settlement
- [durability] Recover with retained model requests
- [agent] Keep host context clippy-clean
- [astra] Use model-specific prompts across runtimes
- [managed] Retain steering and sandbox work across retries
- [durability] Cancel turns at admission
- [managed] Recover sandbox work across runtime changes
- [durability] Replay completed tool outputs exactly
- [bindings] Route durable forks by thread
- [ci] Satisfy durability const lints
- [agent] Resume durable spawned routing
- [agent] Retain provider session routing across branches
- [agent] Replay unstored forks on fresh transports
- [durability] Remove billing uncertainty state
- [durability] Settle uncertain effects safely
- [agent] Reset unstored fresh-agent checkpoints
- [agent] Replay unstored forks on fresh transports
- [ci] Restore cross-package compatibility
- [managed] Preserve exact durable dispatch state
- [agent] Satisfy wasm release clippy
- [subagents] Observe batch children at creation
- Satisfy WASM warnings gate
- [js] Scope direct subagent lifecycle per agent
- [agent] Make rollout flush portable to wasm
- [durability] Rebind runtime policy on cold resume
- [agent] Scope const lint to wasm platform
- [agent] Keep execution branching const-safe
- [durability] Fence authoritative execution end to end
- [agent] Preserve remote workspace paths
- [durability] Terminally commit failed turns
- Fix durable subagent recovery ownership
- [agent] Remove stale prompt request import
- [agent] Journal prompts automatically

### Features

- Undo pending steering and queued messages
- Align Astra defaults and Code Mode with Codex ([#275](https://github.com/gakonst/nanocodex/issues/275))
- [managed] Add scoped VM hand factories
- Complete GPT-6 Astra integration
- Prepare GPT-6 Astra support
- [nanocodex2] Add durable interactive attach
- [durability] Persist clean agent descendants
- [durability] Persist spawned agent trees
- [tools] Align MCP naming OAuth and typed output
- [agent] Align Responses context and rollout replay
- [subagents] Start coordinated agent batches atomically
- [managed] Run nondurable subagents from durable roots
- [web] Retain local agent transcripts
- [subagents] Configure spawned model and thinking
- [agent] Expose durable request identities
- [agent] Journal durable prompts and steps
- [durability] Add portable journal runtime

### Miscellaneous Tasks

- Release nanocodex 0.6.0 ([#330](https://github.com/gakonst/nanocodex/issues/330))
- [ci] Satisfy workspace formatting and Rust 1.97 checks

### Other

- Merge remote-tracking branch 'origin/master' into codex/durable-current-execution
- Merge pull request [#252](https://github.com/gakonst/nanocodex/issues/252) from gakonst/feat/host-vm-pools
- Merge pull request [#251](https://github.com/gakonst/nanocodex/issues/251) from gakonst/feat/gpt-6-astra-readiness
- Revert "feat(durability): persist spawned agent trees"
- Merge pull request [#226](https://github.com/gakonst/nanocodex/issues/226) from gakonst/fix/btw-unstored-fork
- Merge pull request [#217](https://github.com/gakonst/nanocodex/issues/217) from gakonst/refactor/durability-total-state
- Merge remote-tracking branch 'origin/master' into codex/cloud-accounts-playground
- Merge remote-tracking branch 'origin/master' into codex/cloud-accounts-playground
- Merge pull request [#207](https://github.com/gakonst/nanocodex/issues/207) from gakonst/fix/remote-workspace-resolution
- Merge pull request [#181](https://github.com/gakonst/nanocodex/issues/181) from gakonst/feat/durable-runtime
- Merge master into durable runtime
- Merge pull request [#193](https://github.com/gakonst/nanocodex/issues/193) from gakonst/feat/viem-v3-js-api
- Merge remote-tracking branch 'origin/master' into agent/eval-cluster-dashboard

### Refactor

- [durability] Persist bounded execution records across hosts
- [durability] Replay committed effects only
- [durability] Replace journals with total state
- [agent] Isolate local OpenAI implementation
- [agent] Erase lifecycle behind backend seam
- [agent] Make turn results backend-neutral
- [subagents] Extract reusable native and wasm extension
- [durability] Own agent integration above lifecycle
- [agent] Unify durable prompt submission

### Testing

- [agent] Align fork checkpoint expectations
- [agent] Use backend-neutral session identities

## [0.5.0](https://github.com/gakonst/nanocodex/releases/tag/v0.5.0) - 2026-08-12

### Bug Fixes

- [events] Preserve structured results universally
- [events] Retain structured nested tool results

### Miscellaneous Tasks

- [release] Refresh 0.5.0 changelogs
- [release] Prepare 0.5.0

### Other

- Merge pull request [#169](https://github.com/gakonst/nanocodex/issues/169) from gakonst/release/0.5.0
- Merge pull request [#167](https://github.com/gakonst/nanocodex/issues/167) from clabby/cl/structured-events

## [0.4.0](https://github.com/gakonst/nanocodex/releases/tag/v0.4.0) - 2026-08-11

### Bug Fixes

- [http] Initialize rustls at client boundaries
- [eval] Preserve same-role benchmark messages
- [ci] Preserve typed prompt consumer contracts
- Close remaining Codex wire parity gaps
- [tui] Sanitize resume picker metadata
- [oai] Account for usage-uncertain attempts
- [tools] Align Code Mode tool contracts
- [agent] Dispatch unnamespaced hosted tools
- [tls] Standardize rustls on ring
- [agent] Preserve model in adapter checkpoints
- [agent] Retain model in fork checkpoints
- Build Tower services from effective agent config
- Preserve Codex rollout model compatibility

### Features

- [eval] Add benchmark adapter foundation
- [cli] Add interactive resume session picker
- [agent] Describe remote execution context
- [voice] Add Codex realtime parity
- Support Luna
- Match Codex realtime steering

### Miscellaneous Tasks

- [release] Refresh 0.4.0 changelogs
- [release] Prepare 0.4.0

### Other

- Merge pull request [#160](https://github.com/gakonst/nanocodex/issues/160) from gakonst/release/v0.4.0
- Merge pull request [#142](https://github.com/gakonst/nanocodex/issues/142) from gakonst/feat/eval-adapter-foundation
- Merge pull request [#124](https://github.com/gakonst/nanocodex/issues/124) from gakonst/fix/codex-parity-current
- Merge pull request [#122](https://github.com/gakonst/nanocodex/issues/122) from Giulio2002/feat/resume-session-picker
- Merge pull request [#97](https://github.com/gakonst/nanocodex/issues/97) from gakonst/agent/pr61-tower-accounting
- Merge pull request [#96](https://github.com/gakonst/nanocodex/issues/96) from gakonst/agent/pr61-agent-context
- Merge pull request [#95](https://github.com/gakonst/nanocodex/issues/95) from gakonst/agent/pr61-code-mode
- Merge pull request [#75](https://github.com/gakonst/nanocodex/issues/75) from gakonst/feat/wasm-host-transport
- Merge pull request [#86](https://github.com/gakonst/nanocodex/issues/86) from gakonst/fix/ring-only-rustls
- Merge pull request [#84](https://github.com/gakonst/nanocodex/issues/84) from gakonst/fix/committed-session-model
- Merge pull request [#82](https://github.com/gakonst/nanocodex/issues/82) from gakonst/feat/realtime-codex-parity
- Merge pull request [#80](https://github.com/gakonst/nanocodex/issues/80) from clabby/cl/luna
- Merge pull request [#77](https://github.com/gakonst/nanocodex/issues/77) from gakonst/feat/realtime-voice

### Refactor

- [eval] Simplify durable benchmark ownership
- Fix the model for each thread

## [0.3.0](https://github.com/gakonst/nanocodex/releases/tag/v0.3.0) - 2026-07-28

### Bug Fixes

- [ci] Satisfy strict Clippy checks
- [wasm] Satisfy target-specific lint gates
- [agent] Fail malformed continuations before terminal
- [agent] Remove rollout writer locking
- [agent] Align retained context with Codex

### Documentation

- Finalize the PR 50 public API guide

### Features

- Stabilize observability and USD cost

### Miscellaneous Tasks

- [release] Refresh 0.3.0 changelogs
- [release] Prepare 0.3.0

### Other

- Merge pull request [#50](https://github.com/gakonst/nanocodex/issues/50) from gakonst/refactor/05-observability

### Refactor

- [api] Stabilize Tower and lifecycle boundaries
- [oai] Contain agent-only session internals
- [agent] Decompose model lifecycle
- [agent] Decompose driver control
- [agent] Decompose rollout persistence
- Align agent lifecycle with Codex
- Isolate platform runtime boundaries
- Stabilize public SDK surface
- Extract owned agent lifecycle

### Testing

- [observability] Isolate tracing capture
- [agent] Expect stable response item ids
- [observability] Verify full-fidelity turn traces

<!-- generated by git-cliff -->
