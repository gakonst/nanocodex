# System end-to-end measurements — 2026-09-16

This is a fresh production measurement pass, with no application changes or deployments made for the benchmarks. Results below are measured from the Mac client to the deployed service, unless a row explicitly identifies a server or host span.

## Method and scope

- Sequential measurement windows: native Hands/VMs, screens, voice, hosted agents, then installed CLI comparison. Preparation and report analysis may overlap, but active cohorts do not compete with one another.
- Scratch agents, unique scratch files, temporary VM allocations and synthetic/prerecorded input only. No ambient microphone, remote control input, existing personal files, or connector actions.
- Hosted text settings: `gpt-6-astra`, low reasoning, standard mode, fast mode off. Fresh-agent and same-agent follow-up timings are separate. A new agent does **not** establish a cold Worker or cold container.
- Client durations use monotonic clocks. Worker/host spans use their own clocks. Nested/overlapping spans are not added; timestamps from separate isolates are not subtracted.
- Mac host load was unusually high (roughly 267–411 in the preparation/Hand/screen window on 10 logical cores). These are observations under that load, not clean-machine performance guarantees. Screen broker and host execution timings are kept separate from client scheduling and network setup.
- Full SQL statement auditing is disabled in the deployed Worker (`NANOCODEX_PERFORMANCE_TRACE` absent, checked through the settings API). Named read counters remain available. Absence of SQL audit logs does **not** mean zero queries; zero-millisecond Worker timers do not establish zero CPU cost.
- Small cohorts describe observed medians and ranges; they do not establish a p95 or fleet-wide reliability rate.

## Deployment provenance

At start of this pass, source checkout `68a7f4edc` (later changes after `c0d049f68` are documentation), with deployed versions:

| Component | Version |
|---|---|
| Managed agents | `e1877ce4-0170-4de6-9cf9-ccfc003b7ece` |
| Account | `83d1e7ce-0db2-4637-9de8-8e9fd5f6968f` |
| Egress | `d8f1b3e5-2ae9-439b-a587-cd39224eeb92` |
| Connect API | `d7ab1bb1-a22e-4038-87ac-043743d33b82` |

Installed native Hand/CLI revision and screen fixture provenance are recorded in their individual reports. The frame tests use a native debug integration-test runner, not tap-to-frame timing in the installed release UI. Physical iPhone interaction and a live Windows Hand were not measured in this pass.

## Results

| Journey | Fresh/current measurement | Reused path | Evidence |
|---|---|---|---|
| Hosted no-tool prompt → first text | **4.87 s** median | **2.52 s** median | [Agents](agents.md) |
| Hosted create → first text | **7.64 s** median | — | [Agents](agents.md) |
| Installed CLI `yo`, process → first text | Nanocodex2 **7.34 s**, Codex **5.85 s** | Nano **4.00 s**, Codex **5.41 s** | [CLI](cli.md) |
| Brain tools, prompt → final answer | **13.75 s** median | **5.12 s** median | [Agents](agents.md) |
| Cloudflare sandbox, prompt → final answer | **21.18 s** median | **7.22 s** median | [Agents](agents.md) |
| Prepared Linux VM mount | **1.82 s** median | Commands **119–144 ms** | [Hands/VMs](hands-vm.md) |
| Native Linux Hand command | First **448 ms** median | **382–434 ms** | [Hands/VMs](hands-vm.md) |
| Screen → first decoded frame | Linux **607 ms**, VM **778 ms**, Mac **1.32 s** medians | Reconnect cohorts | [Screens](screens.md) |
| Voice media ready, final actual-audio cohort | **2.28 s** fresh | **1.27 /1.61 s** restarts | [Voice + recordings](voice.md) |
| Voice speech end → received audio, final cohort | **1.04 /1.46 /1.26 s** | Same three calls | [Voice + recordings](voice.md) |

Most rows have three samples; sandbox rows have two. Tool first text can be commentary; final-answer timings above deliberately include tool execution and model continuation. Voice setup excludes browser launch and scratch-agent creation. These boundaries differ and are not additive.

## Reliability findings

- All 16 hosted workload turns, two additional no-tool lifecycle turns, two running-turn cancellations, twelve CLI turns, eighteen Hand commands and nine decoded-frame screen samples passed. Stream replay, SSE, screen lease extension and rejected-snapshot recovery passed their scoped checks.
- **Voice is not fully healthy in this test:** across seven spoken-audio calls, six produced decoded audio. One original warm call produced the correct transcript but no detected audio energy within 15 seconds. In the final recorded three-call cohort, every call produced valid audio, but one output caption was wrong. Recorded semantic audio content was not independently transcribed. These are retained failures, not hidden by successful retries.
- A separate native receive-only text/context speech command timed out. Native connection/control/restart worked; this failure does not establish that native microphone-input replies fail. There is no native prerecorded-PCM injection hook in the existing fixture.
- All thirteen root-owned managed agents returned 404 after deletion; five voice agents did too. The six Hand/VM agents were deleted and the Linux factory allocation directory is empty. Codex scratch threads were archived. API absence is distinct from completion of every deferred storage alarm.
- Shared archive/deletion warnings did not match the eight workload or six Hand/VM Durable Objects in the captured traces. They are not counted as failures of those cohorts. The warning evidence is explicitly scoped.

