# Stored checkpoint fork results

Measured on 2026-07-19 on arm64 macOS 26.3.1 with Rust 1.94.1. Nanocodex was
based on `50fd121d65c2995cf4e0ff5de319cb2e8e14df42`; the stock Codex comparator
was built unmodified from `openai/codex@3e2f79727a4e8ddfc8e3acb838d496b121094b9e`.
Both live workloads used `gpt-5.6-sol`, a 600-fact deterministic first prompt,
a ten-turn chain, concurrent historical forks after turns 3, 6, and 9, and a
simultaneously advancing mainline.

## What the benchmark establishes

Nanocodex stores each completed response and treats its private response ID as
an opaque checkpoint. A historical branch opens a fresh WebSocket but sends
only its new user delta with `previous_response_id`. The root keeps its original
WebSocket and can continue independently. All descendants keep one lineage
`prompt_cache_key` while receiving unique session IDs.

Stock Codex's app-server `thread/fork` loads and truncates persisted local
rollout history. Normal OpenAI Responses requests use `store: false`, so its
forked thread sends the retained history again. Its default prompt cache key is
the new thread's session ID rather than the source lineage, making cross-fork
cache placement opportunistic.

## Live API results

Nanocodex was sampled three times. Stock Codex was sampled four times for
latency and three times after adding token-usage collection to the harness.
These are live service observations, not a controlled model-runtime benchmark:
stock Codex also carries a substantially larger system/tool prefix, so absolute
latency and input-token totals are directional rather than apples-to-apples.

| Measurement | Nanocodex stored checkpoint | Stock Codex local-history fork |
| --- | ---: | ---: |
| Historical branch median latency | 1.224 s (9 branches) | 5.082 s (12 branches) |
| Ten-turn chain median-of-medians | 0.966 s | 1.316 s |
| Branch input tokens per request | 8,536 / 8,611 / 8,686 | 24,698–26,946 |
| Branch cached-input ratio | 99.6% (9/9 branches) | 19.3% aggregate (9 branches) |
| Branches with zero cached input | 0/9 | 5/9 |
| Three branch request payloads | 2,175 bytes | full logical replay required |
| Equivalent Nanocodex replay payload | 84,612 bytes | n/a |
| Nanocodex payload reduction | 97.4% | n/a |

The Nanocodex mainline-plus-three-forks wall times were 2.309 s, 3.202 s, and
2.051 s (median 2.309 s). That workload includes two sequential mainline
continuations. Stock Codex's three concurrent `thread/fork` RPCs took 304 ms,
277 ms, 313 ms, and 336 ms (median 308 ms) before any branch model turn.

The raw Nanocodex trials consistently reported 8,502 / 8,577 / 8,652 cached
tokens for the turn-3 / turn-6 / turn-9 branches. The three instrumented stock
trials reported cached branch tokens of `[0, 0, 0]`, `[0, 24,320, 0]`, and
`[9,984, 9,984, 0]`. This variance is consistent with cache routing under a new
per-fork cache key; it is not a durability failure.

All three Nanocodex trials deleted all 15 stored API responses after completion.
The stock harness deletes its root and forked app-server threads in a `finally`
block.

## Consumer smoke proofs

The public `fork-conversations` example was run live after configuring deferred
timeout and concurrency-limit Tower layers. It built ten checkpoints, continued
the root, and concurrently forked turns 3, 6, and 9 plus the latest turn 10.
Every branch opened a distinct WebSocket/session. Later facts were absent from
earlier branches, a branch-only queue override remained absent from the root,
and the root-only Helsinki decision remained absent from every fork.

The initial `subagents` smoke invoked `fork_agent` and `spawn_agent`
concurrently from one Code Mode cell. The forked child recovered inherited
private context while the independent child did not, proving that a Code Mode
tool can request a fork while its own model turn is awaiting that tool result.

The example was then promoted to a topology-free orchestration consumer. Rust
exposed only generic `spawn_agent({ role, task })` and
`fork_agent({ role, task })` tools and supplied a high-level decision goal. In
the live run, Code Mode independently chose a context fork and a clean-slate
reviewer concurrently. The context fork then recursively launched lifecycle,
branching, and snapshot specialists before synthesizing their reports; none of
that fan-out/fan-in structure was encoded in the Rust host.

