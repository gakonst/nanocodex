# Cold nanocodex2 after landing conversation preparation

## Result

**Fresh-process launch to first readable text: 7.871 s median, 7.006–11.429 s
range, across five fresh agents.** All completed with no tool calls and no
response retries. The matching production deployment succeeded; receipts stayed
unchanged throughout the cohort, and every turn trace used the expected managed
Worker version.

| Measured interval | Median | Range | Relationship |
| --- | ---: | ---: | --- |
| Process launch → first readable text | 7.871 s | 7.006–11.429 s | Overall |
| Model call → first assistant text | 4.291 s | 4.135–7.874 s | Largest component |
| Process launch → agent-ready receipt | 2.651 s | 2.186–2.829 s | Startup |
| Turn admission | 0.748 s | 0.500–0.837 s | After startup |
| Model connection | 1.445 s | 1.272–1.983 s | **Included in model call** |
| Remaining dispatch/delivery/boundary time | 0.063 s | 0.037–0.091 s | Calculated per sample |

The slowest sample spent 7.874 s of its 11.429 s total in the model-to-text
interval, including a 1.983 s connection. There was no retry. The remaining
model interval is not split into provider queueing, inference and transport by
these traces.

| Run | Total | Startup | Admission | Model → text | Connection (nested) | Residual |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 7.871 s | 2.651 s | 0.837 s | 4.291 s | 1.272 s | 0.091 s |
| 2 | 7.801 s | 2.829 s | 0.762 s | 4.135 s | 1.779 s | 0.075 s |
| 3 | 9.113 s | 2.623 s | 0.539 s | 5.888 s | 1.364 s | 0.063 s |
| 4 | 7.006 s | 2.186 s | 0.500 s | 4.259 s | 1.445 s | 0.061 s |
| 5 | 11.429 s | 2.770 s | 0.748 s | 7.874 s | 1.983 s | 0.037 s |

## What preparation did on immediate cold launches

The preparation endpoint succeeded with 202 in all five runs, but its work
started **848 / 576 / 352 / 311 / 548 ms after turn admission began**, measured
within each turn's own Durable Object clock. The prompt on the existing live
socket beat the separate background preparation HTTP request. Four requests
joined in-flight startup work; the first arrived after admission completed.

Admission still waited on account discovery/startup metadata. The overlapping
stage durations are retained below; they are not independent additions:

| Stage | Median | Range |
| --- | ---: | ---: |
| `account.hosted_tools` | 548 ms | 334–576 ms |
| `account.catalog` | 723 ms | 500–748 ms |
| `account.mcp_discovery` | 723 ms | 500–748 ms |
| `account.vault` | 730 ms | 498–837 ms |

Each admission recorded 17 `session_state` reads and two
`session_initialization_ownership` reads, both totaling 0 ms at Worker timer
resolution. This does not establish that those reads have no cost, but the
measured half-to-eight-tenths-of-a-second wait is in account discovery/metadata,
not a measured SQL execution span. No full SQL audit was enabled.

Exact agent-specific tool-host requests took **515–654 ms**, median **558 ms**,
inside the startup boundary. Root `/v1/agents/live` requests matching the launch
time windows took **1.204–1.510 s**, median **1.360 s**. Those root requests lack
agent/request IDs, so they are labeled candidates, not exact correlations. The
full 2.651 s startup boundary is not yet attributable to individual client/auth/
network operations.

The next supported experiment is to start preparation during authenticated live
agent creation, before client tool attachment completes, then check whether it
moves this 0.5–0.84 s discovery and 1.27–1.98 s connection work off the immediate
prompt path. This is an experiment to validate, not a promised saving: overlap
and dependencies matter.

## Comparison with previous measurements

The previous single cold diagnostic was 10.698 s with the then-new release and
9.629 s with the installed nightly. This new five-sample median is lower, but
those single earlier samples, deployment changes, live provider variance and
uncontrolled local load do not support assigning a causal speedup to the newly
landed preparation hook. Cold admission and connection costs clearly remain.

The earlier 2.545 s first-prompt interactive median gave preparation five seconds
after a connected composer appeared and excluded agent creation. It measures a
different user journey and must not be presented as cold launch performance.

## Measurement boundary

Five independent optimized `nanocodex2 run yo` processes, each creating a fresh
managed agent, with no deliberate preparation interval. Timing starts before
process spawn and stops when the first nonempty assistant text delta reaches
stdout. This is launch-to-first-text, not Enter-to-text in an already connected
composer. It does not include shell command entry or physical monitor scanout.

Model: `gpt-6-astra`, low effort, standard reasoning, fast mode off. The temporary
workspace is empty; normal account context and local tool definitions remain.
Background hand startup is enabled, as in normal CLI use. This is a no-tool
prompt; actual tool counts and response retries are retained per sample.

Fresh agents do not force fresh shared Cloudflare isolates, database/credential
caches, or OS filesystem pages. No sandbox or VM is deliberately provisioned.
We retain ambient load averages; this is a live-system sample, not a controlled
provider comparison. Five samples cannot establish a reliable p95.

## Provenance

Implementation: `16764704`; tested source and binary build: `470ea709`.
The build command is `cargo build --release --locked -p nanocodex2-bin --bin nanocodex2`.
The release build passed, and a private copy of its binary was used to prevent
concurrent builds from changing the executable during measurement.

Focused post-rebase verification passed: 99 managed Worker tests, 59 SDK tests,
managed Worker and SDK contract type checks, and Rust formatting. These checks
cover the landed preparation implementation; this measurement adds no product
code. The installed nightly launcher was not changed.

## Interpretation rules

Startup ends at the CLI's `Managed agent:` receipt, after agent creation and
its event/tool attachment setup. Server turn admission is correlated by exact
turn ID. Model first-output telemetry includes connection time, so connection
is a nested row, not an additional cost. Account discovery/vault stages overlap
inside admission and likewise must not be summed.

Text residual = total launch-to-text minus startup, admission, and server
model-call-to-first-assistant-text durations. The separate model-first-output
metric can fire on an output item before readable text; both are retained. The
residual includes dispatch/delivery and any uninstrumented boundary work; it is
not a pure network measurement. We subtract durations, not wall timestamps
from unrelated machines. Individual medians do not necessarily add to total
median. Named read counters are not a complete SQL audit; sub-ms synchronous
reads can report zero at Worker timer resolution.

See [measurements](measurements.json), [correlated traces](traces.json), and
[provenance, deployment receipts and cleanup](manifest.json).

The local one-minute load average was **89.6–105.1**. Our release compiler had
finished before all timed trials, but other work on this Mac was not stopped.
Do not generalize the startup measurements to an idle machine.

Binary SHA-256: `23bb1bcf5271a1d8475cdcd93f106f50042354b4822898e79229a2c4928f3547`.
Managed Worker: `ab908259-a160-4f0b-ba28-48c82d766d30`.
Production source: `470ea709f4bda8e186b38be5e713d7820e9db4b4`.
[Production deployment](https://github.com/gakonst/nanocodex/actions/runs/35170652139)
finished its deploy job successfully. Subsequent deployed-thread/cron checks are
separate CI jobs; their status is not claimed by this measurement.

All five scratch agents returned 404 after deletion. The trace collector has
stopped. [Client events](client-events.json) retain the first text and transport/
model/run timing events without full provider payloads. The committed harnesses
show how the measurements were collected; their paths are specific to this
Mac/worktree. Raw local output stays in `output/nanocodex2-cold-master-20260916/`.
No direct Codex, voice, VM, or mobile benchmark was rerun in this cohort.