## Connection warmup follow-up

A further six-agent paired check found **4.51 s TTFT immediately versus 4.55 s after five seconds idle**. Connecting currently warms personalization/event delivery, but does not start the agent runtime or its model connection. Every delayed case still paid that connection during the first turn. [Source audit, experiment and OpenAI/Codex comparison](warmup.md). This supports adding explicit advance preparation; the incremental gain of a `generate:false` request remains to be measured. All six additional agents completed and returned 404 after deletion.

## Measured cost ranking

This ranks observed costs in their affected journeys, not parts that can all be added together. It identifies where further work is justified without promising a speedup before testing a change.

1. **New Cloudflare sandbox preparation: 6.58–7.62 s.** Workspace readiness 3.80–4.56 s, egress binding 1.49–1.67 s, brain mounting 648–758 ms and desktop setup 602–610 ms. This affects tool completion; it occurs after the initial acknowledgement in these fixtures. Warm reuse avoids the one-time preparation.
2. **Model first output and continuation.** No-tool model-first-output spans are 3.82–5.91 s fresh versus 1.59–3.31 s reused. Model connection costs 1.36–2.41 s fresh and zero on reused remote sessions, within the run's measured work. Brain tool work is only 542–843 ms of a 13.5–18.2 s turn. Hand discovery/mount result→next command is a separate observed 3.6–5.6 s agent-continuation interval, not pure VM startup.
3. **Fresh client/agent setup before prompting: 2.58–3.02 s** in no-tool API journeys. Creation is around 1.6 s and event WebSocket readiness around 1 s. Creation's caller-side session wait is much longer than its named commit phase; constructor/allocation/routing time remains incompletely split. Starting an absent Mac screen publisher separately cost 2.44 s in one sample.
4. **VM tool/screen attachment: roughly 1.0–1.5 s.** Local prepared-VM claim already takes less than 1 ms. Tool WebSocket connection is 0.99–1.28 s, followed by 235–302 ms catalog acknowledgement. Tool/screen publishers overlap. A first command then rechecks readiness for 127–141 ms.
5. **Voice startup and response.** Provider call creation waits 338–550 ms; credential-broker round trips take 165–190 ms and relay-boundary residuals 119–227 ms. Client SDP-response→media-ready takes 388–1,336 ms. These startup spans are separate from the final cohort's 1.04–1.46 s speech-end→audio response. All observed voice relay containers were already running.
6. **Screen media setup and first frame.** Mac's post-admission offer/channel/first-frame chain takes 614–1,113 ms. Linux's admitted-to-frame interval is 227–276 ms; VM is 408–419 ms. Capture, encode and decode are not individually timed in these journals.
7. **Repeated hosted-tool refresh on every prompt: 335–365 ms** in the three no-tool follow-ups, occupying the measured admission interval. MCP discovery overlaps it. This is a specific repeatable response-path cost worth addressing. It is not the removed prompt-dependent personalization search.
8. **Native Hand command routing: 382–462 ms** versus only 4.49–5.45 ms command execution on the host. Broker/transport subspans are incomplete. Deletion is another user-facing cost: 2.38 s median in the API cohort, with 0.96–1.70 s labeled container cleanup, even for agents that did no container work.

## Evidence and remaining gaps

Read the linked component reports for exact request/turn/allocation IDs, individual samples, model usage, failure outcomes, clock definitions and raw safe trace records. [Cleanup and unchanged deployment receipts](cleanup-and-provenance.json) close the pass. Actual [input speech](voice-audio/input-please-say-ready.wav) and [recorded response](voice-audio/response-1.wav) are retained.

**Complete SQL query/row totals remain unmeasured.** Production statement tracing is off. Named admission scopes show 17 session-state plus two ownership reads fresh and nine session-state reads on follow-up; these are partial counters, not all SELECT statements. We cannot infer the cost or redundancy of uninstrumented queries.

Also unmeasured: physical phone or installed-release UI tap latency, live Windows Hands, forced cold Worker/container/image conditions, exhausted spare pools, full capture/encode/decode breakdown, barge-in and fleet tail percentiles. No application fix or deployment was made; changes in this checkout are measurement reports and evidence only. Temporary diagnostic tails are stopped.
