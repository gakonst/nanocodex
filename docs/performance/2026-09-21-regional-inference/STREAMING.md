# Buffered baseline versus production streaming

Generated offline by `node scripts/regional-inference-bench/analyze.mjs` from this checkout. [Protocol](PROTOCOL.md), [baseline rows](baseline/results.jsonl), [streaming rows](streaming/results.jsonl), and [complete paired comparison](comparison.json) contain the reproducible evidence. The JSON includes source hashes, all 45 pairs, reconstructed synthetic prompt hashes, nearest-rank p50/p95, per-pair timing and token deltas, completion counts and origin checks. No inference calls are made by the analyzer.

Both stages completed 45/45 and 45/45 calls, respectively; errors 0/0, incomplete 0/0. Each uses the same explicit Luna low candidate across three providers and three placed callers. Short prompts have **n=3 per cell** (pairs 0/2/4); long-prefix prompts have **n=2 per cell** (pairs 1/3). There are 45 exact matched pairs and 0 recorded assertion mismatches.

Baseline source: `89439eb`; first-to-last request starts 2026-09-21T22:59:47.932Z–2026-09-21T23:01:32.871Z. Production streaming source: `ce18887f:5d343432-7e2f-440d-a720-f60bff2add7f`; starts 2026-09-21T23:25:07.248Z–2026-09-21T23:26:52.182Z. These sequential windows are about 25 minutes apart. The streaming production deployment is ce18887f / 5d343432-7e2f-440d-a720-f60bff2add7f; the earlier aborted buffered attempt is excluded.

## What the clocks measure

First meaningful public delta is elapsed time from the placed runner's outbound POST until the first non-whitespace public text or tool-argument delta. Headers, creation, reasoning and empty deltas do not qualify. It includes routing, network, queueing and generation; **it is not isolated model TTFT**. Controller-to-runner network time is excluded. Total completion is body exhaustion (`total_ms`); terminal-event time (`completed_ms`) is retained separately in JSON.

All 45 baseline responses declared `buffered`, despite SSE requests: public delta counts ranged 1–1, with first delta before terminal in 0/45 calls. All 45 streaming responses declared `streaming`, with 8–15 meaningful deltas and first delta before terminal in 44/45 calls (before body exhaustion in 45/45). This is observed incremental public output versus buffered SSE replay. Streaming delivery alone does not guarantee an earlier first public delta in every request. The streaming exception with no observed gap before terminal was vercel/eu-west-1/long-prefix/pair-3 (first and terminal 1453 ms, body end 1609 ms, 10 deltas); event multiplicity and the delivery header do not establish a separately timed arrival for every event in that request.

## Family comparisons

B → S means baseline → streaming. Negative changes mean lower latency. Δ p50 is the difference (or ratio for %) between stage medians; paired Δ p50 is the median of individual matched after-minus-before differences, which need not equal the difference of medians. Quantiles use nearest rank `ceil(p*n)`, so n=2 p50 is the lower observation, not an averaged midpoint. Each cell completed n/n in both stages.

| Provider | Caller region | Family | n | First delta p50 B → S ms | Δ p50 % | Paired Δ p50 ms | Total p50 B → S ms | Δ p50 % | Paired total Δ p50 ms |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| cloudflare | us-east-1 | short | 3 | 1558 → 1873 | +20.2% | +403 | 1558 → 2128 | +36.6% | +691 |
| cloudflare | us-east-1 | long-prefix | 2 | 1700 → 1559 | -8.3% | -141 | 1700 → 1838 | +8.1% | +138 |
| cloudflare | eu-west-1 | short | 3 | 2112 → 1591 | -24.7% | -521 | 2112 → 1899 | -10.1% | -213 |
| cloudflare | eu-west-1 | long-prefix | 2 | 1425 → 1113 | -21.9% | -679 | 1425 → 1478 | +3.7% | -374 |
| cloudflare | ap-northeast-1 | short | 3 | 1953 → 1812 | -7.2% | -141 | 1953 → 2131 | +9.1% | +178 |
| cloudflare | ap-northeast-1 | long-prefix | 2 | 1785 → 1462 | -18.1% | -726 | 1785 → 1744 | -2.3% | -444 |
| openrouter | us-east-1 | short | 3 | 1882 → 1073 | -43.0% | -809 | 1882 → 1291 | -31.4% | -591 |
| openrouter | us-east-1 | long-prefix | 2 | 1031 → 1559 | +51.2% | +528 | 1031 → 1849 | +79.3% | +818 |
| openrouter | eu-west-1 | short | 3 | 1661 → 1896 | +14.1% | +235 | 1661 → 2214 | +33.3% | +553 |
| openrouter | eu-west-1 | long-prefix | 2 | 1351 → 1364 | +1.0% | +13 | 1351 → 1767 | +30.8% | +416 |
| openrouter | ap-northeast-1 | short | 3 | 1663 → 1175 | -29.3% | -488 | 1663 → 1472 | -11.5% | -191 |
| openrouter | ap-northeast-1 | long-prefix | 2 | 1748 → 1203 | -31.2% | -545 | 1748 → 1504 | -14.0% | -244 |
| vercel | us-east-1 | short | 3 | 2045 → 1483 | -27.5% | -562 | 2045 → 1710 | -16.4% | -335 |
| vercel | us-east-1 | long-prefix | 2 | 1763 → 798 | -54.7% | -2295 | 1763 → 1084 | -38.5% | -2009 |
| vercel | eu-west-1 | short | 3 | 1951 → 1370 | -29.8% | -319 | 1951 → 1678 | -14.0% | -11 |
| vercel | eu-west-1 | long-prefix | 2 | 1337 → 1453 | +8.7% | -1006 | 1337 → 1609 | +20.3% | -850 |
| vercel | ap-northeast-1 | short | 3 | 1757 → 1259 | -28.3% | -551 | 1757 → 1523 | -13.3% | -285 |
| vercel | ap-northeast-1 | long-prefix | 2 | 1446 → 1082 | -25.2% | -820 | 1446 → 1387 | -4.1% | -515 |