The context fork's first model request reported 6,878 cached tokens out of
6,951 input tokens, while the clean agent's first request reported zero cached
tokens out of 4,063. The complete autonomous run took 79.2 seconds at the root
and converged on cancellation plus safe branch cleanup as the next slice. It
also exposed the primary production hazard: recursive fan-out consumed roughly
414,000 child input tokens across five workers, so depth, child-count, token,
deadline, and cancellation budgets are required before enabling unrestricted
write-capable orchestration.

The original demo late-bound one strong root handle inside the shared tool
collection. It was subsequently replaced with `tools_factory` and a weak
`AgentHandle` instantiated for every driver. A deterministic three-level
regression now proves that a child tool forks from the child's response ID—not
the root's—and that retaining the weak handle does not keep a driver alive.
The replacement also passed a live recursive Code Mode smoke: the root created
an `orchestration_recommender` fork, that child invoked its own `fork_agent` to
create `second_opinion`, and all three turns completed in about ten seconds.
The grandchild's first request reported 4,389 cached tokens out of 4,448 input
tokens on its own WebSocket/session.

After generalizing that capability from a fork-only handle to `AgentHandle`, a
second live Code Mode smoke invoked `spawn_agent` and `fork_agent` concurrently
from one cell without putting credentials in either tool. The clean child used
a new session and cache lineage, reporting 0 / 4,273 cached input tokens; the
contextual child used a distinct session with the parent's lineage and reported
4,389 / 4,446 cached input tokens. Both returned attributed reports and the root
synthesized them in 6.8 seconds. This validates the intended split: `spawn()`
privately reuses builder configuration but not conversation state, while
`fork()` also inherits the invoking driver's completed checkpoint.

## Runtime-synthesized orchestration

The most interesting result is not merely that the root can call subagents. The
root model can write an executable Code Mode program at runtime, and that
program becomes the temporary control plane for the task. Ordinary JavaScript
constructs—arrays, loops, conditionals, `Promise.all`, and values returned by
earlier workers—determine the graph. Rust exposes capabilities rather than a
workflow definition:

```text
user goal
   └─ root model writes an async JavaScript program
      ├─ tools.spawn_agent({ role, task })
      └─ tools.fork_agent({ role, task })
         └─ program loops, fans out, reduces actual results, and decides when to stop
```

This is **runtime-synthesized orchestration**: the application does not encode a
DAG, scheduler, reducer type, or fixed worker count. The model compiles the
current goal into an ephemeral orchestration program; Code Mode executes it;
the root then continues with the program's structured result. The observed
graph should remain a consequence of real child calls, never a required input
to the SDK.

A live map/reduce trial prompted the root to build four child promises in a
loop, await them concurrently, pass their actual JSON reports to a reducer, and
then pass the reducer's actual report to a clean red-team worker:

```text
root
├─ Promise.all map
│  ├─ spawn independent_skeptic    session …-2
│  ├─ fork  lifecycle_specialist   session …-3
│  ├─ fork  ux_specialist          session …-4
│  └─ fork  durability_specialist  session …-5
├─ fork  reducer(map reports)      session …-6
└─ spawn red_team(reducer report)  session …-7
```

All four map `run.started` events arrived before any map completion. The reducer
started only after all four completed, and the red-team worker started only
after the reducer completed. The three contextual map workers each reused
4,360 cached tokens out of 4,424 / 4,424 / 4,426 input tokens on distinct
WebSockets and sessions. The reducer reused 4,360 / 5,110 tokens. The two clean
workers began on fresh lineages; their later internal calls could still hit
their own newly established caches. The final root answer ranked cancellation
and safe branch cleanup first after incorporating every worker and the red-team
objection. The lifecycle trace retained the emergent graph and cache evidence;
this run did not retain the root's byte-exact generated JavaScript source.

### Novelty calibration and closest prior art

The ingredients are not individually new:

