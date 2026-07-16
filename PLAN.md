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

Status: in progress. All seventeen active public tasks have green low-effort PTC
samples. The table records their last warm samples. `fix-git`, OpenSSL, and
both polyglot tasks, large-scale text editing, log summarization, and binary
secret extraction use the current `openai-coding-v11` prompt. Both database
recovery tasks plus Nginx use v10.
The vulnerability task, multibranch task, and `git-leak-recovery` use v9; the
other task digests have v7 samples:

| task | reward | trial | Rust | generated turns | tool wall | rounds/tools | input/cache/output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `fix-git` | 1.0 | 45.08s | 40.71s | 39.81s | 0.35s | 8/7 | 31,259/14,108/1,600 |
| `openssl-selfsigned-cert` | 1.0 | 41.87s | 37.98s | 37.45s | 0.35s | 3/2 | 7,403/4,306/1,952 |
| `cancel-async-tasks` | 1.0 | 68.43s | 50.07s | 49.51s | 0.88s | 5/4 | 10,951/6,127/2,735 |
| `headless-terminal` | 1.0 | 84.74s | 67.65s | 66.89s | 2.27s | 5/4 | 13,946/8,314/3,897 |
| `regex-log` | 1.0 | 35.04s | 31.17s | 30.24s | 0.07s | 2/1 | 4,163/1,163/1,963 |
| `build-cython-ext` | 1.0 | 160.79s | 154.45s | 153.80s | 67.27s | 21/20 | 240,833/43,306/4,797 |
| `fix-code-vulnerability` | 1.0 | 46.96s | 42.98s | 42.23s | 1.32s | 5/4 | 33,408/5,096/1,033 |
| `git-multibranch` | 1.0 | 100.90s | 87.70s | 87.12s | 1.46s | 5/4 | 17,824/8,758/2,949 |
| `git-leak-recovery` | 1.0 | 31.69s | 27.12s | 26.02s | 0.57s | 3/2 | 7,258/2,888/1,477 |
| `db-wal-recovery` | 1.0 | 66.67s | 62.76s | 62.08s | 0.23s | 7/6 | 20,344/10,232/2,637 |
| `sqlite-db-truncate` | 1.0 | 38.65s | 34.78s | 34.15s | 0.33s | 5/4 | 12,410/7,894/2,321 |
| `nginx-request-logging` | 1.0 | 50.17s | 44.30s | 43.51s | 5.93s | 4/3 | 11,003/6,580/1,914 |
| `polyglot-c-py` | 1.0 | 51.87s | 47.77s | 46.80s | 0.26s | 3/2 | 7,710/3,984/2,347 |
| `polyglot-rust-c` | 1.0 | 64.73s | 60.82s | 60.21s | 0.55s | 2/1 | 4,947/1,346/3,032 |
| `large-scale-text-editing` | 1.0 | 120.37s | 93.35s | 92.71s | 36.53s | 4/3 | 11,935/7,700/2,796 |
| `log-summary-date-ranges` | 1.0 | 31.43s | 22.86s | 22.12s | 0.32s | 3/2 | 11,288/6,354/1,101 |
| `vulnerable-secret` | 1.0 | 32.30s | 28.45s | 27.63s | 0.25s | 6/5 | 37,265/16,536/1,021 |

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

`vulnerable-secret` passed on its first low-effort v11 attempt, adding a
stripped AArch64 binary-analysis path without a new tool surface. The model
inspected the ELF headers, sections, disassembly, and encoded data across five
local phases, decoded and behaviorally checked the XOR-obfuscated flag, and
wrote the exact requested result. One-time native image preparation took 33.19
seconds outside scoring. The warm trial used 1.19 seconds for environment
startup, 0.45 seconds for agent setup, 28.57 seconds for agent execution, and
0.80 seconds for verification. Rust spent 27.63 of 28.45 seconds in API turns
and 0.25 seconds in local tools.

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
