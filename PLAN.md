# Harbor-first OpenAI harness

## Objective

Build a thin Rust coding-agent harness that is deliberately optimized for the
best current OpenAI model and API surface, rather than for provider portability,
old models, or backwards compatibility.

The user-facing surface is a CLI that reads JSONL on stdin and streams every
observable event as JSONL on stdout. There is no TUI initially. The first real
milestone is running our harness on Terminal-Bench through Harbor. OpenAI
execution comes next. JJ-backed checkpoints and review provenance come only
after the basic agent has a measured baseline.

The previous implementation is preserved on
`backup/pre-lean-restart-20260715` at commit `06ccdc8`. This branch starts from
a new root so none of its journal, artifact-store, or checkpoint machinery is
implicitly part of the new design.

## Operating principles

1. Build vertical CLI paths before internal frameworks.
2. Let Harbor own eval orchestration, task isolation, verification, results,
   and ATIF trajectories.
3. Let OpenAI own model state, compaction, prompt caching, programmatic tool
   calling, and hosted orchestration wherever the current API can do so.
4. Keep the local harness responsible for only the irreducible boundary:
   OpenAI events, tools that act on the task environment, and a transparent
   JSONL stream.
5. Measure changes on real tasks before adding abstractions intended to help
   them.
6. Add JJ only when there are real model-produced diffs worth checkpointing.
7. Record API-visible reasoning summaries and causal events. Do not claim to
   capture hidden chain of thought that the API does not expose.

## Shape of the system

```text
Harbor CLI
  |
  | instruction + lifecycle
  v
thin Python external-agent adapter
  |                               |
  | bidirectional JSONL           | BaseEnvironment.exec/upload/download
  v                               v
local Rust process             Terminal-Bench task container
(`cargo run` in development)      |
  |                               v
  | OpenAI Responses API       Terminal-Bench verifier
  v
hosted OpenAI state/tools
```

The Rust process runs on the host during development. Harbor still creates the
task container, but changing our harness does not rebuild that image. When the
Rust process requests a local tool, the adapter executes it through Harbor's
`BaseEnvironment`, so shell commands and filesystem mutations happen inside the
task container rather than on the host.

This is Harbor's external-agent model, not an installed agent. For longer or
remote eval runs, `cargo run` can be replaced by a prebuilt host binary without
changing the JSONL protocol or the Harbor adapter.

## The fast development loop

The first engineering task is to make iteration cheap enough that we actually
use the eval loop continuously.

### Observed baseline on 2026-07-15

- `uv run harbor --version`: about 7.1 seconds.
- Calling an already-synced `.venv/bin/harbor --version`: about 0.7 seconds.
- A cached Terminal-Bench task with Harbor's no-op agent and verification
  disabled: Harbor reported 17 seconds; process wall time was 28.4 seconds.
- The registry's first task, `make-mips-interpreter`, is a bad verifier canary.
  Its cold run was stopped at 157.5 seconds after the verifier alone exceeded a
  minute.
- The implemented local Rust loop takes about 0.14 seconds warm.
- Before teardown tuning, the implemented external-agent Harbor probe took
  about 16.7 seconds warm and a full no-model `fix-git` canary took 26–35
  seconds depending on cache state.
- Profiling identified Docker Compose's ten-second stop grace period as dead
  time: Harbor's `sh -c "sleep infinity"` keepalive does not forward SIGTERM.
  A runtime-only Compose overlay removes that wait after verification and
  artifact capture. Two measured full canaries fell from 26.1 seconds to
  13.6–14.9 seconds; 10.5–11.2 seconds of the remainder is the task's real
  verifier. The verifier-free probe fell to 4.0 seconds.
- Inspection showed that nearly all verifier time was dependency bootstrap:
  `apt-get update`, installing curl, downloading uv, and resolving pytest on
  every fresh container; the two assertions themselves take about 0.1 seconds.
  The warm local eval therefore uses a derived image with those exact pinned
  dependencies baked once and a generated task copy whose `test_outputs.py`
  remains byte-identical. `just harbor-eval-canonical` retains the untouched
  downloaded task as the parity gate. Two warm local samples took 6.7 and 7.2
  seconds wall; verifier execution fell to 1.58–1.63 seconds, including about
  0.11 seconds for the assertions themselves.
