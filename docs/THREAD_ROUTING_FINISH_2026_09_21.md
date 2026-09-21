# Thread routing completion and verification — 2026-09-21

This revision implements the provider/child routing work in PR #436 and merges current master. Later actual-CLI verification found child-result and idle-reconstruction failures, now fixed and verified in the [recovery follow-up](THREAD_ROUTING_CHILD_RECOVERY_2026_09_21.md). The feature remains opt-in; nothing was deployed and no recurring probes were enabled.

## Resulting behavior

- One Jev decision pins a provider, model and thinking level for a new root thread. Later turns reuse that choice.
- Up to 45 supported combinations are available: 15 native Workers AI/ChatGPT choices plus 15 each for configured OpenRouter and Vercel gateways. Missing gateway secrets remove those candidates before selection. Provider secrets stay in deployment-owned transport closures.
- Explicit completion/cost/duration weights and soft targets are supplied to Jev. Low-confidence handling is configurable: `proposed` retains an eligible proposal as an audited, unmeasured fallback; `frontier` preserves conservative substitution. Confidence is not task-success probability.
- When multi-agent execution is enabled, each new child gets its own pinned choice. Explicit model/effort overrides constrain the eligible choices. Continued children never call the chooser again, and the parent never switches.
- Mixed-provider trees share the Rust agent loop through stateless full-history HTTP. Dispatch uses the branch `threadId`; the transport `sessionId` remains the shared lineage. Both IDs keep their original egress meanings.
- Hosted child authorization comes from the retained parent/root-turn snapshot. Routing is rechecked after classification and before every inference request. Missing/revoked ownership and missing pins fail closed. Route binding must complete synchronously before child inference starts.
- Gateway telemetry records bounded transport outcomes without prompts, credentials, raw provider bodies or exception text. Unknown execution/ingress location stays unknown. There is no claim of global region coverage, calibrated latency ranking, generation TTFT or user-delivery measurement.

## Verification

All checks use the isolated Mac worktree and the repository-pinned pnpm 11.25.0. The previous full-service type errors were resolved by installing pinned dependencies and rebuilding the workspace declarations.

- Managed service TypeScript check and public package type/package checks pass.
- Rust subagent tests: 58 passed, including route validation, override preservation, bind failures, batch cancellation and abort-guard cleanup.
- Real Rust/WASM and transport integration suite: 36 passed. This includes two provider-specific two-turn gateway loops, SQLite route persistence, stateless GPT tool execution, and a mixed-provider parent/child whose continuation reuses the child pin.
- Adapter/helper/telemetry suite: 52 passed. Gateway transport responses are mocked in these tests; tools and protocol validation are real.
- Routing policy/controller/gateway unit tests: 95 passed, including abandoned-ticket expiry.
- Hosted admission and child-authorization tests pass, including orphan-route cleanup.
- Worker bundling succeeds with container rollout disabled. The ordinary dry run reaches bundling but requires a Docker CLI for unchanged container images; container images were not rebuilt here.

## Fresh live task regression

Four reused synthetic development tasks ran against live Jev and GLM-5.3 through the rebuilt Rust/WASM agent. Each prompt requested cheap, quick execution. All selected GLM-low, performed actual local tool calls, passed independent output/file checks, and reused the retained route without another Jev call.

| Task | Pass | End-to-end time | Jev time | Model requests | Tool calls |
| --- | --- | ---: | ---: | ---: | ---: |
| File inventory and UTF-8 byte count | yes | 5.080 s | 1.073 s | 3 | 3 |
| Configuration edit with preserved fields | yes | 4.566 s | 0.258 s | 3 | 3 |
| Circuit calculation | yes | 5.972 s | 0.267 s | 4 | 3 |
| Invoice aggregation and output file | yes | 4.948 s | 0.317 s | 3 | 2 |

Wrangler emitted remote internal-error diagnostics during the last task, but the task returned a valid result and all verification checks passed. These are development regressions, not held-out model comparisons; no cost or speed superiority is established.

A separate [12-call Jev preference smoke](THREAD_ROUTING_PREFERENCE_EVAL_2026_09_21.md) covers prompt preferences, conflicting explicit weights, negation, soft targets and 15/45-candidate catalogs. All responses were valid and pinned. One 45-candidate explicit-completion case proposed GLM-medium with confidence 0.07 despite completion-heavy weights. It remains a failed preference-alignment example, not a success. Gateway availability was simulated for this selection-only probe; no authenticated OpenRouter/Vercel generation was tested.

## Actual CLI follow-up

A subsequent [verification with the PR-built native and managed CLI binaries](THREAD_ROUTING_CLI_VERIFICATION_2026_09_21.md) tests live inference, tools and continuation. It also caught and removes a stale public-admission restriction that prevented routed multi-agent creation, plus a CI Clippy warning. Native and managed root journeys pass. The subsequent [recovery fixes and actual-CLI rerun](THREAD_ROUTING_CHILD_RECOVERY_2026_09_21.md) pass live GLM object results, child continuation across two managed idle boundaries, explicit close and public deletion.

## Remaining PoC limits

GLM and the gateway adapter are text-only and buffered. Provider-side opaque compaction is unsupported. Targets do not enforce spending caps or deadlines. Provider-side cancellation is not guaranteed. Durability import/export remains disabled for routed threads. Existing public benchmark references and catalog prices are provisional evidence, not matched task-success/cost/duration measurements. Production rollout and representative held-out calibration remain separate work.
