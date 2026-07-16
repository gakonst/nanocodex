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
capabilities that must touch the Harbor task container: JSONL, local tools,
bounded process cleanup, reconnect replay, and API-visible measurements. Do not
grow a local agent scheduler, transcript manager, compactor, or second eval
record.

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
   do not send the HTTP `stream` field. Prewarm the stable instructions and
   tools with empty input and `generate: false`, continue incrementally with
   `previous_response_id`, and preserve every raw inbound and outbound API
   event.
3. Keep the common local tool surface exclusively behind hosted Programmatic
   Tool Calling with `allowed_callers: ["programmatic"]`. Rust executes typed
   `function_call` items and returns typed `function_call_output` items with
   the original PTC caller. OpenAI runs the generated JavaScript; Rust never
   does.
4. Treat one generated JavaScript program as a bounded mechanical phase. Use
   `Promise.all` for independent reads, sequence dependent work and mutations,
   reduce intermediate results in hosted JavaScript, retry transient work at
   most once, and return to the model only for semantic judgment. Preserve
   every `program`, `program_output`, `call_id`, and `caller` relationship.
5. A completed response is not a completed task until the root emits a final
   assistant message. A response containing only program or tool work
   continues from its response ID.
6. Use `store: true` with `previous_response_id` so the server owns the response
   chain and a reconnect can resend the same incremental continuation. Capture
   `x-codex-turn-state` from the handshake or `response.metadata`, replay it
   unchanged in WebSocket `client_metadata`, and send it on a same-turn
   reconnect. Do not maintain or replay a parallel local transcript.
7. Enable server-side compaction through `context_management` on every
   generated response. Preserve opaque compaction items in API order; never
   interpret, reorder, or replace them with a local natural-language summary.
   Seed the quality-first profile near 350K tokens and evaluate a cost-sensitive
   profile just below GPT-5.6 Sol's 272K long-context pricing boundary.
8. Follow Codex's automatic GPT-5.6 prompt caching behavior. Keep exact base
   instructions, tools, and contextual input in stable order, use the raw
   run/session ID as `prompt_cache_key` for every request in the response chain,
   and let the server cache the longest growing prefix without explicit cache
   options or breakpoints. Record `cached_tokens` and `cache_write_tokens`.
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

1. Open ordinary WebSockets with
   `OpenAI-Beta: responses_websockets=2026-02-06`; append
   `responses_multi_agent=v1` only for Multi-agent WebSockets. Send the stable
   session ID as the session, thread, and client-request identity. Warm each
   exact stable prompt/tool profile with `generate: false`, then chain from that
   response ID.
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

Status: in progress. Thirty-six public tasks are active with green low-effort
PTC samples under the current `openai-coding-v13` prompt. The first required
35-task gate completed every trial without an exception or retry and scored
34/35; its only miss exposed verifier-package contamination rather than a
model failure. The verifier dependencies are now isolated, the affected
focused regressions pass, and the corrected 35-task gate passes 35/35 with zero
exception or retry. `overfull-hbox` and `compile-compcert` are green;
`tune-mjcf` is now a retained variance experiment after two consecutive current
speed misses. CompCert starts the next three-task batch. The table records
representative warm samples:

### Runtime convergence gate: complete

The Alloy-style WebSocket boundary rewrite was a shared behavior change, so it
triggered the earlier full gate before another task was admitted. This work
remains inside Milestone 2; review provenance does not start until the runtime
is both smaller and green again.

1. Fix the audited runtime failures: preserve partial subprocess output when a
   descendant holds a pipe open, clean up the process group on wait failure,
   keep a direct multi-agent tool alive beyond the socket's ordinary idle
   interval, and measure injection acknowledgement from before the send.
2. Contract the request path around one encoded tool catalog, remove dead
   initial-response state, and collapse redundant Rust/Python accounting. The
   convergence slice must remain a material net reduction in production LOC.
3. Add focused deterministic regressions only for the demonstrated subprocess
   and active-tool timeout failures, then run `just check` plus real PTC and
   Multi-agent `just run` smokes.
4. Rerun the unchanged `fix-git` and `openssl-selfsigned-cert` anchors, then run
   the complete 36-task gate and inspect JSONL, ATIF, verifier output, and task
   diffs. Resume one-at-a-time admissions only after that gate is understood.

All four steps are complete. Production code shrank by 221 lines while the two
demonstrated regressions gained focused deterministic coverage. `just check`,
the live PTC and Multi-agent smokes, and both unchanged anchors passed. The
complete gate at `.harness/harbor/jobs/2026-07-16__11-28-41` completed 36/36
trials with no exception, retry, or WebSocket reconnect in 20 minutes 33
seconds and scored 34/36. Its misses were task-output failures: POV-Ray omitted
a canonical source file after a successful build, and Cancel Async Tasks missed
one queued-SIGINT cleanup assertion.

The timing record rules out the local client loop as the gate bottleneck. Mean
trial time was 128.72 seconds: 115.86 seconds in agent execution, 1.89 seconds
in environment setup, 0.72 seconds in agent setup, and 8.36 seconds in the
verifier. Rust recorded only 3.68 milliseconds per trial outside connection,
warmup, and model-call spans (10.21 milliseconds worst case), with zero
reconnects across 257 model calls. Tool waits averaged 59.03 seconds and are a
subset of the model-call spans; CompCert alone occupied the gate's critical
path for 19 minutes 57 seconds. The next action is an unchanged focused retry
of the two output misses, then one-at-a-time task admission resumes.

The first unchanged `overfull-hbox` attempt passed all four assertions but
spent 68.00 of its 135.45 trial seconds reinstalling an already pinned TeX
package and regenerating formats. A guarded verifier-image cache now skips
only the exact reinstall when the installed package files and generated
`pdflatex.fmt`, `pdftex.map`, and `ls-R` state still match their baked hashes;
any mismatch or other `apt` command delegates to the system executable. The
focused rerun passed 4/4 with a 1.06-second verifier, 49.93 Rust seconds, 49.32
generated-model seconds, and 0.69 tool seconds. Unrelated Fix Git and OpenSSL
anchors remained green at 2/2 and 6/6, with 1.16- and 0.99-second verifiers.

