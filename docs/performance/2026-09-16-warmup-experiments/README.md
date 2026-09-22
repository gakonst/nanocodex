# Early runtime and OpenAI preparation experiments

Measured on September 16, 2026 (Pacific time). Evidence only: this experiment changed no production code or configuration. A separately triggered production rollout crossed the confirmation cohort, as documented below.

## Findings

**Advance runtime/socket preparation is the strongest demonstrated change.** With the same five-second lead before submitting, hosted median TTFT fell from **5.62 s to 3.73 s** in the initial stable-deployment cohort (three samples per arm; 34% lower observed median). All three prepared turns reported **0 ms model connection time**, versus 1.28–1.71 s in its event-only group. This is a mechanism-confirmed result with a small, noisy sample, not a guaranteed 1.90-second saving.

A final repeat on the new deployment was mixed: **5.74 → 5.29 s** median (two samples per arm), with one prepared turn requiring a reconnect/retry. Keep this separate from the older-deployment result; reliable connection reuse remains unproven on the current release.

The incremental `generate:false` result is weaker: small-context median 1.49 → 1.27 s, large-context median 1.79 → 1.86 s versus socket-only preparation. The large-context request-warmup outlier had **3,964 ms engine queue time** and 5,634 ms TTFT despite completing preparation before submit. Its matched socket-only sample also had 22,912 cached input tokens versus zero in that warmup sample, so the 4,247 ms paired difference is not attributable entirely to warmup. Neither queueing nor prompt-cache hits were controlled.

When the prompt arrived immediately, waiting for request warmup made all three matched trials slower: median **1.96 → 2.56 s**. Do not make non-generating warmup a prerequisite for sending an already-available prompt.

**Next implementation priorities:**

1. Prepare the runtime and Responses socket when an authorized client activates a conversation. Own one preparation task per session, join it when useful, expire idle preparation, and invalidate it when authority/settings/tool context change. The tested empty tool-host socket is an experimental mechanism; production should use an explicit active-conversation lifecycle, not publish a fake Hand or warm every passive subscription.
2. Reuse a valid account/tool snapshot. All five prepared admissions in the initial/confirmation cohorts still waited **293–397 ms**, precisely matching the hosted-tool refresh span while other account reads overlapped. This is measured remaining work, not a demonstrated additional saving from a cache implementation.
3. Treat provider request-state warmup as an optional follow-up experiment in the actual hosted runtime. Current measurements do not establish a consistent win over opening the socket. Avoid serializing it ahead of an immediate prompt.
4. Keep voice media setup independent. The preceding real-audio experiment accepted speech 869 ms before durable admission completed. Responses preparation should not gate microphone/media readiness. No new voice-media gain is claimed here; [recordings and prior voice measurements](../2026-09-16-system-e2e/voice.md).

For prepared hosted turns, the largest measured span remaining is model first output: **2.40–5.99 s**, median 2.56 s. The total outside that model span is 0.63–1.17 s, including admission plus network/delivery/client scheduling; it is not all relay work. These nested/boundary measurements do not form independent additive medians.

## Repeat on the new deployment

Four additional turns ran after the production rollout finished, two per arm, with five seconds before submit. Start/end receipts and subsequent cleanup receipts agree on the new versions. This batch is separate from the initial stable batch and the confirmation batch that crossed the rollout.

| Arm | Rep | TTFT | Connection during turn | Response retries | Backoff |
|---|---:|---:|---:|---:|---:|
| events_only_5s | 1 | 6391 ms | 1354 ms | 0 | 0 ms |
| runtime_5s | 1 | 6893 ms | 1103 ms | 1 | 192 ms |
| runtime_5s | 2 | 3696 ms | 0 ms | 0 | 0 ms |
| events_only_5s | 2 | 5089 ms | 1712 ms | 0 | 0 ms |

Current-version median: event-only **5740 ms**, runtime preparation **5294 ms** (n=2 each). One prepared turn completed in 3,696 ms with zero connection time; the other took 6,893 ms and reported two connection attempts, one reconnect, two response attempts, one response retry, and 192 ms backoff. Both preparation sockets opened before submit. The retrying turn paid 1,103 ms connection time during the turn.

