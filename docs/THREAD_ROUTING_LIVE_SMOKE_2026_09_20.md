# Live thread-routing smoke test, 2026-09-20

Executed on the user's Mac in an isolated Git worktree based on PR #436. The real Rust/WASM agent executed real local fixture tools. GLM calls used the authenticated remote Workers AI binding through Wrangler's local platform proxy. Frontier calls used the existing ChatGPT subscription through the Node transport. No production Worker was deployed.

## Results

These eight tiny synthetic tasks exercise integration, not the published benchmark families they resemble. The research task reconciles two local memos; repository repair fixes a manifest; no live web research or general coding benchmark was performed.

| Task | GLM first run | GLM after adapter fix, seconds | Astra fallback, seconds |
| --- | --- | ---: | ---: |
| List log files and count UTF-8 bytes | Fail: 9 instead of 11 bytes | Pass, 7.36 | Pass, 10.31 |
| Apply retry configuration, preserve other fields | Pass | Pass, 6.73 | Pass, 9.11 |
| Repair package entry and verify module export | Pass | Pass, 9.87 | Pass, 14.85 |
| Reconcile conflicting dated source memos | Pass | Pass, 3.98 | Pass, 8.78 |
| Circuit current/power through calculator tools | Fail: original tool name rejected | Pass, 8.91 | Pass, 8.92 |
| Weighted mean through calculator tools | Pass | Pass, 10.76 | Pass, 23.97 |
| Aggregate invoice balances into sorted JSON | Pass | Pass, 6.77 | Pass, 8.03 |
| Ask for clarification on ambiguous request | Pass | Pass, 1.39 | Pass, 2.58 |

First GLM baseline: **6/8**. Its byte-count failure remains a model error; the later pass does not establish that it is fixed. A separate diagnostic reproduced the tool-name error. After the adapter correction, the seven substantive tasks passed in one run. Disk exhaustion interrupted saving the last task, so clarification was rerun once after removing only this worktree's disposable build artifacts. Its separate saved result passed. Do not describe this as an uninterrupted 8/8 corrected run. Astra's unchanged baseline passed **8/8**.

The direct GLM path used low effort for terminal/research/business/clarification and medium for configuration/repair/science/math. Astra used low throughout. GLM responses were capped at 2,048 output tokens; the public Node frontier API did not expose an equivalent per-response cap. Each task had a 110-second deadline, eight model-request budget and twelve successful fixture-tool budget. Frontier time includes per-task router and agent setup; direct GLM time excludes the shared router probe. Cache states differ. These numbers are observations, not a controlled speed or quality comparison.

## Actual router behavior and blocker

Every live `typesafe/jev` attempt returned `2021: Insufficient AI Gateway credits`. The existing policy correctly persisted `chatgpt / gpt-6-astra / low` fallback. A retained-route check reused the choice without reclassification. All eight frontier tasks ran through this live fallback. Successful Jev classification and model choice based on task family remain **untested**.

GLM was available on the same Cloudflare account. The GLM task runs were explicitly direct candidate tests; Jev did not select them. Cloudflare authentication alone does not establish sufficient third-party AI Gateway credits.

## Adapter correction

GLM repeatedly returned `read` even though the request advertised an alias such as `tool_4` and displayed `read` in its description. The adapter previously rejected this and the Rust transport retried without making progress. It now resolves an exact original or qualified registered tool name only when unambiguous; canonical aliases retain precedence, and unknown/ambiguous names remain errors. No fuzzy lookup or undeclared tool dispatch is allowed.

Added a regression covering original names, qualified namespace names, and ambiguity rejection. Adapter plus actual Rust/WASM tool-loop tests pass **15/15**. A live circuit diagnostic changed from failure to pass after the correction. The full corrected task results are shown above; original records were retained.

## Usage and limits

Observed GLM usage for the corrected seven-task run plus clarification continuation: 135,441 input tokens (53,568 cached) and 1,480 output tokens. At the documented GLM rates, the token-based estimate is approximately **$0.135**. This excludes prior failed runs, diagnostics, Jev, tools and any unsaved request at disk exhaustion; it is not an invoice.

Astra's eight tasks reported 176,020 input tokens (111,360 cached), 852 output tokens, and a package API-equivalent estimate of **$0.80056**. This is **not billed ChatGPT subscription spending**. There were 32 response requests and 24 fixture-tool executions. Failed Jev requests expose no usage here. Do not infer a cost-effectiveness ratio from these different accounting and effort settings.

The Node/host fixture APIs expose default subagent builtins in addition to the supplied fixture tools, unlike the managed PoC which disables subagents. Recorded fixture handlers only show the intended local tasks; this is not a complete builtin-tool event audit. The harness exercises the shared loop and transports, not a full authenticated hosted Durable Object HTTP journey. Local managed HTTP admission and persistence already have separate tests.

Credentials stayed in the Mac's existing credential mechanisms and in-memory subscription handling. No credentials were copied into evidence. Raw task receipts, safe model outputs and fixture tool traces were saved separately from tracked code.

Next prerequisite: fund/enable the account's AI Gateway access for Jev, then repeat successful classification tests on a held-out set. The current smoke suite is too small and now used for debugging; it cannot calibrate routing success probabilities.
