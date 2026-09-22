# Hosted agents, tools, lifecycle and read timings

Fresh production measurements using the current public SDK access-cache wrapper and real event WebSockets. **All 16 workload turns completed with expected results**, across eight scratch agents. Two additional agents verified creation→response, replay, SSE and active-turn cancellation. Model settings were `gpt-6-astra`, low thinking, standard reasoning, fast mode off.

[Correlated traces and individual samples](agents-traces.json) retain request IDs, turn IDs, model telemetry, tool events and exact-owned Durable Object spans. Raw local harnesses are under `output/system-e2e-20260916/agents/` in the integration checkout. This cohort ran after Hands/screens/voice, with no simultaneous benchmark model calls.

## Text and tool journeys

All values are milliseconds, median (range). Prompt→first text measures the first nonempty text delta, which can be commentary. Final-answer text is separate so an early acknowledgement is not mistaken for the answer. Fresh means a new agent/client authority cache, not a proven cold Worker. Follow-up reuses the agent, authority cache and event connection.

| Workload | State | n | Prompt→first text | Prompt→final text | Turn complete |
|---|---|---:|---:|---:|---:|
| no_tools | Fresh | 3 | 4,867 (4,448–6,814) | 4,867 (4,448–6,814) | 5,345 (4,913–7,351) |
| no_tools | Follow-up | 3 | 2,525 (2,071–3,754) | 2,525 (2,071–3,754) | 2,837 (2,314–4,011) |
| brain_tools | Fresh | 3 | 5,747 (5,347–9,096) | 13,748 (13,215–18,011) | 13,952 (13,480–18,244) |
| brain_tools | Follow-up | 3 | 2,190 (2,037–2,362) | 5,116 (5,008–5,346) | 5,364 (5,142–5,559) |
| sandbox_tools | Fresh | 2 | 4,884 (4,742–5,026) | 21,181 (20,985–21,377) | 21,471 (21,268–21,674) |
| sandbox_tools | Follow-up | 2 | 2,892 (2,444–3,340) | 7,219 (6,609–7,828) | 7,458 (6,828–8,088) |

No-tool answers were `42` then `43`. Brain fixtures created a unique two-number file, read/summed/deleted it, then printed `43` on the follow-up. Sandbox fixtures mounted a new `cf_sandbox`, ran a fixed Python sum, and reused that mount for the next sum. No package installation or external connector calls.

## API operations

These timings include client HTTP transport and the response body, with histories paginated after each terminal event. CRUD probes run **after** the two workload turns, so they do not artificially warm first-turn setup. Creation starts with an empty client access cache; finite requests after it reuse the short-lived snapshot.

| Operation | n | Client milliseconds, median (range) |
|---|---:|---:|
| agents.create | 8 | 1,604 (1,433–1,914) |
| agents.delete | 8 | 2,380 (2,149–2,881) |
| agents.get | 8 | 77 (55–122) |
| agents.list.cached_auth | 8 | 267 (248–353) |
| agents.list.live_auth | 8 | 521 (450–602) |
| events.history | 16 | 187 (67–1,756) |
| events.websocket.ready | 8 | 971 (898–1,385) |
| settings.patch | 8 | 76 (59–115) |
| turns.create | 16 | 114 (80–269) |
| turns.get | 16 | 72 (48–180) |

For the three no-tool journeys, **create→first text was 7,640 ms median (7,032–9,829)**. Before the prompt could be submitted, creation plus WebSocket readiness took 2,584–3,015 ms. The API's quick turn-acceptance response is not TTFT.

Every finite post-create SDK request in this cohort used the cached authority snapshot. Paired list probes show 267 ms cached versus 521 ms live median. Exact server logs show cached auth at zero observed I/O versus roughly 187–218 ms live. WebSocket admission remains a live-authority operation; the token does not remove that setup path.

## Where no-tool response time went

| Component | Fresh samples | Follow-up samples |
|---|---|---|
| Client prompt→first text | 4,867 / 4,448 / 6,814 ms | 3,754 / 2,071 / 2,525 ms |
| Model call→first output | 3,865 / 3,819 / 5,913 ms | 3,313 / 1,589 / 2,117 ms |
| Model connection duration, within run | 1,600 / 1,362 / 2,406 ms | 0 / 0 / 0 ms |
| Durable turn admission | 755 / 501 / 756 ms | 347 / 365 / 335 ms |

Connection duration is not an additional independent addend to model-call latency. Model calls, admission and client arrival have different measurement scopes; no cross-isolate timestamps are subtracted. The first fresh sample, for example, has a 3,865 ms model-first-output span, 755 ms admission span and 4,867 ms client TTFT; the remaining 247 ms is an unpartitioned residual, not proven network time.

