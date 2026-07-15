# Harbor-first OpenAI harness

## Goal

Build a thin Rust coding harness for the current best OpenAI model and API,
without a TUI, provider abstraction, approval system, or backwards compatibility.
The public process surface is JSONL on stdin/stdout. Harbor owns benchmark task
isolation, verification, result storage, and ATIF.

## Architecture

```text
development                       evaluation

task.start                        Harbor job YAML
    |                                  |
cargo run                         task container
    |                                  |
JSONL stdout                      upload Rust binary
                                       |
                                  run in /app
                                   |       |
                                OpenAI   tools
                                       |
                                    verifier
```

The Python `BaseInstalledAgent` integration is only a lifecycle adapter:
upload one static executable, run it headlessly, retain JSONL, and derive ATIF.
Rust performs API calls and tools directly inside the task container.

Local artifacts are built by a native-architecture Linux BuildKit container.
Cargo `dev` is the default; `HARNESS_BUILD_PROFILE=profiling` selects an
optimized profile with full symbols. Hosted jobs will eventually fetch a
versioned, digest-verified artifact instead of requiring the source tree.

## JSONL contract

Every event has this envelope:

```json
{"protocol_version":1,"request_id":"...","seq":1,"type":"...","payload":{}}
```

Initial input is `task.start`. Output includes `run.started`, model events,
`tool.call`, `tool.result`, assistant messages, metrics, and exactly one
`run.completed` or `run.failed`. Stdout is flushed JSONL; diagnostics are stderr.
Raw streams are authoritative and ATIF is derived from them.

## Milestone 0: installed-agent eval baseline

Status: complete.

- Clap `run` command and native `just run`.
- Cached native Linux artifact build without rebuilding task images.
- Harbor-native YAML selecting Terminal-Bench `fix-git`.
- Thin InstalledAgent upload/run adapter with no tool bridge.
- Rust shell call and canonical assertions producing reward `1`.
- Raw input/events/stderr plus valid ATIF retained per trial.
- Content-addressed native eval image with verifier dependencies baked once.

Measured on the development machine:

- native `just run`: about 0.27 seconds warm;
- real source-edit artifact rebuilds: about 2 seconds steady state;
- Harbor environment startup: about 1.2 seconds warm;
- Harbor agent upload/setup: about 0.4 seconds;
- Rust positive-control execution: about 0.1 seconds;
- unchanged canonical assertions: about 0.9 seconds;
- full Harbor trial: about 3.6 seconds warm;
- full `just eval`, including the cached agent build: about 6.7 seconds warm.

The first run after task, platform, or eval-image changes also builds the
content-addressed native image. Keep that cold setup cost separate from warm
source-edit measurements.

## Milestone 1: OpenAI execution

Status: first real model/tool vertical slice complete.

1. Call the Responses API WebSocket endpoint from Rust and preserve every raw
   inbound and outbound API event.
2. Target `gpt-5.6-sol` directly; do not add a provider interface.
3. Expose shell exclusively through hosted Programmatic Tool Calling. Do not
   provide a direct function-call fallback or run generated JavaScript locally.
4. Preserve program caller linkage, execute independent nested calls
   concurrently, and return typed structured outputs to the hosted runtime.
5. Prefer server-managed conversation state, compaction, prompt caching, and
   hosted multi-agent orchestration where the current API supports them.
6. Keep stable instructions/tools in the cacheable prefix and task-specific
   content late. Record model, effort, cache, tokens, cost, latency, tools,
   retries, and compactions in JSONL/ATIF.

Gate: at least one Terminal-Bench task completes with a real OpenAI-driven tool
loop, canonical reward, raw API events, and trustworthy usage/timing metadata.

Gate achieved twice on Terminal-Bench `fix-git`. The final regression run earned
reward `1.0` with 9 model calls, 8 PTC shell calls, 29.5 seconds inside Rust,
and 35 seconds of Harbor runtime. It used 24,186 input tokens (17,712 cached),
4,546 cache-write tokens, and 1,086 output tokens. The benchmark task and
verifier were not modified.

## Milestone 1.1: runtime cleanup

