# API release path

The production preflight selects Workers before scheduling image jobs. Workers are
independent releases: a managed-only edit builds/deploys managed; an X/email-only
release does not require phone or sandbox receipts. Production rechecks the plan
inside its serialized deployment job, then installs and builds selected packages
on that same runner. No second production install or Worker artifact handoff.

Only managed, account and playground require WASM. Egress, X, email, Connect API,
Connect dialog, Astra and Chief of Staff use JavaScript. Explicit build tiers avoid
Turbo's general SDK-to-WASM build edge while preserving compiled dependency order.
When adding a runtime import or generated asset, update `workerSpecs`/build targets
and exercise a clean build with the generated WASM directories absent where the
Worker is declared JS-only. Source keys include runtime/config/assets and relevant
build scripts, not development stress tools or native-image preparation scripts.

The shared `.github/actions/wasm-outputs` action verifies exact-input browser/Node
bindings and retained raw WASM before setting up Rust. Verified hits skip Cargo,
wasm-bindgen and wasm-opt and reattest for the checkout. Misses rebuild with Rust
1.97. Both the outer cache key and inner binding stamp cover generator policy.
Provably non-WASM Cargo target tables and standalone tests/benches are excluded;
unknown targets, optional dependencies and build-script inputs remain conservative.
Python 3.11+ is required for deterministic release planning. Standalone WASM builds
can still compile without cache reuse if input discovery is unavailable; releases
fail early at planning rather than generating an unstable release identity.

Managed fingerprints include the audited phone/sandbox input keys and Cloudflare
account. Selected managed releases require matching immutable registry receipts.
Actions cache is the fast lookup; successful publications also retain receipts in
GitHub Deployments (`nanocodex-image-phone` / `nanocodex-image-sandbox`). Up to 100
recent receipts per image are searched when a cache entry is missing. Only genuinely
missing receipts schedule builders. Production rechecks durable history and exact Actions cache keys if live state
changed after preflight, and can publish a genuinely missing image under the lock.
Every managed release retains both verified receipts, including ordinary cache hits,
before certifying its deployment. This closes the preflight-to-deploy race.

Publication rejects dirty relevant source because image keys hash committed HEAD.
`MANAGED_IMAGE_CACHE_EPOCH` changes image identity, pulls bases and invalidates each
Docker stage's upstream layers; ordinary source changes retain layer reuse. Bump it
to refresh upstream content or recover deliberately deleted registry images. Audit
new Docker COPY/build-script reads and external generated inputs in the input helper.
Preview container decisions compare these same committed keys, so SDK JavaScript
alone cannot trigger native image builds.

Worker release identity combines source/dependency/config keys with account scope.
The ledger records intent before mutation. Success requires command completion,
phase health, and a live Cloudflare deployment serving one 100%-traffic version
with `nc-ci-<fingerprint>`. The GitHub success status stores the live deployment and
version IDs. Reuse requires those IDs and tag still match Cloudflare: manual pnpm /
Wrangler deploys, old-ref rollbacks, split traffic, interruptions and unknown state
cannot masquerade as a current successful release. A source-identical manual deploy
may therefore cause one deliberate reconciliation deployment on the next CI run.

Selected deployments preserve dependency phases: egress/X, managed, consumers,
then account. Independent members run concurrently. Every mutation rechecks current
master; failed phases prevent later ones. Astra secrets are applied additively in
its tagged deploy using a temporary private secrets file, then removed locally.
Each phase checks health before success receipts. The job summary records per-Worker
durations. Explicit production dispatch forces every component, including rollback.
The first run after key/ledger changes is cold; warm-release time must be measured
separately. Preview builds remain unprivileged and separate from credentialed upload.

Automatic CI tests are temporarily paused. Builds, selected lint/type checks,
artifact/receipt checks, current-master guards and deployed health remain active.
`CI_TESTS_ENABLED=false` suppresses image runtime smoke suites; manual/local image
builds default to running them. Live validation is manual-only. Re-enable the test
conditions and image test flag together when the pause ends.
