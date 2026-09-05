# nanocodex-eval

`nanocodex-eval` owns Nanocodex's VM-isolated execution boundary and durable
profile ledger. `Evaluation::add` defines durable work by pre-materializing one
SQLite row per task/treatment/repetition. Opening or running a benchmark never
derives new work from TOML. Workers atomically claim one row and fence its
terminal outcome.

The ledger has no queue, lease, retry state, or host-saturation policy. An
embedding application or the `/benchmark` agent decides only how many
one-row worker processes to launch.

Every benchmark attempt runs tools and verification in a microVM. Native host
execution exists only inside focused crate tests. Harbor JSONL and ATIF are
output formats, not alternate runners.

## Durable API

```rust,no_run
use nanocodex_eval::{Evaluation, EvaluationClaim};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let evaluation = Evaluation::open(
    "nanocodex.toml",
    Some("local-smoke"),
    ".nanocodex/evals",
)?;
match evaluation.claim_next()? {
    EvaluationClaim::Run(claim) => {
        // Execute `claim.task()` with `claim.treatment()` and retain output in
        // `claim.output_directory()`, then record one terminal outcome.
        let evidence = claim.output_directory().to_path_buf();
        claim.succeed(&evidence)?;
    }
    EvaluationClaim::Busy(_) | EvaluationClaim::Complete => {}
}
# Ok(())
# }
```

A local claim holds an OS ownership lock for the worker object's lifetime.
Process death releases that lock and the next ledger observation makes the row
unclaimed again. A remote benchmark retains native child-agent handles in its
service cgroup, and each child owns exactly one foreground one-row worker. On a
terminal child result the benchmark closes that handle and reports one
idempotent exit edge, which changes only an otherwise-running row. Systemd
terminates the complete process group before a benchmark restart reports all
interrupted remote workers once. There are no PID identities, supervisors, or
periodic heartbeats. Coordinator claims use the row's durable claim ID, so a
restarted coordinator reacquires retained running rows before accepting worker
outcomes. Raw SQLite details and artifact paths remain private.

## Profiles

The repository manifest is `nanocodex.toml`:

```toml
default = "local-smoke"

[profiles.local-smoke]
tasks = ["tasks/write-greeting"]
trials = 3
model = ["sol"]
thinking = ["low"]
```

Profiles are optional recipes for `eval add --recipe`. Adding the recipe loads
and fingerprints its task packages, then stores the task selector, canonical
root, content digest, treatment, and every desired repetition in SQLite. After
that, SQLite—not the TOML file—is authoritative. `Evaluation::open` only opens
the newest generation already present in SQLite.

```text
profile -> task/treatment families -> k=1..N pre-materialized rows
                                      \
                                       -> workers atomically claim one row
```

Rows have exactly four durable states: `unclaimed`, `running`, `success`, and
`failed`. `success` means the verifier returned a passing score; `failed` means
the verifier returned a failing score. Preparation and execution both happen
while the row is `running`. Infrastructure failures and lost worker owners are
retained in append-only attempt history, then release the row to `unclaimed`
for another attempt. A claim ID fences late writes.
Local execution uses an ownership lock; remote execution combines
the coordinator's recovered row ownership with the benchmarker's direct child
process observation.

## External harnesses

An omitted harness means the built-in Nanocodex library runner. External
harnesses are ordinary independent coordinates:

```toml
[harness.codex]
command = "harness/codex"
guest_command = "/usr/local/bin/codex"
version = "0.145.0"
arguments = [
  "exec", "--json", "--ephemeral",
  "--dangerously-bypass-approvals-and-sandbox",
  "--skip-git-repo-check",
  "--model", "{model}",
  "--config", "model_reasoning_effort=\"{thinking}\"",
  "--config", "openai_base_url=\"{api_base_url}\"",
  "--", "{prompt}",
]
environment = { CODEX_HOME = "/run/nanocodex-harness-home" }
# Optional defaults shown explicitly; other CLIs can choose their own paths.
home = "/run/nanocodex-harness-home"
auth_file = "/run/nanocodex-harness-home/auth.json"
api_key_environment = "OPENAI_API_KEY"

[profiles.compare]
tasks = ["tasks/write-greeting"]
trials = 5
harness = ["nanocodex", "codex"]
model = ["sol", "luna"]
thinking = ["medium", "high"]
```

