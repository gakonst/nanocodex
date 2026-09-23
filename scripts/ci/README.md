# CI job selection

The main workflow selects native Hand/Docker, voice, Python, Rust quality,
WASM artifacts, JS bindings, JS apps, package preview, policy and Actions analysis
independently. Managed TS changes retain JS binding checks without Rust quality,
native matrices or account UI builds. Shared SDK JS changes validate consumers and
packages without native/voice/Python builds. Apple Swift changes use the separate
Apple workflow, whose Mac and iOS jobs are also selected independently.

WASM artifact need is separate from changed Rust inputs: a JS consumer can restore
verified WASM without Rust setup or Clippy in the bindings job. Automatic CI tests
are temporarily paused with literal false conditions; builds, lint, typechecks and
artifact integrity remain active when their inputs are selected.

`select-jobs.mjs` compares the complete PR diff against its merge base, or the
complete before/after range for a push. Deletions and both sides of renames count.
Unknown paths, shared build manifests/locks, an unavailable diff, scheduled runs, and
manual dispatches select all groups. Keep the allowlist conservative when adding
new cross-language dependencies. The known CUA bridge scripts and their tests
select the native matrix (the scripts are embedded in the Hand helper), but not
the unrelated native voice or Python wheel jobs. The macOS Hand job retains disabled definitions for
the bridge, host lifecycle, and GUI readiness unit tests. New CUA files and
changes to shared Rust/build inputs still select all groups.

The daily 05:23 UTC run selects the full build matrix even when no source changed;
the temporary test pause also applies to scheduled and manual CI runs.
Scheduled, manual, PR, and push concurrency groups are separate so a push cannot
cancel the daily full-matrix run. The final `ci success` check requires every
selected job to succeed and every unselected job to be explicitly skipped;
the paused Rust test job must be skipped. Required package-preview publication is
selected only in its supported upstream repository. Missing outputs or unexpected skips
fail the gate.

Run `node --test scripts/ci/*.test.mjs` and `actionlint -shellcheck= -ignore 'constant expression.*false' .github/workflows/ci.yml`
after changing selection. The tests include real Git histories and execute the
workflow's actual final gate under Bash fail-fast semantics.

For measured run and step timings:

```
node scripts/ci/timings.mjs OWNER/REPO LIMIT OUTPUT_PREFIX [RUN_ID...]
```

Pre-execution elapsed includes dependencies and workflow gates as well as runner
queueing. Compare equivalent workflows; production deploy and the full native
CI suite have different scopes.
