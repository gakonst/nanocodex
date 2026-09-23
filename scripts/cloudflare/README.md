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

Production requires the Worker build, image plan, and any selected image builds.
An entirely skipped image matrix is valid only after successful receipt planning;
configuration checks both exact fingerprints again before deployment. Cache
misses/eviction still rebuild images. Bump `MANAGED_IMAGE_CACHE_EPOCH` to refresh
upstream base images or recover deliberately deleted registry images. The first
run with changed fingerprint logic is cold; subsequent API-only releases reuse it.

Deployment preserves the existing dependency phases: egress and X concurrently,
then managed agent, then independent consumer Workers concurrently, then Astra
secret configuration and account last. Every mutation rechecks current master;
failed consumer deployments prevent account deployment. Explicit production
rollbacks remain supported. Obsolete push builds cancel within the push group;
manual dispatches have distinct build groups and cannot cancel push releases.

Automatic CI test suites are temporarily paused at the user's request. Build,
format/lint/type checks, immutable artifact/receipt checks, release guards, and the
post-deploy health request remain active. `CI_TESTS_ENABLED=false` suppresses
image runtime smoke suites, including the sandbox Dockerfile smoke command;
manual/local image builds default to running those checks. Re-enable the workflow
conditions and image test flag together when the pause ends. Live validation is
manual-only during this pause.
