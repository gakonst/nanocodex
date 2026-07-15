# harness

A small Rust coding-agent harness built around Harbor and the OpenAI API.
It currently runs `gpt-5.6-sol` over the Responses API WebSocket transport and
exposes shell execution exclusively through Programmatic Tool Calling (PTC).

```sh
just bootstrap  # install pinned dependencies once
just run        # native low-effort PTC smoke; no Python, Docker, or Harbor
just eval       # fresh model-driven Terminal-Bench trial
just view       # inspect retained Harbor jobs
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
the same WebSocket. `exec_command` is not available as a direct function call.

For the local `fix-git` loop, Harbor builds a content-addressed native task
image with the pinned verifier dependencies already installed. The downloaded
benchmark task and its assertion file remain unchanged; only its dependency-
installing shell launcher is replaced by a direct `pytest` invocation.

## Build profiles

Local artifacts use Cargo's `dev` profile by default. Set this in `.env` for an
optimized build with full debug symbols:

```env
HARNESS_BUILD_PROFILE=profiling
```

## Eval selection

[`evals/terminal-bench-2.yaml`](evals/terminal-bench-2.yaml) selects datasets
and tasks. The current `fix-git` eval is solved by the real model/tool loop; its
downloaded task and canonical verifier remain unchanged. The first PTC-only
run recovered the lost commit from Git's reflog, merged it, and earned reward
`1.0`. A second post-refactor regression also earned `1.0` in 35 seconds of
Harbor runtime.

Every trial retains `input.jsonl`, `events.jsonl`, `stderr.log`, and
`trajectory.json` under `.harness/harbor/jobs`. Harbor receives aggregate token
counts, while ATIF also records cache writes, reasoning summaries, model/tool
durations, PTC caller linkage, tool arguments, and structured observations.