| task | reward | trial | Rust | generated turns | tool wall | rounds/tools | input/cache/output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `fix-git` | 1.0 | 37.13s | 33.22s | 32.69s | 0.29s | 7/6 | 25,196/8,262/1,666 |
| `openssl-selfsigned-cert` | 1.0 | 32.78s | 29.24s | 28.71s | 0.56s | 3/2 | 7,666/4,368/1,794 |
| `cancel-async-tasks` | 1.0 | 56.91s | 39.67s | 39.21s | 0.54s | 4/3 | 8,042/4,368/1,733 |
| `headless-terminal` | 1.0 | 107.93s | 91.30s | 90.73s | 21.92s | 7/6 | 20,692/10,547/3,581 |
| `regex-log` | 1.0 | 39.14s | 34.89s | 34.09s | 0.03s | 2/1 | 4,530/2,991/1,955 |
| `build-cython-ext` | 1.0 | 160.80s | 154.10s | 153.59s | 60.97s | 21/20 | 246,307/47,542/5,225 |
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
| `merge-diff-arc-agi-task` | 1.0 | 53.43s | 49.26s | 48.40s | 0.19s | 6/5 | 25,618/11,571/2,142 |
| `winning-avg-corewars` | 1.0 | 231.73s | 227.31s | 226.75s | 116.36s | 15/14 | 129,260/30,108/4,032 |
| `sparql-university` | 1.0 | 38.76s | 34.70s | 34.17s | 0.42s | 4/3 | 16,983/8,817/1,956 |
| `pypi-server` | 1.0 | 36.84s | 32.35s | 31.37s | 2.19s | 5/4 | 14,314/9,170/1,957 |
| `schemelike-metacircular-eval` | 1.0 | 156.46s | 88.85s | 88.19s | 16.15s | 6/5 | 107,509/29,224/4,455 |
| `distribution-search` | 1.0 | 50.89s | 46.37s | 45.68s | 2.26s | 5/4 | 13,610/8,342/2,268 |
| `largest-eigenval` | 1.0 | 69.44s | 64.85s | 64.09s | 7.57s | 7/6 | 23,668/8,556/3,038 |
| `constraints-scheduling` | 1.0 | 30.93s | 26.49s | 25.72s | 0.13s | 3/2 | 10,483/5,490/1,787 |
| `write-compressor` | 1.0 | 174.45s | 155.55s | 154.24s | 69.25s | 7/6 | 37,685/15,290/5,210 |
| `tune-mjcf` | 1.0 | 464.00s | 446.70s | 445.93s | 352.61s | 11/10 | 74,022/24,066/5,231 |
| `build-pmars` | 1.0 | 87.24s | 82.99s | 81.45s | 18.05s | 12/11 | 144,910/35,732/3,121 |
| `prove-plus-comm` | 1.0 | 14.98s | 10.88s | 9.95s | 0.34s | 3/2 | 5,866/4,466/566 |
| `custom-memory-heap-crash` | 1.0 | 99.40s | 91.74s | 91.01s | 32.71s | 11/10 | 82,173/14,260/3,394 |
| `circuit-fibsqrt` | 1.0 | 108.20s | 103.24s | 102.20s | 35.70s | 5/4 | 25,256/12,438/3,744 |
| `build-pov-ray` | 1.0 | 139.58s | 128.75s | 128.17s | 26.10s | 21/20 | 349,918/47,542/4,894 |
| `overfull-hbox` | 1.0 | 54.56s | 49.93s | 49.32s | 0.69s | 8/7 | 47,067/15,692/2,605 |
| `compile-compcert` | 1.0 | 960.91s | 953.13s | 952.26s | 855.66s | 15/14 | 312,242/55,370/3,427 |

Generated-turn time includes local tool wait; tool wall is a measured subset.
WebSocket connection and warmup added 0.56--1.31 seconds per task, and Rust
overhead outside connection, warmup, and generated turns was only a few
milliseconds. Earlier warm environment and agent setup took 1.75--2.34
seconds; Write Compressor's first post-build start is separated below. The
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

The `openai-coding-v9` iteration introduced representative preservation
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
that exercise the harness's current common Codex tool surface. Browser
automation, computer-use, and other modality-dependent tasks remain deferred;
they are not counted as failures of the current loop.

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
hint was added, and the active suite remained on the proven v12 prompt at that
checkpoint.

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

A subsequent read-only process audit found that Harbor serialized agent
environment variables as `docker compose exec -e KEY=value`, exposing the API
key to the host process table during setup and execution. The adapter now
removes the key from Harbor's scoped exec environment, stages it through the
provider-neutral file-upload API with mode `0400`, reads and unlinks it inside
the container, and assigns it only to the Rust side of the stdout pipeline.
Rust's existing sensitive-environment filter keeps it out of generated shell
children. Final focused `fix-git` and `openssl-selfsigned-cert` anchors passed;
while each was live, the exact key, an `-e OPENAI_API_KEY` flag, and the staged
file were all absent, and a byte scan found the key in zero retained files.

`merge-diff-arc-agi-task` then passed all five canonical repository and output
assertions. Its one-time preparation took 27.90 seconds, of which 27.09 seconds
was the task/verifier environment build. The focused warm trial spent 1.75
seconds on environment startup, 0.52 seconds on agent setup, 49.38 seconds on
agent execution, and 0.57 seconds in the verifier. Rust spent 48.40 of 49.26
seconds in generated model turns and 0.19 seconds in local tools; six model
calls used 25,618 input, 11,571 cached-input, and 2,142 output tokens. Its
canonical setup's exact `apt-get install -y curl git` command is a no-op against
the existing verifier image, so admission added no package or image dependency.

The required registry-resolved 21-task gate then passed 21/21 with reward 1.0,
zero exceptions, and zero retries in 8 minutes 21.74 seconds. Four-way
concurrency compressed 1,595.56 aggregate generated-model seconds and 357.52
aggregate tool seconds into that wall time; tool time is a measured subset of
generated-turn time. Rust totaled 1,606.81 seconds, including 5.84 seconds of
WebSocket warmup and 5.37 seconds of connection setup, leaving 0.04 aggregate
seconds of harness-local work outside connection, warmup, and model turns.
Environment startup, agent upload/setup, and canonical verification totaled
27.26, 11.43, and 110.30 task-seconds. The suite used 574,208 input, 225,570
cached-input, 4,131 cache-write, and 55,785 output tokens across 112 model calls
and 91 tool calls, with no compaction, hosted subagents, or API-reported cost.
A separate warm native Linux agent-artifact build took 2.24 seconds.

`dna-insert` added the verifier's `primer3`/`oligotm` dependency in an isolated
final image layer, preserving every existing apt and Python layer. Its focused
cold preparation took 29.05 seconds wall and 25.76 seconds of Harbor environment
setup. Preparing the complete 22-task verifier set after that shared image
change took 36.45 seconds wall with four-way concurrency and 116.69 aggregate
environment-seconds; that one-time image work remained outside scored trials.
Both existing regression anchors stayed green.

The first low-effort DNA attempt produced a valid exact reconstruction but
selected a sequence-equivalent insertion boundary two bases away from the
canonical test's hardcoded boundary. Its own annealing lengths and `oligotm`
values passed, while the verifier's alternate segmentation exceeded the Tm
delta by 1.53 degrees. No benchmark-specific prompt hint was added. An unchanged
retry selected the other valid boundary and passed, and the full-suite sample
independently passed again. The latter spent 1.17 seconds on environment startup,
0.46 seconds on agent setup, 62.25 seconds on execution, and 1.00 second in the
canonical verifier. Rust used 61.76 seconds, of which 59.37 seconds was the
generated model envelope and 0.72 seconds was local tool work. A later
full-suite sample selected the first, equally valid boundary again and failed
the same hardcoded segmentation. At two green and two red samples,
`dna-insert` is retained as an ambiguity experiment but excluded from the
active gate.

The required registry-resolved 22-task gate passed 22/22 with reward 1.0, zero
exceptions, and zero retries in 8 minutes 14.31 seconds. Four-way concurrency
compressed 1,577.17 aggregate generated-model seconds and 342.97 aggregate tool
seconds into that wall time; tool time is nested inside generated-turn time.
Rust totaled 1,592.80 seconds, including 9.23 seconds of WebSocket warmup and
6.35 seconds of connection setup, leaving 0.05 aggregate seconds of harness-local
work outside connection, warmup, and model turns. Environment startup, agent
upload/setup, and canonical verification totaled 29.42, 14.48, and 86.02
task-seconds. The suite used 709,089 input, 247,400 cached-input, 4,131
cache-write, and 65,214 output tokens across 133 model calls and 111 tool calls,
with no compaction, hosted subagents, or API-reported cost. Warmup probes used a
separate 37,403 input tokens.