- A command-level trace then found 18 Docker subprocesses consuming 4.5
  seconds. Harbor was issuing Linux bind-mount ownership repairs on macOS,
  probing the daemon twice, validating the original image even though the
  local overlay replaces it, and removing a Compose project before assigning
  it a fresh random name. A Darwin-only local Docker environment removes those
  inapplicable checks while retaining fresh-container isolation. Rebuilding the
  pinned 20KB task context for the native daemon architecture also removes
  amd64 emulation from the warm path. Five final samples took 2.84–3.07 seconds
  wall (2.91-second median); the median Harbor trial was 2.48 seconds: 0.99
  seconds for container setup, 0.05 for the harness, 0.99 for verifier
  upload/execution/CTRF, and 0.38 for cleanup.

These numbers imply three CLI speeds:

1. `just run`: pipe an example request into `cargo run --quiet`; no Harbor and
   no Docker. This is the per-edit loop and should be under two seconds warm.
2. `just harbor-probe`: run one pinned, locally cached Terminal-Bench task
   through the external adapter with verification disabled. This validates the
   real Harbor/container/tool boundary without paying a task verifier.
3. `just harbor-eval`: run the same task through a fresh Harbor trial with the
   canonical assertions and pre-baked verifier dependencies. Use
   `just harbor-eval-canonical` periodically as the untouched parity gate.

Harbor is installed once into the repository virtualenv and invoked directly;
the inner loop must not use `uv run`. Telemetry is disabled for local timing.
The exact task is pinned and addressed by its cached local path. Harbor runs
with `--no-force-build`, which is already its default, so its content-addressed
task image is reused.

We will measure several Terminal-Bench tasks once and select a canary with a
short environment startup and verifier. Cold dataset downloads and cold image
pulls are bootstrap costs and are reported separately from warm iteration time.

### Loop acceptance criteria

- Editing Rust code never invalidates or rebuilds a Terminal-Bench image.
- The adapter starts the harness with `cargo run --quiet` locally.
- Warm `just run` p50 is below two seconds.
- Warm `just harbor-probe` p50 is measured and initially targeted below 30
  seconds; optimize further only after breaking down its timings.
- The full verifier command is one explicit CLI invocation and leaves a normal
  Harbor result directory that can be opened with `harbor view`.
- Ctrl-C terminates the Rust child and lets Harbor clean up the environment.

## JSONL process contract

The JSONL stream is both the public CLI surface and the RPC channel between the
Rust process and Harbor.

Every envelope has:

```json
{"protocol_version":1,"request_id":"...","seq":1,"type":"...","payload":{}}
```

Initial stdin messages:

- `task.start`: instruction, task metadata, workspace, and requested runtime
  configuration.
- `tool.result`: result, exit status, duration, and truncation metadata for a
  prior tool request.
- `control.cancel`: cooperative cancellation.

Initial stdout messages:

- `run.started` and exactly one terminal `run.completed` or `run.failed`.
- `assistant.message` and streaming deltas where useful.
- `openai.event` containing the API event without lossy reinterpretation.
- `tool.call` for work that Harbor must execute in the task container.
- `metric` for timings and usage that are not already represented by an API
  event.

Rules:

- Stdout is JSONL only and flushes after every envelope.
- Human diagnostics and Cargo output go to stderr.
- Sequence numbers are monotonic within a request.
- The Harbor adapter persists the exact input and output streams.
- Small tool results stay inline initially. We do not add a content-addressed
  artifact store before real output sizes require one.
- The adapter converts the stream to ATIF for Harbor. ATIF is the eval/review
  interchange format; it is not duplicated by a second local journal schema.

## Phase 0: prove the CLI loop

Status: complete on 2026-07-15. The local stream, cached Harbor probe, real
`fix-git` verifier, complete ATIF trajectory, and timeout/incomplete-trajectory
recovery paths have all been exercised end to end.

Deliver only enough code to exercise the architecture:

- A Clap-based Rust executable with a `run` command.
- One example `task.start` JSONL request.
- A thin Python `BaseAgent` adapter that launches `cargo run --quiet -- run`.
- Bidirectional, line-buffered JSONL plumbing and cancellation.
- A no-model response that makes no task changes and terminates cleanly.
- Direct Harbor invocation from a pinned virtualenv.
- `just run`, `just harbor-probe`, and `just harbor-eval` recipes that read
  `.env` without printing secrets.