The persisted public events and sanitized logs do not contain the retry cause. Its 1,103 ms connection and 192 ms backoff do not explain the entire slow turn; model first output was 6,150 ms. We cannot label it a stale socket, provider rejection, or rollout failure without that missing reason. This is evidence to validate connection reuse and capture retry reasons, not a reason to drop the sample.

**Current-version results do not reproduce the initial cohort’s clean connection reuse in every sample.** The initial 34% observed median improvement remains an older-deployment result. Preparation is a promising implementation direction, not a reliable improvement already delivered on the current release.

[Current client events and deployment receipts](current-raw.json), [component timings and named reads](current-measurements.json), [version-bearing invocation traces](current-traces.json).

## Hosted agent experiments (pooled descriptive table)

Fresh scratch agents; `gpt-6-astra`, low reasoning, standard service. Prompt: `Return exactly 42. Do not call any tools.` Event stream connected before the experimental lead interval. Runtime preparation used the existing authenticated tool-host WebSocket without publishing any tools or creating a Hand. This exercises existing runtime construction and socket preconnection, not a proposed production prepare API. All hosted request-state warmup durations were zero.

Three repetitions rotating four arms; then two repetitions rotating event-only, five-second preparation, and 35-second preparation. The table pools both cohorts descriptively, with five samples per five-second arm. A production rollout crossed the confirmation cohort: its final runtime_35s and events_only_5s turns used a newer managed Worker. The causal headline uses only the initial 12-trial cohort, whose deployment receipts stayed unchanged. Samples are small and provider/cache/load variation remains.

| Before submit | n | Submit → text median (range) | Model connection in turn median | Admission median | Lead start → text median | Create → text median |
|---|---:|---:|---:|---:|---:|---:|
| events_only_5s | 5 | 5.34 s (4.10 s–6.66 s) | 1.30 s | 0.48 s | 10.34 s | 13.40 s |
| runtime_0s | 3 | 5.28 s (4.72 s–6.62 s) | 1.95 s | 0.67 s | 5.28 s | 8.17 s |
| runtime_1s | 3 | 4.22 s (3.93 s–4.23 s) | 1.40 s | 0.17 s | 5.22 s | 7.74 s |
| runtime_5s | 5 | 3.73 s (3.09 s–6.86 s) | 0.00 s | 0.30 s | 8.73 s | 11.50 s |
| runtime_35s | 2 | 4.01 s (4.01 s–4.02 s) | 0.00 s | 0.37 s | 39.02 s | 41.74 s |

**Lead time is included in the last two columns.** Five seconds is an experimental interval representing time while a user composes; deliberately delaying an already-available prompt by five seconds is not a speed improvement. No immediate-prompt baseline without a preparation request was included in this cohort.

### Individual hosted samples

| Cohort | Arm | Rep | TTFT ms | Connection ms | Admission ms | Model first output ms | Cached / input tokens |
|---|---|---:|---:|---:|---:|---:|---:|
| hosted | events_only_5s | 1 | 5622 | 1710 | 826 | 4513 | 0 / 18554 |
| hosted | runtime_0s | 1 | 6621 | 2506 | 642 | 5824 | 15744 / 18554 |
| hosted | runtime_1s | 1 | 3934 | 1239 | 168 | 3339 | 0 / 18554 |
| hosted | runtime_5s | 1 | 5128 | 0 | 299 | 4361 | 15744 / 18554 |
| hosted | runtime_0s | 2 | 4724 | 1950 | 667 | 3879 | 18432 / 18554 |
| hosted | runtime_1s | 2 | 4216 | 1398 | 175 | 3609 | 15744 / 18554 |
| hosted | runtime_5s | 2 | 3726 | 0 | 305 | 2557 | 15744 / 18554 |
| hosted | events_only_5s | 2 | 6658 | 1298 | 432 | 5166 | 18432 / 18554 |
| hosted | runtime_1s | 3 | 4231 | 1714 | 251 | 3577 | 0 / 18554 |
| hosted | runtime_5s | 3 | 3088 | 0 | 293 | 2454 | 18432 / 18554 |
| hosted | events_only_5s | 3 | 4560 | 1281 | 445 | 3874 | 15744 / 18554 |
| hosted | runtime_0s | 3 | 5283 | 1793 | 833 | 4324 | 15744 / 18554 |
| hosted-confirm | events_only_5s | 1 | 4103 | 1133 | 485 | 3358 | 0 / 18554 |
| hosted-confirm | runtime_5s | 1 | 3165 | 0 | 313 | 2397 | 0 / 18554 |
| hosted-confirm | runtime_35s | 1 | 4011 | 0 | 322 | 1970 | 15744 / 17616 |
| hosted-confirm | runtime_5s | 2 | 6863 | 0 | 397 | 5993 | 15744 / 18554 |
| hosted-confirm | runtime_35s | 2 | 4016 | 0 | 409 | 2746 | 15744 / 18546 |
| hosted-confirm | events_only_5s | 2 | 5338 | 1303 | 837 | 3603 | 0 / 18554 |

