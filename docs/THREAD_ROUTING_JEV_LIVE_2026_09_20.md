# Live Jev routing on Mac — 2026-09-20

After the user added $25 of AI Gateway credit, env.AI.run("typesafe/jev", ...) succeeded through Wrangler's remote AI binding. The response identified Jev 1.13.0 and Unified Billing. Live responses used a Completed/result envelope; the router now accepts this and the documented direct payload, and rejects Pending/Failed envelopes. Three regression cases cover these states.

The actual routing module selected backend and thinking for eight fresh tasks. Each ran the existing Rust/WASM agent with live inference and real local fixture tools. Policy: balanced, OSS medium, frontier low, classifier threshold 0.75. Each classified once; all routes survived reopening ThreadRoutePin over its retained test store.

| Task | Jev family | Confidence | Model / thinking | Task seconds | Passed |
|---|---|---:|---|---:|---|
| File inventory | terminal | .99 | GLM-5.3 / medium | 8.09 | yes |
| Configuration edit | terminal | .94 | GLM-5.3 / medium | 7.40 | yes |
| Repository repair | repository_repair | .92 | Astra / low | 14.43 | yes |
| Conflicting research sources | other | .23 | Astra / low | 8.31 | yes |
| Circuit calculation | terminal | .89 | GLM-5.3 / medium | 8.91 | yes |
| Weighted mean | terminal | .97 | GLM-5.3 / medium | 10.36 | yes |
| Invoice aggregation | terminal | .72 | Astra / low | 11.26 | yes |
| Ambiguous request | other | 1.00 | Astra / low | 3.60 | yes |

Exact inference IDs: @cf/zai-org/glm-5.3 through env.AI and gpt-6-astra through the existing ChatGPT subscription transport. Jev routing took 0.234–1.105 seconds per task. Low-confidence and other-family decisions correctly selected frontier. Validation checked structured answers, file edits, preserved inputs, tool receipts, configured thinking, and route retention. All eight passed.

## Interpretation and limits

This verifies real classification, both inference paths, and the common agent loop. It is a smoke test, not a held-out benchmark or production deployment. These synthetic fixtures were previously used for debugging. The local harness differs from the hosted authenticated Durable Object HTTP journey; it retains the package's default subagent exposure, whereas hosted routing disables subagents. Prior SQLite/WASM tests cover hosted persistence separately.

Jev often classified tool-based domain tasks as terminal, including science and mathematics. Passing these simple fixtures does not validate mapping them to Terminal-Bench or establish domain-level effectiveness. Family definitions and classification accuracy need a separate labeled holdout before broad production routing. Vendor eval scores remain priors: no comparable cost/success/time cohort was supplied, so non-fallback decisions used priors rather than measured optimization. Eight successes do not establish calibrated success probabilities.

Wrangler logged internal-error diagnostics during the run, although every recorded task and route completed successfully. Investigate these if they recur. Earlier unfunded and direct-model runs remain documented in THREAD_ROUTING_LIVE_SMOKE_2026_09_20.md; this report supersedes only its statement that Jev was blocked by billing.

Raw local evidence: js/managed/.poc-live/routed-run-2026-09-20T18-39-06.700Z/report.json (ignored fixture output). No deployment was performed.

Validation: 22/22 routing unit tests and git diff --check pass. Full managed tsc --noEmit does not pass in this worktree: missing nanocodex-tools declarations and catalog/protocol export errors produce cascading diagnostics. This run therefore does not claim a clean whole-service typecheck.
