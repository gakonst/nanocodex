# CI job selection

The main CI workflow always runs workspace Rust tests, formatting/Clippy/docs,
dependency policy, WASM, browser bindings, JavaScript apps, and Actions CodeQL.
The selector controls only the expensive native Hand/Docker matrix, native voice,
and Python wheel jobs. It never skips individual assertions inside a selected job.

`select-jobs.mjs` compares the complete PR diff against its merge base, or the
complete before/after range for a push. Deletions and both sides of renames count.
Unknown paths, shared Rust/build inputs, an unavailable diff, scheduled runs, and
manual dispatches select all groups. Keep the allowlist conservative when adding
new cross-language dependencies. The known CUA bridge scripts and their tests
select the native matrix (the scripts are embedded in the Hand helper), but not
the unrelated native voice or Python wheel jobs. The macOS Hand job also runs
the bridge, host lifecycle, and GUI readiness unit tests. New CUA files and
changes to shared Rust/build inputs still select all groups.

The daily 05:23 UTC run exercises the full matrix even when no source changed.
Scheduled, manual, PR, and push concurrency groups are separate so a push cannot
cancel the daily full-coverage run. The final `ci success` check requires every
selected job to succeed and every unselected job to be explicitly skipped;
missing outputs or unexpected skips fail the gate.

Run `node --test scripts/ci/*.test.mjs` and `actionlint .github/workflows/ci.yml`
after changing selection. The tests include real Git histories and execute the
workflow's actual final gate under Bash fail-fast semantics.

For measured run and step timings:

```
node scripts/ci/timings.mjs OWNER/REPO LIMIT OUTPUT_PREFIX [RUN_ID...]
```

Pre-execution elapsed includes dependencies and workflow gates as well as runner
queueing. Compare equivalent workflows; production deploy and the full native
CI suite have different scopes.