`dna-assembly` reused the same prepared Primer3 verifier contract and passed on
its first low-effort attempt. One-time preparation took 36.81 seconds wall and
33.39 seconds of environment setup. The warm scored trial spent 1.27 seconds on
environment startup, 0.49 seconds on agent setup, 166.08 seconds on execution,
and 0.58 seconds in the canonical verifier. Rust spent 164.90 of 165.62 seconds
inside the generated model envelope and 0.68 seconds in seven local tool
phases. Eight model calls used 37,574 input, 16,373 cached-input, and 7,298
output tokens. The canonical test passed its four-pair BsaI structure, template
binding, annealing-length, `oligotm`, unique-junction, and exact circular-output
assertions. Two subsequent samples failed because their self-check omitted
template-matching bases contributed by an overhang when computing the annealing
Tm; the canonical deltas were 5.45 and 6.32 degrees. At one green and two red
samples, `dna-assembly` is also excluded from the active low-effort gate rather
than receiving a task-specific prompt hint.

`winning-avg-corewars` also passed on its first low-effort attempt. The initial
Debian toolchain and pMARS task-image build took 76.18 seconds wall and 72.89
seconds of environment setup, entirely outside the scored trial. Warm
environment startup, agent setup, agent execution, and verification took 1.23,
0.49, 227.81, and 0.98 seconds. Rust spent 226.75 of 227.31 seconds inside the
generated model envelope; 116.36 seconds of real simulator search ran within
that envelope. Fifteen model calls used 129,260 input, 30,108 cached-input, and
4,032 output tokens. The canonical verifier preserved all five opponent files,
loaded the generated warrior, and passed 500 fixed battles at 78, 94, 87, 42,
and 73 wins against thresholds of 75, 75, 75, 33, and 33. No task-specific
search loop, grader, or prompt hint was added.

`sparql-university` added pinned `rdflib==7.1.4` support in a final verifier
image layer and one exact cached-`uvx` command shape. Cold focused preparation
took 29.89 seconds wall and 26.42 seconds of environment setup; preparing all
then-selected verifier overlays took 31.02 seconds wall with four-way
concurrency. Both regression anchors remained green. The first warm scored
sample passed all three canonical verifier checks in 38.76 seconds: Rust used
34.70 seconds, generated turns used 34.17 seconds, and three local tool phases
used 0.42 seconds. Its four model calls used 16,983 input, 8,817 cached-input,
and 1,956 output tokens.

The first combined 25-task checkpoint completed all trials with zero exceptions
or retries but scored 22/25. `dna-insert` reproduced its boundary ambiguity,
`dna-assembly` exposed the annealing-overlap validation miss, and
`kv-store-grpc` omitted one required proto field. The unchanged focused KV
retry passed in 34.87 seconds, and both Core Wars and SPARQL were green in that
checkpoint. The two DNA tasks were therefore deferred while the unrelated KV
sample was treated as ordinary model variance.

The required registry-resolved 23-task gate then passed 23/23 with reward 1.0,
zero exceptions, and zero retries in 9 minutes 51.16 seconds. Four-way
concurrency compressed 1,812.42 aggregate generated-model seconds and 440.62
aggregate tool seconds into that wall time; tool time is a measured subset of
generated-turn time. Rust totaled 1,825.66 seconds, including 7.10 seconds of
WebSocket warmup and 6.10 seconds of connection setup, leaving 0.04 aggregate
seconds of harness-local work outside connection, warmup, and model turns.
Environment startup, agent upload/setup, and canonical verification totaled
28.74, 12.56, and 82.21 task-seconds. The suite used 1,038,520 input, 314,800
cached-input, 6,885 cache-write, and 74,468 output tokens across 152 model calls
and 129 tool calls; 15,707 output tokens were reasoning tokens. Warmup probes
used a separate 39,125 input tokens. No compaction, hosted subagents, or
API-reported cost occurred.

`pypi-server` required only one exact cached-`uvx` command shape for its pinned
`pip==25.2`; it added no verifier image dependency. Cold task preparation took
23.92 seconds wall and 23.08 seconds of environment setup. Its first warm
low-effort sample passed in 36.84 seconds: Rust used 32.35 seconds, generated
turns used 31.37 seconds, and four local tool phases used 2.19 seconds. Five
model calls used 14,314 input, 9,170 cached-input, and 1,957 output tokens. The
canonical verifier uninstalled any existing package, installed
`vectorops==0.1.0` from the agent's localhost PEP 503 index, and passed all four
dot-product cases in 1.13 seconds.

The first 24-task checkpoint kept `pypi-server` green but scored 23/24 because
`kv-store-grpc` changed the explicitly requested `SetValRequest.value` field to
`val`. This was KV's second failure in nine retained samples. The newer
trajectory initially generated the correct field, then renamed it solely to
satisfy its own incorrect smoke test. That measured pattern justified one
generic v13 prompt sentence: explicitly requested public API, schema, file, and
wire names are invariants, and a conflicting self-written check must be fixed
instead of changing the contract. This matches Codex's GPT-5.6 migration
guidance in
`codex-rs/skills/src/assets/samples/openai-docs/references/upgrading-to-gpt-5p6-sol.md`,
which preserves required schema fields and recommends prompt edits only after
representative traces expose a regression. The focused v13 KV retry passed all
seven canonical checks, and the unchanged Git and OpenSSL anchors remained
green.

The required registry-resolved v13 gate then passed 24/24 with reward 1.0, zero
exceptions, and zero retries in 8 minutes 3.28 seconds. Four-way concurrency
compressed 1,628.32 aggregate generated-model seconds and 306.31 aggregate tool
seconds into that wall time; tool time is a measured subset of generated-turn
time. Rust totaled 1,643.62 seconds, including 8.61 seconds of WebSocket warmup
and 6.64 seconds of connection setup, leaving 0.06 aggregate seconds of
harness-local work outside connection, warmup, and model turns. Environment
startup, agent upload/setup, and canonical verification totaled 30.55, 12.88,
and 85.96 task-seconds. The suite used 1,069,450 input, 325,148 cached-input,
5,837 cache-write, and 75,322 output tokens across 159 model calls and 135 tool
calls; 15,944 output tokens were reasoning tokens. Warmup probes used a
separate 41,906 input tokens. No compaction, hosted subagents, or API-reported
cost occurred.

`schemelike-metacircular-eval` was admitted without a runtime, prompt,
adapter, verifier, or image-dependency change. Its copy-only Python 3.13 task
and verifier images prepared in 17.42 seconds wall and 16.55 seconds of
environment setup. The first low-effort sample passed all 63 canonical Scheme
programs, including the nested self-interpretation cases, in 156.46 seconds.
Environment startup and agent setup used 1.27 and 0.52 seconds; Rust used 88.85
seconds, of which 88.19 seconds was the generated-model envelope and 16.15
seconds was five task-legitimate interpreter test phases. The canonical
verifier used 64.19 seconds. Six model calls consumed 107,509 input, 29,224
cached-input, and 4,455 output tokens, with no compaction, hosted subagent, or
API-reported cost. Scheme was the first admission after the 24-task gate and
remained green in all three subsequent 26-task suite attempts.

`distribution-search` added pinned `numpy==2.3.0` in a final verifier-image
layer and one exact cached-`uvx` command shape. Initial task preparation took
29.01 seconds wall and 27.99 seconds of environment setup; rebuilding after
appending the NumPy layer took 5.43 and 4.57 seconds. Preparing all 26 verifier
overlays then took 32.65 seconds wall. The first low-effort sample passed all
four canonical file, exact-shape, probability-validity, and bidirectional-KL
checks in 45.20 seconds. Environment and agent setup used 1.28 and 0.50
seconds; Rust used 41.09 seconds, including 40.04 generated-model seconds and
2.13 seconds across four local tool phases. Five model calls consumed 14,210
input, 8,342 cached-input, and 2,328 output tokens.

