# API release path

The API production job starts directly on a master push, without `needs` on image
planning, Docker publication, native/phone builds or CI tests. It selects changed
Workers inside its serialized deployment job, then installs/builds only their
packages. There is no Docker builder or image recovery build in this path.

Only managed, account and playground require the Rust SDK WASM build. The private media
Worker bundles its own checked-in FFmpeg WASM; it, egress, X, email, Connect API,
Connect dialog, Astra and Chief of Staff do not schedule a Rust SDK build. Explicit build tiers avoid
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

Managed's upload command also owns its private-account CRM D1 database. It
resolves or creates `nanocodex-crm-production`, pins the returned UUID into the
same generated config used by migration and upload, and applies pending SQL
migrations before deployment. Each mutation rechecks current master; a failed
migration stops the phase and dependent Workers. The production token requires
D1 edit permission. Schema migration files and the preparation helper participate
in managed's release fingerprint. See [managed database operations](../../js/managed/README.md#private-account-crm-database).

Managed preview validation uses `pnpm run preview` in `js/managed`: an isolated
local CRM migration followed by a Worker dry-run. It replaces production D1 IDs,
removes named environments and cloud credentials, and creates no cloud database.

Selected deployments preserve dependency phases: X, private media, managed, egress,
consumers, then account. Managed publishes the private `PhoneProvider` entry point
before egress adds its service binding. A scoped `RELEASE_ONLY=managed` also selects and redeploys
media before managed; unchanged media is otherwise safely reused through the live
Worker deployment ledger. Independent members run concurrently. Every mutation rechecks current
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

## PR previews using the production backend

PRs publish the branch's account app and Connect assets as native Cloudflare
Previews. The default `--backend production` sends account API, authentication,
credential, streaming, and mutation requests through a service binding to the
existing production account Worker. That Worker uses the production managed
service and credential broker. Existing broker credentials stay in the broker;
no secret export, duplicate credentials, or Preview Base storage setup is needed.
The preview uses production account data and operations. It does not test branch
changes inside the managed backend or production Durable Objects.

Run `node scripts/cloudflare/preview-workers.mjs deploy --name pr-123` after
building/restoring the same-revision artifacts. Set `CLOUDFLARE_ACCOUNT_ID`,
`CLOUDFLARE_API_TOKEN`, and `GITHUB_SHA` (or `PREVIEW_REVISION`). `check` performs
read-only readiness checks. `--component assets` publishes only Connect assets.
The authenticated backend receives the original URL, cookies, bearer token,
origin, body, and WebSocket upgrade; the proxy does not assert a user identity or
bypass authentication. A missing production service fails closed with 503.
A preview hostname has its own browser cookies; opening it is not automatic login.

The default mode publishes no branch backend and requires no duplicate production
secrets. The account Preview wraps the built entry point, retaining branch app
rendering and assets. Backend routes run in production, including login and
credential-broker operations. The wrapper is generated beside the built entry
point, then removed after upload. It does not deploy production code.

### Optional isolated backend

Manual dispatch `preview_backend=isolated` (CLI `--backend isolated`) retains
account-to-managed branch routing with a per-deployment authenticated HTTP bridge.
This mode needs Preview Base secrets and separately provisioned storage. Local
Durable Objects/containers are isolated, while foreign service/DO bindings target
production and are recorded in the manifest. It does not automatically copy
existing identity or subscription records into its new namespaces.

Only isolated mode requires matching production secret names in Preview Base.
The optional encrypted CI secret `NANOCODEX_PREVIEW_BASE_SECRETS_JSON` maps Worker
names to secret-name/value objects; it is validated and applied privately without
logging values. Existing Previews do not inherit later Base secret changes.
Isolated mode also checks separate D1/R2/KV/AI Search Base resources. It does not
create storage or run migrations. Parent Worker Preview URLs must be enabled.

### Verification and cleanup

Same-revision artifacts, deployment receipts, HTTP serving probes and boundary
metadata are retained in `output/cloudflare-previews/pr-N/manifest.json` and CI
artifacts. Account root must return HTML and `/v1/credentials` must reject unauthenticated
requests. These probes verify serving and authentication rejection, not a signed-in
model turn. Real workerd routing journeys additionally exercise credential
metadata, authentication failure, mutation origins, streaming and upgrades using
synthetic fixtures. No real credentials or authenticated responses are logged.

On PR close, `preview-workers.mjs delete --name pr-N` deletes named native Previews
and verifies absence. It leaves production Workers, broker credentials, and data
untouched. Individual deployment failure leaves completed receipts in the manifest.
Wrangler output is suppressed because binding output can contain values.
