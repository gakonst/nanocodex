# Jev preference smoke, 2026-09-21

This bounded local test made 12 calls to typesafe/jev through Wrangler getPlatformProxy with remote AI bindings. Existing authentication worked; credentials were neither read nor printed. No candidate provider generated an answer. Six synthetic cases were evaluated once with the 15 native candidates and once with all 45 candidates (provider availability simulated for selection only). All 12 returned valid answers and all 12 persisted choices were reused after a simulated restart. A Wrangler remote-connection warning appeared during the run, but every case returned an answer.

| Candidates | Case | Jev proposal and retained choice | Confidence | Selection |
| --- | --- | --- | --- | --- |
| 15 | cheap-prompt | @cf/zai-org/glm-5.3:low | 0.78 | prior |
| 15 | thorough-prompt | gpt-6-astra:high | 0.78 | prior |
| 15 | explicit-economy | @cf/zai-org/glm-5.3:low | 0.43 | fallback |
| 15 | explicit-completion | gpt-6-astra:high | 0.30 | fallback |
| 15 | negated-cheap | gpt-6-astra:high | 0.84 | prior |
| 15 | soft-target | @cf/zai-org/glm-5.3:low | 0.67 | fallback |
| 45 | cheap-prompt | openrouter:openai/gpt-5.6-luna:low | 0.41 | fallback |
| 45 | thorough-prompt | gpt-6-astra:high | 0.36 | fallback |
| 45 | explicit-economy | openrouter:openai/gpt-5.6-luna:low | 0.29 | fallback |
| 45 | explicit-completion | @cf/zai-org/glm-5.3:medium | 0.07 | fallback |
| 45 | negated-cheap | gpt-6-astra:high | 0.43 | fallback |
| 45 | soft-target | openrouter:openai/gpt-5.6-luna:low | 0.42 | fallback |

## Policy change and boundaries

Policy version jev-direct-v3 keeps min_confidence at 0.75. Its configurable low_confidence_fallback setting defaults to proposed: valid eligible proposals below threshold are retained as unmeasured fallbacks. The default threshold labels uncertainty while still using the proposal; it does not block the proposed model. The frontier setting restores the old replacement behavior. Audit confidence_status and fallback_basis distinguish accepted confidence, a retained proposal, and an invalid/unavailable-output fallback. No probabilities were aggregated: efforts have different behavior, and provider routes can have different costs and latency. Missing prices do not imply free service.

Nine of twelve new cases were below the threshold (all six with 45 candidates). Six would have had a different model/effort under the old frontier replacement rule: native explicit-economy and soft-target, plus 45-choice cheap-prompt, explicit-economy and soft-target are economical choices; the 45-choice explicit-completion also differs. See the rows for exact choices; this is a counterfactual on the same observations, not an A/B comparison.

Malformed, unavailable, ineligible or missing output still uses the conservative eligible fallback. Unsupported modalities filter out GLM. Oversized input avoids Jev. A low-confidence proposal never carries a measured estimate and cannot satisfy min_success_rate, even if matching measurements exist. Eligibility and pinning remain unchanged.

## Previous evidence

The 2026-09-20 preference-probe-initial.json had four of six decisions below 0.75. Cheap-prompt chose GLM low at 0.51 confidence (probability 0.55; GLM medium 0.23) but was replaced with Astra high. The later preference-probe-report.json again had four of six below threshold: explicit-economy chose GLM low at 0.60 confidence (probability 0.62) but was replaced with Astra high. Those historical files tested only 15 candidates, not 45. Candidate confidence differed from the probability attached to the selected choice; neither is calibrated task success.

## Validation and limitations

Deterministic policy tests cover the unchanged threshold, explicit frontier fallback, retained economy proposals, malformed answers, provider eligibility, measured-success refusal, and concurrent/restart pinning. The checked-in fixture replays the 12 actual answer distributions through the policy without network calls. Replay establishes policy behavior, not correctness of Jev's model choice.

The explicit-completion case with 45 candidates proposed GLM medium at 0.07 confidence despite weights completion=90, cost=5, duration=5 and a conflicting cheap/quick prompt. This is unresolved preference ambiguity and must not be counted as a successful preference-alignment result. Retaining a low-confidence proposal can preserve a bad choice as well as a good one. Deployments that require conservative substitution should select low_confidence_fallback=frontier. No model's task quality, actual cost, latency, gateway availability or completion probability was measured here. There are no repeats, held-out cases, model-generation outcomes or calibrated accuracy claims.
