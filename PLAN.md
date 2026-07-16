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

The governing runtime constraint is hosted-first. OpenAI owns model reasoning,
the Programmatic Tool Calling JavaScript runtime, root/subagent orchestration,
stored response state, prompt caching, and compaction. Rust owns the narrow
capabilities that must touch the Harbor task container: JSONL, local shell
execution, bounded process cleanup, and API-visible measurements. Do not grow a
local agent scheduler, transcript manager, compactor, or second eval record.

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

### OpenAI runtime contract

1. Target `gpt-5.6-sol` directly through the Responses API. Do not add a
   provider interface, alternate model path, HTTP/SSE fallback, or legacy wire
   compatibility.
2. Keep one persistent Responses WebSocket connection. Streaming is implicit;
   do not send the HTTP `stream` field. Warm stable request state with
   `generate: false`, continue incrementally with `previous_response_id`, and
   preserve every raw inbound and outbound API event.
3. Use the Codex-trained `exec_command` function shape for local shell work.
   In the default single-agent profile, expose it exclusively through hosted
   Programmatic Tool Calling with `allowed_callers: ["programmatic"]`; Rust
   executes typed `function_call` items and returns typed
   `function_call_output` items with the original PTC caller. OpenAI runs the
   generated JavaScript; Rust never does.
4. Treat one generated JavaScript program as a bounded mechanical phase. Use
   `Promise.all` for independent reads, sequence dependent work and mutations,
   reduce intermediate results in hosted JavaScript, retry transient work at
   most once, and return to the model only for semantic judgment. Preserve
   every `program`, `program_output`, `call_id`, and `caller` relationship.
5. A completed response is not a completed task until the root emits a final
   assistant message. A response containing only program or tool work
   continues from its response ID.
6. Default to hosted state with `store: true` and `previous_response_id` so a
   reconnect can rehydrate the response chain. Do not maintain or replay a
   parallel local transcript. A later explicit ZDR mode may use `store: false`
   and complete encrypted-item replay, but it must not complicate the default.
7. Enable server-side compaction through `context_management` on every
   generated response. Preserve opaque compaction items in API order; never
   interpret, reorder, or replace them with a local natural-language summary.
   Seed the quality-first profile near 350K tokens and evaluate a cost-sensitive
   profile just below GPT-5.6 Sol's 272K long-context pricing boundary.
8. Use explicit GPT-5.6 prompt caching. Put exact stable developer instructions
   and tools before dynamic task/environment content, place an explicit cache
   breakpoint at that boundary, and derive `prompt_cache_key` from the selected
   model, profile version, stable-prefix bytes, and tool-catalog bytes. Record
   `cached_tokens` and `cache_write_tokens`; do not churn the stable prefix.
9. Use `reasoning.context: "all_turns"` while task goals remain stable. Keep
   `reasoning.mode: "standard"` for the interactive tool loop and make effort a
   CLI/eval setting: low for the fast smoke loop, max only when the measured
   quality gain warrants its latency and cost.
10. Keep hosted Responses Multi-agent opt-in with `--multi-agent`; selecting it
    is appropriate when the user explicitly asks for delegation or a genuinely
    difficult task has independent workstreams. Do not spawn for routine,
    short, or sequential work. Start with three concurrent subagents. OpenAI
    owns spawning, messaging, waiting, interruption, contexts, scheduling, and
    result delivery.
11. Keep PTC and Multi-agent as separate request profiles. PTC is the default
    for predictable mechanical control flow. The current live API rejects
    `response.inject` for PTC-nested calls (`caller.type = "program"`) while a
    Multi-agent response is active, so the Multi-agent profile exposes the same
    `exec_command` function to direct callers and omits PTC. Do not hide this
    compatibility boundary behind retries or a local orchestration fallback.
12. In Multi-agent WebSocket turns, execute each direct local call and send its
    output with `response.inject` as soon as it is ready so the waiting agent
    can resume. In PTC turns, continue the stored response with the typed output
    and `previous_response_id`. Preserve caller and agent attribution plus
    injection acknowledgement in JSONL.
13. Multi-agent does not support `reasoning.summary`. Preserve exposed root and
    agent messages, encrypted content, and raw events honestly; never claim to
    have captured hidden chain of thought. If a later single-agent mutation
    phase requests an API-visible summary, label it as a summary.
14. Record model, mode, effort, response and agent IDs, cache activity, tokens,
    latency, tool execution, injections, retries, and compactions in JSONL and
    the Harbor-derived ATIF. Harbor remains the eval record.

Gate: at least one Terminal-Bench task completes with a real OpenAI-driven tool
loop, canonical reward, raw API events, and trustworthy usage/timing metadata.