The first 26-task full-suite attempt scored 25/26 when Core Wars produced
scores of 59/94/92/40/73 against thresholds of 75/75/75/33/33. An unchanged
focused retry passed at 99/100/85/37/70 in 277.14 seconds: Rust used 272.64 seconds,
generated-model time was 272.18 seconds, and 18 tool phases used 95.43 seconds.
Its 19 model calls consumed 179,798 input, 33,426 cached-input, and 7,372 output
tokens. A second full-suite attempt scored 24/26 in 8 minutes 45.12 seconds
with zero exceptions or retries. Build Cython's untested `ccomplexity` path
raised `NameError: int_ is not defined`, while Polyglot C/Python left its
compiled `cmain` self-test artifact behind. Distribution, Scheme, and Core
Wars were green. Unchanged focused reruns then passed all 11 canonical
Cython-task checks and the canonical Polyglot check, so neither miss justified
a shared prompt or runtime change.

The required registry-resolved 26-task gate then passed 26/26 with reward 1.0,
zero exceptions, and zero retries in 9 minutes 20.43 seconds. Four-way
concurrency compressed 1,605.84 aggregate generated-model seconds and 231.88
aggregate tool seconds into that wall time; tool time is a measured subset of
generated-turn time. Rust totaled 1,620.34 seconds, including 7.87 seconds of
WebSocket warmup and 6.59 seconds of connection setup, leaving 0.05 aggregate
seconds of harness-local work outside connection, warmup, and model turns.
Environment startup, agent upload/setup, and canonical verification totaled
32.45, 13.64, and 155.82 task-seconds. The suite used 1,048,224 input, 312,984
cached-input, 5,704 cache-write, and 76,725 output tokens across 162 model calls
and 136 tool calls; 16,084 output tokens were reasoning tokens. Warmup probes
used a separate 45,383 input tokens. No compaction, hosted subagents, or
API-reported cost occurred.

`protein-assembly` at pinned digest
`sha256:eae346d9f193dd962d66c94e218281f37165a9e7524ec0a7a617c0c68bf136bb`
was audited but not admitted. Its instructions require sequences matching the
current RCSB FASTA response, but RCSB now returns `X` for the modified
chromophore residues in 5WJ2 and 2H5Q where the canonical verifier hardcodes
`GYG` and `MYG`, respectively. The bundled reference path also fails during
DnaChisel constraint resolution. It further depends on serial live RCSB,
FPbase, PubChem, and sequence-search requests plus 80 seconds of deliberate
sleeps across 20 PDB IDs, making correctness and runtime service-dependent.
The task is deferred without changing its benchmark or verifier and without
counting a model sample.

`largest-eigenval` at pinned digest
`sha256:89ed1e066ddb3e9fb78eae0c7d9b2fcf263af0962bd93b0ee07833248ecd0450`
required only one exact cached-pip command shape and no verifier-image
dependency. Its first install-only Harbor job used 17.34 seconds, including
16.49 seconds of environment setup.
The first low-effort sample passed all 27 canonical correctness, dominance,
and speed checks in 69.44 seconds. Environment and agent setup used 1.25 and
0.49 seconds; Rust used 64.85 seconds, including 64.09 generated-model seconds
and 7.57 seconds across six local tool phases. Seven model calls consumed
23,668 input, 8,556 cached-input, and 3,038 output tokens. Before admission,
the pinned reference implementation passed 30/30 repeated native-ARM verifier
runs while the unchanged NumPy baseline passed 0/20 complete runs. The
independent full-suite sample also passed all 27 checks in a 52.77-second
trial, with 48.93 seconds in Rust and 48.51 seconds in generated-model time.

The first 27-task full-suite attempt then passed 26/27 with zero exceptions or
retries in 10 minutes 19.61 seconds. Four-way concurrency compressed 2,018.52
aggregate generated-model seconds and 407.76 aggregate tool seconds into that
wall time; tool time is contained within generated-turn time. Rust totaled
2,033.88 seconds, including 8.34 seconds of warmup and 6.97 seconds of
connection setup, leaving 0.04 aggregate seconds of other harness-local work.
Environment startup, agent upload/setup, and canonical verification totaled
34.22, 14.34, and 123.05 task-seconds. The suite used 1,448,275 input, 373,538
cached-input, 9,982 cache-write, and 85,015 output tokens across 177 model
calls and 150 tool calls; 18,688 output tokens were reasoning tokens. Warmup
probes used another 47,004 input tokens. No compaction, hosted subagent, or
API-reported cost occurred. Core Wars was the only red task: after 31 model
calls, 30 tool calls, 524.01 generated-model seconds, and 479,097 input tokens,
it reproduced the earlier failing 59/94/92/40/73 scores exactly. The other 26
tasks, including both independent Largest Eigenvalue samples, remained green.
An unchanged focused Core Wars retry also scored 0.0, this time at
100/100/74/12/19 after 15 model calls, 14 tool calls, 235.41 generated-model
seconds, and 131,330 input tokens. Its retained low-effort history is therefore
8 green and 3 red samples. The task remains active because its deterministic
verifier is measuring real optimization variance; removing it or adding a
task-specific warrior hint would hide that signal.

`constraints-scheduling` at pinned digest
`sha256:757c2b9672df2db1ca879878e6c13dcad20222381694a29920eb452276a69d90`
needed only a YAML admission. Its first install-only Harbor job took 31.28
seconds, including 30.35 seconds of environment setup. The first low-effort
sample passed all three canonical tests covering structure,
conflicts/business hours, earliest-time/tie-breaks, and Carol's buffer in
30.93 seconds. Environment and agent setup used 1.29 and 0.53 seconds; Rust
used 26.49 seconds, including 25.72 generated-model seconds and 0.13 seconds
across two local tool phases. Three model calls consumed 10,483 input, 5,490
cached-input, and 1,787 output tokens; canonical verification used 0.89
seconds. The canonical launcher requests Python 3.13, while this task image and
the cached verifier fast path use Python 3.12.3. Its assertions use only stdlib
behavior with no known 3.12/3.13 sensitivity, so the task is admitted, but
version-sensitive candidates remain deferred until the fast path can preserve
interpreter selection without adding repeated warm installation. This was the
first focused admission in the next three-task batch.

`write-compressor` at pinned digest
`sha256:102167adfe1e831e220cf1e045b4b70fb01a816351fa5cc629b61e2f745fbedc`
also needed only a YAML admission. Its first install-only Harbor job took 97.29
seconds, including 94.44 seconds of environment setup. The job log separates
about 35.03 seconds for the native Ubuntu/Rust/GCC task image from about 59.24
seconds for its first verifier overlay; both layers are cached outside model
trials. The first low-effort sample produced a 2,478-byte compressed artifact
and passed all three canonical existence, exact-decompression, and size checks
in 174.45 seconds. Environment and agent setup used 12.21 and 2.68 seconds on
this first post-build start. Rust used 155.55 seconds, including 154.24
generated-model seconds and 69.25 seconds across six task-legitimate
inspection, encoding, and verification tool phases. Seven model calls consumed
37,685 input, 15,290
cached-input, and 5,210 output tokens; canonical verification used 0.76
seconds. Its cached verifier likewise used task-image Python 3.12.3 for
stdlib-only assertions despite the launcher's Python 3.13 request. This was the
second focused admission in the batch. Its independent full-suite sample
confirmed the steady-state path: environment and agent setup fell to 1.37 and
0.63 seconds, respectively, from the first post-build 12.21- and 2.68-second
materialization phases.