Running-task preparation installs the configured command at `guest_command`
inside the immutable task image. Every coordinate receives a fresh writable
overlay and routes the harness's OpenAI-compatible traffic through the same
capture proxy. The command path,
`arguments`, `environment`, credential paths, API-key environment name, and
`api_upstream` are profile data; argument
templates support `{prompt}`, `{model}`, `{thinking}`, `{web_search}`, and
`{api_base_url}`. Environment values additionally support `{api_base_url}`,
`{harness_home}`, and `{auth_file}`. Authentication is exposed at the neutral
`NANOCODEX_HARNESS_AUTH_FILE` and `NANOCODEX_HARNESS_HOME` paths, so an
agent-specific home variable such as `CODEX_HOME` is only configuration.

An external binary must emit the harness JSONL contract on stdout. The current
contract is the small event vocabulary emitted by `codex exec --json`
(`thread.started`, item events, and one terminal turn event), so Codex works
directly and another CLI can use a thin output wrapper. Rust contains no Codex
binary path, command line, or execution mode.

There is no matched-pair runner or comparison state machine. Each harness emits
its own result JSON, raw JSONL, trajectory, verifier evidence, and ledger row.
Differential reports are ordinary offline queries joining matching coordinate
dimensions.

Prepared task images and memory observations remain content-addressed cache
inputs. Each arm still receives a fresh writable overlay, so filesystem and
process state cannot leak between profile repetitions.

## CLI

```sh
# Materialize a recipe, or add explicit task/treatment rows.
nanocodex eval add local-smoke --recipe local-smoke
nanocodex eval add compare --task tasks/write-greeting \
  --harness codex --model luna --thinking high --trials 5

# Inspect exact SQLite counts. This never adds work.
nanocodex eval status local-smoke --json

# Atomically claim and execute the next pre-materialized row.
nanocodex eval run local-smoke

# Optionally restrict a diagnostic run to an exact configured family.
nanocodex eval run compare --task tasks/write-greeting --harness codex \
  --model luna --thinking high

# Coordinate workers through one SQLite owner. Remote benchmark hosts reach
# this loopback listener through an SSH reverse tunnel.
nanocodex eval coordinator compare --port 8789
nanocodex eval benchmark compare --coordinator http://127.0.0.1:8789

# Let an agent inspect the ledger and choose process fan-out.
nanocodex eval benchmark local-smoke
# Equivalent interactive workflow:
nanocodex
# then enter: /benchmark local-smoke
```

`--state-dir` overrides the default `~/.nanocodex/evals`. `eval add --new`
starts an independent generation; otherwise add idempotently extends the newest
generation. SQLite chooses the next unclaimed row. `eval run --task` and
treatment selectors remain optional diagnostic restrictions, not the normal
worker path.

Remote workers send only retained evaluation evidence: JSON/JSONL trajectories,
events, API exchanges and summaries, plus verifier reward/stdout/stderr. VM
disks, workspaces, task fixtures, caches, and runtime logs remain host-local and
failed writable roots are disposable. Evidence is streamed as a zstd-compressed
tar, validated against the same allowlist by the coordinator, extracted into a
staging directory, and atomically renamed before the fenced terminal update.
Each supervised child has a unique worker ID. Normal workers report their own
terminal outcome; after any process exit the benchmarker sends one idempotent
exit observation, which is a no-op if the row is already terminal and otherwise
changes its running row permanently to failed. A small locked local marker
survives benchmarker termination so a systemd restart can reconcile children
that died with the previous benchmark process.

VM-backed evals consume a prepared host installation. The matching static
`nanocodex-vm-guest` must be installed beside the `nanocodex` executable; VM
state is cached under `~/.cache/nanocodex/vm` (or
`$NANOCODEX_HOME/cache/vm`). Runtime execution never builds, signs, discovers,
or repairs that substrate. Source checkouts can produce the complete local
installation with `just build-eval-host`; an incomplete installation fails
before a row can enter evaluator preparation.
