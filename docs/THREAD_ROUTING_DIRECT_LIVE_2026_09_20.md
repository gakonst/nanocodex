# Direct Jev candidate routing live checks — 2026-09-20

Live Mac checks used real env.AI.run(typesafe/jev), all 15 candidates and the default 0.75 candidate-confidence threshold. These are development smoke cases, not a held-out eval.

| Case | Jev proposed | Confidence | Effective route | Outcome |
|---|---|---:|---|---|
| cheap-prompt | @cf/zai-org/glm-5.3:low | 0.87 | @cf/zai-org/glm-5.3:low | prior |
| thorough-prompt | gpt-6-astra:high | 0.64 | gpt-6-astra:high | fallback |
| explicit-economy | @cf/zai-org/glm-5.3:low | 0.6 | gpt-6-astra:high | fallback |
| explicit-completion | gpt-6-astra:high | 0.18 | gpt-6-astra:high | fallback |
| negated-cheap | gpt-6-astra:high | 0.7 | gpt-6-astra:high | fallback |
| soft-target | @cf/zai-org/glm-5.3:low | 0.81 | @cf/zai-org/glm-5.3:low | prior |

All six classified once and retained the route without another Jev call. Cheap/quick text and a soft target selected GLM-low. Thoroughness and negated cheap/fast language proposed Astra-high. Explicit economy/completion weights proposed the expected direction despite conflicting opening text, but confidence was below threshold. The economy case therefore ultimately fell back to Astra-high: preferences are not a guarantee of economy, and the conservative fallback currently dominates many uncertain choices. No threshold was lowered to improve reported outcomes.

The initial run, before clarifying weight semantics and effort profiles, fell back in four cases and proposed Astra-high for explicit economy. The revised instructions explicitly define cost/duration weights as minimizing spend/time. Initial and revised raw results are preserved separately. These reused development prompts cannot establish generalization or calibrated confidence.

No model ranks, cost estimates, success rates or duration estimates were fabricated for missing measurements. A dedicated held-out cohort and confidence calibration remain required before production cost/success/time optimization. No deployment occurred.

## Actual agent runs

Eight tasks with no explicit preferences all passed, but all eight used Astra-high fallback (candidate confidence 0.43–0.70). This is evidence of runtime compatibility, not successful cost optimization. Task durations ranged from 3.11 to 20.92 seconds including routing.

Two additional runs reused the inventory/configuration tasks with an opening-prompt request to keep the routine work cheap and quick. Both selected GLM-low directly and passed real tool and file checks:

| Task | Candidate confidence | Seconds | Passed | Route retained |
|---|---:|---:|---|---|
| terminal-inventory | 0.86 | 6.43 | true | true |
| engineering-config | 0.86 | 5.33 | true | true |

All ten actual-agent runs classified once and retained their model and effort. GLM used the live Workers AI binding; Astra used the existing subscription transport, with the common Rust/WASM loop. This is not the full hosted authenticated Durable Object HTTP journey; fixtures expose package-default subagent tools unlike the hosted PoC. These are reused development tasks with different prompts, not a controlled speed comparison.

Validation: 36 routing unit tests, two SQLite/WASM tests, focused routing TypeScript check and git diff --check pass. Full managed typechecking remains blocked by missing workspace declarations and catalog/protocol errors, as in the earlier Mac run.