`tune-mjcf` at pinned digest
`sha256:bcf3607c7d8d79c94e451c88930c4e325b7ed084296eab63899b1cbd5cedbd0e`
needed one exact allowlisted `uvx` command shape for the already installed
`mujoco==3.3.5` dependency and no new verifier-image layer. Its install-only
Harbor job took 21.80 seconds, including 20.90 seconds of environment setup.
The first low-effort sample selected a PGS-based model, preserved the final
state exactly, and passed all four canonical integrity, correctness, and speed
checks. It averaged 0.2013 seconds versus the 0.3969-second reference, a 1.97x
speedup and 0.51 time ratio. The 464.00-second trial used 1.27 seconds for
environment startup, 0.53 seconds for agent setup, and 13.45 seconds for the
canonical verifier. Rust used 446.70 seconds, including 445.93 generated-model
seconds and 352.61 seconds across ten MuJoCo inspection, simulation, and
verification tool phases. Eleven model calls consumed 74,022 input, 24,066
cached-input, and 5,231 output tokens. This third admission triggered the
required full-suite gate.

That 30-task gate passed 30/30 with zero exceptions and zero retries in 10
minutes 58.09 seconds. Four-way concurrency compressed 2,113.70 aggregate
generated-model seconds and 536.60 aggregate tool-wall seconds into that wall
time; tool time is contained within generated-turn time. Rust totaled 2,132.55
seconds, including 9.37 seconds of warmup and 9.43 seconds of WebSocket setup,
leaving 0.05 aggregate seconds of other harness-local work. Environment
startup, agent upload/setup, and canonical verification totaled 38.07, 16.57,
and 244.54 task-seconds. The suite used 1,170,343 input, 384,630 cached-input,
8,556 cache-write, and 88,049 output tokens across 193 model calls and 163 tool
calls; 18,097 output tokens were reasoning tokens. Warmup probes used another
52,178 input tokens. No compaction, hosted subagent, API-reported cost,
exception, or retry occurred. Core Wars recovered to green without a prompt or
runtime change. Tune MJCF independently passed again in 386.29 seconds with
1.11 seconds of environment setup, 0.48 seconds of agent setup, 369.77 seconds
in Rust, 369.27 seconds in generated-model time, 250.30 seconds in tools, and
13.41 seconds in the canonical verifier.

`build-pmars` at pinned digest
`sha256:796773c1d8b991b9b936b3513308b1a4d2ea1772ae9d9b29103f58748d6e2c17`
needed only a YAML admission. Its install-only Harbor job took 16.93 seconds,
including 16.19 seconds of environment setup. The first low-effort sample
downloaded Debian source, built and installed the headless simulator, exercised
its debugger and battle modes, and passed all four canonical binary,
no-X11-linkage, Debian-source, and source-build checks. The 87.24-second trial
used 1.26 seconds for environment startup, 0.57 seconds for agent setup, and
0.60 seconds for canonical verification. Rust used 82.99 seconds, including
81.45 generated-model seconds and 18.05 seconds across eleven source,
dependency, build, and verification tool phases. Twelve model calls consumed
144,910 input, 35,732 cached-input, and 3,121 output tokens. The runtime package
and source work is part of the requested build task rather than verifier setup;
the cached environment itself remained a roughly one-second warm start. This
is the first focused admission after the 30-task full-suite gate. The task's
Debian repository is mutable and its bundled reference currently names a stale
`dpkg-dev` patch version, so this sample proves the prompt-compliant current
repository path rather than byte-stable source provenance.

`prove-plus-comm` at pinned digest
`sha256:2c5295786c38135ca86b4b09cab3a82408e25129841f5fc533d31b885f6d737a`
also needed only a YAML admission. Its first install-only Harbor job took 60.10
seconds, including 59.29 seconds for the one-time Ubuntu/Coq task and verifier
image build. The first low-effort sample completed the induction proof,
compiled it with `coqc`, and passed all four canonical source, artifact,
no-admit, and fresh-compilation checks. The 14.98-second trial used 1.27 seconds
for warm environment startup, 0.50 seconds for agent setup, and 0.64 seconds
for canonical verification. Rust used 10.88 seconds, including 9.95
generated-model seconds and 0.34 seconds across two inspection/edit/compile
tool phases. Three model calls consumed 5,866 input, 4,466 cached-input, and
566 output tokens. This was the second focused admission after the 30-task
gate.

`custom-memory-heap-crash` at pinned digest
`sha256:72cdc9cf98c822a1449935ca32efc6612fe3d14928c9465c11285e95dcdf60cb`
completed that batch. Its deliberately expensive install-only Harbor job took
389.08 seconds, including 388.13 seconds to download GCC source, build the two
patched debug/release libstdc++ variants, and create the 4.13 GB task and 4.34
GB verifier images. That one-time compiler bootstrap used no model tokens. The
first low-effort sample isolated the facet-lifetime failure, initialized facets
before installing the temporary heap, and passed all six canonical protected
source, debug/release compile and execution, and Valgrind leak checks. The
99.40-second warm trial used 1.25 seconds for environment startup, 0.51 seconds
for agent setup, and 4.08 seconds for canonical verification. Rust used 91.74
seconds, including 91.01 generated-model seconds and 32.71 seconds across ten
inspection, compile, execution, and Valgrind tool phases. Eleven model calls
consumed 82,173 input, 14,260 cached-input, and 3,394 output tokens. The scored
path was API-dominated once the canonical task image was cached; this third
focused admission triggers the 33-task full-suite gate.

That 33-task gate passed 33/33 with zero exceptions and zero retries in 10
minutes 59.04 seconds. The complete `just eval` command took 663.80 seconds,
only 4.76 seconds beyond Harbor's job wall for the cached native artifact
build, configuration, and launch. Four-way concurrency compressed 2,084.81
aggregate generated-model seconds and 444.55 aggregate tool-wall seconds into
the job wall; tool time is contained within generated-turn time. Rust totaled
2,107.79 seconds, including 13.55 seconds of warmup and 9.38 seconds of
WebSocket setup, leaving 0.06 aggregate seconds of other in-process harness
work. Environment startup, agent upload/setup, and canonical verification
totaled 43.14, 17.42, and 162.82 task-seconds; agent execution outside the Rust
process totaled 15.61 seconds across all trials.

The gate used 1,143,115 input, 384,442 cached-input, 8,633 cache-write, and
87,682 output tokens across 195 model calls and 162 tool calls; 18,348 output
tokens were reasoning tokens. Warmup probes used another 57,075 input tokens.
Model calls averaged 10.69 seconds with 6.67-second median, 36.16-second p95,
and 60.24-second maximum; time to first output averaged 2.02 seconds with a
5.42-second p95. Tool calls averaged 2.74 seconds with 0.15-second median,
20.15-second p95, and 46.03-second maximum. No compaction, hosted subagent,
API-reported cost, exception, or retry occurred. The three new tasks all passed
independently again: Build pMARS in 76.68 seconds, Prove Plus Comm in 25.91
seconds, and Custom Memory Heap Crash in 76.24 seconds. Core Wars also remained
green without a task-specific prompt or runtime change.

Preparing the next Debian Bullseye candidate demonstrated two shared cached-
verifier compatibility failures before any model call. The task image built
successfully during the 116.03-second cold attempt, but uv could not inspect
Bullseye's Python 3.9 without the separately packaged `distutils`; after that
was installed conditionally, the common overlay still tried to install NumPy
2.3 even though that unrelated dependency requires Python 3.11. The overlay now
installs `python3-distutils` only where the distribution offers it and installs
NumPy 2.3 only on a compatible interpreter. The unchanged task then prepared
without an exception in a 9.81-second Harbor job (7.36 seconds of environment
setup; 16.23 seconds for the complete command). Focused regression runs stayed
green: Fix Git passed both canonical tests in a 35.80-second trial with 31.39
seconds in Rust, 30.92 seconds in generated-model time, and 1.02 seconds of tool
wall; OpenSSL passed all six canonical tests in 26.94 seconds with 22.69 seconds
in Rust, 21.82 seconds in generated-model time, and 0.67 seconds of tool wall.
Both had zero exceptions, retries, and agent stderr. This is a verifier-image
portability fix, not a benchmark or assertion change.

