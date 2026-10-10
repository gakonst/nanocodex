# CI job selection

`select-jobs.mjs` compares the complete PR diff against its merge base, or the
complete before/after range for a push. Deletions and both sides of renames count.
Rust paths are mapped to workspace packages with `cargo metadata --no-deps`, then
expanded to every workspace package that depends on them (normal, build or dev).
Jobs are selected by the packages they build:

| Family | Jobs | Selected by |
| --- | --- | --- |
| `rust` | fmt + Clippy on affected crates (fast lane) | any affected package |
| `rust_extra` | independent crate checks, docs | any affected package |
| `hands` | Linux/macOS shared Hand | `nanocodex-bin` and `nanocodex-hand-daemon` closures, `js/desktop-runtime`, CUA bridges |
| `windows` | Windows Hand and installer | `nanocodex-bin` and `nanocodex-hand-daemon` closures, `windows/`, `install.ps1` |
| `vm` | static guest and Docker Hand | `nanocodex-vm` closure, CUA bridges |
| `voice` | native voice runtime | `nanocodex-voice-native` closure, `third_party/codex-voice` |
| `python` | Python wheels | `nanocodex-python` closure, `py/`, `examples/python` |
| `wasm_rust` | WASM Clippy; also selects JS consumers | `nanocodex-wasm` closure |
| `wasm`, `bindings`, `apps`, `preview` | WASM artifact and JS consumers | JS package paths |
| `policy` | cargo-deny, boundaries, typos, Cloudflare script policy | any non-binary path |
| `codeql` | Actions analysis | `.github/` |

Workspace-wide inputs (root Cargo/JS manifests and locks, toolchain, `.cargo/`,
`ci.yml`, `js-preview.yml`, `.github/actions/`, `scripts/ci/`), unknown paths, an
unavailable diff or Cargo graph, and non-PR/push events select everything
(`packages=*`). Files a crate reads outside its directory belong in
`crossPackageInputs`.

Draft PRs get only the fast lane (fmt/Clippy on affected crates, WASM Clippy,
policy, JS typecheck/build); marking ready reruns CI with the heavy lane. Only
superseded PR runs are cancelled. `pnpm check:fast` runs the fast-lane
fmt + Clippy command locally for crates changed since `origin/master`.

Behavioral test steps are gated by the `NANOCODEX_CI_TESTS` owner switch in
`ci.yml` (`on`). The shared Hand job runs its Managed2 and SSH import CLI
journeys, plus the private-input journey on Linux, whenever the job is
selected. Legacy CLI source and SSH journey script changes select that job;
draft PRs still suppress it with the other heavy jobs. Hand changes also select
`vm-guest`, whose Docker tests run `nanocodex2`.
Rust tests use `cargo nextest run --profile ci` (`.config/nextest.toml`).

`ci success` runs `select-jobs.mjs verify` on `toJSON(needs)`: selected jobs must
succeed and all others must be skipped; add new jobs to its `gate` table. Run
`node --test scripts/ci/select-jobs.test.mjs` and `actionlint` after changes.

For measured run and step timings:

```
node scripts/ci/timings.mjs OWNER/REPO LIMIT OUTPUT_PREFIX [RUN_ID...]
```

Pre-execution elapsed includes dependencies and workflow gates as well as runner
queueing. Compare equivalent workflows; production deploy and the full native
CI suite have different scopes.

## Rust compilation

Every Rust job uses `.github/actions/rust-compiler-cache`: a Cargo registry
cache plus sccache shared by all jobs. Per-job target archives (0.8-1.4 GB each)
overflowed the 10 GB cache budget; only the Windows critical path keeps one.
The independent crate checks stay separate Cargo invocations so feature
unification cannot weaken them. JavaScript jobs reuse Turborepo's cache through
`.github/actions/turbo-cache`; only master writes it.

The Windows Hand lifecycle and installer share one Windows 2025 runner and one
CLI build. The real installer build validates its definition.

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

## Cloudflare preview latency

Worker builds and uploads determine `Cloudflare preview success`. Ready dialog
and playground assets upload immediately after artifact restoration, before
unrelated Worker validation, evaluator preparation, or Astra dependency setup.
Worker build or validation failures still fail this gate.

Preview container compilation is skipped on PRs and ordinary preview dispatches.
Production already publishes changed phone and sandbox inputs independently of
Worker deployment. To test Docker changes before merging, dispatch the Cloudflare
workflow with target `preview` and `validate_images: true`. The separate
`Cloudflare image validation` check then requires image selection and both full
image builds; it never delays the Worker readiness gate. Image failures remain
visible instead of being converted into successful Worker results.

Image validation remains unprivileged with no registry writes. Its sandbox build
retains Dockerfile checks; preview Worker validation uses
`--containers-rollout none`. Production release compilation, image verification,
publication and immutable receipts keep their existing behavior.

Production builds applications in deployment order. Infrastructure and managed
Workers upload before unrelated consumer and account UI builds, with successful
health/receipt barriers and account deployed last. Completed shared build targets
are reused between phases. Superseded pushes stop before starting another phase.
The small orchestration tests run in the main CI policy job.

Preview image validation uses BuildKit's `cacheonly` output. It still evaluates
the complete Dockerfile, including its checks, but does not export and load an
unused image into Docker Engine. Production publication retains `--load` for
its runtime verification, registry push, and immutable digest receipt.