### Traces and interpretation

- Event-only connection does not remove first-turn model connection work. Five-second runtime preparation removed the measured connection wait from every matching turn in the initial/confirmation cohorts; the later current-version batch includes a reconnect exception.
- One-second preparation still paid connection time; zero-lead preparation raced the prompt and did not establish a ready connection in advance. The empty tool-host handshake includes its own live authorization and account work, so its timing is not the lower bound for a dedicated preparation API.
- Both 35-second cases retained their prepared model connection. This is a two-sample observation on this route, not a general idle-lifetime guarantee. Source has a 30-second idle default, but construction does not itself install a new idle alarm. The trace sanitizer did not retain `managed.capacity.reason`, so it cannot independently confirm the absence of an idle-shutdown event.
- The provider/model first-output span includes connection time when present. Admission, environment, hosted-tool discovery and MCP spans overlap; do not add them together. `TTFT - model first output` is a boundary residual including request transport, admission, delivery and client scheduling, not a pure relay measurement.
- Prepared admissions still execute account work. The named admission counters dropped from 17 session-state + 2 ownership reads to 14 session-state reads. These are scoped counters, not all SQL statements. Reported 0 ms local-read timers do not prove zero cost.

[Correlated timings, named read counters, turn/request IDs and logs](hosted-measurements.json). Complete client frames and deployment receipts are retained in [the first cohort](hosted-raw.json) and [confirmation cohort](hosted-confirm-raw.json).

## Direct provider experiments

Existing Codex subscription credentials, `wss://chatgpt.com/backend-api/codex/responses`; Astra low, explicit default service tier, Responses Lite controls matching the native transport. No managed API, Durable Object or egress relay in this microbenchmark. Three repetitions per arm and context size, rotating arm order. A shared prefix and cache key within each repetition, new prefix UUID between repetitions. This balances ordering only approximately; it does not force cache hits or eliminate provider variance.

- **Cold:** connect and send at the scheduled submit time.
- **Socket:** connect beforehand; send the full request at submit.
- **Request:** connect, submit known instructions with `generate:false`, wait for completion, then chain the real request with `previous_response_id`.

All arms use the same five-second scheduled lead. If preparation has not finished at submit, submit→text includes the remaining preparation wait. `open_to_text_ms` means experiment/lead start→text, not WebSocket-open→text. For the cold arm, `preparation_ms` includes the intentional lead; `socket_ms` isolates the connection.

The large context is a synthetic 1,500-line reference table, not the production tool/system prompt. Repeating known top-level instructions when chaining produced the same generation input-token count as other arms within each block. This tests supported provider behavior, not Nanocodex integration or actual native Codex UI startup.

| Context | Preparation | n | Submit → text median (range) | Lead start → text median | Generation request → text median | Ready at submit |
|---|---|---:|---:|---:|---:|---:|
| small | cold | 3 | 2089 ms (1661–2403) | 7090 ms | 1685 ms | 0/3 |
| small | socket | 3 | 1493 ms (1415–1566) | 6495 ms | 1493 ms | 3/3 |
| small | request | 3 | 1275 ms (1239–1553) | 6276 ms | 1275 ms | 3/3 |
| large | cold | 3 | 2509 ms (2276–3097) | 7509 ms | 2069 ms | 0/3 |
| large | socket | 3 | 1790 ms (1387–2316) | 6791 ms | 1790 ms | 3/3 |
| large | request | 3 | 1859 ms (1501–5634) | 6859 ms | 1859 ms | 3/3 |