The first downstream scored trial also showed that its unchanged launcher
spells the cached package command `apt-get install -y curl expect`, while the
adapter had recognized only the reverse package order. The packages were
already present and the assertion happened to pass, but the nonzero setup
command was visible in the verifier log, so that sample was not accepted as a
clean admission. The allowlist now recognizes the exact canonical command.
The repeated QEMU verifier ran without a setup warning, and another focused
regression pass kept Fix Git at 2/2 tests in a 46.63-second trial and OpenSSL at
6/6 in a 29.91-second trial. Both again had reward 1.0 with zero exceptions,
retries, and agent stderr.

`qemu-startup` at pinned digest
`sha256:e9796833ca178d98500b4146d107d947226485065f5a79a1dff2cdf7430f4f95`
is the next focused admission. Its task-image build was paid during the initial
115.93-second install-only trial, before the shared verifier overlay exposed
the legacy-Python failures described above; after those generic corrections,
the clean install-only job took 9.81 seconds with 7.36 seconds of environment
setup. The accepted low-effort sample started x86_64 Alpine 3.19 under QEMU,
waited for the real serial login banner, preserved the background VM, and
passed the canonical telnet login and `6.6.4-1-lts` kernel check. Its
122.07-second warm trial used 1.60 seconds for environment startup, 0.52 seconds
for agent setup, and 2.67 seconds for the unchanged verifier. Rust used 115.47
seconds, including 114.83 generated-model seconds and 55.73 seconds across
eight inspection, boot, readiness, and boundary-check tool phases. Nine model
calls consumed 28,518 input, 13,022 cached-input, and 3,307 output tokens, of
which 888 were reasoning tokens; warmup used another 1,536 input tokens. No
compaction, hosted subagent, API-reported cost, exception, retry, or agent
stderr occurred. The earlier green sample with the verifier setup warning is
retained as diagnostic evidence but is not the admission sample.

The first complete overlay rebuild caught a second-order issue in the initial
legacy-Python fix: `apt-cache show` exposes obsolete `python3-distutils`
metadata on modern Debian and Ubuntu even when APT has no installation
candidate. That 161.74-second, zero-model-token preparation job completed all
34 install-only trials but correctly reported 16 image-build exceptions. The
guard now requires a real candidate from `apt-cache policy`. Focused
install-only probes covered both branches: Bullseye QEMU installed the package
and completed 12.60 seconds of environment setup, while modern Debian pMARS
skipped it and completed in 15.10 seconds. The repeated full preparation then
completed 34/34 with zero exceptions or retries in 202.10 seconds of Harbor
wall and 203.27 seconds for the whole command, still with no model tokens.
Fresh scored anchors passed from those images: Fix Git passed 2/2 tests in
49.95 seconds and OpenSSL passed 6/6 in 36.13 seconds, both at reward 1.0 with
clean verifier logs and no exceptions, retries, or agent stderr.

The first 34-task gate then completed in 15 minutes 23.71 seconds with 33
reward-1 trials, one reward-0 trial, one agent exception, and zero Harbor
retries. It used 1,378,131 input, 429,826 cached-input, and 100,431 output
tokens. Tune MJCF was the sole canonical miss: its unchanged verifier passed
the reference-integrity and output-existence checks, but the selected model
ended at a 0.0012515 state difference versus the required `1e-5` and a 62.85%
runtime ratio versus the required 60%. That 336.61-second trial used 10/9
model/tool rounds, 63,347 input, 19,568 cached-input, and 6,700 output tokens.
This is retained as model trajectory variance rather than hidden by a task hint
or verifier change.

The immediate unchanged focused retry confirmed that classification. It passed
all four canonical checks with no exception or stderr: the verifier measured a
0.48 runtime ratio and an all-close final state, while the model's 100-seed
probe found a worst maximum absolute difference of `3.72e-6`. The 262.53-second
Harbor trial used 1.31 seconds for environment startup, 0.51 seconds for agent
setup, 244.47 seconds in Rust, and 12.84 seconds for the unchanged verifier.
Fourteen model calls and thirteen tool phases used 99,643 input, 29,368
cached-input, and 6,005 output tokens; 148.66 seconds was measured local
simulation/tool wall. The red and green samples are both retained, so the
full-suite rerun remains the gate rather than treating either trajectory alone
as the benchmark's stable outcome.

QEMU's canonical telnet/kernel assertion still passed in that gate, but Harbor
correctly reported an agent exception. After a 127.34-second local VM tool
phase, the server had normally closed the idle WebSocket; the next stored
continuation failed before transmission with Tungstenite
`ConnectionClosed`, so Rust emitted `run.failed` even though the workspace was
valid. The runtime now retries only that unambiguous pre-stream boundary (and
`AlreadyClosed`): it opens one new WebSocket and resends the exact
`store: true` request with its existing `previous_response_id`. It does not
retry arbitrary I/O failures, mid-stream reads, or Multi-agent injections.
This follows the Responses WebSocket reconnect contract and the comparable
closed-connection path in Codex's `core/src/client.rs` and
`codex-api/src/endpoint/responses_websocket.rs` without importing Codex's HTTP
fallback or provider machinery.

A deterministic two-connection test closes the first socket while a local
tool is running, then proves that the second request retains both the stored
response ID and tool output. Connection attempts, successful reconnects, and
connection wall time are now present in terminal JSONL and Harbor/ATIF
metadata. The fresh real QEMU trial passed its canonical check with a normal
`run.completed`, zero exceptions, retries, or stderr, and no reconnect needed
in that particular sample. Its 245-second Harbor trial spent 237.09 seconds in
Rust, 236.51 seconds in generated-model turns, and 169.48 seconds in five
legitimate VM setup/readiness tool phases; six model calls used 15,293 input,
9,768 cached-input, and 3,357 output tokens. Fresh runtime anchors also stayed
green: Fix Git passed 2/2 in 43 seconds with 37.94 seconds in Rust, while
OpenSSL passed 6/6 in 39 seconds with 35.04 seconds in Rust. Both had a single
WebSocket connection, zero exceptions, and empty agent stderr. A subsequent
reward-1 Fix Git projection run confirmed all three connection metrics in both
Harbor result metadata and ATIF final/step metrics.

The required post-reconnect 34-task `just eval` gate completed in 11 minutes
19.37 seconds with 33 reward-1 trials, one reward-0 trial, zero exceptions, and
zero Harbor retries. All 34 agents emitted `run.completed`, all agent stderr
files were empty, and all 34 ATIF trajectories and canonical CTRF records were
present. QEMU and Tune MJCF both passed in this sample. The gate used 1,470,884
input, 449,990 cached-input, 10,317 cache-write, and 102,460 output tokens over
224 model calls and 190 tool phases; warmup used another 58,611 input tokens.
Aggregate Rust time was 2,310.65 seconds: 2,283.25 seconds in generated model
turns, 13.40 seconds connecting, 13.95 seconds warming, and about 0.05 seconds
of remaining harness-local work. Local tools occupied 505.21 seconds within
the model envelope. Environment startup, agent setup, and canonical verification
totaled 45.83, 18.99, and 135.31 task-seconds. No reconnect, compaction, hosted
subagent, or API-reported cost occurred in this particular gate.