There is intentionally no OpenAI client, shell tool, JJ dependency, journal,
grader framework, or unit-test suite in this phase.

Gate: the no-model process completes a real Terminal-Bench trial, Harbor runs
the real verifier when requested, raw JSONL is preserved, and Harbor can render
a valid ATIF trajectory. A reward of zero is expected and is not a failure of
this milestone.

## Phase 1: wire the current OpenAI execution path

Target the current frontier coding model (`gpt-5.6-sol`, using its stable alias
only if evals show no difference) through the Responses API. This harness is
allowed to be model-specific.

### API and context

- Preserve the raw Responses event stream in JSONL and derive display/ATIF data
  from it.
- Prefer server-managed conversation state and persisted reasoning context over
  rebuilding full transcripts locally.
- Prefer server-managed compaction. Determine the threshold from current API
  guidance and actual usage rather than freezing the historical Codex heuristic
  into our protocol.
- Structure stable instructions and tool definitions as a cacheable prefix and
  keep per-task data late. Measure cache reads/writes and effective cost.
- Keep the base prompt lean. Add instructions only to correct an observed eval
  failure.
- If a local compaction fallback is ever required, place its summary in the
  model-trained position at the end of the reconstructed context.

### Tools

- Start with the smallest useful local tool surface: shell execution in the
  Harbor environment and a patch/file-edit operation only if shell is
  insufficient.
- Match familiar OpenAI/Codex tool names and result shapes where the current
  model is trained to use them. Avoid novel custom tools for cosmetic
  distinctions.
- Make programmatic tool calling/code mode the default candidate for bounded
  orchestration, including parallel calls. Retain standard direct tool calling
  as the baseline and compare both on the same tasks.
- A `tool.call` emitted by Rust is fulfilled by the Python adapter through
  `BaseEnvironment`; the Rust process never receives a host-shell capability.
- Run in YOLO mode because the task environment is already an eval container.
  There is no approval subsystem.

### Transport and hosted orchestration

- Spike Responses WebSocket mode against HTTP/SSE using the actual required
  features. Choose WebSocket when it supports the full path and measurably
  improves a multi-turn tool loop; otherwise keep the simpler transport.
- Prefer hosted Multi-agent orchestration for genuinely independent workstreams
  once the single-agent baseline works. Do not build a local subagent scheduler
  first.
- Programmatic tool calling and hosted Multi-agent are eval dimensions, not
  presumed universal wins. The direct single-agent path remains the control.

### Model settings

- `just run` and smoke evals accept a CLI `--effort` and default to the lowest
  useful effort for latency.
- Quality runs compare `xhigh` and `max`; the eventual default is chosen by
  reward, time, and cost rather than by the name of the setting.
- Model, effort, transport, compaction, caching, and orchestration settings are
  written into the Harbor/ATIF metadata for every run.

Gate: at least one Terminal-Bench task completes end to end with the OpenAI
agent, real verifier reward, raw API events, valid ATIF, and trustworthy token,
cache, cost, model, tool, and wall-clock measurements.

## Phase 2: tune with evals, not architecture

Harbor is the primary eval runner and result store. We add only data Harbor or
ATIF cannot already represent.

Measure at minimum:

- task reward and verifier details;
- total wall time and time to terminal event;
- API request latency, first-event latency, and response duration;
- per-tool queue, execution, and response latency;
- input, cached input, reasoning/output tokens, and actual cost;
- request, turn, tool, retry, compaction, and subagent counts;
- harness Git revision and complete runtime configuration.

Evaluation ladder:

1. The pinned Terminal-Bench canary for integration regressions.
2. A small fixed Terminal-Bench slice covering file edits, investigation,
   compilation, and long tool output.
3. The broader Terminal-Bench suite for meaningful comparisons.
4. Private taste/regression tasks only after the public baseline is stable.

Initial controlled comparisons:

- standard tool calls versus programmatic tool calling;
- single agent versus hosted Multi-agent on tasks with separable work;
- HTTP/SSE versus WebSocket;
- `xhigh` versus `max` effort;
- cache/compaction policies at long-context boundaries.

Promotion requires repeated runs on the same task set. Report reward, p50/p95
wall time, tool time, tokens, cache utilization, and cost together. Do not tune
to one lucky run.

## Phase 3: introduce JJ checkpoints and review provenance

JJ enters only after the OpenAI agent is producing useful diffs.

Its narrow role is a code-state timeline:

- Establish a baseline change before the task.
- Snapshot after each mutating tool call or coherent mutation batch.
- Associate each checkpoint with run ID, request/turn ID, tool call ID, and the
  exact JSONL sequence interval that produced it.
- Preserve the user prompt, API-visible reasoning summaries, assistant output,
  tool inputs/results, and grader feedback in the Harbor trace; link to that
  trace from JJ metadata.
- Use JJ change IDs for the human review timeline and commit IDs when an exact
  immutable tree is required.
- Ensure failed or cancelled mutating calls still leave reviewable state.

The first implementation should be a small `jj-lib` integration. It should not
introduce a WAL, two-operation sealing protocol, content-addressed artifact
graph, hunk CAS index, or exact replay engine. A reviewer needs to answer “what
observable agent activity led to this diff?” before we solve stronger audit
properties.

Gate: from any checkpointed diff, a CLI command can locate and display the
prompt plus the API-visible trace span that caused it, and a corrected follow-up
can create the next JJ change without destroying the original.

## Phase 4: graders, loops, and human review

After the baseline and checkpoint link work:

- Add verifier/grader subagents that inspect the task result and trace.
- Allow user-defined graders to inject taste and behavioral constraints into a
  retry, without turning every runtime event into a policy engine.
- Add bounded autoresearch loops that vary one prompt/runtime setting, run the
  fixed Harbor slice, and retain comparable results.
- Add hunk-oriented human review over JJ changes, showing the linked prompt and
  trace context and accepting corrections as new changes.
- Reuse an existing JJ UI only if it can expose our trace links cleanly; do not
  fork or port a TUI before the CLI review flow proves useful.

## Explicitly deferred

- TUI work.
- Provider/model abstraction and backwards compatibility.
- Local multi-agent scheduling when hosted orchestration suffices.
- Approval and sandbox policy machinery.
- Durable runtime replay, WAL recovery, and event-sourced state reconstruction.
- A separate journal database or content-addressed artifact system.
- Exact hunk-to-event indexes before JJ checkpoint-level links are insufficient.
- Large unit-test suites, mocks of Harbor, or abstract interfaces written ahead
  of a working vertical path.

## Testing posture

Development is CLI-first and end-to-end-first:

- Eyeball `just run` JSONL constantly.
- Exercise the cached Harbor probe after each vertical behavior change.
- Run the real Terminal-Bench verifier before calling a milestone complete.
- Inspect the produced `result.json`, raw JSONL, `trajectory.json`, verifier
  output, and `harbor view`, not only the process exit code.
- Add a focused unit test later when a deterministic component has nontrivial
  behavior, a regression has occurred, or the E2E loop cannot cheaply cover an
  edge case. Unit-test count and coverage percentage are not goals.

## Immediate work order

1. Pin Harbor and select/measure a fast Terminal-Bench canary.
2. Add the minimal Clap JSONL process and `just run`.
3. Add the external-agent adapter that launches `cargo run`.
4. Make the no-model Harbor probe and full-verifier commands reproducible.
5. Convert the raw stream to ATIF and verify it with Harbor's validator/viewer.
6. Record warm-loop timing and remove the largest avoidable cost.
7. Only then add the Responses API and the smallest container tool bridge.

## Reference surfaces

- [Harbor custom agents](https://github.com/harbor-framework/harbor/blob/1dfdfeba8d78e4f2d90aeee3065020c2d2b058f0/docs/content/docs/agents/index.mdx)
- [Harbor Terminal-Bench workflow](https://github.com/harbor-framework/harbor/blob/1dfdfeba8d78e4f2d90aeee3065020c2d2b058f0/docs/content/docs/tutorials/running-terminal-bench.mdx)
- [Harbor ATIF support](https://github.com/harbor-framework/harbor/blob/1dfdfeba8d78e4f2d90aeee3065020c2d2b058f0/docs/content/docs/agents/trajectory-format.mdx)
- [OpenAI latest-model guidance](https://developers.openai.com/api/docs/guides/latest-model)
- [OpenAI WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode)
- [OpenAI programmatic tool calling](https://developers.openai.com/api/docs/guides/tools-programmatic-tool-calling)
- [OpenAI Responses multi-agent](https://developers.openai.com/api/docs/guides/responses-multi-agent)
- [OpenAI compaction](https://developers.openai.com/api/docs/guides/compaction)
- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