Cold is intentionally unprepared at submit. All lead start totals retain the five-second wait.

### Individual provider samples

| Context | Rep | Arm | Submit → text ms | Socket ms | Warmup ms | Generation cached / input | Warmup input / output |
|---|---:|---|---:|---:|---:|---:|---:|
| small | 1 | cold | 2403 | 696 | 0 | 0 / 55 | 0 / 0 |
| small | 1 | socket | 1415 | 643 | 0 | 0 / 55 | 0 / 0 |
| small | 1 | request | 1553 | 285 | 342 | 0 / 55 | 46 / 0 |
| small | 2 | socket | 1566 | 368 | 0 | 0 / 59 | 0 / 0 |
| small | 2 | request | 1239 | 526 | 364 | 0 / 59 | 50 / 0 |
| small | 2 | cold | 1661 | 363 | 0 | 0 / 59 | 0 / 0 |
| small | 3 | request | 1275 | 511 | 326 | 0 / 54 | 45 / 0 |
| small | 3 | cold | 2089 | 405 | 0 | 0 / 54 | 0 / 0 |
| small | 3 | socket | 1493 | 503 | 0 | 0 / 54 | 0 / 0 |
| large | 1 | cold | 2276 | 270 | 0 | 0 / 23066 | 0 / 0 |
| large | 1 | socket | 2316 | 382 | 0 | 0 / 23066 | 0 / 0 |
| large | 1 | request | 1859 | 317 | 378 | 0 / 23066 | 23057 / 0 |
| large | 2 | socket | 1790 | 522 | 0 | 0 / 23066 | 0 / 0 |
| large | 2 | request | 1501 | 360 | 458 | 0 / 23066 | 23057 / 0 |
| large | 2 | cold | 2509 | 440 | 0 | 0 / 23066 | 0 / 0 |
| large | 3 | request | 5634 | 487 | 798 | 0 / 23067 | 23058 / 0 |
| large | 3 | cold | 3097 | 426 | 0 | 0 / 23067 | 0 / 0 |
| large | 3 | socket | 1387 | 381 | 0 | 22912 / 23067 | 0 / 0 |

### Matched differences

Positive numbers mean preparation was faster. These are individual matched-block observations, not confidence intervals.

| Context | Rep | Socket saving vs cold | Request warmup saving vs socket |
|---|---:|---:|---:|
| small | 1 | 988 ms | -138 ms |
| small | 2 | 95 ms | 327 ms |
| small | 3 | 596 ms | 218 ms |
| large | 1 | -40 ms | 457 ms |
| large | 2 | 719 ms | 289 ms |
| large | 3 | 1710 ms | -4247 ms |

### Provider-side timing fields

These fields are reported by the provider; their internal semantics are not a public timing contract. They are nested and must not be summed as independent stages.

| Context | Arm | Pre-inference median | Engine queue max median | Engine service TTFT median | Warmup engine calls |
|---|---|---:|---:|---:|---:|
| small | cold | 258 ms | 249 ms | 647 ms | n/a |
| small | socket | 294 ms | 286 ms | 746 ms | n/a |
| small | request | 226 ms | 237 ms | 669 ms | [0, 0, 0] |
| large | cold | 520 ms | 286 ms | 1399 ms | n/a |
| large | socket | 293 ms | 364 ms | 1159 ms | n/a |
| large | request | 259 ms | 366 ms | 998 ms | [0, 0, 0] |

All measured generationless warmups reported zero engine calls. They did not run an early inference pass in these traces. Generation-side engine prompt-token accounting differs from response usage accounting; the token table above uses response usage consistently.

[Full provider events, timings, response IDs and usage](provider.json); [group summaries and matched differences](provider-summary.json). Warmup produces zero output tokens but reports input usage and incurs an additional request; no billing/free-warmup claim is made.

