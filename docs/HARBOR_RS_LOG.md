# Harbor Rust runner log

Status: research and measurement. No implementation crate has been promoted.

This document tracks a possible Rust evaluation runner for Nanocodex. The
working name `harbor-rs` is descriptive, not a commitment to API or package
names. Its first consumer is the pinned Terminal-Bench 2.1 evaluation; it is
not part of the Nanocodex SDK and must not introduce scheduler or container
concepts into the agent libraries.

## Current objective

Build the smallest runner that can execute the canonical Terminal-Bench task
matrix faster and more reliably than the current Python Harbor path while
preserving task, environment, verifier, timeout, resource, trajectory, ATIF,
and leaderboard semantics.

The initial slice is deliberately narrow:

- local Docker on one host;
- one prebuilt Nanocodex InstalledAgent artifact;
- the pinned Terminal-Bench 2.1 dataset;
- bounded parallel trials with crash-safe resume;
- byte-complete stdout, stderr, JSONL, verifier, and result retention; and
- a Harbor-compatible export boundary where leaderboard tooling requires it.

Hosted environments, arbitrary agents, a generic workflow DAG, a daemon, and
an SDK-level scheduler are deferred until this slice demonstrates a measured
benefit.

## Current design

The runner should be a work-conserving bounded queue, not a workflow engine.
One owner coordinates job state. Each active trial has one supervisor and one
result publisher.

```text
immutable job spec
      |
      v
duration-aware pending queue -----> bounded worker permits
                                           |
                                           v
                                   isolated trial supervisor
                                     | agent process group
                                     | stdout/stderr capture
                                     | verifier process
                                     | cancellation deadline
                                           |
                                           v
                                  staged immutable trial result
                                           |
                                           v
                                single atomic job-state publisher
```

### Ownership and race-freedom

- A trial supervisor exclusively owns its container, processes, capture files,
  timeout, and terminal transition.
- Agent stdout and stderr are captured by the host process. A container bind
  mount is artifact storage, never the synchronization or completion signal.
- A stream is not published as complete until EOF and child exit have both
  been observed. JSONL parsing reads the host-owned captured bytes.
- Trial state moves monotonically through `pending`, `starting`, `running`,
  `verifying`, and one terminal state. A single owner publishes transitions.
- Result files are assembled under a sibling temporary name on the same
  filesystem, flushed, and atomically renamed. Readers ignore temporary files.
- Cancellation is a terminal protocol: stop accepting work, signal the process
  group, wait a bounded grace period, kill descendants, collect final bytes,
  clean the container, and publish exactly one result.
- Resume reconciles retained terminal results and live container identities
  before requeuing anything. It must not infer completion from directory
  existence or a stale lock file.

### Parallel scheduling

- Global concurrency is explicit and bounded.
- The queue should schedule predicted long trials early so the job does not end
  with one serial long tail. Historical duration is a hint, never correctness
  state.
- Per-resource permits may be introduced only from measured contention, for
  example separate CPU-heavy, memory-heavy, and VM/emulator limits.
- Retries are new attempts with explicit lineage. They never overwrite the
  failed attempt and never silently change the reported denominator.
- A fresh trial gets a fresh workspace and container. Build layers, task
  images, verifier images, and the InstalledAgent artifact may be reused.

### Compatibility boundary

The canonical task remains authoritative. The runner may not patch task setup,
instructions, timeouts, resources, or verifiers to improve a score. A result
exporter can derive Harbor job JSON and ATIF after the immutable trial record
exists; those formats do not define the internal scheduler.

The first compatibility audit must identify which behavior is required by:

1. Terminal-Bench task definitions and verifiers;
2. leaderboard admission and upload;
3. retained trajectory inspection; and
4. Harbor conveniences that Nanocodex does not use.

Only the first three are mandatory.

## Success criteria

Correctness gates:

- no partial stream can be mistaken for a completed stream;
- exactly one terminal result per accepted attempt;
- crash/restart cannot duplicate a verifier or lose a completed result;
- cancellation removes trial processes and descendants without touching
  unrelated containers;
- a fixed synthetic corpus survives randomized cancellation and restart with
  identical final accounting;
- canonical task and verifier inputs match the current Harbor run byte for
  byte where the upstream formats permit that comparison; and
- exported result counts, rewards, exceptions, token usage, and ATIF reconcile
  with the runner's immutable trial records.

Performance gates are comparative, not aspirational constants:

- measure cold bootstrap separately from warm trial execution;
- compare concurrency at 1, 2, 4, 6, 8, and the first contention point;
- report trials/hour, CPU, memory, Docker time, verifier time, scheduler time,
  cleanup time, and tail idle time;
