# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Bug Fixes

- [cli] Fall back to another installed Chromium-family browser when the default
  Brave installation is absent, or omit browser tools when none is available,
  while keeping explicit browser selection strict.
- [web] Prepare the managed QuickJS WASM asset before starting Cloudflare Vite
  development workers.
- [vite] Serialize concurrent Rust/WASM binding generation across dev workers.

## [0.5.0](https://github.com/gakonst/nanocodex/releases/tag/v0.5.0) - 2026-08-12

### Bug Fixes

- [update] Retry transient asset downloads
- [tui] Handle terminal input as shell output
- [events] Preserve structured results universally
- [events] Retain structured nested tool results
- [oai] Drop notifications orphaned by compaction
- [eval] Keep neural waits within one tool call
- [eval] Allow per-attempt evidence directories
- [eval] Admit only missing neural workers
- [eval] Disable browser in neural controller
- [eval] Scope neural occupancy to its board

### Features

- [eval] Isolate workers in a systemd slice

### Miscellaneous Tasks

- [release] Prepare 0.5.0
- [eval] Remove deprecated Python Harbor stack

### Other

- Merge pull request [#167](https://github.com/gakonst/nanocodex/issues/167) from clabby/cl/structured-events
- :broom:
- Merge pull request [#168](https://github.com/gakonst/nanocodex/issues/168) from clabby/cl/fix-orphaned-notifs
- Merge pull request [#161](https://github.com/gakonst/nanocodex/issues/161) from gakonst/agent/simple-neural-eval-runtime
- Merge pull request [#162](https://github.com/gakonst/nanocodex/issues/162) from gakonst/agent/remove-python-harbor

### Refactor

- [eval] Combine neural wait and observation
- [eval] Own canonical task execution
- [eval] Strip neural controller tools
- [eval] Name workers as systemd instances
- [eval] Replace compiled supervisor with neural control
- [eval] Remove obsolete ledger migrations

## [0.4.0](https://github.com/gakonst/nanocodex/releases/tag/v0.4.0) - 2026-08-11

### Bug Fixes

- [http] Initialize rustls at client boundaries
- [eval] Initialize rustls clients
- [egress] Install rustls provider
- [release] Normalize colored dependency trees
- [ci] Stabilize process timing tests
- [eval] Extract GPQA archive in process
- [eval] Select the pinned Arena-Hard smoke case
- [eval] Order artifact archive options
- [eval] Requeue scored lifecycle failures
- [oai] Allow long silent response generations
- [eval] Stage files in isolated verifier VMs
- [eval] Join agents before verifier isolation
- [eval] Canonicalize pull worker evidence roots
- [eval] Send task roots to pull workers
- [eval] Preserve same-role benchmark messages
- [ci] Preserve typed prompt consumer contracts
- [eval] Satisfy strict coordinator clippy
- [eval] Pause admission after capacity deaths
- [eval] Account for pending worker claims
- [eval] Make headless benchmark supervision deterministic
- [eval] Keep controller reconciliation bounded
- [eval] Release every exited worker claim
- [eval] Keep libkrun sockets on short temp paths
- [vm] Wait for gvproxy packet activation
- [eval] Keep cold verifiers off cache disks
- [eval] Move verifier cache warming off worker admission
- [eval] Keep coordinator responsive under worker bursts
- [eval] Keep gvproxy socket paths short
- [evals] Keep live task reads database-only
- [evals] Refresh retried result projections
- [evals] Estimate missing benchmark costs
- [eval] Recover cleanly from worker infrastructure failures
- [eval] Make benchmark saturation autonomous
- [eval] Let workers outlive benchmark controller
- [eval] Separate verifier and infrastructure failures
- [eval] Reserve headroom for live VM growth
- [eval] Continuously backfill benchmark capacity
- [eval] Observe capacity between admissions
- [eval] Preserve compact capacity counts
- [eval] Make capacity telemetry executable
- [eval] Saturate benchmark from live host capacity
- [ci] Stabilize cross-platform runtime checks
- [eval] Preserve active benchmark workers
- [oai] Recover forbidden websocket handshakes
- [ci] Satisfy durable baseline gates
- [eval] Complete durable runtime ownership
- [eval] Preserve inherited worker config
- [eval] Forbid recursive benchmark agents
- [eval] Aggressively saturate benchmark hosts
- [eval] Keep benchmark orchestration direct
- [eval] Expose benchmark cli executable
- [eval] Simplify benchmark orchestration
- [eval] Autoheal stalled coordinators
- [eval] Make benchmark launch gates explicit
- [eval] Launch benchmark runs before adapting
- [eval] Drive benchmark hosts aggressively
- [eval] Bound orchestration status and reclaim dead workers
- [eval] Isolate benchmark agent from source checkout
- [eval] Preserve worker affinity in orchestration
- [eval] Route supervised benchmark through coordinator
- [eval] Read current sqlite work on every claim
- [eval] Satisfy Linux prepared-host Clippy ([#134](https://github.com/gakonst/nanocodex/issues/134))
- [eval] Install harnesses during task preparation
- [eval] Make profile identity host independent
- [eval] Prepare task images under the durable lease
- [browser] Keep deferred schema lookup private
- [tools] Restore stock Codex code mode parity
- [oai] Preserve code mode notifications in replay
- [cli] Honor browser cookie opt-out
- [cli] Default browser to Brave
- Close remaining Codex wire parity gaps
- [tui] Sanitize resume picker metadata
- [cli] Remove Tempo charge cap
- [rivet] Keep sandbox previews durable
- [rivet] Use direct actor preview gateway
- [cloudflare] Preserve snapshot tool definition
- [cloudflare] Proxy sandbox previews through worker
- [rivet] Reuse cached cloud credentials
- [web] Follow live eval evidence incrementally
- [eval] Harden durable differential progress
- [tui] Strip quote chrome from copied markdown
- [cloudflare] Synchronize live browser clients
- [cloudflare] Clarify deployed session authorization
- [cloudflare] Harden subscription edge demo
- [web] Retain immutable comparison evidence
- [web] Require immutable paired eval plans
- [eval] Harden toolbox runtime fallbacks
- [eval] Match curl with its CA bundle
- [tui] Initialize configured fast mode
- [tui] Support Option-Backspace word deletion
- [egress] Preserve middleware error chains
- [vm] Stabilize image cache identity
- [vm] Own guest command cleanup
- [tools] Keep tool search visible in code mode
- [tools] Align Code Mode tool contracts
- [browser] Gate Safari discovery by platform
- [browser] Decouple cookies from executable
- [rivet] Reserve turns before replay lookup
- [mpp] Prefer NanoUSD via provider policy
- [rivet] Own local server lifecycle
- [agent] Dispatch unnamespaced hosted tools
- [wasm] Harden host socket upgrades
- [tui] Inherit terminal foreground colors
- [tls] Standardize rustls on ring
- [examples] Remove duplicate transcript-tail arm
- [agent] Preserve model in adapter checkpoints
- [examples] Handle realtime transcript tails
- [agent] Retain model in fork checkpoints
- Build Tower services from effective agent config
- Preserve Codex rollout model compatibility
- [update] Cache-bust rolling release assets
- [ci] Allow release builds to finish
- [ci] Restore nightly platform builds
- Cancel voice-started turns from the TUI
- Finish voice platform cleanup
- Allow unsupported audio stubs
- [ci] Stabilize loaded Linux checks
- [ci] Stabilize observability tests
- [vm] Harden cache and session lifecycle
- [vm] Preserve parallel tool execution
- [tui] Preserve streaming frame boundaries

### Dependencies

- [rivet] Remove AgentOS and Pi dependencies

### Documentation

- Keep eval debugging active during waves
- Streamline experimental eval iteration
- [web] Explain generated Harbor data
- Refresh project roadmap
- [rivet] Remove stale AgentOS reference
- [browser] Keep agent example at consumer boundary

### Features

- [cli] Enable subagents by default
- [tui] Add simplify workflow
- [eval] Add external harness adapter
- [eval] Add OpenAI Evals adapter
- [eval] Add Agents Last Exam adapter
- [eval] Add ARC-AGI-3 public smoke adapter
- [eval] Add BrowseComp adapter
- [eval] Add GPQA Diamond adapter
- [eval] Add GDPval adapter
- [eval] Add HealthBench Professional adapter
- [eval] Add MRCR adapter
- [eval] Add GraphWalks adapter
- [eval] Add GeneBench Pro adapter
- [eval] Add SWE-Atlas QnA adapter
- [eval] Add SWE-bench adapter
- [eval] Add Arena-Hard adapter
- [eval] Add Harbor benchmark adapter
- [eval] Isolate model judges behind verifier runtime
- [eval] Route profiles through adapter catalog
- [eval] Add benchmark adapter foundation
- [web] Add durable eval task board
- [cli] Port Tact subagent runtime
- [eval] Externalize benchmark orchestration policy
- [eval] Add work through coordinator API
- [eval] Place supervised runtime on explicit volume
- [eval] Expose retained eval coordinator API
- [eval] Make sqlite own benchmark work
- [eval] Supervise durable benchmarks with systemd
- [mpp] Configure Tempo payment token
- [eval] Label coordinator workers
- [eval] Pin differential harness versions
- [eval] Retain native coordinator trajectories
- [eval] Coordinate directly over Tailscale
- [eval] Coordinate pull workers over HTTP
- [eval] Add durable profile ledger
- [browser] Add pixel-calibrated captures
- [cli] Enable browser tools by default
- [tools] Align current Codex parity
- [cli] Add interactive resume session picker
- [examples] Use exe.dev as external sandbox
- [examples] Add retained exe.dev agent
- [rivet] Verify hosted sandbox previews
- [examples] Add durable platform sandboxes
- [model] Support Terra and routed OpenAI model IDs
- [web] Add live evaluation dashboard
- [cli] Add durable evaluation commands
- [eval] Add durable evaluation SDK
- [cli] Default credits to hosted API
- [tui] Improve markdown math rendering
- [cloudflare] Add resumable web client and local subscription egress
- [cloudflare] Run locally with Codex subscription auth
- [cloudflare] Add detachable durable REPL
- Run Nanocodex on Cloudflare Durable Objects
- [vercel] Add durable Workflow actor demo
- [eval] Add priority-processing fast path
- [vm] Add disposable guest overlays
- [agent] Describe remote execution context
- [browser] Import Firefox and Safari cookies
- [browser] Select cookie source profiles
- [browser] Import Brave profile cookies
- [rivet] Synchronize actor clients
- [rivet] Deploy subscription demo to Compute
- [rivet] Package demo for Rivet Compute
- [rivet] Add resumable browser client
- [rivet] Run locally with Codex subscription auth
- [rivet] Add detachable actor REPL
- Add Rivet Actors and AgentOS example
- [egress] Harden secret policy and VM routing
- [wasm] Support CSP-safe direct host tools
- Expose reusable WASM host transport
- [voice] Add Codex realtime parity
- Support Luna
- Close Codex realtime parity gaps
- Match Codex realtime steering
- Add reusable realtime voice sessions
- [cli] Integrate deferred browser tooling
- [browser] Add experimental VM-backed browser
- [tui] Render display math with Ratatex
- [tools] Expose the ambient sensitive environment
- [vm] Add retained VM-backed workspace tools
- [cli] Cache and switch installed versions

### Miscellaneous Tasks

- Exclude nanocodex-bin tests
- [release] Refresh 0.4.0 changelogs
- [release] Prepare 0.4.0
- [eval] Update Harbor adapter lockfile
- Update JavaScript package size budget
- Update JavaScript package size budget
- Retire Harbor delivery workflows
- Decouple Harbor from nightly releases
- [mpp] Pin merged challenge selection
- Update ruint past advisory

### Other

- Merge pull request [#160](https://github.com/gakonst/nanocodex/issues/160) from gakonst/release/v0.4.0
- Merge pull request [#127](https://github.com/gakonst/nanocodex/issues/127) from gakonst/feat/simplify-workflow
- Merge pull request [#157](https://github.com/gakonst/nanocodex/issues/157) from gakonst/feat/eval-adapter-external-v2
- Merge pull request [#156](https://github.com/gakonst/nanocodex/issues/156) from gakonst/feat/eval-adapter-openai-evals-v2
- Merge pull request [#155](https://github.com/gakonst/nanocodex/issues/155) from gakonst/feat/eval-adapter-agents-last-exam-v2
- Merge pull request [#154](https://github.com/gakonst/nanocodex/issues/154) from gakonst/feat/eval-adapter-arc-agi-3-v2
- Merge pull request [#153](https://github.com/gakonst/nanocodex/issues/153) from gakonst/feat/eval-adapter-browsecomp-v2
- Merge pull request [#152](https://github.com/gakonst/nanocodex/issues/152) from gakonst/feat/eval-adapter-gpqa-diamond-v2
- Merge pull request [#151](https://github.com/gakonst/nanocodex/issues/151) from gakonst/feat/eval-adapter-gdpval-v2
- Merge pull request [#150](https://github.com/gakonst/nanocodex/issues/150) from gakonst/feat/eval-adapter-healthbench-pro-v2
- Merge pull request [#149](https://github.com/gakonst/nanocodex/issues/149) from gakonst/feat/eval-adapter-mrcr-v2
- Merge pull request [#148](https://github.com/gakonst/nanocodex/issues/148) from gakonst/feat/eval-adapter-graphwalks-v2
- Merge pull request [#147](https://github.com/gakonst/nanocodex/issues/147) from gakonst/feat/eval-adapter-genebench-pro-v2
- Merge pull request [#146](https://github.com/gakonst/nanocodex/issues/146) from gakonst/feat/eval-adapter-swe-atlas-v2
- Merge pull request [#145](https://github.com/gakonst/nanocodex/issues/145) from gakonst/feat/eval-adapter-swe-bench-v2
- Merge pull request [#144](https://github.com/gakonst/nanocodex/issues/144) from gakonst/feat/eval-adapter-arena-hard-v2
- Merge pull request [#143](https://github.com/gakonst/nanocodex/issues/143) from gakonst/feat/eval-adapter-harbor-v2
- Merge pull request [#142](https://github.com/gakonst/nanocodex/issues/142) from gakonst/feat/eval-adapter-foundation
- Merge pull request [#135](https://github.com/gakonst/nanocodex/issues/135) from gakonst/agent/durable-task-board-20260806
- Merge pull request [#139](https://github.com/gakonst/nanocodex/issues/139) from gakonst/fix/websocket-403-fallback
- Merge pull request [#137](https://github.com/gakonst/nanocodex/issues/137) from gakonst/feat/tact-subagents
- Merge pull request [#136](https://github.com/gakonst/nanocodex/issues/136) from gakonst/feat/eval-systemd-supervision
- Merge pull request [#124](https://github.com/gakonst/nanocodex/issues/124) from gakonst/fix/codex-parity-current
- Merge pull request [#122](https://github.com/gakonst/nanocodex/issues/122) from Giulio2002/feat/resume-session-picker
- Merge pull request [#123](https://github.com/gakonst/nanocodex/issues/123) from gakonst/feat/exe-dev-ssh-multiplex
- Merge pull request [#119](https://github.com/gakonst/nanocodex/issues/119) from gakonst/feat/exe-dev-spike
- Merge pull request [#114](https://github.com/gakonst/nanocodex/issues/114) from gakonst/feat/hosted-platform-sandboxes
- Merge pull request [#121](https://github.com/gakonst/nanocodex/issues/121) from Slokh/kartik/upstream-contributions
- Merge pull request [#61](https://github.com/gakonst/nanocodex/issues/61) from gakonst/agent/eval-diff
- Merge pull request [#117](https://github.com/gakonst/nanocodex/issues/117) from gakonst/fix/decouple-harbor-nightly
- Merge pull request [#118](https://github.com/gakonst/nanocodex/issues/118) from gakonst/codex/hosted-credits-api-default
- Merge pull request [#115](https://github.com/gakonst/nanocodex/issues/115) from gakonst/fix/markdown-copy-blockquotes
- Merge pull request [#113](https://github.com/gakonst/nanocodex/issues/113) from gakonst/feat/platform-sandbox-demos
- Merge pull request [#112](https://github.com/gakonst/nanocodex/issues/112) from gakonst/feat/vercel-workflow-demo
- Merge pull request [#109](https://github.com/gakonst/nanocodex/issues/109) from brendanjryan/brendanjryan/immutable-harbor-comparisons
- Merge pull request [#108](https://github.com/gakonst/nanocodex/issues/108) from brendanjryan/brendanjryan/eval-no-rollouts
- Merge pull request [#105](https://github.com/gakonst/nanocodex/issues/105) from brendanjryan/brendanjryan/eval-tool-install
- Merge pull request [#106](https://github.com/gakonst/nanocodex/issues/106) from brendanjryan/brendanjryan/eval-fast-mode
- Merge pull request [#107](https://github.com/gakonst/nanocodex/issues/107) from brendanjryan/brendanjryan/eval-task-images
- Merge pull request [#103](https://github.com/gakonst/nanocodex/issues/103) from 0xahzam/fix/tui-option-backspace
- Merge pull request [#111](https://github.com/gakonst/nanocodex/issues/111) from gakonst/fix/egress-payment-error-chain
- Merge pull request [#100](https://github.com/gakonst/nanocodex/issues/100) from gakonst/agent/pr61-vm-overlays
- Merge pull request [#99](https://github.com/gakonst/nanocodex/issues/99) from gakonst/agent/pr61-vm-images
- Merge pull request [#98](https://github.com/gakonst/nanocodex/issues/98) from gakonst/agent/pr61-vm-guest
- Merge pull request [#97](https://github.com/gakonst/nanocodex/issues/97) from gakonst/agent/pr61-tower-accounting
- Merge pull request [#96](https://github.com/gakonst/nanocodex/issues/96) from gakonst/agent/pr61-agent-context
- Merge pull request [#95](https://github.com/gakonst/nanocodex/issues/95) from gakonst/agent/pr61-code-mode
- Merge pull request [#101](https://github.com/gakonst/nanocodex/issues/101) from gakonst/docs/refresh-project-plan
- Merge pull request [#93](https://github.com/gakonst/nanocodex/issues/93) from gakonst/feat/brave-browser-cookies
- Merge pull request [#94](https://github.com/gakonst/nanocodex/issues/94) from gakonst/feat/rivet-synchronized-clients
- Merge pull request [#92](https://github.com/gakonst/nanocodex/issues/92) from gakonst/fix/nanousd-mpp-selection
- Merge pull request [#90](https://github.com/gakonst/nanocodex/issues/90) from gakonst/fix/rivet-subscription-cloud-demo
- Merge pull request [#74](https://github.com/gakonst/nanocodex/issues/74) from gakonst/feat/rivet-actors-wasm
- Merge pull request [#88](https://github.com/gakonst/nanocodex/issues/88) from gakonst/feat/secret-egress
- Merge pull request [#70](https://github.com/gakonst/nanocodex/issues/70) from gakonst/feat/composable-egress
- Merge pull request [#75](https://github.com/gakonst/nanocodex/issues/75) from gakonst/feat/wasm-host-transport
- Merge pull request [#87](https://github.com/gakonst/nanocodex/issues/87) from vovw/fix/terminal-foreground-colors
- Merge pull request [#86](https://github.com/gakonst/nanocodex/issues/86) from gakonst/fix/ring-only-rustls
- Merge pull request [#85](https://github.com/gakonst/nanocodex/issues/85) from gakonst/fix/realtime-tail-unreachable
- Merge pull request [#83](https://github.com/gakonst/nanocodex/issues/83) from gakonst/fix/realtime-parity-ci
- Merge pull request [#84](https://github.com/gakonst/nanocodex/issues/84) from gakonst/fix/committed-session-model
- Merge pull request [#82](https://github.com/gakonst/nanocodex/issues/82) from gakonst/feat/realtime-codex-parity
- Merge pull request [#80](https://github.com/gakonst/nanocodex/issues/80) from clabby/cl/luna
- Merge pull request [#81](https://github.com/gakonst/nanocodex/issues/81) from gakonst/fix/voice-turn-cancellation
- Merge pull request [#77](https://github.com/gakonst/nanocodex/issues/77) from gakonst/feat/realtime-voice
- Merge pull request [#78](https://github.com/gakonst/nanocodex/issues/78) from gakonst/agent/browser-tui-integration
- Merge pull request [#69](https://github.com/gakonst/nanocodex/issues/69) from gakonst/agent/pr60-experimental-browser
- Merge pull request [#68](https://github.com/gakonst/nanocodex/issues/68) from gakonst/agent/ratatex-tui
- Merge pull request [#66](https://github.com/gakonst/nanocodex/issues/66) from gakonst/codex/pr64-redraw-opt
- Merge pull request [#59](https://github.com/gakonst/nanocodex/issues/59) from cjustice/feat/ambient-sensitive-environment
- Merge pull request [#58](https://github.com/gakonst/nanocodex/issues/58) from gakonst/refactor/09-eval

### Performance

- [eval] Import only selected MRCR tasks
- [evals] Normalize treatment storage
- [evals] Accelerate live analytics reads
- [eval] Adapt benchmark fanout to host memory
- [examples] Reuse exe.dev SSH connections
- [ci] Streamline release artifact builds
- [eval] Disable rollout persistence
- [eval] Skip redundant tool installation
- [eval] Reuse prepared task images
- [ci] Avoid fat LTO for release artifacts
- Harden realtime voice audio paths
- [tui] Bound streaming redraw CPU
- [tui] Scope animation redraws

### Refactor

- [eval] Share adapter identity helpers
- [eval] Simplify durable benchmark ownership
- [eval] Simplify benchmark driver
- [eval] Make profiles pure sqlite inputs
- [eval] Simplify distributed worker runtime
- [eval] Reduce profiles to durable coordinates
- [eval] Remove residual sweep completion fields
- [eval] Collapse profile execution API
- [eval] Delete redundant orchestration
- Trim Codex parity implementation
- [mpp] Extract composable egress transport
- Fix the model for each thread

### Testing

- [cli] Own interrupt workspaces with tempfile
- [eval] Stress live coordinator scheduling
- [eval] Satisfy all-target clippy
- [rivet] Reattach long hosted smoke turns
- [vercel] Harden hosted sandbox lifecycle
- [cloudflare] Harden hosted sandbox tools
- [observability] Consume OTLP request bodies
- Synchronize realtime response queue
- [browser] Prove Nanocodex tool integration

## [0.3.0](https://github.com/gakonst/nanocodex/releases/tag/v0.3.0) - 2026-07-28

### Bug Fixes

- [release] Support Ubuntu Python baseline
- [release] Validate dynamic Python version
- [ci] Satisfy strict Clippy checks
- [ci] Build optimized Python wheels
- [js] Permit immutable preview versions
- [web] Join controller agent shutdown
- [example] Own browser CDN agent creation
- [wasm] Join graceful agent shutdown
- [web] Cancel replaced active turns
- [wasm] Support host-owned browser sockets
- [examples] Consume typed turn results
- [web] Close replaced payment sessions
- [web] Own browser agent lifecycle
- [tui] Deduplicate terminal failures
- [react] Own worker generations
- [wasm] Satisfy target-specific lint gates
- [js] Keep transport mechanics private
- [js] Isolate concurrent event observers
- [js] Preserve configured workspace
- [js] Snapshot hosted tool boundaries
- [js] Bound and release event iterators
- [docs] Honor configured Cargo target directory
- [code-mode] Schedule timers on the host worker
- [react] Accept the current SDK package
- [python] Align binding package version
- [cli] Flush terminal event after interrupt
- [mcp] Bound stdio stderr buffering
- [agent] Fail malformed continuations before terminal
- [oai] Reject empty continuation checkpoints
- [oai] Bound WebSocket pump backlog
- [mcp] Own and bound stdio subprocesses
- [mcp] Stop cross-origin credential redirects
- [wasm] Enable clock support and enforce runtime tests
- [js] Honor configured Cargo target directory
- [oai] Ignore non-assistant final messages
- [bench] Support shared ChatGPT auth
- [oai] Preserve stable item ids in ephemeral requests
- [tui] Bound syntax highlighting work
- [mcp] Send a deterministic HTTP user agent
- [agent] Remove rollout writer locking
- [ci] Include every benchmark target in Harbor builds
- [wasm] Release completed turn controls
- [api] Restore refactored consumer builds
- [agent] Align retained context with Codex
- [release] Package the current crate graph
- [docs] Open facade docs from workspace root
- [release] Tolerate npm propagation delay

### Documentation

- Show per-turn stream consumption
- Show agent event consumption
- Rename the minimal API example
- Streamline the public README
- Finalize the PR 50 public API guide
- [python] Show the owned binding workflow
- [oai] Prepare package changelog
- [parity] Classify Codex through be2e4afc
- [plan] Record PR 50 CI completion
- [perf] Record paired model latency
- [parity] Classify Codex through 3418498f
- [facade] Keep packaged API guides canonical
- [plan] Scope delivery to PR 50
- [tui] Explain event batching
- Frame nanocodex as frontier agent building blocks

### Features

- [python] Expose the owned agent lifecycle
- Stabilize observability and USD cost

### Miscellaneous Tasks

- [release] Refresh 0.3.0 changelogs
- [release] Prepare 0.3.0
- [consumers] Gate promoted language bindings
- [js] Ship compiled tui entrypoints
- Disable Python bindings on pull requests
- Run preview checks from package directory
- Keep hanging wasm tests non-blocking
- Cancel superseded PR artifact builds
- Build PR binaries on demand
- Ignore local VM caches

### Other

- Merge pull request [#50](https://github.com/gakonst/nanocodex/issues/50) from gakonst/refactor/05-observability
- Run full leaderboard jobs from PR artifacts

### Performance

- [web] Cap initial bundle request fan-out
- [python] Gate binding resource budgets
- [web] Gate controller lifecycle throughput
- [web] Gate production bundle graph
- [example] Isolate browser mpp runtime
- Gate PR 50 hot paths
- Keep Tempo out of direct CLI builds

### Refactor

- [api] Stabilize Tower and lifecycle boundaries
- [js] Preserve typed turn results
- [oai] Contain agent-only session internals
- [agent] Decompose model lifecycle
- [tools] Decompose runtime ownership
- [agent] Decompose driver control
- [agent] Decompose rollout persistence
- Align agent lifecycle with Codex
- Isolate platform runtime boundaries
- [api] Enforce canonical crate boundaries
- Stabilize public SDK surface
- Extract owned agent lifecycle
- Consolidate tools and MCP
- Consolidate the OpenAI Responses API

### Styling

- [oai] Format final-message regression
- [oai] Format item id policy

### Testing

- [python] Gate lifecycle and binding performance
- [js] Isolate performance gates
- [js] Gate binding overhead and package size
- [wasm] Lock owned session lifecycle
- [js] Validate installed package boundary
- [observability] Isolate tracing capture
- [agent] Expect stable response item ids
- [observability] Verify full-fidelity turn traces
- [oai] Compare tool search arguments semantically
- [mcp] Exercise provider-native discovery
- [tui] Lock lifecycle parity
- [mcp] Verify read-only parallel dispatch
- [tools] Serialize traced runtime integration tests
- [tui] Preserve stored checkpoint branch coverage

## [0.2.0](https://github.com/gakonst/nanocodex/releases/tag/v0.2.0) - 2026-07-26

### Bug Fixes

- Preserve recovered resume and TUI work
- [mpp] Prevent paid request replays
- [cli] Disambiguate Tempo API base argument
- [mpp] Prefer session payments for Responses
- [tui] Show model connection progress
- [bindings] Resume rollout snapshots in Node
- [mpp] Harden paid Responses transports
- [credits] Support primitive Tempo wallet signers
- [wasm] Retain snapshot resume compatibility
- [harbor] Make leaderboard runs non-interactive
- [cli] Retain Tempo session access keys
- [web] Provision scoped Tempo payment keys
- [nanousd] Persist signed mints before broadcast
- [cli] Honor Tempo session deposit default
- Use the Tempo API MCP endpoint
- Harden Harbor run recovery
- Cancel headless turns on interrupt
- Estimate visible context before compaction
- Match Codex tool behavior
- Align Responses request serialization with Codex
- Retry API errors classified by type
- [cli] Bound MPP egress concurrency
- Omit IDs from Responses Lite tools ([#26](https://github.com/gakonst/nanocodex/issues/26))
- [cli] Bound MPP egress origin concurrency ([#20](https://github.com/gakonst/nanocodex/issues/20))
- [cli] Surface terminal MPP payment failures ([#19](https://github.com/gakonst/nanocodex/issues/19))
- Match Codex compaction boundaries
- Compact before follow-on sampling
- [cli] Keep MCP tests last
- [mpp] Correlate paid egress retries ([#15](https://github.com/gakonst/nanocodex/issues/15))
- [tui] Refine running activity presentation
- [cli] Batch Tempo session top-ups
- [cli] Take charge autoswap fixes
- [cli] Avoid replaying Tempo key authorizations
- [tui] Match Amp markdown selection semantics
- [tui] Render live code-mode activity
- [tui] Copy fenced code without chrome
- [ci] Resolve linked website dependencies
- [tui] Highlight TypeScript patches

### Dependencies

- [deps] Finalize Tempo accounts pins
- [deps] Update Tempo Accounts wallet
- [deps] Update Tempo Alloy accounts wallet
- Bump mpp-rs autoswap diagnostics ([#25](https://github.com/gakonst/nanocodex/issues/25))
- Bump mpp-rs session rollback ([#22](https://github.com/gakonst/nanocodex/issues/22))
- Bump mpp-rs session fixes ([#21](https://github.com/gakonst/nanocodex/issues/21))
- Merge pull request [#17](https://github.com/gakonst/nanocodex/issues/17) from gakonst/fix/mpp-rpc-rate-limit-retry
- [mpp] Bump RPC retry support

### Features

- [python] Expose steer, cancel, spawn, and fork controls
- [tempo] Use NanoUSD Charge over HTTPS
- [credits] Support loopback Stripe deployment
- [tui] Restore rollout activity
- [tui] Render resumed rollout messages
- Resume Codex rollouts in Nanocodex
- [mcp] Add OAuth login and hot reload
- Add MPP-backed JavaScript agent sessions
- Add NanoUSD credits service
- Default to high reasoning
- Preserve stable response item IDs
- Align code mode with Codex
- [mcp] Prewarm deferred default servers
- [tui] Add runtime mode controls
- [cli] Autoswap Tempo session deposits
- [cli] Route OpenAI through Tempo MPP
- [agent] Resume sessions from durable snapshots ([#13](https://github.com/gakonst/nanocodex/issues/13))
- [code-mode] Stream nested tool lifecycles
- [tui] Improve tool activity presentation
- [tools] Track nested call start offsets
- [agent] Support dynamic fast mode ([#14](https://github.com/gakonst/nanocodex/issues/14))
- Support HTTPS and Responses replay policies ([#12](https://github.com/gakonst/nanocodex/issues/12))
- [agent] Support changing thinking between turns
- [web] Render plan updates in browser TUI
- [tui] Render plan updates as checklists
- [agent] Load global Codex instructions

### Miscellaneous Tasks

- [release] Prepare 0.2.0
- [tempo] Use final shared SDK revisions
- [tempo] Finalize Accounts dependency stack
- [tempo] Pin Accounts wallet fixes
- Publish Harbor-compatible nightlies
- [mcp] Clarify browser login status
- Raise Rust baseline to 1.97
- Refresh Harbor Rust builder
- [ci] Allow bounded Hudsucker fork
- [mpp] Take expiring session nonces

### Other

- Merge pull request [#44](https://github.com/gakonst/nanocodex/issues/44) from gakonst/chore/tempo-accounts-final
- Merge pull request [#41](https://github.com/gakonst/nanocodex/issues/41) from Ayush7614/feat/python-lifecycle-controls
- Merge pull request [#39](https://github.com/gakonst/nanocodex/issues/39) from gakonst/agent/mpp-charge-runtime-safety
- Merge pull request [#35](https://github.com/gakonst/nanocodex/issues/35) from gakonst/agent/mpp-runtime-fixes
- Merge pull request [#36](https://github.com/gakonst/nanocodex/issues/36) from gakonst/feat/nanousd-http-charge
- Merge pull request [#30](https://github.com/gakonst/nanocodex/issues/30) from gakonst/bench/mcp-oauth-hot-reload
- Merge remote-tracking branch 'origin/master' into bench/mcp-oauth-hot-reload
- Merge pull request [#31](https://github.com/gakonst/nanocodex/issues/31) from gakonst/codex/rollout-resume-bench
- Merge pull request [#29](https://github.com/gakonst/nanocodex/issues/29) from gakonst/agent/harbor-nightly-binary
- Merge pull request [#27](https://github.com/gakonst/nanocodex/issues/27) from gakonst/fix/mpp-egress-resource-bounds
- Merge pull request [#18](https://github.com/gakonst/nanocodex/issues/18) from clabby/cl/compact-before-turn
- Merge pull request [#2](https://github.com/gakonst/nanocodex/issues/2) from gakonst/feat/mpp-integration
- [tui] Gate code-mode completion churn

### Performance

- [tempo] Minimize SDK integration surface
- [tui] Incrementally render reasoning streams
- [web] Defer repository surfaces
- [harbor] Download hosted agents in sandbox
- [harbor] Avoid duplicate event logs
- [mcp] Cache OAuth metadata across reloads
- [cli] Allow 128 concurrent MPP requests
- Preserve COW history during compaction
- [cli] Accelerate Tempo session cold starts
- [tui] Cache nested tools and streaming markdown

### Refactor

- [mpp] Use Tempo Accounts charge provider

### Testing

- Ignore JSON argument key order
- Add xhigh Terminal-Bench presets
- Add stock Codex parity differential
- Stress parallel MPP egress replay ([#24](https://github.com/gakonst/nanocodex/issues/24))
- Synchronize code cell termination output ([#23](https://github.com/gakonst/nanocodex/issues/23))

## [0.1.1](https://github.com/gakonst/nanocodex/releases/tag/v0.1.1) - 2026-07-23

### Bug Fixes

- [wasm] Scope host tools to agent sessions
- [tui] Render images and wrap patches
- [ci] Update dependency policy
- [release] Build Docker images natively
- [shell] Serialize session input and process interrupts
- [code-mode] Preserve tool results across yields
- [tui] Pace newline-heavy stream scrolling
- [tui] Cancel pending scroll on manual input
- [tui] Finish deferred branch switches
- [auth] Disable response storage for ChatGPT
- [cli] Prefer OPENAI_API_KEY by default
- [harbor] Keep hosted build manifest complete
- [ci] Remove typo-triggering auth test
- [install] Handle shell profile update failures

### Documentation

- Record evaluation runner boundaries
- Explain shared prompt caches
- Record evaluation runner boundaries
- Explain Nanocodex design thesis
- [perf] Document runtime profiling results

### Features

- [js] Add CDN previews and package releases
- [web] Embed the reusable wasm agent terminal
- [tui] Add reusable browser terminal packages
- [react] Add typed worker lifecycle bindings
- [js] Redesign runtime-specific agent API
- Support GPT-5.6 Pro reasoning mode
- Share prompt-cache warmups
- Persist Codex-compatible rollouts
- [tui] Polish transcript and composer UX
- Expose VM-ready standard tools
- [release] Add nightly and GHCR delivery
- [web] Add browser agent terminal
- [tui] Refine steering and tool activity
- [agent] Propagate reasoning mode across runtime surfaces
- [bindings] Publish JavaScript and Python clients
- [wasm] Add full agent lifecycle control
- [tools] Align the WASM host runtime contract
- [agent] Add shared cache and resumable rollouts
- [tui] Polish transcript and composer UX
- Expose VM-ready standard tools
- [tui] Improve transcript and clipboard interaction
- [tui] Improve live transcript interaction
- [tools] Allow replacing workspace tools
- [tui] Switch branches from live navigator
- [tui] Add history editing and branch navigation
- [agent] Expose clean sibling spawn
- [tools] Embed QuickJS code mode
- [cli] Add same-session completion audit
- [agent] Refine task execution guidance
- [telemetry] Measure end-to-end TUI stream latency
- [cli] Prefer stored ChatGPT login

### Miscellaneous Tasks

- [release] Prepare 0.1.1
- [web] Refresh repository data
- Add code mode validation batch
- [eval] Remove benchmark-specific tuning

### Other

- Merge remote-tracking branch 'origin/master'
- Merge pull request [#11](https://github.com/gakonst/nanocodex/issues/11) from gakonst/agent/gpt-5-6-pro-config
- Merge pull request [#9](https://github.com/gakonst/nanocodex/issues/9) from gakonst/agent/cloneable-nanocodex-builder
- Make NanocodexBuilder cloneable
- Merge pull request [#8](https://github.com/gakonst/nanocodex/issues/8) from gakonst/agent/embedded-quickjs-code-mode
- Merge pull request [#7](https://github.com/gakonst/nanocodex/issues/7) from gakonst/agent/completion-audit

### Performance

- [tui] Optimize long-session rendering and interaction
- [tui] Make streaming rendering content-size independent

### Refactor

- [service] Exhaustively classify WASM retry errors

### Testing

- [tools] Cover custom tools in code mode
- [cli] Keep subagents opt-in

## [0.1.0](https://github.com/gakonst/nanocodex/releases/tag/v0.1.0) - 2026-07-21

### Bug Fixes

- [observability] Retain yielded tool lineage
- [tools] Preserve live shell session ids
- [tui] Reconcile pending steer state
- [harbor] Provision portable CLI tools
- [tui] Suppress cancellation error rows
- [tui] Distinguish cancelled tools
- Emit completed assistant items from Responses ([#4](https://github.com/gakonst/nanocodex/issues/4))
- Preserve assistant message phases in events ([#3](https://github.com/gakonst/nanocodex/issues/3))
- [cli] Select one command configuration
- [service] Own proxy-aware WebSocket connector
- [service] Honor SSL_CERT_FILE for WebSockets
- [wasm] Align checkpoint turn handling
- [ci] Allow pinned WebSocket forks
- [service] Honor proxy settings for WebSockets
- [eval] Publish Harbor streams from host capture
- [eval] Atomically publish Harbor JSONL
- [eval] Provision Node for canonical task images
- [cli] Satisfy steering UI lints
- [ci] Satisfy observability stress lints
- [observability] Satisfy rustfmt
- [ci] Tolerate OTLP warm-up connections
- [ci] Read complete OTLP test headers
- [ci] Use portable MCP fixture path
- [ci] Support Windows shell tooling
- Include macros crate in agent image build
- Preserve master lifecycle behavior after rebase
- Recover from unsupported direct tools
- Normalize and bound shell sessions
- Preserve canonical context through compaction
- Match Codex context token accounting
- Recover from invalid image requests
- Bound Codex compaction inputs
- Match Codex compacted history retention
- Follow sol context window growth
- Follow sol reasoning summary default
- Identify responses lite websocket sessions
- Validate code mode stored values
- Preserve eval task completion state
- Preserve failed code mode output
- Validate code mode image outputs
- Accept nullable usage details
- Accept completed responses without usage
- Report selected shell in model context
- Match Codex Sol compaction limit
- Keep apply patch compatible with Rust 1.85
- Support Linux artifact Rust version
- Normalize image inputs for the model
- Harden local code mode runtime
- Keep API diagnostics valid JSONL
- Isolate verifier python packages
- Cache scientific verifier dependencies
- Reconnect stale Responses websockets
- Keep api key out of process arguments
- Service websocket keepalives independently
- Preserve Rust 1.85 compatibility

### Dependencies

- Lock fork benchmark dependencies
- Cache system verifier dependencies

### Documentation

- [tui] Record research and keybindings
- Simplify configuration section
- Move example comments above code
- Sharpen repository positioning
- Add complete agent lifecycle example
- Streamline readme presentation
- Center readme on public agent lifecycle
- Document the lifecycle API design
- Lead README with Codex comparison
- Fix Harbor spelling
- [eval] Start Rust runner design log
- Explain checkpoint orchestration tradeoffs
- Record orchestration decision context
- [observability] Add local Jaeger workflow
- Plan efficient steering and branching
- Align roadmap with the library-first SDK
- Lead with the library API
- Record nanocodex terminal bench gate
- Demonstrate detached event handling
- Record Tower validation results
- Plan eval-driven UI tool parity
- Advance Codex review checkpoint
- Track Codex upstream review checkpoint
- Exclude skills from harness scope
- Record intentional runtime boundaries
- Record responses retry rewrite
- Describe Codex session and tool behavior
- Prefer local Codex reference
- Record tune mjcf variance
- Record 33-task eval gate
- Record custom heap crash eval
- Record Coq proof eval
- Record build pmars eval
- Record 30-task eval gate
- Record write compressor eval
- Record constraints scheduling eval
- Record largest eigenvalue eval
- Record 26-task eval gate
- Record schemelike eval
- Record 24-task eval gate
- Record 23-task eval gate
- Record core wars eval
- Record dna assembly eval
- Record 22-task eval gate
- Record 21-task eval gate
- Refine full-suite timing breakdown
- Record cleanup prompt regressions
- Record ambiguous ELF eval boundary
- Record forensic prompt regressions
- Record git recovery baseline
- Record sanitizer benchmark boundary
- Record multibranch benchmark baseline
- Record vulnerability benchmark baseline
- Record Cython benchmark baseline
- Record regex benchmark baseline
- Record headless terminal baseline
- Record three-task eval baseline
- Restore hosted-first runtime contract
- Plan model runtime cleanup
- Restart with Harbor-first plan

### Features

- Add ChatGPT subscription authentication
- [observability] Export full-fidelity agent traces
- [agent] Checkpoint active turn boundaries
- [core] Expose event stream request IDs
- [cli] Add steerable queues and cancellation
- [agent] Add controllable conversation lifecycle
- [cli] Add steerable queues, btw forks, and subagents
- [agent] Add checkpoint forks and active-turn steering
- [web] Add commit navigation rail
- [observability] Add end-to-end OTLP tracing
- [tools] Reuse persistent Node code-mode host
- Add Cloudflare WASM playground
- Add MCP observability and release automation
- Add embedded web and MCP integrations
- Add embedded Python and WASM bindings
- [cli] Add ratatui daily driver
- Unify tool registry and add tool macro
- Support typed custom tools
- Refactor SDK around Tower Responses service
- Improve agent lifecycle parity
- Advance eval guidance and results viewer
- Support Codex-style multimodal task input
- Centralize model context history
- Add Codex image generation
- Support code mode notifications
- Honor server turn continuation
- Match Codex shell selection
- Match Codex apply patch semantics
- Match Codex image preparation
- Align code mode tool shapes with Codex
- Add standalone web search
- [web] Redesign NanoCodex dashboard
- Align task context with Codex
- Add nanocodex web app
- Add PTY shell sessions
- Add resumable code-mode cells
- Align runtime with Codex Responses Lite
- Load dotenv for direct runs
- Align agent system prompt with Codex
- Load project agent instructions
- Add hosted orchestration profiles
- Add hosted response state controls
- Use native shell with programmatic calls
- Expand Harbor eval slice
- Add model-driven Harbor agent loop
- Establish lean Harbor installed-agent baseline
- Establish fast Harbor eval loop

### Miscellaneous Tasks

- [release] Refresh 0.1.0 changelog
- [release] Prepare 0.1.0
- [release] Refresh 0.1.0 changelogs
- [release] Add per-crate changelogs
- [release] Finalize 0.1.0 changelog
- [release] Update 0.1.0 changelog
- [release] Refresh 0.1.0 changelog
- [release] Automate publishing and native updates
- Defer Windows test coverage
- Update repository identity
- Sync Codex Sol base instructions
- Add terminal hyperlink smoke test

### Other

- Add stateful paired parity harness
- Add reproducible Codex parity workload
- Pin leaderboard Terminal-Bench 2.1 configuration
- Compare checkpoint forks with transcript replay
- Harden Harbor adapter for Terminal-Bench 2.1
- Demonstrate dynamic fork orchestration
- Compose subagents with unified events
- Refine tool execution and web search wiring
- Add terminal-bench lifecycle eval cohorts
- Streamline architecture callgraph
- Admit three scientific tasks
- Record Responses Lite parity baseline
- Admit CompCert build task
- Defer unstable mjcf tuning task
- Admit overfull hbox task
- Record green 35-task gate
- Admit build pov ray task
- Admit circuit fibsqrt task
- Exclude unstable core wars task
- Require installable verifier packages
- Add qemu startup benchmark
- Accept qemu verifier package order
- Support legacy Python verifier images
- Add custom heap crash benchmark
- Add Coq proof benchmark
- Add build pmars benchmark
- Add tune mjcf benchmark
- Add write compressor benchmark
- Add constraints scheduling benchmark
- Add largest eigenvalue benchmark
- Defer stale protein assembly benchmark
- Add distribution search benchmark
- Add schemelike benchmark
- Add pypi server benchmark
- Preserve explicit contracts
- Defer unstable dna benchmarks
- Add sparql benchmark
- Add core wars benchmark
- Add dna assembly benchmark
- Add dna insert benchmark
- Add merge diff benchmark
- Defer raman fitting benchmark
- Defer query optimization benchmark
- Add grpc service benchmark
- Preserve background processes after exit
- Add inference scheduler benchmark
- Add sqlite gcov benchmark
- Bootstrap verifier apt over TLS
- Add cobol modernization benchmark
- Preserve forensic inputs first
- Exclude cyber-policy benchmark
- Add binary secret benchmark
- Add log summary benchmark
- Preserve canonical verifier setup
- Add Rust C polyglot benchmark
- Add Python C polyglot benchmark
- Add nginx service benchmark
- Add truncated database recovery benchmark
- Add database WAL recovery benchmark
- Focus ladder on shell code tasks
- Separate image preparation from scored runs
- Add git leak recovery benchmark
- Verify destructive transformations
- Add sanitizer benchmark controls
- Use installed Chromium driver
- Add multibranch deployment benchmark
- Add single-task eval loop
- Add vulnerability benchmark
- Add Cython build benchmark
- Add regex log benchmark
- Add headless terminal benchmark
- Verify external lifecycle boundaries
- Add async cancellation benchmark

### Performance

- [tools] Share code mode history snapshots
- [shell] Share process drain grace deadline
- [tools] Align nested shell yield deadlines
- [service] Profile and trim response hot path
- [tools] Prewarm code mode node host
- [core] Iterate incremental history suffixes
- [tui] Coalesce streaming renders
- Cache guarded texlive verifier setup

### Refactor

- [agent] Simplify error propagation
- [agent] Flatten the public error surface
- [tools] Return typed handler results
- Rename project to nanocodex
- Expose pending turn results
- Simplify owned agent API
- Move code mode failure evidence
- Simplify code mode cell IDs
- Own tool runtime directly
- Store conversation deltas by boundary
- Share response stream ingestion
- Narrow retained compaction history
- Simplify websocket model runtime
- Centralize model run lifecycle
- Remove obsolete runtime modes

### Testing

- [python] Align empty credential error
- [tui] Cover escape cancellation
- [observability] Add retained-session stress coverage
- Stabilize PTY readiness checks

<!-- generated by git-cliff -->
