# Development instructions

## Workflow

- Follow `PLAN.md` in order and build vertical CLI slices.
- Prefer `just run` and a real `just eval` over mocks or speculative layers.
- Inspect the JSONL stream, Harbor result, trajectory, and verifier output.
- Measure cold bootstrap separately from warm source-edit iteration.
- End-to-end Harbor trials are the milestone gates. Do not add tests merely for
  coverage or during cleanup; add a focused deterministic test only when the
  plan calls for one or a demonstrated regression justifies it.
- Prefer deleting obsolete paths and data flow over introducing abstractions.
  A cleanup should produce a material net reduction in production LOC.

## Codex reference implementation

- Do not invoke the OpenAI docs skill, fetch the Codex manual, use the OpenAI
  Docs MCP server, or browse the web for Codex/OpenAI behavior unless the user
  explicitly asks. Use the local Codex checkout and retained runtime/eval
  traces as the default sources in this repository. This repository rule
  overrides automatic skill-trigger matching, including when a request would
  otherwise match the `openai-docs` skill description.
- For agent architecture and implementation decisions, first inspect how the
  locally cloned OpenAI Codex repository handles the comparable concern. The
  expected checkout is `~/github/openai/codex`, with Rust code under
  `codex-rs/`.
- Use the checked-out source rather than memory or general descriptions. Refer
  to concrete Codex files and types when explaining the comparison.
- Treat Codex as a reference, not a requirement to copy its abstractions.
  Preserve this repository's `PLAN.md`, runtime boundary, milestone order, and
  deliberately narrower scope when the designs differ.
- Copy relevant invariants and operational behavior, not Codex compatibility
  surface. Expose only tool fields and lifecycle behavior implemented here.
- The upstream review checkpoint is
  `openai/codex@3ac476bed22a7b7322a710a6ca79a0dbe917d604`. When asked to keep
  pace with Codex, fetch its `origin/main` and review every commit after this
  checkpoint, prioritizing supported model changes, defaults, prompts, API
  semantics, tools, and lifecycle behavior. Present candidates as port,
  evaluate, defer, or out of scope before implementing unless the user asks to
  implement directly. Advance the checkpoint only after the full range is
  reviewed; retain material deferred work in `PLAN.md`, and cite the exact
  upstream commit for behavior that is adopted.

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
- Contractual events are emitted explicitly through the JSONL protocol.
  Tracing is for stderr diagnostics and must not become the protocol transport.
- Use typed serde messages for repeated protocol shapes. Compact `json!` values
  are appropriate for one-off static request or tool schemas when dedicated
  types would only duplicate the JSON structure.
- Never print secrets or `.env` contents.

## Rust practices

- Follow Alloy-style Rust: small typed components and explicit data flow.
- Construct required configuration into owning types near `main`; do not retain
  required values as `Option` or repeatedly validate them after construction.
- Put stateful asynchronous lifecycle operations on owning structs and `impl`
  blocks. Reserve free functions for stateless transformations and utilities.
- Let the model run own its client session, event writer, task context, timing,
  and statistics. Avoid threading mutable statistics or long context argument
  lists through the call graph.
- Keep repeated wire types at protocol boundaries and domain types internally.
  Avoid types for one-use static JSON whose only purpose is increasing ceremony.
- Prefer moving owned tool and protocol values over cloning them to satisfy an
  unnecessarily borrowed interface.
- Return errors with context; avoid `unwrap`, `expect`, and silent fallback in
  runtime paths.
- Keep subprocess output memory-bounded while it is produced; post-exit
  truncation is not a memory bound.
- Keep cancellation and process cleanup explicit. Timeout or cancellation must
  terminate the subprocess group and descendants, following the relevant Codex
  implementation where applicable.
- Keep `eyre` for top-level application error reporting; use focused typed
  errors where callers need to distinguish runtime failures.
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
- Skills are out of scope for this project. Do not add skill discovery, skill
  catalogs, `SKILL.md` injection, bundled skills, or plugin-provided skills.
- Model execution is the only runtime mode. Do not restore milestone positive
  controls, compatibility modes, or duplicate shell implementations.
- Do not add event buses, collector traits, shared mutable run state, or generic
  client/provider layers without a concrete current consumer.
- Preserve unrelated work. Never commit `.env`, caches, jobs, or build output.