Two initial canaries were rejected for missing Lite-required `reasoning.context: all_turns` and `parallel_tool_calls:false`. Both failures are retained, separately from a successful three-arm canary and the main experiment: [canary 1](provider-canary.json), [canary 2](provider-canary-v2.json), [canary 3](provider-canary-v3.json). The successful canary used a two-second lead and is excluded from five-second results.

OpenAI documents this non-generating warmup and response-ID reuse, but gives no guaranteed latency saving: [official WebSocket guide](https://developers.openai.com/api/docs/guides/websocket-mode#connect-and-create-responses).

## Immediate-submit provider experiment

Six additional large-context trials, three per arm, alternating which arm runs first within each block. No deliberate lead time. Both arms begin with an unopened socket; the request arm additionally waits for its non-generating request to complete before sending the user prompt. This tests serial waiting, not cancellation or bypass of an already in-flight warmup.

| Rep | Socket only TTFT | Connection + request warmup TTFT | Extra wait |
|---|---:|---:|---:|
| 1 | 1924 ms | 2079 ms | 154 ms |
| 2 | 1955 ms | 2584 ms | 629 ms |
| 3 | 2082 ms | 2563 ms | 481 ms |

Median: socket **1955 ms**, serial request warmup **2563 ms**. The extra warmup request itself took 402–432 ms. All six outputs were correct; all three warmups reported zero engine calls and zero output tokens. Individual network/provider timings and cache variation remain in the [raw evidence](provider-zero-lead.json).

## Provenance, reproduction and cleanup

- 46 measured generation turns: 22 hosted, 18 provider with a five-second lead, six provider with immediate submit. All returned exactly `42`; hosted turns executed no tools. Three additional successful provider canary generations are excluded from comparisons. Six rejected protocol probes are retained separately. Ten successful non-generating warmups across canary/main/immediate cohorts produced no text.
- Every hosted scratch agent was deleted, then independently checked with GET: **22/22 returned 404**. [Cleanup and deployment receipts](cleanup.json). Diagnostic tails and experiment sockets were stopped. The initial hosted cohort had unchanged deployment receipts; a concurrent GitHub Actions deployment crossed the confirmation cohort. Its last two admissions ran on managed version `de592775-e274-44a9-9578-1d713c42e0ae` versus initial `e1877ce4-0170-4de6-9cf9-ccfc003b7ece`. Account, egress and connect versions also changed across that interval. The new rollout identifies commit 68a7f4edc; older components have different deployment annotations. Code differences and cold rollout effects are not controlled. Confirmation samples are retained and labeled; the headline uses the initial cohort only. [Version-bearing invocation traces](hosted-traces.json) preserve exact admission correlation.
- This pass made no tracked source edits, deployment, commit or push. Working checkout: Nanocodex `68a7f4edc39dfdd35388270160c0ecf935848658`. The original dirty checkout was preserved.
- Local load averages were captured per sample. This was a sequential live-service experiment on a busy developer Mac, not a controlled load lab; samples are insufficient for p95/confidence estimates.
- Trace correlation uses exact turn IDs, request IDs and the corresponding Durable Object. Full SQL tracing was not enabled; named-read counters must not be described as exhaustive SQL counts. Account refresh spans overlap.
- Preparation lifetime and resource use after longer pauses, failures/reconnects, settings changes, and real hosted `generate:false` reuse remain implementation validation work. Source-level context and the pinned Codex startup-prewarm inspection are in [the preceding investigation](../2026-09-16-system-e2e/warmup.md).

Harnesses used: [hosted](harness/hosted.mjs), [hosted retention/confirmation](harness/hosted-confirm.mjs), [provider](harness/provider.mjs), [tail sanitizer](harness/tail.py), [analysis](harness/analyze.py), [cleanup](harness/cleanup.mjs). They use existing local credentials without writing credential values. Paths are intentionally pinned to this developer checkout; do not run without adapting paths and confirming the target account. Provider defaults are three rotated repetitions, both contexts, five-second lead. Canary: `REPS=1 SIZES=small LEAD_MS=2000 OUTPUT=provider-canary-v3`; immediate cohort: `REPS=3 SIZES=large ARMS=socket,request LEAD_MS=0 OUTPUT=provider-zero-lead`.