Gate achieved twice on Terminal-Bench `fix-git`. The final regression run earned
reward `1.0` with 9 model calls, 8 PTC shell calls, 29.5 seconds inside Rust,
and 35 seconds of Harbor runtime. It used 24,186 input tokens (17,712 cached),
4,546 cache-write tokens, and 1,086 output tokens. The benchmark task and
verifier were not modified.

## Milestone 1.1: runtime cleanup

Status: complete. Reduce production
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
5. Keep a deliberately one-shot subset of Codex's `exec_command` function:
   `cmd`, `workdir`, `login`, `timeout_ms`, and `max_output_tokens`, with a
   structured output schema. Do not advertise PTY sessions, `write_stdin`,
   approval fields, or yield behavior until the runtime implements them.
6. Collapse redundant model-stream state and processing: consume completed
   output once, remove unread response state and unused error variants, move
   owned function calls into concurrent execution instead of cloning them, and
   remove `Result` layers from operations whose expected failures are already
   represented as tool outcomes.
7. Replace post-hoc command-output truncation with bounded collection while the
   subprocess runs. Preserve useful truncation metadata without retaining
   unbounded stdout or stderr, and adopt Codex's process-group cleanup pattern
   so timeout or cancellation also cleans up descendants.
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

## Milestone 1.2: hosted API compatibility matrix

Status: complete.

Before treating the hosted surface as the eval baseline, prove its combined
event matrix with one real vertical smoke rather than separate mocks:

1. Open ordinary WebSockets without a beta header and Multi-agent WebSockets
   with only `OpenAI-Beta: responses_multi_agent=v1`. Warm each exact stable
   prompt/tool profile with `generate: false`, then chain from that response ID.
2. Prove the default PTC profile with a hosted JavaScript program, one
   programmatic `exec_command`, caller-preserving continuation, and one final
   assistant message.
3. Prove the opt-in Multi-agent profile with one named subagent, one direct
   `exec_command`, an accepted live `response.inject`, agent attribution, and
   one `/root` final answer.
4. Retain the negative compatibility evidence that a PTC-nested output is
   rejected as “not ready for an output” under Multi-agent, rather than adding
   an arbitrary retry or claiming the combination works.
5. Use a deliberately low smoke-only threshold to force automatic compaction,
   then continue to one final root assistant message.
6. Inspect raw JSONL, cache/usage metrics, continuation or injection timing,
   compaction event ordering, and the final task result. Retain the Harbor
   trajectory; do not create a fixture or local journal until a demonstrated
   regression needs one.

Gate: both supported profiles complete without Python tool plumbing, the
unsupported composition is explicit, Rust emits exactly one task terminal, and
all non-API wall time is measured.

The live gate passed on both profiles. The PTC smoke used one hosted program,
one caller-linked `exec_command`, one stored-response continuation, and a final
message. The Multi-agent smoke spawned one named subagent, accepted one direct
tool-output injection, and returned one `/root` final answer. At a smoke-only
1,500-token threshold, the latter also emitted six generation-time compaction
items and completed; 27.2 of 27.9 seconds were model/API time while the local
shell used about 20 milliseconds. PTC-nested injection was tested separately
and reproducibly rejected, which is why it is not a supported profile.

## Milestone 2: eval-driven tuning

Status: in progress. The first eight-task low-effort PTC baseline is green on
the same `openai-coding-v7` stable prompt:

| task | reward | trial | Rust | generated turns | tool wall | rounds/tools | input/cache/output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `fix-git` | 1.0 | 38.06s | 33.67s | 32.95s | 0.52s | 8/7 | 31,737/12,827/1,510 |
| `openssl-selfsigned-cert` | 1.0 | 27.18s | 22.99s | 22.15s | 0.51s | 3/2 | 6,657/2,326/1,616 |
| `cancel-async-tasks` | 1.0 | 68.43s | 50.07s | 49.51s | 0.88s | 5/4 | 10,951/6,127/2,735 |
| `headless-terminal` | 1.0 | 84.74s | 67.65s | 66.89s | 2.27s | 5/4 | 13,946/8,314/3,897 |
| `regex-log` | 1.0 | 35.04s | 31.17s | 30.24s | 0.07s | 2/1 | 4,163/1,163/1,963 |
| `build-cython-ext` | 1.0 | 160.79s | 154.45s | 153.80s | 67.27s | 21/20 | 240,833/43,306/4,797 |
| `fix-code-vulnerability` | 1.0 | 34.05s | 29.90s | 29.23s | 1.50s | 6/5 | 33,896/5,815/1,324 |
| `git-multibranch` | 1.0 | 65.40s | 54.22s | 53.51s | 4.57s | 6/5 | 23,667/10,501/3,209 |

