# Deployment and CI execution paths

Cloudflare production has two independent managed image publishers (phone and
sandbox), followed by the ordered Worker deployment. Each publisher restores a
small receipt keyed by the committed inputs used by its Dockerfile, the account,
platform, helper version, and `MANAGED_IMAGE_CACHE_EPOCH`. A hit skips dependency
installation, Docker build, image smoke tests, and registry push; these already
ran when that exact image receipt was produced. A miss builds and verifies the
image, pushes it through Wrangler, and records its actual registry SHA-256 digest.
Before mutating production, push deployments check for a newer Cloudflare push
under the production concurrency lock and skip superseded runs. Explicit manual
deployments remain possible. The deployment accepts only receipts matching the current source inputs and
account. It has no fallback to a mutable tag or a Dockerfile build.

These input keys are cache keys, not image digests. Docker base tags and package
repositories may change independently of source. To refresh upstream image
contents, or recover from a deleted registry image, increment the repository
variable `MANAGED_IMAGE_CACHE_EPOCH` (default `1`) and dispatch Cloudflare with
`target=production`. An Actions cache eviction is harmless: the image is rebuilt.
Keep the resulting registry digests while their receipts remain cached. Config,
resource, and binding changes are still applied with the reused image; Wrangler
compares the existing application configuration and skips an identical rollout.

Production completion includes a bounded public account Worker health check.
The separate **Cloudflare live validation** workflow follows successful master
production deployments, including manual production runs. Its gate excludes
preview-only runs. It retains the full cron, large-input, and 96-turn durability
journeys, checks out the deployed run's revision, and reports its own failures.
Only one live validation workflow runs at a time; newer pending runs coalesce.
It may also be dispatched manually on master. These checks exercise live
production, which can advance during a long test; they do not certify a frozen
preview environment.

CI builds WASM once and uploads both `pkg-node` and `pkg-web`, including package
markers, declarations, and the attestation. Binding tests and JavaScript app
checks download the same artifact and run independently. The success gate still
requires every job. The desktop Hand integration test has a separate 60-second
watchdog because a node:test timeout cannot reliably interrupt child-process
teardown; phase logs identify the blocked operation without losing coverage.

## Measurement

Report runner wait, actual job execution, deployment completion, and endurance
completion separately. Compare the first cold run and a subsequent run with
unchanged image inputs; a cold receipt miss intentionally still publishes images.
Record cache hit outputs, registry push duration, and any failures before claiming
a speedup. The prior observed production workflow took 47m04s: 5m09s initial wait,
24m31s deployment, 1m44s wait for durability, and 15m40s durability. Its managed
container step took 20m03s, including a 7m59s sandbox push. No measured after figure
is implied by the new execution graph.
