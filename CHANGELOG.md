# Changelog

All notable changes to Nanocodex are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

- Renamed persistent builder configuration to `instructions(...)`, made custom
  Responses services factory-only, and extended exact-turn cancellation to
  queued as well as active turns. Workspace policy now belongs exclusively to
  the agent builder, prompts contain only user input, and ignored `Turn` handles
  produce a compiler warning because dropping one does not cancel its work.
- Flattened lifecycle failures into `NanocodexError`, so callers match variants
  such as `TurnCancelled` directly, and exposed Responses failure
  classification through caller-provided Tower middleware with
  `NanocodexError::responses_error()`.

## [0.1.0](https://github.com/gakonst/nanocodex/releases/tag/v0.1.0) - 2026-07-20

### Highlights

- Initial library-first Nanocodex SDK, persistent Responses WebSocket client,
  Tower service stack, code-mode tools, MCP integration, embedded bindings,
  CLI, and observability support.
- Checksummed maxperf native installers and self-updates, dependency-ordered
  crates.io publishing, contributor-attributed GitHub release notes, and
  docs.rs archive validation.

### Bug Fixes

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

- Defer Windows test coverage
- Update repository identity
- Sync Codex Sol base instructions
- Add terminal hyperlink smoke test

### Other

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

- Cache guarded texlive verifier setup

### Refactor

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

- [observability] Add retained-session stress coverage
- Stabilize PTY readiness checks

<!-- generated by git-cliff -->