`winning-avg-corewars` was the sole canonical miss. The full-gate trajectory
completed normally after 25/24 model/tool rounds but knowingly left Stone at
59 wins against the required 75; its other four opponents passed. That
319.09-second trial spent 314.71 seconds in Rust, 314.18 seconds in generated
turns, and 29.09 seconds in simulator tools, using 275,197 input, 49,772
cached-input, and 10,523 output tokens. The immediate unchanged focused
`just eval-task` retry also completed normally but selected a different near
miss: Stone, Vampire, Paper, and G2-Clear passed at 78, 93, 92, and 65 wins,
while Snake reached 22 against the required 33. It took 350.12 seconds, with
345.44 seconds in Rust, 344.81 seconds in generated turns, and 65.17 seconds in
tools over 26/25 rounds, using 317,830 input, 37,264 cached-input, and 10,204
output tokens. Both red samples had zero exception, reconnect, or stderr, while
the earlier unchanged green sample passed all five opponents at 78, 94, 87,
42, and 73. At one green and two red low-effort samples with different failure
modes, Core Wars is retained as a variance experiment but excluded from the
stable active gate rather than receiving a benchmark-specific search loop or
prompt hint.

The pinned dataset ranks `make-mips-interpreter` before the next admitted task,
but its success criterion requires generating and validating Doom render
frames. It is deferred as an image-output task under the current non-modal
shell/code scope; no preparation or scored attempt was run for this milestone.

`circuit-fibsqrt` at pinned digest
`sha256:fe25ab474ba626c4f45f57e757e86f2ad292de80b505e5b7ba0ba76c30d7fcc8`
is the first admission in the next batch. Its cold install-only command took
55.93 seconds, with 52.64 seconds in Harbor and 51.59 seconds constructing the
task and verifier images; it made no model call. The first low-effort scored
attempt generated a 3,267-line gate circuit and passed all three canonical
existence, size, and Fibonacci-of-integer-square-root checks. The model also
self-checked 30 boundary and seeded-random inputs, and its final workspace
listing contained only the supplied simulator source and requested gate file.

The warm Harbor trial took 108.20 seconds: environment startup used 1.29
seconds, agent setup 0.52 seconds, execution 103.76 seconds, and canonical
verification 1.25 seconds. Rust used 103.24 seconds, including 102.20 seconds
in generated model turns and 35.70 seconds across four inspection, generation,
compile, and boundary-check tool phases. Five model calls used 25,256 input,
12,438 cached-input, and 3,744 output tokens; warmup used another 1,718 input
tokens. JSONL and ATIF agreed on the 5/4 model/tool rounds, the agent emitted
`run.completed`, and no exception, retry, reconnect, compaction, hosted
subagent, API-reported cost, verifier setup warning, or agent stderr occurred.
No shared harness change was needed, so the next full `just eval` remains due
after two further admissions complete this three-task batch, or earlier if a
shared runtime or environment change is required.

The first `build-pov-ray` attempt demonstrated the next generic verifier-cache
gap. The agent completed normally and produced a plausible render, but the
canonical assertions never started: the shared adapter rejected the exact
`apt-get install -y curl imagemagick` and pinned Pillow/NumPy/scikit-image
`uvx` command shapes. That setup-only reward 0 is retained and is not counted
as a model result. The reusable verifier image recognizes only those exact
canonical commands; the benchmark task and assertions remain untouched.

Rebuilding the focused POV-Ray overlay took 11.90 seconds in Harbor and 14.23
seconds for the complete install-only command. The complete 35-overlay
`just prepare-evals` then passed 35/35 with zero exception or retry in 142.89
seconds of Harbor wall and 144.43 seconds for the command, with no model calls.
Focused scored regressions remained green with no setup warning or agent
stderr: Fix Git passed 2/2 checks in 40.02 Rust seconds, OpenSSL passed 6/6 in
32.76 seconds, and NumPy-dependent Distribution Search passed 4/4 in 40.47
seconds. These focused results established that the new command shapes and
packages worked, while the full gate below caught that the first cache layout
had changed an agent-visible package version.

`build-pov-ray` at pinned digest
`sha256:b64f3fd6f47dc8848fdd6ce990fbedce9577d97ea4f7fd1c489c42338229d078`
is the second admission in this batch. Its initial task/verifier image
preparation took 60.01 seconds in Harbor and 62.91 seconds for the command,
including 59.20 seconds of environment construction. After the shared setup
fix above, the first valid low-effort sample downloaded the original source and
documentation archives, patched the 1990s C for a modern compiler, installed
the AArch64 binary, and preserved the supplied scene unchanged.

The accepted warm trial took 129.63 seconds: environment startup used 1.29
seconds, agent setup 0.54 seconds, execution 118.37 seconds, and canonical
verification 7.70 seconds. Rust used 117.79 seconds, including 117.00 seconds
in generated model turns and 29.78 seconds across seventeen source-discovery,
patch, compile, render, and cleanup tool phases. Eighteen model calls used
291,544 input, 49,408 cached-input, and 4,370 output tokens; warmup used another
1,598 input tokens. The unchanged verifier passed all three checks: the
rendered scene reached SSIM `0.8731`, the binary identified as POV-Ray 2.2, and
the required source tree remained present. JSONL and ATIF agreed, the terminal
was `run.completed`, and there was no exception, retry, reconnect, compaction,
hosted subagent, API-reported cost, setup warning, or agent stderr. No further
runtime change was needed.

The required 35-task `just eval` after that shared verifier change completed
all 35 trials with zero Harbor exceptions or retries in 16 minutes 41.92
seconds, but scored 34/35. Every agent emitted `run.completed`, every stderr
file was empty, and all 35 JSONL streams, ATIF trajectories, and CTRF records
were present. The canonical aggregate was 136/137 passing tests. Four-way
concurrency compressed 2,721.996 aggregate generated-model seconds and 841.51
aggregate tool-wall seconds into the job wall; tool time is nested inside
model time. Rust totaled 2,746.00 seconds, including 10.00 seconds of
WebSocket setup and 13.94 seconds of warmup, leaving only 0.06 aggregate
seconds of other in-process work. Environment startup, agent setup, agent
execution, and verification totaled 48.40, 19.86, 2,764.54, and 180.26
task-seconds. The run used 1,476,992 input, 455,038 cached-input, 7,131
cache-write, and 106,830 output tokens across 219 model calls and 184 tool
phases; 23,552 output tokens were reasoning tokens and warmup used another
60,191 input tokens. No reconnect, compaction, hosted subagent, agent message,
or API-reported cost occurred.

`build-cython-ext` was the sole miss, and its evidence made the cause
deterministic: 10/11 assertions passed, including all compiled-extension and
behavior checks, while `test_numpy_version` found that the verifier layer had
replaced the task's required NumPy 2.3.0 with verifier-only NumPy 2.3.1 before
the agent started. The scientific image stack now installs under
`/opt/harness-verifier/pov`, and only the exact POV-Ray `uvx` launcher receives
that path through `PYTHONPATH`; the agent and all other verifiers keep their
system interpreter unchanged.

The corrected Cython overlay prepared in 13.39 seconds and its unchanged
focused trial passed 11/11 assertions in 160.80 seconds. Rust used 154.10
seconds, generated-model time was 153.59 seconds, and 20 local tool phases used
60.97 seconds; 21 model calls consumed 246,307 input, 47,542 cached-input, and
5,225 output tokens. The isolated POV overlay prepared in 10.80 seconds and
then passed 3/3 assertions, including SSIM `0.8731`, in 139.58 seconds. Rust
used 128.75 seconds, generated-model time was 128.17 seconds, and 20 tool
phases used 26.10 seconds; 21 model calls consumed 349,918 input, 47,542
cached-input, and 4,894 output tokens. Finally, Distribution Search's overlay
prepared in 10.12 seconds and passed 4/4 assertions in 50.89 seconds, with
46.37 Rust seconds, 45.68 generated-model seconds, 2.26 tool seconds, and
13,610/8,342/2,268 input/cache/output tokens over 5/4 rounds. All three
focused trials had empty stderr and zero exception, retry, reconnect,
compaction, or hosted subagent.

