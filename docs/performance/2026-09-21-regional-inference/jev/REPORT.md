# Actual Jev regional comparison

Executed 18 outer Jev classifications and 18 downstream Luna-low API requests (36 attempts), with no retries. All 18 downstream calls completed and all 18 matched both the configured server-owned API ingress header and Ray suffix. Nine Jev binding calls failed; nine returned actual valid choice distributions. Of these, eight were below the unchanged 0.75 confidence threshold and retained the proposal under the configured fallback policy; one was accepted. Thus 17 routes were fallbacks and one was a prior selection. This does not establish a regional routing improvement. Raw failures and null probabilities are retained.

Both arms used the identical 27-row frozen dataset (streaming pairs 0–2 only), SHA-256 `408b7a66ff0b27b0de01342f1e869577ba0b397288ef6d6e6337306f5a0ffdbd`. All global projections used deployment_global cohorts (n=9 per candidate), and regional projections used client_ingress cohorts (n=3 per candidate). The source field labels API output delivery as an experimental TTFT proxy; it is not internal generation TTFT.

The accepted case was NRT / short analysis / regional: OpenRouter choice probability 0.97, Vercel 0.03, Cloudflare 0, candidate confidence 0.95. Its global counterpart proposed OpenRouter with choice probabilities 0.65/0.34/0.01 and confidence 0.47 (fallback). Downstream API delivery was 1136 versus 1690 ms; outer-router-plus-delivery was 1407 versus 1894 ms. This single pair cannot establish a causal effect. Probability and confidence are distinct fields and neither predicts task success.

| Region | Fresh task | Global selection/status | Regional selection/status | Global API delivery ms | Regional API delivery ms | Regional − global ms |
|---|---|---|---|---:|---:|---:|
| IAD | short-code | openrouter / unavailable_or_invalid | openrouter / unavailable_or_invalid | 4415 | 4103 | -312 |
| IAD | short-math | openrouter / low | openrouter / unavailable_or_invalid | 1884 | 5357 | 3473 |
| IAD | short-analysis | openrouter / low | openrouter / low | 945 | 941 | -4 |
| LHR | short-code | openrouter / unavailable_or_invalid | openrouter / unavailable_or_invalid | 4103 | 8538 | 4435 |
| LHR | short-math | openrouter / unavailable_or_invalid | openrouter / low | 8490 | 4027 | -4463 |
| LHR | short-analysis | openrouter / low | cloudflare / low | 1237 | 1736 | 499 |
| NRT | short-code | openrouter / unavailable_or_invalid | openrouter / unavailable_or_invalid | 6107 | 4806 | -1301 |
| NRT | short-math | vercel / low | openrouter / unavailable_or_invalid | 2444 | 6498 | 4054 |
| NRT | short-analysis | openrouter / low | openrouter / accepted | 1690 | 1136 | -554 |

The first nine chronological binding calls failed and later calls returned distributions. This temporal split and changing API-internal routing overhead are material confounders; the sanitized binding evidence does not establish their underlying cause. API `route.router_duration_ms` is disclosed in every downstream receipt. Outer combined delivery excludes protected-wrapper HEAD preflight, durable reservation, and controller network time; separate preflight timing is retained. Three response placement headers lacked a named execution colo; all API-ingress headers/Rays were known.

Calibration pools two short and one long-prefix task per provider/region, while fresh tasks are all short. There are three tasks per arm/region, sequential arm order with alternating phase, no repeated runs to quantify classifier stochasticity, and no verified task correctness scoring. Completion is protocol completion. Do not pool mixed provider choices or interpret tail statistics as population estimates. No long-prompt bypass was exercised because these new prompts are short; invalid/unavailable classifications and low-confidence fallbacks remain explicit.

Validation: five focused tests passed, covering heldout exclusion/freshness, actual resolver origin projections, spoof rejection, durable duplicate reservations, and actual resolver-to-downstream timing/low-confidence evidence. All three Wrangler dry-run bundles passed. Initial deployment was definitively rejected because one calibration text binding exceeded Cloudflare’s 5.1 kB limit; splitting unchanged bytes into smaller bindings resolved it without inference attempts. Three isolated deployments and secret provisioning then succeeded. Production source was not modified.

Budget: prior upper bound144 +36 experiment attempts =180 of200, leaving20. This counts outer Jev invocations plus downstream API attempts; API-internal classifier activity is part of each API request and its timing is disclosed. $0.54 was reserved at the campaign’s conservative $0.015 per counted attempt; no actual billing total is asserted.

Provider choice changed in 2/9 matched global/regional pairs (NRT math: Vercel→OpenRouter; LHR analysis: OpenRouter→Cloudflare). Both changes involve fallback selections, not two accepted decisions. All three isolated Jev Workers were deleted successfully after receipt preservation. Parent-authorized optional classifier-only followup arrived after cleanup; no redeployment or diagnostic inference was performed. Historic binding errors were sanitized before capture, so their underlying cause remains unknown.