Status: planned. Complete this before eval-driven tuning. Reduce production
surface area while preserving the working OpenAI/Harbor vertical slice; avoid
new framework layers whose main effect is moving code around.

1. Delete the `phase0` and `fix_git` modes, their CLI/config dispatch, and the
   synchronous shell path used only by the positive control. Model execution
   becomes the only runtime path.
2. Construct a fully configured model client in `main`. Required API and model
   configuration is validated at construction and stored directly on the
   client, rather than represented as `Option` and checked again during the
   run. Keep `eyre` for top-level error reporting.
3. Give the model run an owning struct and `impl` block. It owns the client
   session, event writer, task context, timing, and run statistics; helpers use
   that state instead of threading long argument lists and mutable statistics
   references through free functions.
4. Keep contractual JSONL events separate from diagnostic tracing. Use typed
   protocol events at the repeated wire boundary, but use compact `json!`
   values for one-off static tool schemas where dedicated serde types only add
   lines. Do not add event buses, channels, collector traits, or shared mutable
   statistics.
5. Expose only the tool fields the harness implements. Remove ignored or
   compatibility-only schema fields, including `yield_time_ms` and `tty`, rather
   than modeling Codex features that this runtime does not support.
6. Collapse redundant model-stream state and processing: consume completed
   output once, remove unread response state and unused error variants, move
   owned function calls into concurrent execution instead of cloning them, and
   remove `Result` layers from operations whose expected failures are already
   represented as tool outcomes.
7. Replace post-hoc command-output truncation with bounded collection while the
   subprocess runs. Preserve useful truncation metadata without retaining
   unbounded stdout or stderr, and adopt Codex's process-group/parent-death
   cleanup pattern so timeout or cancellation also cleans up descendants.
8. Consolidate repeated defensive bookkeeping where the runtime already has a
   hard invariant: sample terminal duration once, avoid silent saturating
   counters, and eliminate validation repeated by constructed types.

Validation for this cleanup is `cargo fmt`, Clippy with warnings denied, a real
native `just run`, and a real `just eval`. Inspect the JSONL stream, Harbor
result, trajectory, verifier output, long-output truncation behavior, and
timeout cleanup. Do not add unit tests in this milestone.

Gate: the model-only path retains the canonical reward and exactly one terminal
event per accepted request, long command output remains memory-bounded,
timed-out commands leave no descendant processes, and the cleanup produces a
material net reduction in Rust LOC.

## Milestone 2: eval-driven tuning

Use Harbor as the runner and result store. Establish a fixed Terminal-Bench
slice covering investigation, editing, compilation, and long tool output, then
compare changes with repeated attempts. Report reward alongside p50/p95 wall
time, tool time, tokens, cache utilization, and cost. Add private taste or
regression tasks only after the public baseline is stable.

## Milestone 3: review provenance

Only after useful model-produced diffs exist, add a narrow `jj-lib` timeline:
baseline the workspace, checkpoint coherent mutation batches, and link each JJ
change to the prompt and exact JSONL sequence interval that caused it. Do not
add a second event journal, WAL, artifact graph, or hunk index first.

## Milestone 4: graders and review loops

After checkpoint links work, add verifier/grader subagents, bounded autoresearch
loops, user-defined taste constraints, and hunk-oriented human review. Reuse an
existing UI only if it cleanly exposes trace links; keep the CLI as the control.

## Deferred

- TUI work.
- Provider/model abstraction and backwards compatibility.
- Local multi-agent scheduling where hosted orchestration suffices.
- Approval and policy machinery.
- Durable replay, a parallel journal, or content-addressed artifact storage.
- Large mock-heavy unit-test suites ahead of working end-to-end behavior.
- Unit tests for the current runtime cleanup; rely on the real run/eval gates
  until a demonstrated regression justifies a focused deterministic test.
- Improving the environment-secret-name heuristic.
- Preserving byte-exact inbound WebSocket frames instead of parsed and
  reserialized API events.
- Removing duplicate derived assistant/reasoning delta events or otherwise
  reducing event volume; first establish which representation the ATIF adapter
  should consume.
