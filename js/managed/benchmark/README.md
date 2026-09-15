# Isolated managed-service benchmark fixture

This fixture deploys the real managed and credential-broker implementations under
separate service names, with an isolated account, private Durable Object namespaces
and an R2 bucket. `front.ts` imports the production account forwarding function.
It is used by the [differential benchmark](../../../output/cloudflare-agents-2026-09-11/README.md).
Production entrypoints do not import this directory.

`configure.py` derives the configurations from the current production files. Before
reusing it, change the service/bucket names, account-specific container image, and
fixture account UUID for your own isolated deployment. The committed names and
version IDs describe the September 11 experiment, not general defaults. Never
point the fixture initialization route at an existing user account.

```sh
uv run --no-project --with json5 python js/managed/benchmark/configure.py
npx --yes pnpm@11.25.0 --filter nanocodex-managed-service exec \
  tsc --noEmit -p benchmark/tsconfig.json
```

Provision the R2 bucket, then deploy `bootstrap.json` to establish the service name
for the broker's ownership binding. Deploy `egress.json` with its private namespaces
and a `CREDENTIAL_ENCRYPTION_KEY` secret (32 random bytes, base64url encoded). Deploy
`managed.json` with its actual Sandbox container binding, then `front.json`. The
full managed deployment replaces the bootstrap Worker. Use Wrangler's existing
container deployment workflow to register the referenced image/application; do
not replace Sandbox cleanup with a stub to make deletion appear successful.

The benchmark-only managed secrets are:

- `BENCHMARK_INIT_TOKEN`: an independent strong random initialization credential.
- `BENCHMARK_API_KEY`: a new fixture key with the ordinary format
  `ncx_live_<12 base64url characters>_<43 base64url characters>`.
- `BENCHMARK_OPENAI_KEY`: the authorized provider key used by the comparison client.

Set secrets with `wrangler secret bulk --config benchmark/managed.json`, reading
from a private file outside the repository. Do not put values in command-line
arguments, tracked files or logs. `BENCHMARK_USER` is the fresh account UUID in the
configuration. POST `/__benchmark/init` on the managed service with the init bearer
credential once: it uses the normal account/key creation and provider-binding
implementations. Ordinary `/v1/agents` requests then use the fixture API key and
normal authorization. The fixture exposes no unauthenticated account mutation.

Point the benchmark client at the front Worker. The client journals session IDs
as soon as they are known and deletes sessions after each job; recover journaled
sessions if the client exits unexpectedly. Capture optional managed Worker traces
using `wrangler tail --config benchmark/managed.json --format=json` to a private
local file. Reduce it with `cloudflare-agents.profile.py`; never commit raw request
headers. Keep one managed version deployed throughout each matrix.

After measuring, delete fixture sessions, the container application, Workers,
isolated Durable Object namespaces and R2 objects/bucket, then remove private
fixture credential files. Inspect resource names before deletion and leave
production resources and the original provider key intact.