Generated-turn time includes local tool wait; tool wall is a measured subset.
WebSocket connection and warmup added 0.56--0.93 seconds per task, and Rust
overhead outside connection, warmup, and generated turns was only a few
milliseconds. Warm environment and agent setup took 1.75--2.34 seconds. The
async task's verifier took 14.67 seconds because its canonical assertions
exercise real sleeps and SIGINT cleanup. The headless-terminal verifier
similarly took 13.94 seconds for interactive waits. Neither verifier installed
anything during the trial. The first headless-terminal trial spent 13.06
seconds preparing new task and verifier image layers; the warm rerun reduced
that environment phase to 1.26 seconds.

The first Ubuntu-based regex trial found that the cached verifier image assumed
a pre-existing, non-PEP-668 Python installation. The build stopped before the
agent and used no model tokens. The verifier layer now installs a minimal
Python runtime and explicitly owns packages in its disposable image; after the
13.63-second cold build, warm environment startup was 1.50 seconds.

The Cython task spent 37.95 seconds on its first cold task/verifier image build
and 1.54 seconds on warm environment startup. Two green attempts took 15/14
and 21/20 model/tool rounds. The warm run spent 67.27 of 153.80 generated-turn
seconds compiling, installing, and testing. The remaining path variance was
API reasoning over successive compiler and runtime failures, so no extra local
orchestration layer or task-specific prompt was added.

The vulnerability task's cold environment build took 50.52 seconds; its warm
environment startup took 1.56 seconds, agent setup 0.47 seconds, and verifier
0.86 seconds. The generic fast verifier preserves this task's two canonical
phases: all 367 repository tests and all 6 hidden assertions ran and passed,
with separate CTRF records. Harbor's `--task` override now backs `just
eval-task terminal-bench/<name>`, so a one-task warm iteration keeps the shared
agent, environment, and verifier configuration without editing the suite YAML.

All three multibranch deployment attempts passed real password-authenticated
SSH pushes, post-receive deployment, and HTTPS checks. The first attempt spent
51.09 seconds building its task/verifier image and 10.87 seconds verifying.
Caching the canonical test's system dependencies and package indexes reduced
verification to 7.38 seconds; warm environment startup was 2.02 seconds and
agent setup 0.49 seconds. Model-path variance was 54--71 seconds across the
three green runs, so no prompt or tool change was inferred from a faster single
trajectory.

The first async attempt also exposed a verifier-adapter bug: its canonical
assertions require a sibling `test.py`, while the fast verifier had staged only
the assertion module. The generic adapter now stages canonical support files
without modifying them. Once that infrastructure failure was removed, the
model failed the queued-SIGINT cleanup edge case. A narrow prompt correction
requiring verification at the requested external lifecycle boundary produced a
green attempt that tested queued cancellation and a real subprocess signal.
Earlier tasks were rerun and stayed green after the prompt change.

Use Harbor as the runner and result store. First rerun `fix-git` and
`openssl-selfsigned-cert` independently against the hosted-runtime baseline.
Then add one Terminal-Bench task at a time in `evals/*.yaml`, ordered from small
repository investigation/editing through compilation, debugging, and long tool
output. Never modify a benchmark task or verifier to make the harness pass.

For every new task:

1. Run one attempt and inspect the JSONL, ATIF trajectory, verifier output, and
   task-container diff before changing the harness.
2. Separate cold artifact/image work from the warm source-edit loop. Break wall
   time into local artifact build, Harbor setup/upload, task-container startup,
   WebSocket connection/warmup, model generation, local tool execution,
   injection/continuation, verifier, and teardown.
3. Attribute model time further with time-to-first-event/output, per-response
   duration, per-agent activity, tool wait, token/cache usage, and compaction.
4. Optimize only an observed bottleneck. The intended steady state is dominated
   by OpenAI API time; local compilation, upload, execution, and verifier
   overhead should remain small and measured.
5. Prefer deletion, prompt/tool-contract correction, or one narrow typed path
   over a framework. Do not add speculative abstractions, mock-heavy suites, or
   benchmark-specific Rust cheats.
6. Commit each proven vertical improvement with its eval evidence before moving
   to the next task. Re-run earlier tasks after changes to model/tool behavior.

Report reward alongside wall time, model time, tool time, Harbor overhead,
tokens, cache utilization, compactions, and cost when the API reports it. Once
one attempt works, use repeated attempts to estimate variance and p50/p95 rather
than drawing tuning conclusions from one lucky trajectory. Add private taste or
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
