# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0](https://github.com/gakonst/nanocodex/releases/tag/v0.5.0) - 2026-08-12

### Miscellaneous Tasks

- [release] Prepare 0.5.0

## [0.4.0](https://github.com/gakonst/nanocodex/releases/tag/v0.4.0) - 2026-08-11

### Bug Fixes

- [tools] Align Code Mode tool contracts

### Features

- [examples] Use exe.dev as external sandbox
- [model] Support Terra and routed OpenAI model IDs
- [voice] Add Codex realtime parity
- Support Luna
- Match Codex realtime steering
- Add reusable realtime voice sessions

### Miscellaneous Tasks

- [release] Refresh 0.4.0 changelogs
- [release] Prepare 0.4.0

### Other

- Merge pull request [#160](https://github.com/gakonst/nanocodex/issues/160) from gakonst/release/v0.4.0
- Merge pull request [#119](https://github.com/gakonst/nanocodex/issues/119) from gakonst/feat/exe-dev-spike
- Merge pull request [#121](https://github.com/gakonst/nanocodex/issues/121) from Slokh/kartik/upstream-contributions
- Merge pull request [#95](https://github.com/gakonst/nanocodex/issues/95) from gakonst/agent/pr61-code-mode
- Merge pull request [#82](https://github.com/gakonst/nanocodex/issues/82) from gakonst/feat/realtime-codex-parity
- Merge pull request [#80](https://github.com/gakonst/nanocodex/issues/80) from clabby/cl/luna
- Merge pull request [#77](https://github.com/gakonst/nanocodex/issues/77) from gakonst/feat/realtime-voice

### Refactor

- Fix the model for each thread

## [0.3.0](https://github.com/gakonst/nanocodex/releases/tag/v0.3.0) - 2026-07-28

### Bug Fixes

- [api] Restore refactored consumer builds
- [release] Package the current crate graph

### Documentation

- Finalize the PR 50 public API guide
- [facade] Keep packaged API guides canonical

### Features

- Stabilize observability and USD cost

### Miscellaneous Tasks

- [release] Refresh 0.3.0 changelogs
- [release] Prepare 0.3.0

### Other

- Merge pull request [#50](https://github.com/gakonst/nanocodex/issues/50) from gakonst/refactor/05-observability

### Refactor

- Align agent lifecycle with Codex
- Isolate platform runtime boundaries
- [api] Enforce canonical crate boundaries
- Stabilize public SDK surface
- Extract owned agent lifecycle
- Consolidate tools and MCP
- Consolidate the OpenAI Responses API

## [0.2.0](https://github.com/gakonst/nanocodex/releases/tag/v0.2.0) - 2026-07-26

### Bug Fixes

- Preserve recovered resume and TUI work
- [mpp] Prevent paid request replays
- [bindings] Resume rollout snapshots in Node
- [mpp] Harden paid Responses transports
- [wasm] Retain snapshot resume compatibility
- Estimate visible context before compaction
- Omit IDs from Responses Lite tools ([#26](https://github.com/gakonst/nanocodex/issues/26))
- Match Codex compaction boundaries
- Compact before follow-on sampling

### Features

- [tui] Restore rollout activity
- [tui] Render resumed rollout messages
- Resume Codex rollouts in Nanocodex
- [mcp] Add OAuth login and hot reload
- Default to high reasoning
- Preserve stable response item IDs
- Align code mode with Codex
- [mcp] Prewarm deferred default servers
- [agent] Resume sessions from durable snapshots ([#13](https://github.com/gakonst/nanocodex/issues/13))
- [code-mode] Stream nested tool lifecycles
- [tools] Track nested call start offsets
- [agent] Support dynamic fast mode ([#14](https://github.com/gakonst/nanocodex/issues/14))
- Support HTTPS and Responses replay policies ([#12](https://github.com/gakonst/nanocodex/issues/12))
- [agent] Support changing thinking between turns
- [agent] Load global Codex instructions

### Miscellaneous Tasks

- [release] Prepare 0.2.0
- Raise Rust baseline to 1.97

### Other

- Merge pull request [#39](https://github.com/gakonst/nanocodex/issues/39) from gakonst/agent/mpp-charge-runtime-safety
- Merge pull request [#35](https://github.com/gakonst/nanocodex/issues/35) from gakonst/agent/mpp-runtime-fixes
- Merge pull request [#30](https://github.com/gakonst/nanocodex/issues/30) from gakonst/bench/mcp-oauth-hot-reload
- Merge remote-tracking branch 'origin/master' into bench/mcp-oauth-hot-reload
- Merge pull request [#31](https://github.com/gakonst/nanocodex/issues/31) from gakonst/codex/rollout-resume-bench
- Merge pull request [#18](https://github.com/gakonst/nanocodex/issues/18) from clabby/cl/compact-before-turn

### Performance

- Preserve COW history during compaction

### Testing

- Ignore JSON argument key order

## [0.1.1](https://github.com/gakonst/nanocodex/releases/tag/v0.1.1) - 2026-07-23

### Bug Fixes

- [wasm] Scope host tools to agent sessions
- [observability] Retain yielded tool lineage
- Emit completed assistant items from Responses ([#4](https://github.com/gakonst/nanocodex/issues/4))
- Preserve assistant message phases in events ([#3](https://github.com/gakonst/nanocodex/issues/3))
- [wasm] Align checkpoint turn handling
- [observability] Satisfy rustfmt

### Documentation

- Align roadmap with the library-first SDK
- Lead with the library API

### Features

- Support GPT-5.6 Pro reasoning mode
- Share prompt-cache warmups
- Persist Codex-compatible rollouts
- Expose VM-ready standard tools
- [agent] Propagate reasoning mode across runtime surfaces
- [wasm] Add full agent lifecycle control
- [agent] Add shared cache and resumable rollouts
- Expose VM-ready standard tools
- [tools] Allow replacing workspace tools
- [agent] Expose clean sibling spawn
- [tools] Embed QuickJS code mode
- [telemetry] Measure end-to-end TUI stream latency
- Add ChatGPT subscription authentication
- [observability] Export full-fidelity agent traces
- [agent] Checkpoint active turn boundaries
- [agent] Add controllable conversation lifecycle
- [agent] Add checkpoint forks and active-turn steering
- [observability] Add end-to-end OTLP tracing
- Add MCP observability and release automation
- Add embedded web and MCP integrations
- Add embedded Python and WASM bindings

### Miscellaneous Tasks

- [release] Prepare 0.1.1
- [release] Prepare 0.1.0
- [release] Refresh 0.1.0 changelogs
- [release] Add per-crate changelogs
- [release] Automate publishing and native updates

### Other

- Merge pull request [#11](https://github.com/gakonst/nanocodex/issues/11) from gakonst/agent/gpt-5-6-pro-config
- Merge pull request [#9](https://github.com/gakonst/nanocodex/issues/9) from gakonst/agent/cloneable-nanocodex-builder
- Make NanocodexBuilder cloneable
- Merge pull request [#8](https://github.com/gakonst/nanocodex/issues/8) from gakonst/agent/embedded-quickjs-code-mode
- Compose subagents with unified events

### Performance

- [tui] Optimize long-session rendering and interaction
- [tools] Share code mode history snapshots

### Refactor

- [agent] Simplify error propagation
- [agent] Flatten the public error surface
- [tools] Return typed handler results
- Rename project to nanocodex

<!-- generated by git-cliff -->
