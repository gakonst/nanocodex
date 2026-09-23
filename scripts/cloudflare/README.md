# API release path

The API production job starts directly on a master push, without `needs` on image
planning, Docker publication, native/phone builds or CI tests. It selects changed
Workers inside its serialized deployment job, then installs/builds only their
packages. There is no Docker builder or image recovery build in this path.

Only managed, account and playground require WASM. Egress, X, email, Connect API,
Connect dialog, Astra and Chief of Staff use JavaScript. Explicit build tiers avoid
Turbo's general SDK-to-WASM build edge while preserving compiled dependency order.
When adding a runtime import or generated asset, update `workerSpecs`/build targets
and exercise a clean build with the generated WASM directories absent where the
Worker is declared JS-only. Source keys include runtime/config/assets and relevant
build scripts, not development stress tools or native-image preparation scripts.

The shared `.github/actions/wasm-outputs` action verifies exact-input browser/Node
bindings and retained raw WASM before setting up Rust. Actions cache is the fast
lookup; trusted master builds also retain exact-key artifacts for 90 days. Cache
eviction restores that artifact before considering compilation. Publication
deduplicates recent artifacts and refreshes those nearing expiry; PR/fork outputs
cannot seed production reuse. Verified hits skip Cargo,
wasm-bindgen and wasm-opt and reattest for the checkout. Misses rebuild with Rust
1.97. Both the outer cache key and inner binding stamp cover generator policy.
Provably non-WASM Cargo target tables and standalone tests/benches are excluded;
unknown targets, optional dependencies and build-script inputs remain conservative.
Python 3.11+ is required for deterministic release planning. Standalone WASM builds
can still compile without cache reuse if input discovery is unavailable; releases
fail early at planning rather than generating an unstable release identity.

Managed releases use already-published immutable phone/sandbox images. Selection
prefers a successful receipt for the current image inputs, falling back to the
latest successful published image while a new image builds independently. The
selected digests are pinned for the job and included in the Worker identity.
New Rust/image source alone never makes the API wait for publication. If no image
has ever been published, bootstrap publication is an explicit prerequisite; CI
never silently starts a native build inside an API release.

Image preflight and publishers run independently. Same-input publishers coalesce
and recheck durable GitHub Deployment receipts after acquiring their slot. Once
publication and the API release finish, a separate serialized rollout updates only
the relevant Workers. Its managed images must match current source inputs. If a
newer master supersedes that rollout, published digest changes are detected by the
next API release. Image failure cannot block the API production job.

Publication rejects dirty relevant source because image keys hash committed HEAD.
`MANAGED_IMAGE_CACHE_EPOCH` changes image identity, pulls bases and invalidates each
Docker stage's upstream layers; ordinary source changes retain layer reuse. Bump it
to refresh upstream content or recover deliberately deleted registry images. Audit
new Docker COPY/build-script reads and external generated inputs in the input helper.
Preview container decisions compare these same committed keys, so SDK JavaScript
alone cannot trigger native image builds.

The account relay image also publishes independently, once for all regions, from
its audited Docker inputs. API releases consume a published receipt; before the
first such receipt, they preserve each existing Cloudflare application's image.
Account image selection is pinned during planning. Vite's config is rewritten
beside its output, preserving relative paths. Account releases explicitly stamp
the revision, and health must report it before the release is certified.

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