First-delta p50 fell in 13/18 family cells; total p50 fell in 10/18. These are cell counts, not a pooled latency or a statistical significance test. First-delta extremes: vercel/us-east-1/long-prefix: -965 ms (-54.7%); openrouter/us-east-1/long-prefix: +528 ms (+51.2%). Total-completion extremes: vercel/us-east-1/long-prefix: -679 ms (-38.5%); openrouter/us-east-1/long-prefix: +818 ms (+79.3%). All matched per-request gains and regressions remain in comparison.json.

## Mixed descriptive medians: n=5 only

Each row below mixes exactly three short and two long-prefix prompts. These n=5 medians describe this fixed workload mix; they do not establish a homogeneous latency distribution, a provider winner, or an optimal caller region. Interpret them alongside the family table.

| Provider | Caller region | Family | n | First delta p50 B → S ms | Δ p50 % | Paired Δ p50 ms | Total p50 B → S ms | Δ p50 % | Paired total Δ p50 ms |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| cloudflare | us-east-1 | mixed | 5 | 1700 → 1873 | +10.2% | +205 | 1700 → 2128 | +25.2% | +310 |
| cloudflare | eu-west-1 | mixed | 5 | 2112 → 1526 | -27.7% | -521 | 2112 → 1831 | -13.3% | -213 |
| cloudflare | ap-northeast-1 | mixed | 5 | 1953 → 1812 | -7.2% | -141 | 1953 → 2079 | +6.5% | +178 |
| openrouter | us-east-1 | mixed | 5 | 1290 → 1277 | -1.0% | +83 | 1290 → 1596 | +23.7% | +402 |
| openrouter | eu-west-1 | mixed | 5 | 1661 → 1896 | +14.1% | +235 | 1661 → 2214 | +33.3% | +553 |
| openrouter | ap-northeast-1 | mixed | 5 | 1748 → 1203 | -31.2% | -488 | 1748 → 1504 | -14.0% | -191 |
| vercel | us-east-1 | mixed | 5 | 2045 → 1483 | -27.5% | -562 | 2045 → 1710 | -16.4% | -335 |
| vercel | eu-west-1 | mixed | 5 | 1951 → 1453 | -25.5% | -319 | 1951 → 1678 | -14.0% | -11 |
| vercel | ap-northeast-1 | mixed | 5 | 1757 → 1206 | -31.4% | -551 | 1757 → 1523 | -13.3% | -285 |

## Generated tokens and cache state

The following are sums across each cell, not per-request medians. Output tokens are provider-reported generated usage (including reasoning where reported), not a measured count of visible text tokens. Cache “hits” count rows with positive provider-reported cached tokens. Missing token fields remain unknown in JSON.