- demonstrate better warm throughput or lower tail time on the same task order,
  agent artifact, model, effort, and verifier definitions; and
- keep runner overhead below the natural measurement noise of model and tool
  work on representative retained traces.

The first end-to-end comparison should use cheap deterministic fixtures. A
model-driven Terminal-Bench run is a release gate, not the development loop.

## Planned slices

1. **Compatibility inventory.** Record the exact Harbor inputs, outputs, task
   lifecycle, result schema, ATIF derivation, and upload requirements used by
   the pinned run.
2. **Deterministic trial fixture.** Run a local container, capture ordered
   streams, invoke a verifier, and publish one immutable result.
3. **Fault-injection persistence.** Kill the runner during every lifecycle
   transition and prove deterministic resume and cleanup.
4. **Bounded scheduler.** Add work-conserving parallelism, duration-aware
   ordering, cancellation, and per-stage telemetry.
5. **Terminal-Bench adapter.** Consume the pinned task definitions without
   modifying them and reproduce a focused Harbor result.
6. **Compatibility export.** Derive ATIF and any required Harbor/leaderboard
   artifacts from retained native records.
7. **Comparative benchmark.** Run identical warm fixtures through Python Harbor
   and the Rust runner, then decide whether the rewrite earns promotion.

## Open questions

- What exact Harbor artifact and upload fields are checked for Terminal-Bench
  2.1 leaderboard admission?
- Can the runner consume Terminal-Bench task packages directly, or should a
  pinned conversion step produce a smaller immutable manifest?
- Which Docker operations dominate warm orchestration time after image setup?
- Is a directory of atomic immutable records sufficient, or does high trial
  concurrency justify SQLite for the job index? SQLite must not become the
  owner of stream artifacts.
- How should abandoned live containers be authenticated to a job before resume
  cleans them up?
- Which result semantics from Raindrop Evals and Eve Eval are worth adopting?

## Running log

Append dated entries here. Each entry should state evidence, decisions, open
risks, and the next falsifiable experiment. Retained Harbor jobs remain the
source of truth for individual trajectories; this is a design log, not a copy
of their event streams.

### 2026-07-20 — Baseline from Terminal-Bench 2.1 attempts

Evidence:

- Run `2026-07-19__tb21-leaderboard-high-k5-r2` completed 321 of 445
  attempts over 6.93 observed hours at six-way concurrency: 46.3 trials/hour.
- The median agent duration was 185 seconds, p90 was 631 seconds, and p95 was
  1,092 seconds. Model work averaged 204 seconds and tool wall time averaged
  103 seconds. Connection plus warmup averaged approximately one second.
- Five task families accounted for roughly 28% of aggregate agent time. The
  existing task order therefore leaves meaningful long-tail scheduling room,
  while Rust scheduler micro-optimization cannot materially shorten a normal
  model turn.
- The r2 runner stopped after a host reader observed a partially propagated
  bind-mounted JSONL file. Guest-side temporary-file rename did not fix the
  Docker Desktop visibility race; r3 reproduced it after 139 attempts.
- Publishing the fully captured stream from the host fixed focused success and
  refusal smokes. The interrupted r4 run completed 29 attempts without the
  malformed-JSONL failure, which is encouraging but not a full stress proof.
- The r2 agent telemetry recorded only two WebSocket reconnects across 306
  instrumented trials. The proposed runner should not duplicate retry policy
  already owned by Nanocodex.

Decisions:

- Host process capture is the completion boundary for agent streams.
- Duration-aware ordering belongs in the first parallel scheduler experiment.
- Runner speed claims must separate model/tool time, verifier time, container
  orchestration, cold images, and avoidable tail idle time.
- The rewrite will not begin as a generic Harbor-compatible framework. It will
  prove one canonical local Terminal-Bench slice first.

Open risks:

- We have not yet audited leaderboard upload compatibility.
- The host-publication fix has focused coverage but not a completed 445-trial
  stress run.
- The current high/k=5 attempts did not enable subagents, so they say nothing
  about orchestration-aware scheduling or child usage accounting.

Next experiment:

- Build a no-model fixture with randomized stdout chunk boundaries, verifier
  delays, process-tree leaks, cancellation, and runner termination. Use it to
  compare the current Harbor adapter and a minimal single-trial Rust prototype
  before adding parallel scheduling.

### 2026-07-20 — Raindrop Evals and Eve Eval research

