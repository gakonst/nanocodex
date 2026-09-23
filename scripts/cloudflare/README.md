# API release path

Worker builds restore exact-input WASM outputs before setting up Rust. The shared
`.github/actions/wasm-outputs` action restores browser and Node bindings plus
`.ci-wasm-cache` metadata/raw WASM. The build script verifies input and output
hashes before skipping Cargo, wasm-bindgen, and wasm-opt, and writes an attestation
for the current checkout. A miss or failed verification rebuilds with Rust 1.97.
Python 3.11+ is needed to resolve Cargo manifests without installing Rust; missing
Python disables WASM cache reuse rather than blocking ordinary compilation.

The image planner restores and verifies immutable phone/sandbox receipts. Only
missing receipts create image-builder matrix jobs. Publication fingerprints use
committed Docker inputs and a conservative local Rust dependency closure, rather
than entire unrelated source trees. Python 3.11+ is required for image planning.
Both optional/platform dependencies and all workspace Cargo manifests remain
inputs; relevant changes still rebuild. When adding dynamic build-script reads or
external generated inputs, extend the input audit with them.

Production plans, installs, builds, and deploys on one serialized runner. It does
not wait for the preview Worker-build job or upload/download its Worker outputs.
The image plan and any selected image builds remain prerequisites. A skipped image
matrix is valid only after successful receipt planning; configuration checks both
exact fingerprints again. Cache misses/eviction still rebuild images. Bump
`MANAGED_IMAGE_CACHE_EPOCH` to refresh upstream base images or recover deliberately
deleted registry images.

`release-plan.mjs` fingerprints each Worker's source/config/assets, local package
dependency closure and relative imports, plus shared lockfiles/build configuration.
WASM consumers include Rust build inputs; managed also includes the two immutable
image references. Compare against the latest successful GitHub Deployment for each
`nanocodex-production-<worker>` environment. Only changed components install/build
and deploy. A managed-only edit therefore skips the account UI and other nine
Workers; Wrangler bundles managed once during deployment. The first run establishes
the ledger and deploys everything. Explicit production dispatch always redeploys
all components, including when rolling back.

The deployment ledger records intent before mutation, and success only after an
actual successful deployment (including Astra secrets and account health where
applicable). Unknown, failed, or interrupted latest attempts require redeployment;
an older successful hash never overrides a newer rollback or incomplete attempt.
This is durable release state, not an evictable build cache. Production's
`deployments: write` permission and job-wide concurrency are required. Out-of-band
manual Wrangler deployments are not tracked; use production dispatch to reconcile
them before relying on selective releases.

Selected deployments preserve dependency phases: egress and X concurrently, then
managed, independent consumers concurrently, and account last. Every mutation
rechecks current master; failed phases prevent dependent deployments. The job
summary records individual deployment durations. Preview builds remain separate
and unprivileged. Obsolete push image builds cancel within the push group; manual
dispatches have distinct build groups and cannot cancel push releases.

Automatic CI test suites are temporarily paused at the user's request. Build,
format/lint/type checks, immutable artifact/receipt checks, release guards, and the
post-deploy health request remain active. `CI_TESTS_ENABLED=false` suppresses
image runtime smoke suites, including the sandbox Dockerfile smoke command;
manual/local image builds default to running those checks. Re-enable the workflow
conditions and image test flag together when the pause ends. Live validation is
manual-only during this pause.