| Provider | Caller region | Family | n | Output tokens B → S | Reasoning tokens B → S | Cache-hit rows B → S | Cached tokens B → S |
|---|---|---|---:|---:|---:|---:|---:|
| cloudflare | us-east-1 | short | 3 | 79 → 74 | 31 → 28 | 0 → 0 | 0 → 0 |
| cloudflare | us-east-1 | long-prefix | 2 | 30 → 30 | 0 → 0 | 1 → 2 | 5035 → 10070 |
| cloudflare | eu-west-1 | short | 3 | 42 → 65 | 0 → 18 | 0 → 0 | 0 → 0 |
| cloudflare | eu-west-1 | long-prefix | 2 | 48 → 57 | 16 → 24 | 1 → 2 | 5035 → 10070 |
| cloudflare | ap-northeast-1 | short | 3 | 60 → 58 | 12 → 12 | 0 → 0 | 0 → 0 |
| cloudflare | ap-northeast-1 | long-prefix | 2 | 37 → 60 | 0 → 31 | 2 → 2 | 10070 → 10070 |
| openrouter | us-east-1 | short | 3 | 43 → 42 | 0 → 0 | 0 → 0 | 0 → 0 |
| openrouter | us-east-1 | long-prefix | 2 | 30 → 50 | 0 → 16 | 0 → 2 | 0 → 10070 |
| openrouter | eu-west-1 | short | 3 | 60 → 60 | 12 → 12 | 0 → 0 | 0 → 0 |
| openrouter | eu-west-1 | long-prefix | 2 | 32 → 54 | 0 → 22 | 2 → 2 | 10070 → 10070 |
| openrouter | ap-northeast-1 | short | 3 | 64 → 42 | 18 → 0 | 0 → 0 | 0 → 0 |
| openrouter | ap-northeast-1 | long-prefix | 2 | 49 → 31 | 16 → 0 | 2 → 2 | 10070 → 10070 |
| vercel | us-east-1 | short | 3 | 64 → 41 | 15 → 0 | 0 → 0 | 0 → 0 |
| vercel | us-east-1 | long-prefix | 2 | 62 → 37 | 31 → 0 | 1 → 2 | 5035 → 10070 |
| vercel | eu-west-1 | short | 3 | 60 → 55 | 12 → 12 | 0 → 0 | 0 → 0 |
| vercel | eu-west-1 | long-prefix | 2 | 44 → 72 | 12 → 40 | 1 → 2 | 5035 → 10070 |
| vercel | ap-northeast-1 | short | 3 | 47 → 42 | 0 → 0 | 0 → 0 | 0 → 0 |
| vercel | ap-northeast-1 | long-prefix | 2 | 35 → 34 | 0 → 0 | 2 → 2 | 10070 → 10070 |

For accounting across the entire fixed 45-call workload (not a latency aggregate), input tokens were 91737 → 91737, output tokens 886 → 904 (+18), and reasoning tokens 175 → 215. Long-prefix cache-hit rows were 12 → 18 of 18, and cached-token sums 60420 → 90630. Short-prompt cache-hit rows were 0 → 0 of 27. Cached usage is known for 45/45 baseline and 45/45 streaming rows. The long repeated prefix offered reuse in both stages, but observed cache state changed. Generated and reasoning token differences also confound total completion and first-delta timing; no token-normalized causal estimate is claimed.

## Location evidence and assertion checks

Runner placement hints were aws:us-east-1, aws:eu-west-1 and aws:ap-northeast-1. Named authenticated runner-response placements corroborate IAD, LHR and NRT. Baseline placement counts: {"remote-":22,"remote-IAD":6,"remote-LHR":10,"remote-NRT":7}; streaming: {"remote-":8,"remote-IAD":10,"remote-LHR":15,"remote-NRT":12}. Named execution evidence is missing for 22/45 baseline and 8/45 streaming requests (`remote-` is not a named colo). Do not replace those missing observations with inferred per-request execution locations.

The controller ingress was {"SJC":45} baseline and {"SJC":45} streaming; SJC is not runner execution geography. The production API ingress header is absent in all baseline rows and reports {"IAD":15,"LHR":15,"NRT":15} in streaming. API Ray suffix counts are {"IAD":15,"LHR":15,"NRT":15} baseline and {"IAD":15,"LHR":15,"NRT":15} streaming. API ingress is not provider compute geography. Explicit route origin is missing in 45/45 baseline and 45/45 streaming rows; this fixed-provider campaign cannot verify classifier-origin propagation.

The analyzer checks named runner placements against hints, API ingress against expected colo and Ray, exact pairing, requested/returned provider and effort, stage delivery mode, and family/split identities. Recorded mismatches: **0**. Missing evidence is counted separately and is not a passed origin assertion. HTTP/protocol errors: **0**. No contradictory observed origin/placement assertions were found within these checks.

## Limits, exclusions and budget

This is a low-N sequential before/after observation. Time, load, cache state, routing overhead and generated/reasoning token variation remain confounded with delivery mode and deployment changes. Per-family p95 (and mixed n=5 p95) equals the maximum; neither robust population tails nor confidence in a regional ranking follows. No isolated model TTFT, randomized causal speedup, task-success probability or regional router quality claim is supported.

The four local calls and initial failed regional attempts are excluded. The earlier post-deployment attempt saved 24 **buffered** rows and conservatively counted 27 attempts; its preserved data in [postdeploy-buffered-aborted](postdeploy-buffered-aborted/results.jsonl) is excluded from the streaming arm. Campaign accounting at streaming close is **99 prior + 45 = 144 attempts**, leaving 56 of the 200-attempt cap before any separate Jev experiment. The incremental budget ceiling is $5; these records do not contain an actual-spend receipt, and output-token counts do not establish dollars spent.

This report consumes only the two completed fixed-provider matrices. Calibration/heldout labels are preserved per pair; descriptive reporting of the existing matched rows does not select or tune on the heldout rows. The separate active global-versus-regional Jev experiment and future samples are outside this analysis and belong in JEV.md.
