# Nanocodex on Rivet Actors

This example runs the real Rust/WASM Nanocodex harness as a durable
[Rivet Actor](https://rivet.dev/docs/actors/). Nanocodex is already an agent
harness, so it runs directly in the actor host with one owner for model history,
tools, retries, and cancellation. Rivet AgentOS supplies only the actor-owned
isolated VM, persistent filesystem, processes, and preview routing underneath
Nanocodex's caller-defined tools; its optional agent/session adapters are not
used.

The published AgentOS core currently declares Pi software as a transitive npm
dependency. This example never imports, mounts, or invokes it. The dependency
guard rejects direct Pi dependencies and source imports, while allowing that
dormant upstream package to remain in AgentOS's lockfile subtree.

## Architecture

Each `nanocodex` actor owns one conversation:

- the live WASM driver, event watcher, and turns live in ephemeral `c.vars`;
- the actor's embedded SQLite database supplies atomic load and
  compare-and-append for opaque Nanocodex journal batches;
- the AgentOS root filesystem is chunked into the same actor-owned SQLite
  storage and restored automatically across VM sleep/wake cycles;
- `sandbox_exec`, `sandbox_read_file`, `sandbox_write_file`,
  `sandbox_list_files`, `sandbox_start_process`, and `sandbox_preview` expose
  bounded AgentOS operations to Nanocodex without putting model credentials in
  the guest;
- Rust/WASM owns typed checkpoints, terminal results, input conflicts, and
  recovery across model and tool steps;
- duplicate turn IDs share one in-flight promise or replay Nanocodex's durable
  result without another model call;
- `onSleep` and `onDestroy` cancel turns, close the Responses WebSocket, and
  release WASM resources;
- the opaque Rivet actor ID is deterministically projected to a stable UUID for
  Nanocodex's session contract.

Actions execute in parallel in Rivet. The actor therefore caps fan-in at 16
turns and keeps each prompt action awake through its complete model turn.
The host's `completed_operations` table is presentation metadata for status
counters and duplicate-broadcast suppression; it is not a second replay log.

Local development reads the current access token and account ID from the
mode-`0600` Codex auth file for each connection. It never uses the refresh token
or stores credentials in Rivet SQLite. Deployments can instead use the singleton
`nanocodexAuth` actor to own dedicated rotating credentials, refresh five
minutes early, single-flight concurrent refreshes, and retry one WebSocket
upgrade after revision-guarded 401 recovery. Disposable deployments may omit
the refresh token and use only the current Codex access token, matching the
Cloudflare demo. In both paths bearer credentials remain in host code and never
enter Nanocodex WASM.

## Build and run

Build the repository's browser-compatible WASM package first:

```sh
just build-wasm
npm ci --prefix examples/rivet-actors
npm run check --prefix examples/rivet-actors
```

The dependency check fails if this application directly declares or imports a
Pi adapter or upstream Pi harness package.

Log in to Codex with the ChatGPT subscription you want to use, then start the
real subscription server:

```sh
codex login
npm run dev:subscription --prefix examples/rivet-actors
```

The process starts the actor engine on <http://127.0.0.1:6420> and a tiny web
client on <http://127.0.0.1:6422>. (Rivet reserves 6421 for its local control
plane.) The page uses RivetKit's browser client
directly—there is no Nanocodex app-server protocol or model credential in the
browser. It stores only the endpoint, actor key, bounded transcript, and any
unfinished turn ID/input. Detach or close the tab during inference; reopening
it reissues the same idempotent request and rejoins or replays the actor turn.
The local launcher keeps the web listener alive after engine readiness and
reaps only its recorded `rivet-engine` child if npm or the app exits abruptly,
so Ctrl-C does not leave the reserved ports occupied.

This reads `~/.codex/auth.json` by default. Override it with
`NANOCODEX_CODEX_AUTH_FILE` or `CODEX_HOME`. The access token is reread when a
new socket opens, but the refresh token is never used or persisted by the
local path. If ChatGPT rejects the current token, run `codex login` again. Actor
state defaults to `$XDG_STATE_HOME/nanocodex/rivet-subscription-demo` or
`~/.local/state/nanocodex/rivet-subscription-demo`.

In another terminal:

```sh
npm run repl --prefix examples/rivet-actors
npm run smoke --prefix examples/rivet-actors
npm run multiclient --prefix examples/rivet-actors
npm run stress --prefix examples/rivet-actors
npm run brutalize --prefix examples/rivet-actors
```

`multiclient` attaches two independent realtime clients to one actor and
requires both to receive the same accepted prompt, Nanocodex event stream, and
durably committed terminal result. The browser uses the same broadcasts, so
tabs or devices connected with the same actor key stay synchronized. A newly
connected client also reconstructs any active prompt from `status()` before
joining its remaining event stream.