The compared snapshots are
[`raindrop-ai/workshop@914d74d`](https://github.com/raindrop-ai/workshop/tree/914d74dc2c5dbfc13fa19ab9eb9bae0ecd48939e)
and
[`vercel/eve@e8d2aa1`](https://github.com/vercel/eve/tree/e8d2aa13207cd3894c335dd047a9ad87fa310b65).
The systems have different scopes, so this is not a feature checklist.

Raindrop Workshop is a local trace debugger and replay loop, not a public
general-purpose benchmark scheduler. Its useful invariants are:

- OTLP traces are persisted before WebSocket notification. SQLite runs in WAL
  mode, spans have stable IDs, and repeated final-span delivery is upserted by
  span ID. Live observations have monotonic sequence IDs and cursor reads. See
  the [ingestion path](https://github.com/raindrop-ai/workshop/blob/914d74dc2c5dbfc13fa19ab9eb9bae0ecd48939e/src/server.ts#L739-L824)
  and [database code](https://github.com/raindrop-ai/workshop/blob/914d74dc2c5dbfc13fa19ab9eb9bae0ecd48939e/src/db.ts#L67-L79).
- Replay reconstructs messages, prompt, model, and context from a retained
  trace, calls the application's real endpoint, and requires that endpoint to
  wait for completion. Fire-and-forget cannot report failure reliably. See the
  [replay contract](https://github.com/raindrop-ai/workshop/blob/914d74dc2c5dbfc13fa19ab9eb9bae0ecd48939e/skills/setup-agent-replay/SKILL.md#L117-L166).
- Its public loop is failure trace, inspection, regression eval, local replay.
  Raindrop does not publish the architecture of its private eval platform, so
  no scheduling or isolation claims should be inferred from Workshop.
- Workshop's replay correlation still includes an in-memory map, polling, and
  timestamp/name fallback. Those are useful UX compromises, not completion
  primitives for this runner.

Eve Eval is a real runner for Eve agents, but it shares one live Eve HTTP server
rather than isolating Terminal-Bench containers. Its useful mechanics are:

- Evals have deterministic path-derived IDs; dataset cases fan out into stable
  zero-padded IDs. See
  [discovery](https://github.com/vercel/eve/blob/e8d2aa13207cd3894c335dd047a9ad87fa310b65/packages/eve/src/evals/runner/discover.ts).
- Scheduling is a simple bounded, work-conserving FIFO pool with default
  concurrency eight. Execution finishes out of order, while reporting is
  restored to discovery order. See
  [`run-evals.ts`](https://github.com/vercel/eve/blob/e8d2aa13207cd3894c335dd047a9ad87fa310b65/packages/eve/src/evals/runner/run-evals.ts#L13-L138).
- Reporter callbacks run on a serialized queue outside the execution permits,
  so slow output work does not consume runner capacity.
- Each eval receives a timeout `AbortSignal`; typed events from its sessions are
  retained and assertions can inspect intermediate turns. Eve emits summary,
  result JSONL, per-eval detail JSON, event NDJSON, JUnit, and optional
  Braintrust results. See
  [task execution](https://github.com/vercel/eve/blob/e8d2aa13207cd3894c335dd047a9ad87fa310b65/packages/eve/src/evals/runner/execute-task.ts#L43-L90)
  and the [runner documentation](https://eve.dev/docs/evals/running).

Eve's persistence is unsuitable for Terminal-Bench without strengthening. It
holds artifacts in memory until the complete run finishes, writes files
directly rather than staging and renaming, has no resume or retry lineage, and
lets reporter failure prevent final artifact publication. Its timeout cancels
the client operation but does not supervise a process tree or container. A
shared server also permits interference through global application state.

Decisions adopted from the comparison:

- Persist committed native state first and treat sockets, UIs, reporters, and
  uploads as recoverable observers.
- Give every job, attempt, trace event, artifact, verifier execution, export,
  and upload a stable identity. Observation uses a monotonic cursor.
- Use Eve's small bounded pool and reporter separation. Preserve deterministic
  identity and presentation order without forcing execution order.
- Store raw host-captured streams as immutable checksummed files and use
  idempotent upsert only for structured state and indexes.
- Make `replay <attempt-id>` reconstruct the exact task image, agent artifact,
  configuration, prompt, and verifier. Overrides create explicit lineage.
- Export Harbor, ATIF, JUnit, or remote-upload formats only after the native
  attempt result commits. Export failure cannot change trial status.
- Cache content-addressed images, agent binaries, manifests, and deterministic
  bootstrap layers, never trial outcomes or mutable workspaces.

Explicit rejections:

- WebSocket delivery, bind-mount visibility, directory existence, timestamps,
  polling correlation maps, and reporter success are not completion signals.
- Results may not wait in memory for the whole job to finish.
- Retries never overwrite attempts, and attempts never share mutable task
  environments.
- An aborted stream is not proof of target cleanup; the supervisor must reap
  the owned process group and container descendants.

The resulting native record shape is:

```text
job manifest
  job ID, task/benchmark digests, runner/agent/git hashes, model policy

attempt record
  task ID, attempt ID, ordinal, optional parent attempt
  lifecycle state, lease owner/epoch, timestamps, backend/resource identity

artifacts
  stdout, stderr, JSONL, trajectory, verifier output
  byte length, checksum, atomic complete publication

terminal result
  passed | failed | error | timeout | cancelled
  reward, exception class, usage, timings, artifact references

export record
  exporter/version, destination, idempotency key, attempts, remote identifier
```

The next fixture must kill the runner after every durable transition and at
random output chunks, restart it, and prove exactly one terminal attempt, at
most one committed verifier result, byte-identical captured streams, no leaked
descendants, and deterministic exported accounting.

### 2026-07-21 — Abandon benchmark-specific agent tuning

We removed the merged completion-audit path and discarded the unmerged
execution-feedback experiment. Their useful evidence is retained here; their
code, prompt changes, and one-off lifecycle/subagent candidate configs are
deliberately discarded. Frozen baselines, stock-Codex controls, and ordinary
Terminal-Bench runner configs remain available as measurement infrastructure.

Evidence:

- The frozen four-task Terminal-Bench 2.1 high-effort baseline, with web search,
  subagents, and completion audit disabled, passed 13/20 attempts in 8m15s. Its
  seven failures were semantic task errors: one incomplete Git webserver
  cleanup, three incorrect DNA melting-temperature results, one wrong KV field,
  and two wrong MTEB/HumanEval selections. They were not refusals, malformed
  streams, lost exit codes, or reconnect failures.
- A second model-driven completion audit reached 16/20, but took 16m22s and
  consumed 6.98M input tokens versus 3.01M for the unaudited run. The extra pass
  was therefore a costly retry strategy, not a generally useful runtime
  invariant.
- An execution-feedback prototype made non-zero nested commands explicit,
  exposed structured cell outcomes, added a Code Mode assertion helper, and
  rejected finalization with live cells or sessions. Its deterministic tests
  passed, but it did not establish benchmark lift before being abandoned.
- The attempted identical Daytona rerun at 20-way concurrency made zero model
  calls: all 20 trials hit Harbor's 360-second agent-setup timeout. A ten-way
  retry was manually cancelled when this experiment was ended. These jobs are
  infrastructure evidence only and must not be reported as agent scores:
  `tb21-daytona-runtime-assert-noaudit-k5-20260721` and
  `tb21-daytona-runtime-assert-noaudit-k5-r2-20260721`.

Decision:

- No completion-audit prompt, benchmark-specific assertion instruction,
  evaluator-specific task guidance, subagent escalation, custom finalization
  gate, or non-Codex shell-tool shape belongs in the default agent merely to
  raise Terminal-Bench scores.
- Keep fixes only when they follow from the public SDK/runtime contract and are
  justified independently of a benchmark: exact command results, bounded
  output, cancellation and descendant cleanup, durable event publication, and
  faithful Codex-compatible tool schemas.
- Treat Terminal-Bench as a release measurement, not the product-design loop.
  Future comparisons start from released product behavior and separate agent
  execution from image, sandbox, setup, verifier, and scheduler time.

### 2026-07-22 — Arize, Harbor, Eve, and Raindrop boundary review

The reviewed upstream heads are
[`Arize-ai/phoenix@ca2ee69`](https://github.com/Arize-ai/phoenix/tree/ca2ee69073c73c1011c26317a76bf42b438511fe),
[`harbor-framework/harbor@00c19fe`](https://github.com/harbor-framework/harbor/tree/00c19fe2a9c1b9b7ed07efc270412007ac4cb3da),
[`vercel/eve@a75496c`](https://github.com/vercel/eve/tree/a75496cf5072e44ecbbce5585fe50957281eecd1),
and
[`raindrop-ai/workshop@d46bcef`](https://github.com/raindrop-ai/workshop/tree/d46bcef06cea7d223cf6b169359841cb728de888).
The Eve and Raindrop changes since the July 20 snapshots do not alter the
earlier conclusions.

These systems occupy different evaluation layers:

- Harbor owns the benchmark trial: task package, isolated environment, agent
  installation and execution, verifier, reward, trajectory, repeated attempts,
  and local or hosted environment provider.
- Arize Phoenix/AX owns datasets, experiment runs, evaluators, annotations,
  traces, comparison, and production evaluation. Phoenix's SDK runner can call
  arbitrary application task functions, while its restart-resumable background
  runner executes Playground prompt/model jobs. The server runner explicitly
  stops task dispatch for anything other than an `ExperimentPromptTask`; it is
  not a container or arbitrary-agent supervisor. See the
  [background-runner design](https://github.com/Arize-ai/phoenix/blob/ca2ee69073c73c1011c26317a76bf42b438511fe/src/phoenix/server/daemons/experiment_runner.py#L1-L69)
  and its
  [task boundary](https://github.com/Arize-ai/phoenix/blob/ca2ee69073c73c1011c26317a76bf42b438511fe/src/phoenix/server/daemons/experiment_runner.py#L1426-L1431).
- Eve Eval owns application-level tests against a live Eve server. It provides
  typed intermediate-event assertions, deterministic discovery IDs, a bounded
  pool, ordered presentation, and reporters, but no trial environment or
  process-tree supervision.
- Raindrop Workshop owns local trace inspection and application-provided replay.
  It helps turn a production failure into a project-native regression test, but
  its public Workshop is not a benchmark scheduler or canonical evaluator.

Phoenix adds an important persistence reference that was missing from the
July 20 comparison. Its background runner claims jobs in the database, derives
missing task and evaluator work from persisted rows, paginates bounded buffers,
uses global and per-experiment concurrency, separates task and evaluation
phases, and reconstructs transient queues after restart. This is stronger than
holding a whole run in memory and is directly relevant to native-runner
recovery. It does not solve Nanocodex's environment boundary: a cancelled LLM
call is not proof that a task process tree or container is gone, and a
persisted experiment output is not a canonical verifier result.

Harbor resume also needs a precise characterization. Both the locally pinned
`harbor==0.18.1.dev202607150126` and the reviewed upstream support
trial-granularity job resume. Harbor preserves readable completed
`result.json` files, removes a trial directory without a result, and reruns the
missing trial. Its retry loop removes the failed trial directory before the
next attempt. See
[`Job._maybe_init_existing_job`](https://github.com/harbor-framework/harbor/blob/00c19fe2a9c1b9b7ed07efc270412007ac4cb3da/src/harbor/job.py#L230-L312)
and
[`TrialQueue._execute_trial_with_retries`](https://github.com/harbor-framework/harbor/blob/00c19fe2a9c1b9b7ed07efc270412007ac4cb3da/src/harbor/trial/queue.py#L194-L232).
The gap is therefore not absence of resume. The gap is crash-consistent,
attempt-preserving recovery: direct result writes can be interrupted, a crash
after a side effect but before publication can repeat work, and retry history
is overwritten instead of receiving explicit lineage.

Decisions:

- Keep Harbor as the canonical Terminal-Bench execution and compatibility
  boundary until a native runner passes the planned parity and fault-injection
  gates. Do not replace canonical verifiers with LLM judges or experiment
  annotations.
- Keep Arize/Phoenix optional and downstream. It may consume Nanocodex OTLP
  traces or derived results for analysis, dataset curation, and evaluator
  experiments, but it is not the benchmark source of truth and must not become
  a runtime dependency.
- Borrow Phoenix's data-derived recovery: persisted completion determines
  missing work; transient queues, UI state, and sockets do not. Evaluate stale
  claims or leases, bounded paginated buffers, task/evaluator phase separation,
  and global versus per-job concurrency in the deterministic runner fixture.
- Keep the earlier Eve decisions: deterministic IDs, a small work-conserving
  pool, reporter work outside execution permits, and presentation order
  independent of completion order. Do not wait until run completion to publish
  native results.
- Keep the earlier Raindrop decisions: persist before notification, expose a
  monotonic observation cursor, and make replay reconstruct a retained failure.
  Do not use polling, timestamp/name fallback, or in-memory correlation as a
  completion primitive.
- Keep the native runner outside `nanocodex` and its lower crates. It is a
  narrow Terminal-Bench consumer, not an SDK scheduler, generic eval platform,
  hosted observability service, or replacement for application-native tests.

The first fixture is unchanged but now has an explicit comparison target. For
each durable transition, kill and restart the current Harbor path and the
minimal native prototype. Cover at least: agent exit before stream publication,
stream publication before parsing, agent completion before verifier start,
verifier exit before result commit, result commit before export, retry
scheduling, and cancellation during every phase. The native result must retain
every attempt, commit at most one verifier result per attempt, reproduce raw
bytes and final accounting, clean all owned descendants, and export the same
Harbor/ATIF semantics without using export success as trial completion.
