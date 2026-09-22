# Auto routing and stream timing, 2026-09-22

Baseline evidence is complete; no production deployment or post-deployment comparison was performed by this harness. Calls originate from the Mac development host. Network location, cache, model output, and sequential load confound before/after timings. These small samples do not establish tail latency or route quality.

## Production baseline

Nine bounded API attempts, stream=true, store=false, max_output_tokens=256, no retries. Seven completed; one Cloudflare Sol 502; one invalid harness model ID (400) excluded from provider results. Original raw rows and reservations remain unchanged in [baseline](baseline/results.jsonl) and [family checks](baseline-families/results.jsonl). The invalid `cloudflare:zai-org/glm-5:low` request was corrected in the later catalog-validated family suite to `@cf/zai-org/glm-5.3:low`. Scripts now reject any explicit model absent from the live catalog before inference.

| Case | Router ms | First reasoning ms | First text ms | Total ms | Deltas in earlier chunks than terminal |
|---|---:|---:|---:|---:|---:|
| auto code | 874 | none | 2037 | 2769 | 57 |
| auto math | 205 | 2392 | 3542 | 4816 | 166 |
| auto analysis | 228 | none | 1745 | 4034 | 75 |
| explicit Cloudflare Luna | 0 | none | 3222 | 4922 | 71 |
| explicit Workers AI GLM | 0 | none | 1103 | 3513 | 65 |
| explicit OpenRouter Kimi | 0 | 1360 | 2210 | 2662 | 7 |
| explicit OpenRouter MiMo | 0 | 3871 | 4645 | 10567 | 63 |

All successful calls show actual earlier network reads, not merely delta events synthesized in the terminal chunk. Public API buffering is therefore not reproduced by these cases. They do not verify the managed app's WASM/UI projection. `none` means no matching reasoning event observed, not no internal reasoning.

All three auto routes reported `selection:fallback`. Initial projection did not retain route diagnostics, so those rows alone cannot distinguish low-confidence proposals from classifier errors. Harness now preserves the public route diagnostics for post-deployment runs. The distinct families and candidates are suggestive of valid classification but are not substituted for missing diagnostics.

## Actual Jev binding comparison

The isolated Wrangler `getPlatformProxy` uses only an AI binding with `remote:true`; it does not deploy a Worker or load production app state. It calls actual `env.AI.run('typesafe/jev', payload)`. The old source is pinned to `18452080c9e463ddef8ab56efe47ef54fe163cf8`; compact source SHA256s are in each manifest. Both use the full eligible 67-candidate catalog, including native candidates. Production API catalog has 55 candidates because native subscription routes are excluded, so these resolver measurements are not production API route timings.

For each case, baseline then compact are called once. Resolver completion and underlying binding settlement are captured separately. No retries are made; reservations precede dispatch. [Family raw results](jev-binding/results.jsonl), [manifest](jev-binding/manifest.json).

| Prompt | Baseline binding ms | Compact binding ms | Candidate in both |
|---|---:|---:|---|
| code | 1220 | 1415 | Workers AI GLM high |
| math | 3572 | 367 | Workers AI GLM low |
| analysis | 979 | 327 | Workers AI GLM medium |

Both arms: 3/3 classifier successes, no timeout, all low candidate confidence and `fallback_basis:valid_proposal`. Compact bytes for code fell 50,124 → 12,786. Compact success latency spans 327–1415 ms; the 1.5-second cap used here leaves little margin for the slowest observed result. The routing agent subsequently raised the deadline to 2 seconds and added cancellation handling; the measured family compact source is the earlier 1.5-second revision, with unchanged ranking/prompt semantics. Neither “instant” Jev nor a safe latency tail is established. The compact arm is slower for the code pair. There is no causal latency-speedup claim from three sequential pairs.

## Paired preference checks

[Exact synthetic prompts and policies](jev-preferences/prompts.json), [raw results](jev-preferences/results.jsonl), [manifest](jev-preferences/manifest.json). Another six classification calls, no downstream generations. All six classifier outcomes are success; all candidate confidences are low and all routes retain valid proposals.

| Preference case | Baseline ms / candidate | Compact ms / candidate |
|---|---|---|
| cheap, careful repair | 1461 / Workers AI GLM medium | 918 / OpenRouter GLM medium |
| explicit completion=100, cost=0, duration=0 overriding urgent cheap prompt | 824 / native Astra high | 500 / Workers AI GLM high |
| explicitly do not prioritize speed or cheapest model | 590 / native Astra high | 523 / native Astra high |

The negated-speed pair agrees, and all deliberate cases select medium/high effort. The explicit override changes model family across arms; cheap/careful changes provider. These results show that the smaller prompt is accepted with preference inputs but do not establish semantic equivalence, instruction adherence, or task-success quality. No generated task outcome was graded. Low confidence makes these provisional selections, not validated model recommendations.

## Reproduction and boundaries

Auth is read only into process memory from the pre-existing operator key batch (`AUTO_BENCH_KEY_FILE` / `AUTO_BENCH_KEY_INDEX`, default index 8). Wrangler uses its existing login. No credential values, generated text, or raw error bodies are retained. Exact case reservation, source hashes, route metadata, usage, first reasoning/text times, read chunks, and terminal timing are retained.

From repository root:

```sh
node --test scripts/auto-router-bench/measure.test.mjs
node scripts/auto-router-bench/run.mjs postdeploy docs/performance/2026-09-22-auto-router/postdeploy
node scripts/auto-router-bench/run.mjs postdeploy-families docs/performance/2026-09-22-auto-router/postdeploy-families families
node scripts/auto-router-bench/jev-live.mjs docs/performance/2026-09-22-auto-router/jev-new families
node scripts/auto-router-bench/jev-live.mjs docs/performance/2026-09-22-auto-router/jev-preferences-new preferences
```

Each output directory must be new. API baseline suite makes at most six attempts; family suite at most three. Each Jev suite makes at most six classification calls. Do not rerun a directory after an uncertain request; preserved reservations count as attempts. `AUTO_BENCH_BASELINE_REVISION` can explicitly override the pinned historical comparison. `AUTO_BENCH_BASE_URL` can select a reviewed HTTPS preview/API endpoint.

Validation: three meaningful harness tests pass: reasoning before terminal across split UTF-8/SSE framing, same-chunk deltas not counted as incremental transport, and errors/missing terminal retained as failures. No additional production builds were run by this research agent. Post-deployment and managed-app observations remain outstanding.