- [CodeAct](https://arxiv.org/abs/2402.01030) established executable code as a
  compositional action space for language-model agents.
- [OpenAI Agents SDK orchestration](https://openai.github.io/openai-agents-python/multi_agent/)
  documents agents-as-tools, LLM-directed delegation, and host-written parallel
  orchestration with `asyncio.gather`.
- [LangGraph's `Send` API](https://langchain-ai.github.io/langgraph/agents/tools/)
  dynamically creates orchestrator-worker map/reduce workers inside a graph
  runtime.
- [AutoGen GraphFlow](https://microsoft.github.io/autogen/dev/user-guide/agentchat-user-guide/graph-flow.html)
  supports parallel, conditional, and looping flows using an explicitly built
  directed graph.
- [Recursive Agent Harnesses](https://arxiv.org/abs/2606.13643) is the closest
  conceptual match found: a parent generates and runs executable scripts that
  spawn subagent harnesses in parallel and aggregate their work.

The defensible claim is therefore not “the first system to generate code that
calls agents.” The differentiated combination is model-written orchestration
code plus clean-versus-contextual child primitives, server-retained response
checkpoints, stable cross-fork cache lineage, independent WebSockets and tool
runtimes, and recursive handles bound to the agent that actually invokes them.
That composition is rare and materially different from requiring callers to
predeclare a graph.

### Product implications

- Keep `spawn()` and `fork()` as capability primitives. Do not promote a DAG API
  into the library merely to visualize a graph that Code Mode can synthesize.
- Derive an observed execution graph from child lifecycle calls, parent session
  IDs, checkpoint lineage, timing, and results.
- Make generated-source capture an explicit host-controlled debug artifact.
  Contractual telemetry should expose a program hash and structural execution
  metadata by default rather than leaking task prompts or embedded worker
  reports.
- Add recursive cancellation and hard budgets for depth, concurrent children,
  total children, tokens, wall time, and tool permissions before treating
  unrestricted recursive orchestration as production-safe.
- Preserve the program, observed graph, and worker results together when a host
  opts into replay/debugging. The generated program describes intent; the
  observed graph records what actually ran.

Finally, the Ratatui `/btw` flow was exercised through a real PTY. After the
main thread stored `COBALT-42`, `/btw` opened a side-by-side branch which
returned that inherited value. Tab switched input back to the main pane; the
main thread reported `UNKNOWN` when asked about the branch's activity, proving
the branch continuation did not flow back into the root. `/close` dismissed the
branch without stopping the root session.

## Local history benchmark

Criterion compares a branch followed by one append using Nanocodex's immutable
committed segments against `Arc<Vec<ResponseItem>>` copy-on-write. Median times:

| Retained items | Immutable segments | `Arc<Vec>` COW | Speedup |
| ---: | ---: | ---: | ---: |
| 100 | 333.5 ns | 9.23 us | 27.7x |
| 1,000 | 333.7 ns | 89.72 us | 268.9x |
| 10,000 | 332.3 ns | 1.088 ms | 3,274x |

The segmented fork-and-append remains constant-time as retained history grows;
the copy-on-write baseline scales linearly because the first branch append
copies the shared vector.

The active-boundary path additionally seals the current delta, snapshots it,
and appends one item to the parent. Its median times on the same machine were:

| Retained items | Immutable boundary | `Arc<Vec>` COW boundary | Speedup |
| ---: | ---: | ---: | ---: |
| 100 | 531.6 ns | 13.08 us | 24.6x |
| 1,000 | 823.9 ns | 204.95 us | 248.8x |
| 10,000 | 987.1 ns | 1.391 ms | 1,409x |

Incremental iteration over only the last item measured 33.6 ns, 29.9 ns, and
25.8 ns for histories containing 100, 1,000, and 10,000 sealed segments. This
guards the wire path against accidentally walking the whole retained history
when constructing `previous_response_id + delta` requests.

The deterministic WebSocket suite separately verifies the wire contract: a
latest fork includes an active prompt or the complete tool-result-plus-steer
delta, retries with complete local history when the stored response is absent,
and remains distinct from `fork_from(&completed_turn)`, which stays pinned to
the exact historical checkpoint.

## Reproduce

From a directory containing `OPENAI_API_KEY` in `.env`:

```sh
cargo run --manifest-path /Users/georgios/github/gakonst/nanocodex-stored-forks/examples/Cargo.toml \
  --release --bin fork-checkpoint-bench
```

For the public library API demo:

```sh
cargo run --manifest-path examples/Cargo.toml --bin fork-conversations
```

For local history scaling:

```sh
cargo bench -p nanocodex-core --bench fork_history -- --noplot
```

For stock Codex, build `codex-app-server` from the recorded comparator commit,
then run:

```sh
python3 benchmarks/stock_codex_fork_bench.py \
  --app-server /path/to/codex-app-server \
  --source-commit 3e2f79727a \
  --cwd /Users/georgios/github/gakonst/nanocodex-stored-forks
```

The stock harness emits machine-readable JSON for each run.