For the hosted preview demo, use AgentOS's Node runtime for listening servers.
Its `python3` command is Pyodide-backed and does not provide a bindable server
socket. `sandbox_preview` probes the requested port before minting a signed URL,
so a failed or exited server cannot produce a dead preview capability.

The REPL is intentionally disposable. It stores only the Rivet endpoint,
actor key, and an unfinished turn ID/input in `.nanocodex/rivet-repl.json`.
The file is mode `0600` because it may contain unfinished prompt text.
The `start` action transfers ownership to the actor with `keepAwake` before it
returns; the REPL then waits through the ordinary idempotent `prompt` action.
Pressing Ctrl-C kills only that waiter. Re-running the command with the same
`NANOCODEX_REPL_SESSION` reattaches to the active turn or replays its committed
result, then continues the actor's durable conversation. Use `/status` or
`/exit` at the prompt. Set `NANOCODEX_REPL_STATE` to isolate another local
REPL state file.

This demonstrates durable client detachment, not distributed exactly-once
inference. If the actor host itself dies before a turn commits, reopening the
REPL resubmits the same turn from Nanocodex's last committed checkpoint. A
completed model step is replayed after actor loss; an unsafe tool start without
a committed completion stops with an explicit ambiguous-outcome error.

The local Rivet endpoint is `http://127.0.0.1:6420`. Set
`RIVET_PUBLIC_ENDPOINT` for another deployment. Set `NANOCODEX_WEB_HOST` or
`NANOCODEX_WEB_PORT` to move the static browser client; it binds to loopback by
default. On Rivet Compute it runs RivetKit's serverless listener on the injected
`PORT`, satisfying the runner health check and serving both the actor routes and
demo page from the same container. A fresh hosted page selects its same-origin
`/api/rivet` endpoint automatically. A separately hosted static page can select
a deployment without embedding it in source by adding
`?endpoint=https%3A%2F%2F...` to the page URL.

The smoke also forces the real model to write, execute, and read through
AgentOS, starts a native Python HTTP server, creates a signed preview, and
fetches that preview from outside Rivet. It requires completed Nanocodex
`tool.call`/`tool.result` event pairs, so it verifies the WASM tool loop,
isolated VM, and public proxy rather than only a text response. If one client
request reaches the Rivet gateway deadline while the actor turn is still
active, the smoke reattaches with the same turn ID instead of starting another
model call.

The stress driver reuses persistent actor connections and bounds fan-out to
avoid benchmarking the gateway's per-route rate limiter. Tune
`NANOCODEX_STRESS_ACTORS`, `NANOCODEX_STRESS_REPLAYS`, and
`NANOCODEX_STRESS_CONCURRENCY_PER_ACTOR` when sizing a deployment.
The longer `brutalize` soak reconnects every client between seeding and replay,
resets a bounded actor pool after each wave, and reports replay latency
percentiles from a constant-memory histogram plus best/worst wave throughput.
Tune its corresponding `NANOCODEX_SOAK_*` variables for larger runs. Set
`NANOCODEX_STRESS_KEYSPACE`
or `NANOCODEX_SOAK_KEYSPACE` when running multiple drivers concurrently; the
stable defaults prevent repeated local runs from accumulating actor records.

## Deployment-managed subscription authentication

The local command above is the safest demo path. A deployed actor cannot read
your local Codex auth file. For a disposable hosted demo, the deployment helper
securely reads the current access token and account metadata, checks the auth
file permissions and token lifetime, and does not copy the rotating refresh
token:

```sh
# The first invocation can set RIVET_CLOUD_TOKEN; Rivet stores it in
# ~/.rivet/credentials for subsequent deploys.
export RIVET_CLOUD_TOKEN=cloud_api_xxxxx
npm run deploy:subscription --prefix examples/rivet-actors
```

This is the same access-token-only policy as the Cloudflare demo. It keeps
working until the current access token expires or ChatGPT rejects it; then run
`codex login` and deploy again. Once the Rivet CLI has cached credentials,
`RIVET_CLOUD_TOKEN` may be omitted. `NANOCODEX_CODEX_AUTH_FILE`, `CODEX_HOME`,
and `RIVET_NAMESPACE` override the auth path and target namespace. The helper
derives the disposable credential actor key from a one-way token fingerprint,
so a newly authenticated deployment seeds fresh state instead of reopening an
expired access-token-only actor. Set `RIVET_REUSE_IMAGE=1` to update only this
deployment environment after the current source image has already been pushed.

For a long-lived deployment, give the auth actor dedicated rotating
subscription credentials instead. This still does not require
`OPENAI_API_KEY`:

