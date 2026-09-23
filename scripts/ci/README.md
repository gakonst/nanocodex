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

## Rust compilation critical path

The quality matrix runs workspace Clippy, CLI/benchmark Clippy, independent
public crate checks, and documentation concurrently. The independent crate
checks intentionally remain separate Cargo invocations: merging their package
flags would unify features and weaken that check. CLI and benchmark targets
belong to the same package and share one Clippy invocation. All quality lanes
read the workspace Clippy dependency cache; only a successful workspace Clippy
run on master can write it. This removes three overlapping archives without
serializing the jobs. Cargo still checks fingerprints and builds missing feature
or compilation variants in each lane. `ci success` requires the complete matrix
to pass. Compare per-lane compilation time when changing this sharing policy.

Native Hand, Windows installer, VM guest, and Python wheel builds use pinned
sccache with GitHub's cache backend, in addition to the dependency cache. This
allows unchanged library compilation to be reused across fresh checkouts;
linking and unsupported compiler invocations still run normally. Native Cargo
caches include the job identity and compiler environment, so concurrent jobs
cannot publish different target subsets under one immutable key.

Baseline: CI run 35824188289 on 2026-09-23 spent 9m57s in quality: 6m08s in
Clippy, 2m07s in isolated crate checks, and 1m03s in docs. Parallel lanes remove
that serial dependency, but new cache namespaces need warming. Compare cold and
warm runs before claiming a measured improvement; parallel jobs can increase
aggregate runner minutes even as elapsed time falls. Existing paused tests are
unchanged by this optimization.

The Windows Hand lifecycle and installer now share one Windows 2025 runner and
one CLI build. Linux and macOS retain their shared-Hand matrix lanes. The real
installer build validates its definition, so CI no longer builds a placeholder
installer before building the real one. Paused behavioral test definitions are
retained, including the Windows media, capture, and JS lifecycle coverage.

WASM Clippy runs directly after selection, in parallel with the optimized WASM
artifact producer. JavaScript bindings download the artifact and immediately
start their consumer checks. The final gate separately requires WASM Clippy
when both Rust and binding checks are selected.

Preview publishing uses the supplied artifact whenever its workflow input is
present, including a manually dispatched parent CI. A standalone preview still
builds its own artifact. Preview concurrency separates parent workflows and
manual/full runs, so an unrelated push cannot cancel a required preview.

## Cache storage and writers

PR compiler caches are read-only. Only master push, manual, and scheduled runs
write sccache entries; cache misses still compile normally. This avoids concurrent
PR-local uploads competing with reusable master entries for the cache API quota.

Docker intermediate layers use the public `ghcr.io/<repository>-hand` package,
with separate `buildcache-*` tags per consumer and architecture. They no longer
consume the Actions cache capacity needed by Cargo and other dependency caches.
PRs import anonymously; only trusted master runs log in and export. A missing
cache or a failed cache login/export leaves normal builds available. These tags
are cache metadata, independent of runnable image tags and deployment receipts.

Master CI seeds its Hand caches on push, schedule, or manual dispatch. Toolkit
caches seed on master dispatch; Cloudflare seeds when a trusted deployment needs
an image build. Successful cache availability checks emit a Docker registry cache
notice. Compare a later run after seeding before attributing a speedup to reuse.
Old Actions cache entries can expire normally; no cache deletion is required.

The selection job always runs the small compiler and Docker cache policy tests,
including the Wrangler Docker argument/exit-status boundary tests. The existing
behavioral test pause is otherwise unchanged.

## Cloudflare preview latency

Preview Worker bundling and image selection start independently. Only changed
phone or sandbox image inputs create container validation jobs, which run in
parallel. Selection compares the committed production image dependency closure
against the PR base; missing history or an uncertain comparison selects both.
Manual preview dispatches always validate both images.

The Worker preview job restores its same-revision artifacts and validates
Wrangler configuration with `--containers-rollout none`, so Docker compilation
does not delay asset preview URLs. Independent container jobs still prepare and
build the same images, preserving Dockerfile checks and importing registry caches
without deployment credentials or cache writes. Production publication and its
receipt validation keep their existing behavior.

`Cloudflare preview success` requires the Worker build, selection, Worker
validation/publication, and all selected image builds to succeed. An image build
may be skipped only when a successful selection explicitly requires none.
The small orchestration tests run in the main CI selection job even while
behavioral test suites remain paused.

Preview image validation uses BuildKit's `cacheonly` output. It still evaluates
the complete Dockerfile, including its checks, but does not export and load an
unused image into Docker Engine. Production publication retains `--load` for
its runtime verification, registry push, and immutable digest receipt.
