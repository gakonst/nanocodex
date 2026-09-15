# Matched Agents / Responses / Nanocodex benchmark

11 September 2026. This directory accompanies the
[codex-rs refactor review](../../docs/CODEX_RS_REFACTOR_REVIEW.md).

The experiment compares the exact prompt:

> Return only the sum of 17 and 25 as decimal digits.

Every path receives the same supplied instruction:

> Follow the user request precisely. Keep the final answer short.

The full matrix has four models (Luna, Terra, Sol, Astra), low reasoning, Standard
and Fast processing, four paths, and three repetitions per cell: 96 generations.
Four preliminary Luna/Standard pilot generations are separate from the matrix.

## Results

The matrix ran from 09:00:02 to 09:11:20 UTC. **96/96 generations returned `42`**;
all trials recorded visible TTFT and usage, and all 24 hosted sessions were
deleted. Including the pilot: 100 correct generations and 25 deleted sessions.
No Nanocodex trial called a tool. Nothing was deployed.

Median visible TTFT in seconds (three observations per cell):

| Model | Tier | Responses HTTP | Responses WS | Nanocodex Node | OpenAI Agents |
| --- | --- | ---: | ---: | ---: | ---: |
| Luna | Standard | 1.471 | 1.119 | 1.965 | 9.372 |
| Luna | Fast | 0.779 | 0.622 | 3.837 | 9.535 |
| Terra | Standard | 1.417 | 0.933 | 3.031 | 8.625 |
| Terra | Fast | 1.069 | 0.829 | 2.170 | 10.013 |
| Sol | Standard | 1.363 | 3.068 | 4.765 | 9.676 |
| Sol | Fast | 0.817 | 0.801 | 2.010 | 8.566 |
| Astra | Standard | 2.640 | 1.559 | 3.303 | 10.278 |
| Astra | Fast | 1.422 | 1.094 | 3.063 | 10.408 |

WS timings exclude connection establishment; fresh Nanocodex turns include it.
Nanocodex Node is the local runtime, not our deployed managed Worker. Hosted
Agents includes session startup. These are observed paths, not an isolated
measurement of provider versus harness overhead.

Nanocodex Node had lower median TTFT than hosted Agents in all eight paired
configurations. Fast reduced direct Responses medians in this sample but did not
consistently reduce fresh agent latency. Outliers matter: one Astra/Fast Nano
turn took 29.234 seconds to complete despite that cell's 3.242-second median.
The chart and full table retain observed ranges rather than hiding that sample.

The harnesses sent substantially more context: median input usage was 6,018
tokens for Nano and 6,077 for Agents on the 5.6 models; Astra used 6,570 and
7,116 respectively. Direct Responses used 36. All cells had median output usage
of five tokens. This supports investigating base-context cost separately from
latency; it does not establish a quality difference.

Estimated model subtotal on reported usage: **$2.1361–$2.3810** for the matrix,
or **$2.1389–$2.3841** including the pilot. The interval reflects missing cache-write
counts in Agents usage, not a bound on the account bill. Pricing assumptions and
other limitations are below.

## Paths and controls

| Path | Setup and execution |
| --- | --- |
| Responses HTTP | Node fetch with SSE; its connection pool can reuse HTTP connections. Timings include request/response transport. Connection establishment is not separately observable here. |
| Responses WebSocket | One socket per model/tier, reused across repetitions. Each response has fresh input, with no `previous_response_id`. Socket setup is recorded separately and excluded from send-to-text/completion timings. |
| Nanocodex Node | Fresh Rust/WASM agent for every sample, using `nanocodex/node`, direct OpenAI WebSocket transport, no prewarm, an empty temporary workspace, and no application tools or MCP. Agent creation is separate; turn timings include the fresh connection. |
| OpenAI Agents | Fresh session with `environment.type: none`, no configured tools, initial input and SSE in one create request. Timings include hosted session startup. Retrieve turn usage/items after terminal, then delete the session. |

The Nanocodex path runs our actual local harness, **not the deployed managed
Worker**. It excludes account routing, Durable Object admission, history storage,
webhook delivery and artifact publication. It cannot establish production managed
service latency. OpenAI Agents necessarily includes its hosted control plane.

Only one measured generation is in flight at a time. Within each repetition,
model/tier/path order is shuffled with seed `20260911`. There are no automatic
application-level generation retries; Nanocodex's own retry telemetry remains in
the wire capture. Requests have a 120-second observation deadline. Direct
Responses caps output at 128 tokens; neither hosted Agents nor the tested public
Nanocodex creation API exposes that same cap. This was a short, fixed task, not a
hard billing-cap mechanism.

