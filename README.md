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
just eval           # fresh full model-driven Terminal-Bench suite
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
the same WebSocket continuation chain. A dedicated socket pump services API
keepalives while the response consumer is waiting on local tools.

The adapter removes `OPENAI_API_KEY` from Harbor's per-exec environment before
launching Docker. It uploads a mode-`0400` transient file for the agent user,
reads and deletes it inside the container, and scopes the value only to the
Rust process. The key is therefore absent from host process arguments, `tee`,
verifier commands, retained logs, and model-generated shell environments.

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
and tasks. The current development slice contains thirty-four public shell/code
tasks. The latest completed full-suite gate passed the first 33/33 with zero
exceptions or retries, including independent samples of Build pMARS, Prove
Plus Comm, and Custom Memory Heap Crash. QEMU Startup is independently green at
low effort against its canonical telnet/kernel verifier and is awaiting the
34-task full-suite gate. Browser automation, computer-use, GUI interaction, and
image/video perception are outside this milestone. Downloaded tasks and
canonical verifier assertions remain unchanged.

Candidate admission is evidence-driven. Cold task preparation is measured
before model work, and a task that repeatedly requires benchmark-specific
prompt hints is deferred rather than adding that hint to the shared harness.
New verifier dependencies are appended as isolated image layers so prior apt
and Python layers remain reusable. The pinned RDFLib layer used by the active
SPARQL task was paid once during preparation and adds no warm-trial
installation. The active PyPI-server task needs only an exact cached verifier
command shape and adds no image dependency. Distribution Search uses a pinned
final NumPy layer paid during preparation and performs no warm-trial install;
that layer is skipped on legacy task interpreters because NumPy 2.3 requires
Python 3.11 and unrelated older-base verifiers do not request it. Debian images
that package Python 3.9's `distutils` separately receive that package only when
APT exposes a real installation candidate, so uv can inspect the interpreter
without breaking newer distributions that retain obsolete package metadata.
The compatibility path was prepared across all 34 task images and regressed
against Fix Git and OpenSSL without changing canonical tests.
Largest Eigenvalue likewise uses an exact cached pip command and adds no image
dependency. Tune MJCF uses an exact cached `mujoco==3.3.5` command shape and
also adds no verifier-image dependency.
Primer3 support is retained for the deferred DNA experiments, but `dna-insert`
and `dna-assembly` are excluded from the active gate after respectively scoring
2/4 and 1/3 across unchanged low-effort samples.
The retained Raman-fitting experiment, for example, scored 0.0 in three
canonical low-effort runs; its generic units prompt increased work without
producing the required fit, so both the prompt change and task admission were
reverted.

Every trial retains `input.jsonl`, `events.jsonl`, `stderr.log`, and
`trajectory.json` under `.harness/harbor/jobs`. Harbor receives aggregate token
counts, while ATIF also records cache writes, reasoning summaries, model/tool
durations, PTC caller linkage, tool arguments, and structured observations.
