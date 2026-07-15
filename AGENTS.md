# Development instructions

## Workflow

- Follow `PLAN.md` in order and build vertical CLI slices.
- Prefer `just run` and a real `just eval` over mocks or speculative layers.
- Inspect the JSONL stream, Harbor result, trajectory, and verifier output.
- Measure cold bootstrap separately from warm source-edit iteration.
- Add focused unit tests only for nontrivial deterministic behavior or a
  demonstrated regression; end-to-end Harbor trials are the milestone gates.

## Codex reference implementation

- For agent architecture and implementation decisions, first inspect how the
  locally cloned OpenAI Codex repository handles the comparable concern. The
  expected checkout is `~/github/openai/codex`, with Rust code under
  `codex-rs/`.
- Use the checked-out source rather than memory or general descriptions. Refer
  to concrete Codex files and types when explaining the comparison.
- Treat Codex as a reference, not a requirement to copy its abstractions.
  Preserve this repository's `PLAN.md`, runtime boundary, milestone order, and
  deliberately narrower scope when the designs differ.

## Runtime boundary

- `just run` is native `cargo run`; it must not require Harbor or Docker.
- `just eval` builds a static Linux artifact for the Docker daemon's native
  architecture. Do not force amd64 on Apple Silicon.
- Harbor owns task containers and verifiers. The Rust executable is uploaded
  as an InstalledAgent and runs inside `/app`.
- Python may upload/run the process and derive ATIF. Model decisions, API calls,
  tools, and mutations belong in Rust; never add a per-tool Python bridge.
- Do not modify benchmark tasks or compile Rust inside their images.
- Eval selection belongs in `evals/*.yaml`, not the Justfile.
- Local artifacts default to Cargo `dev`; honor `HARNESS_BUILD_PROFILE` and use
  `profiling` for optimized builds with debug symbols.

## JSONL contract

- Stdout is flushed JSONL only; diagnostics go to stderr.
- Every event has protocol version, request ID, monotonic sequence, type, and
  object payload.
- Emit exactly one terminal event for each accepted request.
- Preserve exact input/output streams before deriving ATIF.
- Never print secrets or `.env` contents.

## Rust practices

- Follow Alloy-style Rust: small typed components and explicit data flow.
- Keep wire types at protocol boundaries and use domain types internally.
- Return errors with context; avoid `unwrap`, `expect`, and silent fallback in
  runtime paths.
- Keep cancellation and process cleanup explicit.
- Run rustfmt and Clippy with warnings denied before handoff.
- Add dependencies only for a concrete need in the current vertical slice.

## Scope

- Harbor results and ATIF are the eval record; do not create another journal or
  artifact database.
- Record only API-visible reasoning summaries, never purported hidden chain of
  thought.
- Tasks run YOLO inside their eval container; there is no approval subsystem.
- Do not add provider portability, backwards compatibility, a TUI, JJ, graders,
  or local subagent orchestration before their planned milestone.
- Preserve unrelated work. Never commit `.env`, caches, jobs, or build output.
