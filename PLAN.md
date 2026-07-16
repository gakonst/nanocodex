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
source-edit measurements. `just prepare-evals` performs this construction for
the configured suite with Harbor's no-op install-only path, while `just
prepare-task terminal-bench/<name>` prepares one newly added task. Neither path
builds or runs the harness, calls a model, or invokes a verifier; their Harbor
records live outside the scored jobs directory.

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

Status: in progress. All twenty active public tasks have green low-effort PTC
samples with the current `openai-coding-v12` prompt. The table records
representative warm samples; the registry-resolved 20-task checkpoint
described below is the authoritative correctness gate:

| task | reward | trial | Rust | generated turns | tool wall | rounds/tools | input/cache/output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `fix-git` | 1.0 | 37.13s | 33.22s | 32.69s | 0.29s | 7/6 | 25,196/8,262/1,666 |
| `openssl-selfsigned-cert` | 1.0 | 32.78s | 29.24s | 28.71s | 0.56s | 3/2 | 7,666/4,368/1,794 |
| `cancel-async-tasks` | 1.0 | 56.91s | 39.67s | 39.21s | 0.54s | 4/3 | 8,042/4,368/1,733 |
| `headless-terminal` | 1.0 | 107.93s | 91.30s | 90.73s | 21.92s | 7/6 | 20,692/10,547/3,581 |
| `regex-log` | 1.0 | 39.14s | 34.89s | 34.09s | 0.03s | 2/1 | 4,530/2,991/1,955 |
| `build-cython-ext` | 1.0 | 183.96s | 178.03s | 177.59s | 79.77s | 12/11 | 158,073/34,169/4,328 |
| `fix-code-vulnerability` | 1.0 | 37.90s | 33.89s | 33.23s | 1.71s | 6/5 | 44,858/16,691/1,576 |
| `git-multibranch` | 1.0 | 223.56s | 95.09s | 94.56s | 1.55s | 10/9 | 47,696/18,103/5,125 |
| `git-leak-recovery` | 1.0 | 76.33s | 72.65s | 72.02s | 0.30s | 7/6 | 22,794/12,948/3,876 |
| `db-wal-recovery` | 1.0 | 33.80s | 30.47s | 29.80s | 0.23s | 6/5 | 15,793/9,170/1,596 |
| `sqlite-db-truncate` | 1.0 | 45.28s | 41.61s | 40.97s | 0.23s | 6/5 | 16,576/10,547/2,420 |
| `nginx-request-logging` | 1.0 | 44.91s | 39.10s | 38.47s | 5.90s | 4/3 | 11,482/6,769/2,090 |
| `polyglot-c-py` | 1.0 | 51.97s | 47.97s | 47.38s | 0.16s | 3/2 | 8,163/5,392/2,687 |
| `polyglot-rust-c` | 1.0 | 54.61s | 51.00s | 50.39s | 0.56s | 3/2 | 7,059/4,368/2,994 |
| `large-scale-text-editing` | 1.0 | 88.43s | 56.98s | 54.18s | 18.45s | 5/4 | 13,618/7,122/2,129 |
| `log-summary-date-ranges` | 1.0 | 30.50s | 26.97s | 26.37s | 0.44s | 5/4 | 18,965/10,194/1,672 |
| `cobol-modernization` | 1.0 | 80.94s | 77.05s | 76.31s | 0.65s | 7/6 | 37,877/14,996/4,046 |
| `sqlite-with-gcov` | 1.0 | 57.17s | 53.43s | 52.44s | 22.36s | 6/5 | 33,122/17,715/1,425 |
| `llm-inference-batching-scheduler` | 1.0 | 150.47s | 146.25s | 145.58s | 67.66s | 7/6 | 51,936/18,068/4,588 |
| `kv-store-grpc` | 1.0 | 35.13s | 28.88s | 28.21s | 5.03s | 4/3 | 10,669/6,769/1,596 |

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
with separate CTRF records. `just eval-task terminal-bench/<name>` filters the
pinned dataset, so a one-task warm iteration keeps the shared agent,
environment, verifier, and curated task revision without editing the suite
YAML.

Multibranch deployment attempts passed real password-authenticated SSH pushes,
post-receive deployment, and HTTPS checks. The first attempt spent
51.09 seconds building its task/verifier image and 10.87 seconds verifying.
Caching the canonical test's system dependencies and package indexes reduced
verification to 7.38 seconds; warm environment startup was 2.02 seconds and
agent setup 0.49 seconds. Model-path variance was 54--71 seconds across the
three green runs, so no prompt or tool change was inferred from a faster single
trajectory.

The eval config now pins the Terminal-Bench-2 dataset digest. Single-task runs
filter that dataset instead of using Harbor's standalone `--task` resolution;
the latter had followed each package's independent moving `latest` and caused
both task drift and extra image variants. The affected green baselines were
rerun on dataset-curated revisions. Their warm environment phases were
1.25--1.56 seconds, confirming that Rust and adapter edits do not rebuild task
or verifier images.

`filter-js-from-html` is a retained but excluded red experiment. Its Selenium
test initially treated failure to discover the installed Chromium driver on
native arm64 as if no alert had fired. The verifier now exports the installed driver
path when present; real Chromium launches in every counted run. This changed
the canonical verifier from a misleading 7-second pass over the security test
to roughly 197--200 seconds of real browser work across 28 alert-detection
batches.

After the verifier was trustworthy, the sanitizer remained red across effort
and orchestration settings:

| profile | reward | trial | Rust | generated turns | tool wall | rounds/tools | input/cache/output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| low PTC | 0.0 | 299.76s | 95.00s | 94.40s | 1.02s | 6/5 | 22,687/9,782/5,022 |
| medium PTC | 0.0 | 363.88s | 158.74s | 157.98s | 1.99s | 7/6 | 40,234/15,152/9,571 |
| medium hosted Multi-agent | 0.0 | 374.30s | 172.34s | 171.75s | 0.86s | 6/6 | 39,557/26,502/7,669 |
| high PTC | 0.0 | 559.84s | 358.48s | 357.90s | 2.28s | 9/8 | 96,490/26,916/23,950 |

The generic `openai-coding-v9` prompt now requires representative preservation
checks for destructive transforms and mature parsers for security-sensitive
grammars. It improved self-verification and parser choice but did not clear the
same malformed browser-corpus batch, so no vector-specific prompt hint was
added. The hosted run spawned no subagents (`hosted_multi_agent_calls=0`,
`agent_messages=0`) and was slower than PTC; enabling orchestration without
actual delegation is not an improvement. Raising effort to high more than
doubled medium agent time and usage without reward. Low-effort v9 regression
runs kept `fix-git` and `openssl-selfsigned-cert` green at 51.44 and 42.03
seconds of Harbor trial time, respectively.

The active Milestone 2 ladder is limited to text-terminal and repository tasks
that exercise the harness's current shell/code surface. Browser automation,
computer-use, image/video perception, and other modality-dependent tasks are
deferred until those tool surfaces are intentional milestones; they are not
counted as failures of the shell-only loop.

`vulnerable-secret` is retained as an excluded cyber-safety experiment. One
low-effort v11 attempt completed normally in 32.30 seconds and passed all three
canonical assertions after inspecting and decoding the stripped AArch64
binary; one-time preparation took 33.19 seconds, while Rust spent 27.63 of
28.45 seconds in API turns and 0.25 seconds in tools. During the next
full-suite checkpoint, the same pinned task received a typed Responses API
`cyber_policy` error after four tool phases; Harbor classified it as
`AgentSafetyRefusalError`. The request was not rephrased or retried. Because a
valid scored run is not reliably available without broader cyber
authorization, the task and its earlier green sample are not counted in the
active ladder.

That initial 17-task checkpoint finished in 5 minutes 12 seconds with 15 green
rewards, the cyber-policy exclusion, and an independent WAL-recovery miss to
diagnose. Concurrent execution compressed 868.23 aggregate model/API seconds
and 121.20 aggregate tool seconds into 312.46 seconds of suite wall time. The
run used 480,519 input, 141,344 cached-input, and 42,275 output tokens across
94 model calls and 77 tool calls.

`extract-elf` is also retained as an excluded red experiment because its public
instruction and reference disagree on address semantics. The example output
uses a `0x400000`-based address, while the supplied binary is PIE and the
reference scores raw virtual addresses from only `.text`, `.data`, and
`.rodata`. Two low-effort attempts and one medium-effort attempt consistently
implemented the defensible load-segment view with PIE rebasing and therefore
had zero key overlap with the reference. Medium effort raised model/API time
to 122.05 seconds without changing the interpretation. A generic v11 prompt
experiment about examples and unseen inputs also had no effect, so it was
reverted instead of retaining ineffective benchmark-shaped prompt weight.

The dataset-curated `git-leak-recovery` attempt passed all five canonical
assertions: it recovered the secret, preserved good history, removed reachable
and unreachable secret objects, and retained the expected repository contents.
Its warm trial spent 1.41 seconds on environment startup, 0.49 seconds on agent
setup, 27.24 seconds on agent execution, and 0.84 seconds in the verifier.
Within the 27.12-second Rust run, 26.02 seconds was model/API time, 0.38 seconds
was the WebSocket warmup, and 0.57 seconds was local tool work.

The first `db-wal-recovery` attempt opened SQLite before preserving the input
WAL. SQLite consumed the unreadable working WAL, and the model then fabricated
plausible replacement values after validating only record count and IDs. The
v10 prompt adds Codex's generic prohibition on guessing plus a narrow forensic
invariant: copy original inputs before tools that may consume them and validate
requested values, not only output shape. On the retry, the second tool call
copied both database files before the first normal SQLite open. The agent
restored that copy, decoded the WAL, recovered the changed values, and passed
all seven canonical assertions. The one-time image preparation took 49.33
seconds outside the scored job; the warm trial used 1.50 seconds for environment
startup, 0.51 seconds for agent setup, 62.90 seconds for agent execution, and
0.54 seconds for verification. Of the 62.76-second Rust run, 62.08 seconds was
model/API time and 0.23 seconds was local tool work.

Low-effort v10 regressions also kept `fix-git` and
`openssl-selfsigned-cert` green. Their warm Harbor trials took 54.63 and 42.95
seconds; Rust spent 47.54 of 48.35 seconds and 37.74 of 38.57 seconds in
model/API calls, respectively. Tool work remained below 0.53 seconds in both,
so the trials stayed API-bound.

The first full-suite checkpoint re-exposed the WAL preservation failure under
v11. Its initial compound probe exited when the optional `file` utility was
absent; the next phase queried SQLite before making a copy, which consumed the
WAL. The model recovered all IDs but substituted the base values for two
updates, failing two of seven assertions. The v12 prompt makes preservation a
first-tool-phase invariant before any file-type, database, archive, or
application inspection, even if a preliminary probe fails.

On the v12 retry, the first shell phase copied the complete workspace to both
an immutable original and a disposable work tree before inspecting anything.
All subsequent SQLite operations used copies, and all seven assertions passed.
The warm trial took 43.20 seconds end to end; Rust spent 38.54 of 39.24 seconds
in API turns and 0.22 seconds in tools. Low-effort v12 regressions also kept
`fix-git` and OpenSSL green in 32.68 and 34.98 seconds, respectively.

`sqlite-db-truncate` then passed on its first v10 attempt, independently
exercising the recovery invariant. The first shell command copied the database
before inspecting it; later calls parsed SQLite record varints and serial types
from that copy and recovered all ten rows without guessing. The cold image
preparation took 24.45 seconds outside the scored job. The warm trial used 1.22
seconds for environment startup, 0.60 seconds for agent setup, 34.89 seconds
for agent execution, and 0.79 seconds for verification. The Rust run spent
34.15 of 34.78 seconds in model/API calls and 0.33 seconds in local tools.

`nginx-request-logging` passed all eight canonical checks on its first
low-effort attempt, adding a real service lifecycle and HTTP/logging boundary.
Its cold image preparation took 24.66 seconds outside scoring. The warm trial
used 1.29 seconds for environment startup, 0.50 seconds for agent setup, 44.40
seconds for agent execution, and 2.76 seconds for verification. Task-required
tool work took 5.93 seconds: 3.56 seconds installed/configured/started Nginx and
2.35 seconds stress-tested rate limiting. Rust still spent 43.51 of 44.30
seconds inside API turns, which include the hosted program waiting on those
nested local calls.

The first `polyglot-c-py` attempt correctly implemented and tested arbitrary-
precision Fibonacci in both Python and C, but left its generated `cmain` and
`__pycache__` beside a deliverable explicitly required to be a single file.
The v11 prompt adds a generic final-state check that removes temporary test and
build artifacts unless they are requested outputs. The retry compiled and
tested both runtimes in each tool phase, removed the binary, cache, and
diagnostics, and passed the canonical single-file assertion. Cold preparation
took 33.83 seconds outside scoring. The warm trial used 1.79 seconds for
environment startup, 0.45 seconds for agent setup, 47.91 seconds for agent
execution, and 0.58 seconds for verification. Rust spent 46.80 of 47.77
seconds in model/API calls and 0.26 seconds in local tools.

Low-effort v11 regressions kept both anchors green. `fix-git` took 45.08
seconds end to end, with 39.81 of 40.71 Rust seconds in model/API calls and
0.35 seconds in tools. OpenSSL took 41.87 seconds end to end, with 37.45 of
37.98 Rust seconds in model/API calls and 0.35 seconds in tools. The cleanup
invariant did not remove requested outputs or distort either workflow.

The hard `polyglot-rust-c` task then passed on its first low-effort v11
attempt. A single hosted program wrote the Rust/C++ lexical polyglot, compiled
both forms, compared runtime outputs through Fibonacci(100), and removed both
generated binaries before returning to the model. The canonical verifier saw
only `main.rs` and passed. Cold preparation took 42.37 seconds outside scoring.
The warm trial used 1.23 seconds for environment startup, 0.50 seconds for
agent setup, 60.94 seconds for agent execution, and 0.84 seconds for
verification. Rust spent 60.21 of 60.82 seconds in model/API calls and 0.55
seconds in the sole local tool phase.

`large-scale-text-editing` produced valid Vim macros and transformed the
million-row input byte-for-byte on its first model attempt, but exposed a
verifier-adapter integrity bug: the old direct-pytest shortcut skipped the
canonical launcher's removal of agent-touched CSVs and trusted input
regeneration, then replayed the macros against already transformed data. The
generic adapter now executes each untouched canonical `test.sh`; it clears old
reward/CTRF output and skips only exact, allowlisted dependency-install shapes
already present in the cached verifier layer. Task-specific setup, multiple
pytest phases, and reward calculation therefore remain benchmark-owned.

The corrected large-file run passed all five assertions. Its one-time image
preparation took 23.11 seconds outside scoring. The warm trial used 1.64
seconds for environment startup, 0.53 seconds for agent setup, 93.47 seconds
for agent execution, and 23.43 seconds for verification. The verifier spent
21.64 seconds regenerating a fresh 51 MB input and replaying the submitted
macros. Rust spent 92.71 of 93.35 seconds in API turns; those turns include
36.53 seconds waiting for the three nested local tool phases.

Targeted adapter regressions then stayed green: the ordinary `fix-git` uvx
launcher verified in 0.87 seconds, the vulnerability task preserved both its
367-test repository phase and six hidden assertions in 0.86 seconds, and the
async launcher's canonical support-file copy plus real cancellation checks
completed in 14.62 seconds. Their full warm trials took 41.56, 43.87, and
67.17 seconds respectively.

`log-summary-date-ranges` passed on its first low-effort v11 attempt. The agent
used two local phases to inspect the log corpus, generate all 15 requested CSV
rows, and independently recompute every count. One-time native image
preparation took 21.56 seconds outside scoring. The warm trial used 1.82
seconds for environment startup, 0.86 seconds for agent setup, 23.05 seconds
for agent execution, and 1.32 seconds for verification. Rust spent 22.12 of
22.86 seconds in API turns and 0.32 seconds in local tools.

The first v12 full-suite command was interrupted after three green trials, then
Harbor's package-registry metadata endpoint repeatedly returned Cloudflare 522
before task setup. Harbor 0.18 resolves package metadata even when task bytes
are already cached, and its resume path repeats that lookup. As a provisional
gate, the interrupted job lock selected the exact 16 task digests and Harbor's
own packager rehashed every cached tree to the same digest before those trees
were run through the supported local-dataset path. This offline-local run is
canonical in task and verifier bytes but not in package provenance, so it does
not replace the required registry-resolved final checkpoint. It passed 16/16
with no exceptions in 6 minutes 42 seconds. Concurrency compressed 890.39
aggregate model/API seconds and 132.65 aggregate tool seconds into that wall
time; the run used 431,203 input, 166,009 cached-input, and 41,222 output tokens
across 90 model calls and 74 tool calls. `build-cython-ext` was the longest
agent path at 178.03 Rust seconds after it chose to repair an additional
upstream test, while `git-multibranch` was the largest verifier outlier at
124.88 seconds.

`cobol-modernization` then passed all three canonical assertions on its first
low-effort v12 attempt. During the same registry outage, its locked task tree
was run through Harbor's supported one-task local path at digest `0200cda…`;
the result is therefore also labeled offline-local pending a registry-resolved
rerun. Cold task/verifier preparation took 29.66 seconds, of which 28.91
seconds built the environment. The warm trial used 1.27 seconds for environment
startup, 0.50 seconds for agent setup, 77.16 seconds for agent execution, and
0.79 seconds for verification. Rust spent 76.31 of 77.05 seconds in model/API
turns and 0.65 seconds in six local tool phases. The agent compiled the COBOL
reference and compared all three data files across valid, invalid, and
same-account transaction cases before finishing.

`sqlite-with-gcov` exposed a cold-image portability issue before any model was
called. Its stripped Ubuntu ARM image pointed APT at `ports.ubuntu.com` over
HTTP, which spent 105 seconds timing out; changing that host to HTTPS then
failed in 10.6 seconds because the image had no trusted roots. The generic
verifier layer now bootstraps only `ca-certificates` when roots are absent,
using APT's signed package metadata during that bootstrap, and immediately
returns to normal TLS verification for the real update and dependency install.
Install-only preparation then succeeded in 22.67 seconds, with 21.92 seconds
in the one-time environment build.

The focused warm `sqlite-with-gcov` trial passed all three canonical compile,
PATH, and gcov assertions in 57.17 seconds. Environment startup, agent setup,
and verification took 1.19, 0.46, and 0.53 seconds. Rust ran for 53.43 seconds,
of which 52.44 seconds was model/API time and 22.36 seconds was task-legitimate
compilation and coverage work inside five local tool phases. The run used
33,122 input, 17,715 cached-input, and 1,425 output tokens across six model
calls.

Once the package registry recovered, the earlier registry-resolved checkpoint
passed all 18 tasks with reward 1.0 and zero exceptions in 7 minutes 16.84
seconds. Four-way concurrency compressed 1,017.57 aggregate model/API seconds
and 188.63 aggregate tool seconds into that wall time; tool time is a measured
subset of generated-turn time. The suite used 504,670 input, 190,493 cached
input, and 47,617 output tokens across 99 model calls and 81 tool calls.

That checkpoint deliberately included the cold cost of the shared verifier
Dockerfile change: 15 task-specific verifier overlays rebuilt while the three
already prepared overlays started warm. Cold environment phases totaled
309.03 task-seconds versus 3.87 seconds for the three cached phases; no
canonical task image rebuilt. The four large canonical verifiers accounted
for 91.2% of the aggregate 165.01 verifier seconds, led by the real SSH/HTTPS
deployment test and million-row Vim replay. Rust exceeded model/generated-turn
time by only 10.80 seconds across all 18 tasks, confirming that ordinary warm
harness overhead remains negligible.

`configure-git-webserver` was considered next but not admitted to the active
suite. Its published task image is amd64-only, while its canonical native-ARM
Dockerfile runs APT against `ports.ubuntu.com` over unreachable HTTP. An
install-only build remained in that task-owned step for four minutes before it
was stopped, with no agent or model call. Forcing amd64 emulation, rewriting
benchmark bytes, or adding a host HTTP-to-HTTPS proxy would violate the native,
lean loop, so the candidate is deferred rather than hidden behind setup
plumbing.

`query-optimize` was later rejected at the same cold viability gate. Its native
ARM Ubuntu Dockerfile spent 170.1 seconds in its first task-owned
`apt-get update && apt-get install -y curl` layer, then failed because package
downloads from `ports.ubuntu.com:80` timed out. The complete install-only Harbor
attempt took 172.99 seconds and stopped before agent setup, verification, or a
model call. The task's pinned WordNet download and canonical verifier were
never reached. The task is deferred instead of modifying its Dockerfile or
adding transport workarounds to the harness.

`raman-fitting` passed the cold viability gate but is not admitted to the
active low-effort suite. Its copy-only task and verifier images prepared in
16.97 seconds, then two unchanged v12 runs scored 0.0 in 103.02 and 120.09
seconds. Both fitted the raw first column instead of deriving the spectrum's
coordinate semantics. A third experiment added a generic instruction to
establish scientific units and validate coordinate conversions. That changed
the trajectory, but still scored 0.0 after 12 model calls, 11 tool calls, and
180.78 Rust seconds; it used 75,245 input, 26,014 cached-input, and 6,741 output
tokens. The added prompt weight was reverted because it increased exploration
without satisfying the canonical peak parameters. No task-specific conversion
hint was added, and the active suite remains on the proven v12 prompt.

The hard `llm-inference-batching-scheduler` task then passed all six canonical
schema, integrity, feasibility, coverage, and performance checks on its first
low-effort v12 attempt. Its network-free native task image plus verifier
overlay prepared in 16.45 seconds; the subsequent warm trial used 1.23 seconds
for environment startup, 0.49 seconds for agent setup, 146.41 seconds for
agent execution, and 0.88 seconds for verification. Rust spent 145.58 of
146.25 seconds in model/API turns and 67.66 seconds in six task-legitimate
analysis and plan-generation tool phases.

The generated plans preserved both input hashes and covered all 800 requests
in each bucket exactly once. Bucket 1 reached cost `2.856e11`, padding ratio
`0.05394`, p95 latency `2.036e6` ms, and sequential time `2.678e8` ms; bucket
2 reached `4.543e10`, `0.13808`, `1.924e5` ms, and `3.198e7` ms. The 150.47-
second Harbor trial used 51,936 input, 18,068 cached-input, and 4,588 output
tokens across seven model calls, with no compaction or hosted subagents.

Before adding a long-lived service task, the shell's successful-exit process
lifecycle was compared directly with Codex's `codex-rs/core/src/exec.rs`.
Codex gives inherited output pipes a two-second drain grace after the shell
exits but does not then kill an otherwise successful process group. The local
harness had coupled drain expiry to process-group termination. It now disarms
the group guard after a successful shell exit while retaining group termination
for command timeout and cancellation. A focused regression starts a plain
background process with inherited descriptors and proves it remains alive
after the foreground shell returns.

`kv-store-grpc` then passed all seven canonical source, dependency, TCP,
handshake, and stateful Set/Get assertions on its first low-effort attempt. Its
network-free task/verifier preparation took 3.41 seconds. The focused scored
trial took 35.78 seconds: environment startup, agent setup, agent execution,
and verification used 1.27, 0.54, 30.16, and 0.71 seconds. Rust spent 28.72 of
30.05 seconds in generated model turns and 5.90 seconds in three local tool
phases, primarily installing gRPC. Four model calls used 10,451 input, 6,769
cached-input, and 1,643 output tokens. The agent-launched service remained
reachable across the agent-to-verifier boundary; Harbor container teardown
owns final cleanup.

The first shared 20-task gate passed 19 tasks but exposed a transport lifecycle
bug on `git-multibranch`. A local tool ran for its full 120-second timeout after
`response.completed`; during that interval the response driver no longer
polled the raw WebSocket, so an API keepalive ping went unanswered and the
server closed the connection with code 1011. Current Codex handles this in
`codex-rs/codex-api/src/endpoint/responses_websocket.rs`: a private socket pump
continuously services Ping/Pong independently of response consumption and
channels application frames to the consumer. The harness now implements that
same narrow boundary. A deterministic local WebSocket regression deliberately
leaves `next_json` idle, requires a matching pong, and then proves the queued
JSON event remains available. The focused benchmark rerun passed with reward
1.0 and no exception in 105.59 seconds; its different trajectory spent only
2.95 seconds in tools, so the deterministic test—not model variance—is the
direct keepalive proof.

The required registry-resolved 20-task gate then passed 20/20 with reward 1.0,
zero exceptions, and zero retries in 8 minutes 21.21 seconds. Four-way
concurrency compressed 1,298.07 aggregate generated-model seconds and 177.32
aggregate tool seconds into that wall time; tool time is a measured subset of
generated-turn time. Rust totaled 1,311.74 seconds, including 8.37 seconds of
WebSocket warmup and 5.26 seconds of connection setup, leaving only 0.04
aggregate seconds of harness-local work outside connection, warmup, and model
turns. Environment startup, agent upload/setup, and canonical verification
totaled 29.56, 12.42, and 109.03 task-seconds. The suite used 860,739 input,
267,891 cached-input, 5,508 cache-write, and 62,860 output tokens across 125
model calls and 105 tool calls, with no compaction, hosted subagents, or
API-reported cost.

The scheduler was the main trajectory-variance outlier in that gate: it stayed
green but used 14/13 model/tool rounds, 207.04 generated-model seconds, and
238,230 input tokens, versus 7/6 rounds, 145.58 seconds, and 51,936 input tokens
in its first green sample. That spread is retained as evidence against drawing
latency or token conclusions from one successful trajectory.

These public tasks are the development/tuning set: their instructions,
verifiers, trajectories, and failure cases may be inspected while improving
the harness. Report tuned development results separately from a later held-out
task slice so repeated verifier-guided changes are not mistaken for blind
generalization.

The first async attempt also exposed a verifier-adapter bug: its canonical
assertions require a sibling `test.py`, while the old direct-pytest shortcut
had staged only the assertion module. Running the canonical launcher now
performs its own support-file copy. Once that infrastructure failure was
removed, the model failed the queued-SIGINT cleanup edge case. A narrow prompt
correction requiring verification at the requested external lifecycle boundary
produced a green attempt that tested queued cancellation and a real subprocess
signal. Earlier tasks were rerun and stayed green after the prompt change.

Use Harbor as the runner and result store. First rerun `fix-git` and
`openssl-selfsigned-cert` independently against the hosted-runtime baseline.
Then add one Terminal-Bench task at a time in `evals/*.yaml`, ordered from small
repository investigation/editing through compilation, debugging, and long tool
output. Never modify a benchmark task or verifier to make the harness pass.

For every new task:

1. Run `just prepare-task terminal-bench/<name>` once, then run one scored
   attempt and inspect the JSONL, ATIF trajectory, verifier output, and
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
