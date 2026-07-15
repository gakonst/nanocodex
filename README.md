# harness

A Harbor-first Rust coding-agent harness optimized for the current OpenAI API.
The active implementation is the Phase 0 JSONL CLI loop described in
[`PLAN.md`](PLAN.md).

```sh
just bootstrap
just run
```

The development loop has three speeds:

| Command | What it proves | Expected Phase 0 result |
| --- | --- | --- |
| `just run` | Rust CLI and JSONL transport only; no Harbor or Docker | Three JSONL lifecycle events in well under a second |
| `just harbor-probe` | Real Harbor task container and external-agent adapter | Job completes without errors; Harbor shows zero *scored* trials because the verifier is skipped |
| `just harbor-eval` | Full Terminal-Bench task, adapter, and pre-baked verifier | One completed trial, zero infrastructure errors, reward `0` |

Phase 0 has no model and makes no task changes, so reward `0`, zero tokens, and
zero cost are expected. At this stage, `just harbor-eval` proves the complete
evaluation plumbing works; it does not yet prove that the harness can solve the
task. Phase 1 replaces that stub with the OpenAI tool loop.

Bootstrap downloads the exactly pinned Terminal-Bench `fix-git` canary. To
inspect the most recent evals in Harbor's web viewer, run:

```sh
just harbor-view
```

Use `just harbor-view probe` to inspect probe jobs instead.

The local eval image reconstructs the pinned canary environment on the host's
native container architecture and bakes the canonical verifier's pinned Python
dependencies once, while retaining its byte-identical `test_outputs.py`. This
avoids CPU emulation and installing `curl`, `uv`, and pytest inside every
trial. Run
`just harbor-eval-canonical` to check against the untouched downloaded task;
that slower control includes the benchmark author's dependency bootstrap.

Set `HARBOR_CANARY_TASK` in `.env` only when deliberately overriding the task
used by `harbor-probe` and `harbor-eval-canonical`. The fast `harbor-eval`
stays tied to the pinned `fix-git` canary because its verifier image is built
specifically for that task.

The Harbor adapter is an external agent: it starts `cargo run` on the host and
leaves the Terminal-Bench task and verifier inside Harbor's environment.

Phase 0 deliberately makes no model or tool calls, so `just harbor-eval`
records reward zero. Each trial retains `input.jsonl`, `events.jsonl`,
`stderr.log`, and a validated `trajectory.json` under `.harness/harbor/jobs`.
