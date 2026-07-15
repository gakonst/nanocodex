# Development instructions

## Current objective

Follow `PLAN.md` in order. The active milestone is a fast CLI-driven Harbor
loop for one real Terminal-Bench task. Do not introduce OpenAI execution, JJ,
checkpointing, graders, a journal, or a TUI before that milestone passes.

## Development posture

- Work vertical slices from the CLI inward.
- Prefer a real `cargo run` and Harbor trial over mocks or speculative internal
  abstractions.
- Eyeball the complete JSONL stream and inspect Harbor's generated artifacts.
- Time the actual commands while optimizing the loop; separate cold bootstrap
  costs from warm iteration costs.
- Add unit tests later and selectively: for nontrivial deterministic logic,
  demonstrated regressions, or edge cases that the E2E path cannot cover
  cheaply. Do not create test scaffolding or coverage targets by default.
- Keep changes small enough that their effect can be observed in the next CLI
  run.

## Harbor boundary

- The Rust harness runs locally through `cargo run` during development.
- Terminal-Bench tasks and all mutating tools run in Harbor's task environment.
- The Python external-agent adapter must remain thin: process lifecycle, JSONL
  transport, `BaseEnvironment` tool dispatch, log capture, and ATIF conversion.
- Do not compile Rust in a task image, upload a Linux binary in the local loop,
  or invalidate the task image when harness source changes.
- Invoke the pinned `.venv/bin/harbor` directly in the warm loop rather than
  paying `uv run` startup on every trial.
- Use one exact cached task and `--no-force-build` for the probe. Use a fresh
  Harbor trial and the real verifier for the gate.

## JSONL invariants

- Stdout from the Rust process is valid JSONL only.
- Diagnostics, build output, and panic context go to stderr.
- Flush every event immediately.
- Preserve the exact input/output streams before deriving ATIF or summaries.
- Every event carries protocol version, request ID, monotonic sequence, type,
  and payload.
- Emit exactly one terminal event for every accepted request.
- Never print secrets or the contents of `.env`.

## Rust practices

Follow the style used in the Alloy ecosystem:

- Prefer small typed modules and explicit data flow over framework layers.
- Use strong domain types where values can be confused; keep wire types at the
  protocol boundary.
- Return structured errors with useful context. Avoid `unwrap`, `expect`, and
  silent fallback in runtime paths.
- Keep async cancellation and process cleanup explicit.
- Avoid unnecessary allocation and cloning in streaming paths, but optimize
  only after measurement.
- Keep public APIs documented and implementations readable; comments should
  explain invariants or surprising decisions.
- Run rustfmt and Clippy before a milestone handoff. Treat warnings in our code
  as errors once the crate exists.
- Prefer established crates already in the dependency graph. New dependencies
  need a concrete benefit to the current vertical slice.

## Scope guardrails

- Harbor result files and ATIF are the eval record. Do not add a parallel
  journal or artifact database.
- API-visible reasoning summaries may be recorded; never label them as hidden
  chain of thought.
- No approval system: tasks run in the designated YOLO eval container.
- No provider abstraction or backwards compatibility unless the plan changes.
- Preserve unrelated dirty work and never commit `.env`, local Harbor jobs,
  task caches, or build artifacts.