The corrected 35-task `just eval` then passed 35/35 with zero exception or
retry. Harbor used 1,112.29 seconds; the complete timed command used 1,115.66
seconds, leaving 3.37 seconds for the warm native artifact build,
configuration, and launch. Four-way concurrency compressed 3,087.47 aggregate
trial-seconds into that wall time. Environment startup, agent setup, agent
execution, and canonical verification totaled 44.94, 20.02, 2,829.08, and
149.47 task-seconds. Rust totaled 2,811.21 seconds: 2,789.47 seconds in the
generated-model envelope, 10.82 seconds connecting, 10.86 seconds warming,
and 0.07 seconds of other in-process harness work. Local tools occupied
1,032.37 seconds within the model envelope.

The gate used 1,614,579 input, 477,678 cached-input, 11,842 cache-write, and
104,053 output tokens across 231 model calls and 196 tool phases; 21,436
output tokens were reasoning tokens and warmup used another 60,191 input
tokens. Model calls averaged 12.08 seconds with 5.49-second median,
52.86-second p95, and 116.34-second maximum. Time to first output averaged
1.99 seconds with 1.12-second median and 4.49-second p95. Tool calls averaged
5.27 seconds with 0.15-second median, 43.61-second p95, and 103.73-second
maximum. All 137 canonical assertions passed. All 35 input streams, event
streams, ATIF trajectories, and CTRF records were present; every terminal was
`run.completed`, every ATIF terminal payload matched raw JSONL, and every
agent stderr and verifier warning scan was empty. No reconnect, compaction,
hosted subagent, agent message, injection, or API-reported cost occurred.

Tune MJCF determined the suite tail rather than local setup. Its 766.43-second
trial spent 749.11 seconds in Rust, 748.33 seconds in generated-model time,
602.53 seconds in 22 simulation/tool phases, and 13.42 seconds in the unchanged
verifier. Twenty-three model calls used 197,807 input, 44,250 cached-input, and
9,014 output tokens. The canonical checks passed the unchanged reference,
output existence, correctness, and speed assertions at a 0.51 time ratio and
1.96x speedup. The corrected full sample also passed Cython 11/11, POV-Ray
3/3 at SSIM `0.8731`, Distribution Search 4/4, and QEMU 1/1. The focused green
Overfull HBox admission completed this batch.

The subsequent literal 36-task `just eval` completed every trial with zero
Harbor exception or retry in 921.72 seconds; the complete timed command used
925.00 seconds. It scored 33/36 and 138/141 canonical assertions. Four-way
concurrency compressed 3,375.18 aggregate trial-seconds into the job wall.
Environment startup, agent setup, execution, verification, and teardown/gaps
used 50.37, 27.39, 2,956.24, 292.73, and 48.45 task-seconds. Rust totaled
2,933.51 seconds: 2,908.15 seconds in the generated-model envelope, 12.59
seconds connecting, 12.70 seconds warming, and 0.07 seconds of other in-process
work. Local tools occupied 842.47 seconds inside model time.

The gate made 256 model calls and 220 tool calls, using 1,923,660 input,
493,502 cached-input, 13,268 cache-write, and 111,989 output tokens; 24,129
output tokens were reasoning tokens and warmup used another 61,723 input
tokens. Model calls averaged 11.36 seconds with 6.04-second median, 43.45-second
p95, and 131.47-second maximum. Mean time to first event and output was 0.13
and 1.94 seconds. Tool calls averaged 3.83 seconds with 0.10-second median,
22.69-second p95, and 120.02-second maximum. All 36 input streams, raw event
streams, ATIF trajectories, CTRF records, rewards, and artifact manifests were
present; every terminal was `run.completed`, every ATIF terminal payload
matched raw JSONL, and all agent stderr files were empty. No reconnect,
compaction, hosted subagent, agent message, injection, verifier-cache warning,
or API-reported cost occurred.

The three misses were solution-level variance, not harness failures. Write
Compressor round-tripped correctly but produced 2,649 bytes against a
2,500-byte limit; its unchanged retry passed 3/3 in 159.78 command seconds,
with 146.45 Rust seconds, 145.64 model seconds, 79.72 tool seconds, 6/5 rounds,
and 29,065/9,768/4,042 input/cache/output tokens. Cython passed 10/11 but left
`pyknotid` as a namespace package; its unchanged retry passed 11/11 in 177.64
command seconds, with 163.95 Rust seconds, 161.57 model seconds, 78.43 tool
seconds, 16/15 rounds, and 332,126/52,700/5,746 tokens. Tune MJCF preserved the
reference and exact final state but reached only 1.03x reference time in the
gate. Its unchanged 326.69-second retry again passed correctness but reached
0.88x reference time rather than the required 0.60x, despite 298.42 Rust,
297.82 model, and 177.58 tool seconds over 14/13 rounds. Earlier green samples
show this task can pass, but two consecutive current failures and its extreme
cost make it unsuitable for the stable gate without a benchmark-specific hint;
it is therefore retained as an excluded variance experiment.

`compile-compcert` at pinned digest
`sha256:59b8bdc7ad56243291afbc14cc1947d996db745a488ac018be4936643a47a999`
starts the next batch. Its task/verifier image preparation was already cached:
Harbor used 5 seconds and the complete command used 8.41 seconds. The first
unchanged low-effort trial then built CompCert 3.13.1 from source for native
AArch64 and passed all three authenticity and functionality assertions in
960.91 trial seconds; the complete command used 965.02 seconds. Environment
startup used 1.45 seconds, agent setup 0.72 seconds, agent execution 954.52
seconds, and canonical verification 1.02 seconds.

Rust used 953.13 seconds, including 952.26 seconds in generated-model turns
and 855.66 seconds across fourteen tool phases; connection, warmup, and other
in-process work used 0.59, 0.27, and less than 0.01 seconds. Fifteen model calls
consumed 312,242 input, 55,370 cached-input, and 3,427 output tokens; 593 output
tokens were reasoning tokens and warmup used another 1,528 input tokens. The
two longest tools spent 345.98 seconds building Coq 8.16.1 and 384.99 seconds
rebuilding CompCert proofs. Several failed configure/build probes preceded the
valid artifact, so this baseline records agent strategy cost rather than a
local harness bottleneck.

The unchanged verifier confirmed the exact version, executable source build,
native ELF output, randomized and edge-case behavior, and rejection of an
unsupported VLA. Its launcher reported that `apt-get install -y curl binutils`
was not a cached command, then continued because both tools were already
present from the source build; the assertions passed in 0.08 seconds. With a
1.02-second verifier phase, that warning is retained rather than adding a new
shared dependency layer for no measured gain. Raw JSONL and ATIF terminal
payloads matched, stderr was empty, and there was no exception, retry,
reconnect, compaction, hosted subagent, injection, or API-reported cost.

The scheduler was the main trajectory-variance outlier in the earlier 20-task
gate: it stayed green but used 14/13 model/tool rounds, 207.04 generated-model
seconds, and 238,230 input tokens, versus 7/6 rounds, 145.58 seconds, and 51,936
input tokens in its first green sample. That spread is retained as evidence
against drawing latency or token conclusions from one successful trajectory.

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
- Broad unit-test expansion for the current runtime cleanup; retain only
  focused deterministic tests justified by demonstrated regressions.
- Improving the environment-secret-name heuristic.
- Preserving complete byte-exact inbound WebSocket frames, including framing
  whitespace; raw event bodies already avoid a JSON value round trip.
- Removing duplicate derived assistant/reasoning delta events or otherwise
  reducing event volume; first establish which representation the ATIF adapter
  should consume.
