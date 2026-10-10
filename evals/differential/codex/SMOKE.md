# Observed checks — 2026-10-07, macOS

- 10 stdlib unit tests passed: schema, exact-byte grader mutation/symlink rejection, stock/Nanocodex terminal decoding, mock/report, CI/hillclimb decision, explicit effort forwarding, nonzero-exit zero scoring.
- Full mock matrix: 36 cases × 4 configurations × 2 repetitions = **288 trials**, with two grader passes per trial, JSONL/transcripts/static HTML. All mock trials score 1 by construction. This establishes plumbing only, not headroom, quality or superiority.
- Nanoeval export produced 36 canonical task directories. VM execution was **not** run; no assertion of working isolated stock parity.
- Local saved Codex CLI credentials were available; no secret was printed. Fresh credential home was populated privately using the existing stock benchmark pattern. No environment API key was present.
- Real `patch-repeated-context` smoke, GPT-6 Astra: stock Codex low **1/1**, 44,189 ms; stock high **1/1**, 24,595 ms. Their measured input/output tokens were 59,039/408 and 58,939/376 respectively.
- Initial Nanocodex runs exposed a real CLI plumbing bug: `--cwd` before `run` was rejected (exit 2). Fixed adapter option order; retained failed attempts rather than counting them as model failures. A fresh real Nanocodex low run then scored **1/1**, 14,294 ms, 54,352 input / 357 output tokens.
- These single easy-case successful runs were not a fully reinterleaved post-fix comparison. No statistical speed/quality conclusion, headroom claim, or stronger-model/effort monotonicity conclusion is justified. No hillclimb was accepted or attempted.

Local raw evidence is intentionally gitignored under `mock-smoke/` and `real-smoke/` (provider-native transcripts may contain account metadata). Open each directory's `index.html`. Public evidence here is aggregate only. Reproduce with README commands; CI publishes mock artifacts only and never requires credentials. Before adoption: human review cases/grades, run a larger matched isolated baseline with N≥5, review infrastructure failures, inspect provider-effective effort, test another model, and reject saturated/always-failing/within-noise comparisons.
