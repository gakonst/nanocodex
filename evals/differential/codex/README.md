# Stock Codex CLI vs Nanocodex GPT harness

36 tagged, source-informed synthetic cases, each with a predeclared reason it is hard and deterministic checkable claims. 24 train / 12 held-out cases, interspersed across categories; split frozen in cases.json. These are small production-shaped reductions, **not sampled private production traffic or a validated release benchmark**. Human review of input representativeness and sample grades is required before optimizing production. No capability or monotonicity claim follows from mock scores. The high/low matrix is a calibration experiment, not a guarantee that more effort wins.

Read methodology: https://claude.dev/blog/automating-eval-design-and-hillclimbing/ (entire article, 2026-10-07). Implementation follows its design checks: predeclared difficulty, independent trial state, twice-run cheapest valid graders, headroom/variance diagnostics, paired CIs, held-out gating, and no failure-text prompt editing. There is no LLM judge because every current claim is mechanically verifiable. For a future open-ended case, require checkable binary rubric claims, blinded randomized pair order, evidence spans, a judge model different from tested model, and repeat grades; do not silently substitute an LLM score here.

## Commands

```
python3 -m unittest discover -s evals/differential/common -v
python3 evals/differential/common/run.py --cases evals/differential/codex/cases.json --config evals/differential/codex/config.json --out /some/new/results --repeats 5 --mock
# On a disposable host with authenticated CLI or environment OPENAI_API_KEY:
python3 evals/differential/common/run.py --cases evals/differential/codex/cases.json --config evals/differential/codex/config.json --out /some/new/live --repeats 5 --allow-native --auth-file /private/codex/auth.json
```

The complete matrix costs 36 × 4 × N trials. Review cases and budget first. Add a second model to the config to test model-strength calibration; label ordering and inspect same-agent matched contrasts. Both adapters pass model/effort explicitly. No cached local sessions are resumed, and user-wide project configuration is excluded. Native permission behavior differs (stock workspace-write, Nanocodex native runtime): this is a deployment differential, not proof of identical sandbox policy. Only host-safe fixture operations are included. No remote API calls besides model inference are requested. Fresh directories cannot stop an agent from accessing the evaluator on the same host; for valid held-out claims move the evaluator/answers to a separate host and use isolated workers. Native local runs are smoke only.

## Codebase evidence / coverage limits

Inspected external Codex checkout at `1427825c4044d48b513c7d4ea32b84e58806a188`:

- `codex-rs/core/gpt_5_codex_prompt.md`: minimal scoped edits, preservation of user work, review and shell conventions → dirty-work, edit/no-op and trust cases.
- `codex-rs/tools/src/tool_spec.rs`: function/freeform/namespace schemas → common task intent rather than hardcoding one tool-call syntax.
- `codex-rs/core/src/tools/handlers/shell_spec.rs`: cwd, output cap, PTY/session yield timing → quoting, exit status, hidden files, long-output needle and completion cases.
- `codex-rs/apply-patch/src/file_update_tests.rs`: contextual first-line replacement and EOF-sensitive hunks → repeated context, rename, first line, EOF, UTF-8 and multi-file fixtures.
- `codex-rs/core/src/agents_md.rs`: traversal stops at project root, directory ordering, AGENTS.override.md precedence → root, nested, override and sibling-scope cases.
- `codex-rs/core/tests/suite/exec_policy.rs`: approval-policy and permission overrides → explicit no-run/plan-only/scoped-delete negative controls. These test model compliance, not OS enforcement.
- `codex-rs/core/src/compact.rs`: replacement history and initial-context reinjection → stale-summary and late-constraint reductions. Tagged `context-proxy`: they do **not** force or claim actual compaction.
- `codex-rs/core/tests/suite/rollout_list_find.rs`: ID/name, archive and cwd metadata → `resume-proxy` selection fixtures. They do **not** claim actual thread listing/resume integration.

Read/reused repository infrastructure rather than forking provider SDKs: `benchmarks/paired_parity_bench.py` interleaving; `benchmarks/stock_codex_fork_bench.py` fresh credential home pattern and stock protocol reference; `benchmarks/codex_request_parity.py` shell/session/cancellation scenarios; `scripts/codex-parity/README.md` and native-behavior fixture rationale. Exact request/tool parity remains those suites' responsibility. Existing stock fork benchmark provides actual multi-turn/fork measurement; this suite intentionally does not relabel offline proxies as lifecycle coverage. Future actual compaction/resume trials must capture the corresponding terminal lifecycle events and verify continued state before scoring.

Inspected adjacent nanoeval README, task loader and write-greeting fixture. `common/export_nanoeval.py` exports canonical tasks and identical filesystem graders; nanoeval provides fresh VM state and artifact retention. We do not modify that adjacent checkout or claim an unrun VM integration. Prefer that VM path for Nanocodex-only isolation; matching stock VM adapter remains future work.

## Hillclimbing

Run baseline/candidate as SINGLE-config runs on identical frozen cases and repeat counts, then use `common/hillclimb.py --baseline BASE/results.jsonl --candidate NEXT/results.jsonl --baseline-config base-object.json --candidate-config next-object.json --kept-config round1-kept.json --decision round1.json`. Object configs are the single objects from each runner config array. Only one model OR effort coordinate changes; keep requires positive train AND test lower paired CI bounds. Errors, mocks, mismatched coverage or suite hashes reject evidence. Within-noise gains are not accepted. The controller never reads failing transcripts. No actual hillclimb was performed by this PR.

See `SMOKE.md` for observed local checks and remaining calibration work.
