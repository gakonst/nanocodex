# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.5](https://github.com/gakonst/nanocodex/releases/tag/v0.6.5) - 2026-09-24

### Bug Fixes

- [managed] Always allow steering and cancellation
- [router] Preserve existing managed event protocol for route updates ([#523](https://github.com/gakonst/nanocodex/issues/523))
- [runtime] Prevent image replay crashes and reconcile steering delivery ([#435](https://github.com/gakonst/nanocodex/issues/435))

### Features

- Add running-agent navigation and tmux overview ([#560](https://github.com/gakonst/nanocodex/issues/560))
- [router] Add Kimi and MiMo with mobile model controls ([#519](https://github.com/gakonst/nanocodex/issues/519))
- [tui] Add authenticated rich terminal integration ([#344](https://github.com/gakonst/nanocodex/issues/344))
- Add opt-in autoroute UX and retained provider display

### Other

- Merge remote-tracking branch 'origin/master' into perf/screen-direct-latency-20260923
- Merge remote-tracking branch 'origin/master' into perf/screen-latency-20260923
- Merge [#528](https://github.com/gakonst/nanocodex/issues/528): retire legacy facts and preserve Codex memory APIs
- Retire legacy facts while preserving Codex memory and background personalization
- Merge remote-tracking branch 'origin/master' into remove-legacy-memory
- Remove legacy memory storage and personalization
- Merge remote-tracking branch 'origin/master' into fix/turn-controls-merge
- Integrate GPT-6 Sol and Luna across Nanocodex ([#526](https://github.com/gakonst/nanocodex/issues/526))
- Restore unchanged repository files omitted from frontier REST publication
- Support configured Cloudflare REST transport for frontier inference ([#495](https://github.com/gakonst/nanocodex/issues/495))
- Run one persistent Hand daemon per machine ([#483](https://github.com/gakonst/nanocodex/issues/483))
- Merge pull request [#436](https://github.com/gakonst/nanocodex/issues/436) from gakonst/poc/thread-model-routing
- Merge latest master and retain voice lifecycle controls
- Add opt-in Jev thread routing with Workers AI GLM transport
- Forward Codex turn metadata and surface Sky consent in foreground Hands ([#429](https://github.com/gakonst/nanocodex/issues/429))

### Performance

- [cli] Present first responses immediately and finish shutdown promptly
- [cli] Make startup editable before backend discovery


## [0.6.4](https://github.com/gakonst/nanocodex/releases/tag/v0.6.4) - 2026-09-19

### Bug Fixes

- [tui] Open file links from their owning Hand
- [managed] Satisfy access cache lint checks

### Features

- [tui] Add Vault credential approval without raw JSON ([#361](https://github.com/gakonst/nanocodex/issues/361))
- [cli] Add managed native voice and measure end-to-end flows
- [agent] Scope personal memories and attribute startup callers
- [connect] Add ChatGPT account failover and per-session pins ([#343](https://github.com/gakonst/nanocodex/issues/343))
- [hands] Unify computer publishers and reduce startup latency

### Miscellaneous Tasks

- Prepare release 0.6.4 with minified QuickJS fix
- Prepare corrected release 0.6.3 ([#418](https://github.com/gakonst/nanocodex/issues/418))
- Prepare release 0.6.2 ([#416](https://github.com/gakonst/nanocodex/issues/416))

### Other

- Merge origin/master into feat/mobile-project-threads
- Merge master and align image preparation and replay with codex-rs
- Merge remote-tracking branch 'origin/master' into feat/background-cua-integration
- Merge remote-tracking branch 'origin/master' into feat/hand-rtmp
- Merge remote-tracking branch 'origin/master' into perf/hand-video-60fps
- Merge origin/master into Windows Hand installer

### Performance

- [managed] Prepare active conversations before prompt submission
- [managed] Reuse short-lived request authorization

### Testing

- Verify request arrival in incomplete-download fixture ([#388](https://github.com/gakonst/nanocodex/issues/388))

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

- [durability] Store complete turns and memory without arbitrary content quotas
- [remote] Restore VM desktop before tool reconnection
- [apple] Preserve repository tree in native playtest update
- [apple] Preserve reading position and playtest navigation edge cases
- [managed] Retry transient thread history reads
- [managed] Keep durable sessions responsive across reconnects
- [managed] Document VM connector boundary
- [managed] Harden VM hand lifecycle
- [managed] Control attached turns by durable id
- [durability] Cancel turns at admission
- [managed] Harden workspace execution clients
- [managed] Release test locks before await
- [managed] Retain cloud fallback on initial attach failure
- [managed] Await reverse tool attachment readiness
- [managed] Separate event sequence from durable cursor
- [managed] Keep lifecycle identities backend-neutral

### Features

- Undo pending steering and queued messages
- [managed] Expose settings and durable schedule controls
- Align Astra defaults and Code Mode with Codex ([#275](https://github.com/gakonst/nanocodex/issues/275))
- [managed] Add scoped VM hand factories
- Complete GPT-6 Astra integration
- Prepare GPT-6 Astra support
- [managed] Share VM hands across an account
- [managed] Attach retained VM compute hands
- [managed] Route execution through cwd namespaces
- [managed] Harden durable brain and hands
- [managed] Add brain and hands workspace fabric
- [nanocodex2] Add hosted model controls
- [nanocodex2] Stream managed turns over websocket
- [nanocodex2] Ship instant managed TUI
- [nanocodex2] Add durable interactive attach
- [managed] Add separate lifecycle backend crate

### Miscellaneous Tasks

- Release nanocodex 0.6.0 ([#330](https://github.com/gakonst/nanocodex/issues/330))

### Other

- Merge remote-tracking branch 'origin/master' into codex/durable-current-execution
- Merge pull request [#264](https://github.com/gakonst/nanocodex/issues/264) from gakonst/fix/remove-astra-entitled
- Merge pull request [#252](https://github.com/gakonst/nanocodex/issues/252) from gakonst/feat/host-vm-pools
- Merge pull request [#251](https://github.com/gakonst/nanocodex/issues/251) from gakonst/feat/gpt-6-astra-readiness
- Merge pull request [#235](https://github.com/gakonst/nanocodex/issues/235) from gakonst/feat/named-workspace-fabric
- Merge remote-tracking branch 'origin/master' into codex/pr243-managed-wallet
- Merge nanocodex2 TUI and live startup
- Merge pull request [#217](https://github.com/gakonst/nanocodex/issues/217) from gakonst/refactor/durability-total-state

### Performance

- [nanocodex2] Create agents over the live socket

### Refactor

- [astra] Remove catalog entitlement projection
- [durability] Replace journals with total state

### Testing

- [managed] Complete attachment drain handshake

<!-- generated by git-cliff -->