```sh
export NANOCODEX_AUTH_MODE=chatgpt
export NANOCODEX_AUTH_ACTOR_KEY=my-deployment-subscription
export NANOCODEX_AUTH_CAPABILITY=a-separate-random-secret-of-at-least-32-bytes
export CHATGPT_ACCESS_TOKEN=...
export CHATGPT_REFRESH_TOKEN=...
export CHATGPT_ACCOUNT_ID=...
export RIVET__file_system__path=/tmp/nanocodex-rivet-demo/engine-db
npm run dev --prefix examples/rivet-actors
```

`CHATGPT_FEDRAMP=true` and `CHATGPT_TOKEN_ENDPOINT` are optional. Set
`NANOCODEX_AUTH_ACTOR_KEY` to a stable key unique to the deployment; the local
fallback is `subscription`. Reusing a key intentionally resumes the persisted
rotating credential instead of reseeding it from environment variables.
`NANOCODEX_AUTH_CAPABILITY` is required in subscription mode. Every credential
action receives a short-lived, operation-bound HMAC proof with replay defense;
the capability itself never crosses the actor RPC boundary. Keep it separate
from the actor key.

ChatGPT refresh tokens rotate. Use credentials dedicated to this deployment;
do not share the same refresh token with a local Codex installation or another
deployment. Protect the auth actor and the Rivet endpoint with application
authentication before exposing them publicly. The example intentionally never
returns access or refresh tokens from its status actions.

## Events and lifecycle

Subscribe before prompting:

```ts
const session = client.nanocodex.getOrCreate(["conversation"]);
const connection = session.connect();
connection.on("agentEvent", (event) => console.log(event));
connection.on("turnCompleted", (result) => console.log(result));

await session.turn({ id: crypto.randomUUID(), input: "Hello" });
await session.unload(); // Nanocodex journal remains durable
await connection.dispose();
```

Use `reset()` to delete the Nanocodex journal and completion metadata.
Rivet automatically sleeps idle actors after 30 seconds.

## Deployment

Rivet Actors can run on Rivet Compute or a self-hosted Rivet platform. The
included production image builds Nanocodex WASM, compiles the actor server, and
starts it without a TypeScript runtime dependency. AgentOS's native sidecar is
installed with the production dependencies and its actor runtime socket is
enabled by the AgentOS registry setup. From the repository root,
get a cloud token from the Rivet dashboard's **Connect > Rivet Cloud** page and
deploy with API-key model authentication:

```sh
export RIVET_CLOUD_TOKEN=cloud_api_xxxxx
export OPENAI_API_KEY=sk-...
npx @rivetkit/cli@2.3.10 deploy \
  --dockerfile examples/rivet-actors/Dockerfile \
  --build-context . \
  --namespace production \
  --env NANOCODEX_AUTH_MODE=api_key \
  --env OPENAI_API_KEY="$OPENAI_API_KEY"
```

For ChatGPT subscription authentication, use the access-token-only helper from
the preceding section. Set `CHATGPT_REFRESH_TOKEN` before invoking it only when
that refresh token is dedicated to this deployment; never copy the refresh
token still used by a local Codex installation.

Once the pool is ready, copy a client-safe publishable endpoint from the
namespace's **Connect** page in the Rivet dashboard. Use that endpoint with the
REPL, smoke driver, or browser client:

```sh
export RIVET_PUBLIC_ENDPOINT='https://<namespace>:pk_...@api.rivet.dev'
npm run smoke --prefix examples/rivet-actors
npm run repl --prefix examples/rivet-actors
```

Set `NANOCODEX_PUBLIC_URL` before running the subscription deployment helper to
the client-safe Rivet endpoint from the namespace's **Connect** page (the
`https://namespace:pk_...@api.rivet.dev` form). `sandbox_preview` converts that
to an actor-specific gateway URL with `rvt-namespace` and `rvt-token` query
parameters, which browsers and `fetch` can open. Without it, the tool returns
the actor-relative `/fetch/<token>` path. This is Rivet AgentOS's equivalent of
Vercel Sandbox's `domain(port)`: it creates a temporary signed proxy to that
port. This example bounds preview tokens to 15 minutes and keeps the actor's
server launch specification in actor SQLite. The VM can sleep normally; its
`onVmStart` hook restarts unexpired preview servers before AgentOS proxies the
waking request. Workspace files and preview tokens survive the same cycle.

The browser bundle is static and can be published on any static host after
`npm run build:web --prefix examples/rivet-actors`; paste the same public
endpoint into its endpoint field. Anyone with that endpoint can invoke this
example's actors and spend model tokens because the demo intentionally does not
add application authentication. Use a dedicated namespace/token and remove or
protect it after the demo.

Monitor the hosted runner with
`npx @rivetkit/cli@2.3.10 logs --follow --namespace production`. See the official
[Rivet Compute deployment guide](https://rivet.dev/docs/deploy/rivet-compute/)
and [endpoint guide](https://rivet.dev/docs/general/endpoints/).
