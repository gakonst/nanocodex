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
keepalives while the response consumer is waiting on local tools. If the
server normally closes that idle socket before the next `response.create`,
Rust reconnects once and resends the same stored continuation; connection
attempts, reconnects, and connection wall time remain visible in JSONL and
Harbor/ATIF.

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
One canonical TeX verifier requests an exact
`apt install -y --reinstall texlive-latex-base` before asserting its output.
For that command only, the verifier image records the installed package files
and generated TeX state. A small `apt` wrapper skips the reinstall only when
the package is still installed and both manifests match byte-for-byte;
otherwise it delegates the original arguments to `/usr/bin/apt` unchanged.

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
and tasks. The configured development slice contains thirty-six public
shell/code tasks. The first 35-task gate after admitting Circuit Fib/Sqrt and
Build POV-Ray completed every trial without an exception or retry in 16
minutes 41.92 seconds and scored 34/35. Its only miss was a verifier-cache
regression: POV-Ray's scientific stack had replaced the Cython task's required
NumPy 2.3.0 with NumPy 2.3.1 before the agent started. The stack is now
verifier-isolated, and unchanged focused Cython, POV-Ray, and Distribution
Search runs pass 11/11, 3/3, and 4/4 canonical checks. The corrected 35-task
gate then passed 35/35 and all 137 assertions with zero exception or retry in
18 minutes 32.29 seconds of Harbor wall; the complete `just eval` command took
18 minutes 35.66 seconds. Overfull HBox is green and its guarded TeX verifier
cache stayed green in the subsequent 36-task trial. That trial completed
without a Harbor exception or retry in 15 minutes 21.72 seconds, scoring 33/36;
unchanged focused retries recovered the Compressor and Cython misses. Tune
MJCF missed its speed threshold both in the gate and an unchanged retry, so it
joins Core Wars as a retained variance experiment excluded from the stable
slice rather than receiving a benchmark-specific hint. CompCert 3.13.1 is the
first green admission in the next three-task batch; the next full gate follows
two more green admissions unless a shared behavior change triggers it earlier.
Browser automation, computer-use, GUI interaction, and image/video perception
are outside this milestone. Downloaded tasks and canonical verifier assertions
remain unchanged.

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
POV-Ray's Pillow/NumPy/scikit-image verifier stack is cached under
`/opt/harness-verifier/pov` instead of the system interpreter. The adapter adds
that path only for the exact canonical POV-Ray `uvx` command, so verifier-only
versions cannot mutate the agent's task environment.
Largest Eigenvalue likewise uses an exact cached pip command and adds no image
dependency. The retained Tune MJCF experiment uses an exact cached
`mujoco==3.3.5` command shape and adds no verifier-image dependency.
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
