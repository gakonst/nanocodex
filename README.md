# harness

A small Rust coding-agent harness built around Harbor and the OpenAI API.
It currently runs `gpt-5.6-sol` over the Responses API WebSocket transport.
Programmatic Tool Calling (PTC) is the default orchestration profile; hosted
Multi-agent is an explicit profile for requested delegation or hard parallel
work.

```sh
just bootstrap      # install pinned host dependencies once
just prepare-evals  # build/cache native task and verifier images; no model
just run            # native low-effort PTC smoke; no Python, Docker, or Harbor
just eval           # fresh model-driven Terminal-Bench trial
just view           # inspect retained Harbor jobs
```

`just eval` performs this path:

```text
native BuildKit compile -> static Linux binary
                       -> Harbor task container
                       -> /installed-agent/harness
                       -> Rust executes tools in /app
                       -> Harbor verifier
```

The Python `BaseInstalledAgent` shim only uploads and starts the executable,
then converts its retained JSONL to ATIF. It never dispatches tool calls.
OpenAI runs the model-generated JavaScript in its hosted PTC runtime. The Rust
process executes only the nested `exec_command` calls returned by the API,
preserves their caller linkage, and sends their structured results back over
the same WebSocket continuation chain.

`--multi-agent` switches to hosted Multi-agent with direct `exec_command`
calls and live `response.inject`. The profiles are separate because the live
API currently rejects injection of PTC-nested outputs during a Multi-agent
response. Multi-agent remains opt-in and its developer prompt forbids spawning
for routine or sequential work.

For the local eval loop, Harbor builds each canonical task Dockerfile for the
Docker daemon's native architecture, then adds one content-addressed layer with
the pinned verifier dependencies. Downloaded benchmark tasks and assertion
files remain unchanged, and their canonical `test.sh` launchers still own
task-specific setup, assertion phases, CTRF output, and reward calculation.
The adapter skips only allowlisted dependency-install commands already
satisfied by the cached verifier layer; an unknown install shape fails closed.

`just prepare-evals` pays those image-build costs outside measured eval jobs by
running Harbor's install-only path with its no-op agent. When adding one task,
`just prepare-task terminal-bench/<name>` prepares only that task. Preparation
records go under `.harness/harbor/setup`; scored jobs remain under
`.harness/harbor/jobs`.

The eval YAML pins an immutable Terminal-Bench-2 dataset digest. `prepare-task`
and `eval-task` filter that dataset rather than resolving a standalone task's
moving `latest`, so a one-task run uses the same curated task revision as the
full suite.

Rust and adapter edits do not invalidate task images. `src/**` rebuilds only
the final Cargo artifact layer, which Harbor uploads during agent setup.
Task-image rebuilds occur only when a task's `environment/**`, native platform,
or the deliberately pinned dataset digest changes. Editing
`evals/pytest/Dockerfile` rebuilds the verifier overlay once per task. A warm
environment phase should therefore be container startup rather than package
installation.

## Build profiles

Local artifacts use Cargo's `dev` profile by default. Set this in `.env` for an
optimized build with full debug symbols:

```env
HARNESS_BUILD_PROFILE=profiling
```

## Eval selection

[`evals/terminal-bench-2.yaml`](evals/terminal-bench-2.yaml) selects datasets
and tasks. The current development slice contains sixteen public shell/code
tasks, all with green samples from the real model/tool loop. Browser
automation, computer-use, GUI interaction, and image/video perception are
outside this milestone. Downloaded tasks and canonical verifier assertions
remain unchanged.

Every trial retains `input.jsonl`, `events.jsonl`, `stderr.log`, and
`trajectory.json` under `.harness/harbor/jobs`. Harbor receives aggregate token
counts, while ATIF also records cache writes, reasoning summaries, model/tool
durations, PTC caller linkage, tool arguments, and structured observations.
