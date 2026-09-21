# Regional inference performance comparison

The live tests verify streaming delivery, but **do not establish a regional-routing speedup**. We ran the same fixed-provider workload before and after deployment (45 requests each), then an isolated global-versus-regional Jev experiment (18 classifier calls and 18 generation requests, nine matched pairs). The frozen calibration set excluded the original heldout pairs, and the routing experiment used three fresh synthetic tasks.

## Streaming result

All 45 streaming requests completed with the requested provider/model/effort and trusted API ingress matching IAD, LHR or NRT. Each delivered 8–15 nonempty text deltas, compared with one buffered delta before deployment. Median first-output lead over body completion ranged from 255 to 318 ms across the nine provider/region cells. One request's first delta and terminal event arrived at the same measured timestamp; the report preserves that exception.

These are mixed-family descriptive medians, five observations per provider/region. They measure placed-caller POST to first public output, including API routing and network time; they are not isolated model TTFT.

| API ingress | Provider | Buffered baseline ms | Streaming ms |
|---|---|---:|---:|
| IAD | Cloudflare | 1700 | 1873 |
| IAD | OpenRouter | 1290 | 1277 |
| IAD | Vercel | 2045 | 1483 |
| LHR | Cloudflare | 2112 | 1526 |
| LHR | OpenRouter | 1661 | 1896 |
| LHR | Vercel | 1951 | 1453 |
| NRT | Cloudflare | 1953 | 1812 |
| NRT | OpenRouter | 1748 | 1203 |
| NRT | Vercel | 1757 | 1206 |

The before/after windows were about 25 minutes apart. Cache hits and generated/reasoning token counts changed, so these differences cannot be attributed entirely to streaming. See [STREAMING.md](STREAMING.md) for separate short/long families, matched deltas, tail limitations and all exclusions.

## Routing result

All 18 downstream responses completed, and all region checks passed. The regional arm changed the provider in two of nine pairs. However, nine classifier binding calls failed; of the nine valid classifier responses, eight fell below the configured 0.75 confidence threshold. Only one decision was accepted; 17 used fallback behavior (including eight retained low-confidence proposals).

The nine binding failures occurred first in the campaign, followed by nine successes. Error details were not retained, so the historical failures' cause is unknown. Median outer classifier duration was 3630 ms for failed bindings and 299 ms for successful bindings. Classifier availability and overhead materially affected this run.

The median paired regional-minus-global time to first output was -119 ms, but differences ranged from -7769 to +7667 ms. This small, temporally confounded sample, dominated by fallbacks, does not support a stable latency improvement or provider ranking. There were no pairs in which both arms were accepted at the configured confidence threshold.

The isolated harness uses the production resolver with frozen API-delivery measurements as an explicitly labelled proxy for internal generation TTFT. It performs an outer Jev selection and then calls the public API, which runs its own inner routing. Its combined clock therefore includes an extra classifier compared with normal production autorouting. Wrapper origin preflight and reservation are excluded and recorded separately. It is a controlled resolver experiment, not a production end-to-end rollout A/B test. See [JEV.md](JEV.md) and [raw routing results](jev/results.jsonl).

## Reproducibility

- [Protocol and budget accounting](PROTOCOL.md)
- [Paired fixed-provider analysis](comparison.json)
- [Frozen routing plan](jev-plan/plan.json), [calibration](jev-plan/calibration.json), and [source hashes](jev-plan/source-hashes.json)
- [Routing receipt](jev/receipt.json) and [summary with probabilities](jev/summary.json)
- [Original runner cleanup](runner-cleanup.json)

`node scripts/regional-inference-bench/analyze.mjs` regenerates the fixed-provider report offline. The benchmark scripts retain failed and excluded attempts and refuse implicit retries. These measurements support retaining opt-in routing while its latency benefit remains unproven; no routing policy was tuned on these heldout results.
