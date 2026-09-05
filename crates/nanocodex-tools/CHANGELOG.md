# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0](https://github.com/gakonst/nanocodex/releases/tag/v0.5.0) - 2026-08-12

### Bug Fixes

- [tui] Handle terminal input as shell output
- [events] Preserve structured results universally
- [events] Retain structured nested tool results

### Miscellaneous Tasks

- [release] Prepare 0.5.0

### Other

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