**The repeated hosted-tool refresh remains on the response path even for no-tool prompts.** On those three follow-ups, exact turn traces show `account.hosted_tools` at 347/365/335 ms, spanning the full admission duration. MCP discovery is 182/199/202 ms and overlaps that refresh. Do not add the two spans. Fresh startup additionally waits for the account environment/catalog/vault scopes, reaching 501–756 ms. These are concrete candidates for a later bounded-cache or deferred-discovery change; this pass changes no behavior.

The first two fresh model calls reported zero cached input tokens; the third reported 15,744. All three follow-ups reported 18,560 cached out of 18,729 input tokens. That establishes different provider cache state, but does not prove how much of each timing difference it caused.

Agent creation itself is also material. Exact request-matched creation logs show 1,198–1,656 ms in the session-create call across the first six cases, while its returned commit phase is 239–328 ms. The remaining caller-side wait is not split into object allocation, construction, routing and transport by current instrumentation. It should not all be labeled SQL or authentication.

## Tools and container startup

Brain first turns made two tool calls and three model calls. Tool-wall time was 542/843/676 ms, compared with 13,952/18,244/13,480 ms to finish the turn. Follow-up print commands recorded zero at the runtime's timer resolution; that does not prove zero CPU or zero tool work. Model continuation is the largest observed part of these journeys.

The two new sandbox mounts expose actual first-use preparation. They are new resource IDs, with the service already deployed and warm; this is not a controlled cold Worker or cold-image experiment.

| Sequential preparation stage | New sandbox 1 | New sandbox 2 |
|---|---:|---:|
| Bind egress | 1,492 ms | 1,666 ms |
| Workspace ready | 3,800 ms | 4,555 ms |
| Workspace alias | 38 ms | 32 ms |
| Brain mount | 648 ms | 758 ms |
| Desktop setup | 602 ms | 610 ms |
| Total `sandbox.prepare` | **6,580 ms** | **7,621 ms** |

These stage durations sum to the preparation total, within the same tracing scope. Initial command preparation after mount was 239/245 ms; subsequent turn reuse was 200/190 ms for workspace/alias/brain checks. End-to-end tool-wall time was 7,645/8,583 ms on the first turns versus 788/710 ms on reuse. Desktop setup is paid even in this shell-only sandbox fixture. First text precedes mounting, so sandbox preparation affects the final answer, not the initial commentary TTFT in these samples.

## Streams and cancellation

[Lifecycle fixture evidence](agents-lifecycle.json):

- Fresh create→first text: 8,063 and 7,005 ms; completed normally.
- Reconnect with cursor zero: ready in 945/1,011 ms, prior terminal replay in 945/1,012 ms. The original stream stayed open during this explicit second-connection probe; this is replay coverage, not forced-network-failure recovery.
- SSE first body frame: 245/276 ms, HTTP 200.
- Cancel after the model-start event: client receives `turn_cancelled` in **191/209 ms**, both successful. The terminal event can arrive before the cancel HTTP response finishes.
- Delete accepted for all ten scratch agents; all ten subsequently returned HTTP 404, recorded in [cleanup checks](cleanup-and-provenance.json). Background cleanup must be distinguished from API deletion acceptance.

## SQL/read coverage and cleanup

Full statement auditing was disabled in this deployment. We cannot provide honest total SELECT/UPDATE counts or rows read/written from this pass. Existing exact-turn named counters show:

- First no-tool admission: **17 session-state reads plus two initialization-ownership reads**.
- Follow-up admission: **nine session-state reads**.
- Turn-accept handler: **six session-state reads**.
- GET state/history/turn: typically one session-state read; PATCH settings: two.

Scopes can overlap, so these are not additive SQL totals. Measured read intervals were zero at Worker clock granularity; neither their CPU cost nor redundant-query safety can be inferred from that zero.

DELETE took 2,380 ms median. Exact-owned stage logs show containers cleanup at 963–1,698 ms across the eight workload agents, including no-tool agents; registry initial detach 224–284 ms, memory cleanup 203–220 ms, and archives 260–419 ms. Stages with zero observed duration still execute code. The logged containers stage does not by itself establish that a container was allocated for a no-tool agent.

Archive/deletion warnings appeared elsewhere in the shared tail, but none matched the eight workload agents' exact Durable Object IDs in the captured cohort. They are not counted as failures of these journeys. Hand/VM object matching is recorded separately in [lifecycle-warnings.json](lifecycle-warnings.json); its matched warning list is also empty. This does not establish fleet-wide absence of cleanup failures.

## Measurement limitations

No forced Worker eviction, exhausted VM pool, cold image cache, physical phone taps, live Windows host, or full SQL audit. Mac load declined substantially during this pass, so cross-cohort client timings are not controlled A/B measurements. Deployed versions were unchanged at both ends of the 16-turn workload cohort. No performance fix, commit or deployment was made for these measurements.