The supplied prompt/instructions match, but built-in harness prompts and tool
catalogs do not. Nanocodex retains its default orchestration/tool machinery even
with an empty application tool list. No assumption of token-identical requests
is made. The analyzer verifies the supplied prompt and instruction occur in every
recorded Nanocodex generation request. Input usage captures the actual overhead.

## Metrics and evidence

- [TABLE.md](TABLE.md): every model/tier/path cell, including success counts,
  median visible TTFT, completion, observed completion range, tokens and cost.
- [summary.json](summary.json): also includes first-event timing, TTFT ranges,
  first-text-to-completion gap, connection timing, cache reads/writes, reasoning
  tokens, reported tiers and failures.
- [measurements.json](measurements.json): per-trial timings, raw usage objects,
  model-call records, requested/reported tiers, final output and cleanup receipts.
- [pilot-summary.json](pilot-summary.json): the four preliminary calls, including
  the pilot's TTFT instrumentation correction; excluded from matrix statistics.
- [latency.svg](latency.svg) / [latency.png](latency.png): medians and observed
  ranges. Whiskers are not confidence intervals.
- `matrix.json` and `pilot.json`: verbose local wire captures, intentionally not
  committed. Encrypted reasoning blobs and response obfuscation are omitted from
  the full matrix capture. Credentials are never recorded.

Visible TTFT is the first nonempty text delta: provider `output_text.delta` or
Nanocodex `assistant.delta.text`. A response-created event and a reasoning item
are not visible TTFT. Completion is the terminal response/root-turn event, or
Nanocodex's resolved `turn.result()`. Post-turn usage polling, item retrieval and
session deletion are excluded. Node agent setup and WebSocket setup are separately
recorded; `inclusive_ttft_median_ms` adds those explicit setup durations where
applicable. This does not make all samples equally cold.

Three samples per cell support descriptive medians and observed ranges, not p95,
p99, a latency SLA, or reliable statistical significance. An arithmetic answer is
also unsuitable for generation throughput: billed output tokens include reasoning
and protocol tokens, while visible output is just `42`. We do not manufacture a
meaningful tokens/second figure from that difference.

## Cost interpretation

[OpenAI prices](https://developers.openai.com/api/docs/pricing) were checked on
11 September. Per million tokens, Standard ordinary input / cache read / cache
write / output rates used are:

| Model | Input | Cache read | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| Luna | $0.20 | $0.02 | $0.25 | $1.20 |
| Terra | $2 | $0.20 | $2.50 | $12 |
| Sol | $4 | $0.40 | $5 | $20 |
| Astra | $10 | $1 | $12.50 | $50 |

Fast uses 2x rates. Returned `priority` and `fast` are treated as the documented
aliases. When the provider reports its processed tier, use that value; Agents
turn usage does not report it, so those estimates assume the requested tier.

Cache reads/writes are subsets of input, not additional input tokens. Unknown
usage stays unknown. When usage lacks cache-write counts, the cost interval prices
all reported noncached input at ordinary rates for its low endpoint and at
cache-write rates for its high endpoint. That interval covers only the missing
write premium on **reported tokens**, not omitted/delayed usage or other charges.
It is neither a billing reconciliation nor an upper bound on the account bill.
[Agents usage limitations](https://developers.openai.com/api/docs/guides/agents-api/observability)

Cache state is observed, not controlled. Fresh sessions can still hit a provider
cache. Inspect the recorded cache categories before attributing a cost difference
to the harness itself. The earlier September 10 HTTP trials used fresh Python
urllib connections; their timings should not be treated as directly comparable
to this Node connection-pool experiment.

## Reproduce

Use the PR's built SDK/WASM on a machine with Node and its workspace dependencies.
Supply `OPENAI_API_KEY` through your process environment; the script sends it only
to `api.openai.com`.

```sh
node js/nanocodex/scripts/agents-comparison.bench.mjs --output=output/new-run/matrix.json
node js/nanocodex/scripts/agents-comparison.analyze.mjs output/new-run/matrix.json
uv run --no-project --with matplotlib python js/nanocodex/scripts/agents-comparison.plot.py output/new-run/summary.json
```

A small pilot uses `--models=gpt-5.6-luna --tiers=default --repetitions=1` and a
separate output filename. The CLI bounds repetitions to 1–5. Cleanup operates
only on session IDs created by the run.

Runtime: Node v26.8.1, macOS arm64. Nanocodex source before the benchmark-only
changes: `55d9b53a1bc568d443df40b6f5de3424bf93fd9d`, using the release WASM built
for PR #307. Location/VPN routing was not controlled. The arithmetic checks do not
measure coding quality, long-task reliability, reconnect recovery or tool safety.
