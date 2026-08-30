# Nanocodex web

The public product site, native documentation, live browser-agent consumer,
repository browser, and evaluation evidence for Nanocodex. The coding agent is
the library; this application proves that the same owned Rust lifecycle can sit
behind an opinionated web interface without turning that interface into an SDK
protocol.

The public demo family is explicit in the shared navigation:

- **Home** explains the library and links the independent proofs.
- **Agent** is one player using the browser-owned Rust/WASM agent in the TUI.
- **Multiplayer** is many humans in one ordered, replayable Durable Object room
  with one private, member-invoked, connector-free managed agent.
- **World** is one human in a game world populated by many browser-owned AI
  residents.

Multiplayer is the managed-agent deployment proof rather than another browser
agent. The website Worker forwards only `/v1/rooms` through its
`NANOCODEX_BACKEND` Service Binding. Invite capabilities stay in URL
fragments until exchanged for room-scoped HttpOnly cookies; the browser sees
room cursors and final agent replies, never managed agent/turn capabilities or
provider credentials. The managed runtime, in turn, has only a private
credential-broker Service Binding and fixed placeholders for both OAuth and
normal OpenAI API-key modes.

Every authenticated room member can invoke the shared agent; only the room
owner can end the room. Those turns spend from the one deployment-wide broker
credential under per-member, room, and global quotas. Membership never exposes
or selects that credential.

Exact `POST /v1/rooms` requests receive a create-room-only capability from the
website Worker's `MULTIPLAYER_ALLOCATOR_TOKEN` secret. The proxy strips every
browser-supplied `Authorization` header, and the Multiplayer page never asks
for or stores a deployment credential. Configure it to the same random value
as the private managed Worker's `NANOCODEX_ROOM_ALLOCATOR_TOKEN`; it is
deliberately distinct from `NANOCODEX_ADMIN_TOKEN` and cannot create, inspect,
prompt, or delete raw managed agents:

```bash
cd web
npx wrangler secret put MULTIPLAYER_ALLOCATOR_TOKEN
```

The managed Worker remains `workers_dev = false`; its
`NANOCODEX_BACKEND` Service Binding is the production entry point. Production
also fails closed unless the checked-in per-client and global room-allocation
rate-limit bindings are available; cross-origin allocation requests are
rejected before the server capability is used. A singleton backend quota object
adds the authoritative cross-location ceiling: 16 active two-hour rooms, 32
allocations/hour, and 240 admitted agent turns/hour across the deployment.

## Stack

- Vite + React
- Cloudflare Vite plugin and Workers runtime
- Wrangler for preview and deployment
- just-bash over the thread's OPFS filesystem, with browser `git` and `gh` compatibility commands
- Pierre Trees and Diffs for the file tree, source viewer, and the single virtualized commit stream
- TanStack Virtual for the commit quick-jump and evaluation indexes
- Derived job, trial, trajectory, and verifier views

The visual and content direction is captured in [`DESIGN.md`](DESIGN.md): a
Berkeley Mono-first, black-and-white simplification inspired by fx.sh and shaped
around Nanocodex's library ownership model. Treat that brief as the north star
while the existing surfaces are recomposed incrementally.

## Development

Local development is ordinary loopback HTTP. It needs Node and the repository
toolchain, not OrbStack, Docker, local TLS, or `.local` DNS. The primary checkout
runs at `http://nanocodex.localhost:5173`; worktrees receive a deterministic
single-label `.nanocodex.localhost` host and isolated port. Browsers treat the
reserved `.localhost` domain as a secure context.

All instances use WebAuthn RP ID `nanocodex.localhost`. A signed HttpOnly
parent-domain record containing only public passkey metadata lets a fresh,
isolated worktree verify the same credential without sharing mutable
Wrangler/Miniflare state. Exact origin, challenge, credential, and signature
checks remain local to each instance.

Provider OAuth applications register the four fixed
`http://127.0.0.1:47891/v1/connectors/<provider>/callback` URLs. A standalone
stateless relay verifies a ten-minute HMAC-authenticated routing envelope and
returns the browser to a fixed callback path on the initiating worktree. The
OAuth relay HMAC key is separate from passkey portability. Original broker
state, PKCE, code exchange, and provider credentials remain private.

```bash
cd web
npm install
npm run dev
```

That one command owns the complete production-shaped local stack. It asks the
canonical incremental Rust/WASM builder to validate its exact source/tool
fingerprint and repairs missing, malformed, or stale bindings. On the first run
it also prepares missing Worker dependencies, applies the local D1 migrations,
starts the website, managed, egress, and Connect API Workers in one local
Cloudflare session, and publishes the current committed Git `HEAD`
through the real repository publisher into local R2 and the repository Durable
Object. It reports ready only after a generation-pinned Source blob, commit
metadata and page, patch, Evals, and the read-only `/git` advertisement all
resolve that `HEAD`. The primary checkout retains Cloudflare state under
`~/.nanocodex/web-development`; worktrees use instance-scoped children of that
directory. Their Vite ports, application hosts, Durable Objects, R2, and D1
state are isolated, so multiple versions can run concurrently.
Passkey eligibility and verified credential identity are deliberately shared;
agent, account-session, connector, and repository state are not.

Set `NANOCODEX_DEV_INSTANCE=<name>` to pin an explicit instance name, or
`NANOCODEX_DEV_ORIGIN=http://127.0.0.1:<port>` to resolve a rare deterministic
port collision. Startup prints both the app and Connect playground URLs for the
instance. Ordinary shutdown retains that instance's state.

The launcher verifies Connect through the exact `.localhost` authority and
prints the Account page, playground, relay, and CLI command when ready:

```bash
NANOCODEX_CONNECT_DEVICE_BASE_URL=http://nanocodex.localhost:5173/v1/device nanocodex login
NANOCODEX_CONNECT_DEVICE_BASE_URL=http://nanocodex.localhost:5173/v1/device nanocodex connect github
```

The printed browser URL is the supported exact device authority for
host-managed browser verification. Its host remains under the shared
`nanocodex.localhost` WebAuthn RP ID. Do not substitute `127.0.0.1`, another
Wrangler port, or a standalone Connect dialog: those change the account and
WebAuthn boundary being tested.

The orchestrator loads the main worktree's root `.env` once before it selects
auth or starts a child, including when an agent launches the stack from a linked
worktree. It reconstructs every child environment explicitly: only the private
managed launcher and its credential broker receive `OPENAI_API_KEY` or Codex
auth configuration. Vite receives only the derived
`NANOCODEX_LOCAL_MODEL_ACCESS=managed` and non-secret auth mode; it maps those
to the website Worker's private `MODEL_EGRESS` binding. Vite and both Workers
otherwise receive only generated local tokens, secretless bindings, and
non-secret runtime settings. The legacy credential-bearing development proxy
is not part of managed localhost.
Vite env loading is disabled, and website `.dev.vars*` files are rejected; keep
local development settings in the main worktree's one root `.env` instead.

Local account connectors read their OAuth application credentials from the
same main-worktree `.env` using the production deployment names. The launcher
projects them only into the private auxiliary egress Worker:

```text
NANOCODEX_GITHUB_OAUTH_CLIENT_ID=...
NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET=...
NANOCODEX_GOOGLE_OAUTH_CLIENT_ID=...
NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET=...
NANOCODEX_X_OAUTH_CLIENT_ID=...
NANOCODEX_X_OAUTH_CLIENT_SECRET=...
NANOCODEX_WHOOP_OAUTH_CLIENT_ID=...
NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET=...
```

Register the callback paths under the instance URL printed at startup, as
described in `services/egress/README.md`. WHOOP requires HTTPS and therefore
cannot use the HTTP localhost relay. Connector controls remain visible
but disabled for browser-only guest sessions; a persistent passkey account is
required even when that guest session already has a ChatGPT connection.

When `OPENAI_API_KEY` is configured, `npm run dev` uses it without inspecting
another credential. Otherwise it automatically discovers an existing, valid `0600`
Codex login on the host and starts the private credential broker and managed
Multiplayer Worker in subscription mode. Local startup never opens an OAuth or
device-code flow: if neither credential exists, it stops with the exact
`codex login`, root `.env`, and web-only options instead. The website Worker
receives neither credential: it reaches the managed Worker over a local Service
Binding, and that Worker can reach only the private broker binding. Managed
readiness is a private child-process attestation emitted only after the broker
proves its configured auth with the fixed Responses WebSocket upgrade and the
managed Worker health check succeeds. The outer launcher then requires website
health to attest the same managed, non-interactive auth mode and opens one
same-origin `/api/responses` WebSocket through `MODEL_EGRESS`; it accepts only
the exact `nanocodex.proxy.ready` frame before reporting ready.
The explicit variants are:

```bash
npm run dev:subscription # require the local Codex login
npm run dev:api-key      # require OPENAI_API_KEY
npm run dev:web          # omit the managed Multiplayer stack
```

The homepage consumes the publishable `nanocodex`, `nanocodex-react`, and
`nanocodex-terminal` packages under `../js`; it does not reach into generated
WASM artifacts. Its
React integration creates the browser agent with
`useNanocodex({ config, threadId })` and observes its typed
event stream with `useAgentEvents`. React owns no Worker lifecycle, agent
history, credential policy, or model-loop state.

The local Worker and Vite client run together on their printed
`http://*.nanocodex.localhost:<development-port>` origin using the Cloudflare
Vite-plugin layout. No Docker daemon, Cloudflare account, or remote binding is
used by the normal development command. Provider credentials remain behind
private Worker bindings; see the
[Cloudflare Worker example](../services/managed/README.md#multiplayer-managed-agent-rooms)
for the deployment and live-smoke workflow.

### Documentation

The product guide lives in `docs/src/pages` and is rendered by the lazy native
Docs surface under `/docs`. The Markdown stays the source of truth; the Vite
application supplies the shared shell, responsive navigation, heading links,
code copy controls, and route-aware reading layout. `npm run build` checks that
every page entered the Docs bundle and generates `llms.txt` plus
`llms-full.txt` in the Cloudflare asset tree. The docs are not a second service,
generator, or visual system.

Development deliberately has no Vite-only Git shortcut. Source and Commits use
the same generation-qualified `/api/repository` objects and `/git` protocol as
production, backed by local R2 and the same publication Durable Object. Dirty
and untracked working-tree files therefore never leak into those surfaces.
Restart after committing to publish the new `HEAD`; set `NANOCODEX_REPO` before
startup to exercise a different committed checkout.

`npm run build` does not inspect Git or generate repository assets. Production
repository data is published separately to R2 by `npm run
publish:repository`. The publisher derives one coherent generation from a Git
commit, projects only the canonical `master` ref, uploads only
previously unseen source blobs and commit patches, builds one verified clone
pack for exactly those refs, uploads that pack in bounded immutable parts, and
stores new Git objects once in bounded immutable pack-entry shards. The Worker
streams the pack parts byte-for-byte as the complete pack for a fresh clone,
but uses the object graph and reusable shards to send only the closure missing
from an incremental or shallow fetch. Shards are compacted after a bounded
number of generations. Publication advances one
Durable Object pointer only after every referenced R2 object exists, so a failed
or concurrent publisher cannot expose mixed tree, history, or Git data. The
commit view resolves an immutable generation manifest, streams its aggregate
patch from bounded parts instead of issuing a request per commit, then parses
and publishes it in bounded batches while yielding between batches so the
first diff and scrolling stay responsive.

For this single-repository deployment, R2 owns immutable bytes and one Durable
Object owns the current generation with compare-and-swap publication. D1 is
deliberately absent: there is no repository registry, account model, search
index, or relational query to justify it. Publishing requires the same
`GIT_MIRROR_TOKEN` secret on the Worker and `NANOCODEX_GIT_TOKEN` in the
publisher environment. The publisher also requires `/api/health` to attest the
same complete Git SHA before it makes an authenticated request or uploads an
object:

```bash
NANOCODEX_GIT_ORIGIN=https://nanocodex.gakonst.workers.dev \
NANOCODEX_GIT_TOKEN=... \
npm run publish:repository
```

If the Durable Object contains an obsolete publication shape, the publisher
stops before uploading anything. Repair it atomically after deploying the
current Worker by explicitly opting into a current-format replacement:

```bash
NANOCODEX_GIT_ORIGIN=https://nanocodex.gakonst.workers.dev \
NANOCODEX_GIT_TOKEN=... \
NANOCODEX_REPAIR_INVALID_PUBLICATION=1 \
npm run publish:repository
```

The replacement is accepted only while the stored publication is invalid; it
cannot overwrite a valid generation or bypass its compare-and-swap head.

Production serves the website indexes, immutable file and patch objects, and a
read-only Git protocol-v2 endpoint from that publication. Clone the mirror with
`git clone https://nanocodex.gakonst.workers.dev/git`. GitHub remains the write
remote. After each current `master` commit passes CI, the website job deploys
the exact tested Worker with that SHA, waits for `/api/health` to return it as
`deployment_sha`, publishes the repository generation, and verifies both the
snapshot and Git protocol advertise the same SHA. An obsolete queued CI run is
not allowed to deploy or publish.

Each browser thread owns an OPFS working tree and an `origin` Cloudflare Git
remote on branch `nanocodex`. The Files and Commits surfaces read that thread's
actual Git objects in the browser; file blobs and commit patches are generated
on demand and released when the view refreshes. Push and pull notifications
cross the page/agent Worker boundary so an open repository view can preserve
its last complete render until the replacement snapshot is ready.

### Live eval view

`/evals` is part of the same production Vite and React application as the
Nanocodex homepage, embedded TUI, repository tree, and commit history. The
website reads its public API directly from the Cloudflare Worker. D1 owns the
task board and normalized result index; R2 owns task packages, case records,
and complete evidence. There is no coordinator host, tunnel, origin override,
or Access credential in the website read path.

Native benchmark hosts are disposable compute clients. They claim R2-backed
tasks from the Worker and authenticate every mutation with
`NANOCODEX_EVALS_WRITE_TOKEN`; they are never an authority for website reads.

The API is deliberately workset-oriented: the client loads the retained
workset index, drills into one workset's task summaries, loads one selected
treatment matrix, then requests a single opaque case ID for terminal evidence.
TanStack Query is the only application cache and owns polling, cancellation,
retry, and the overview/workset/task/case query lifetimes. There is no second eval-only HTML entry,
React root, Vite configuration, Node eval server, or browser-side SQL path.

The homepage is also a real embedded-agent demo with three deliberately thin
layers:

- `../js/bindings` publishes `nanocodex`, the viem-v3-style imperative client.
  Runtime entrypoints expose flattened `Agent.create` factories, decorated
  domain actions, standalone `Actions` namespaces, and typed watcher handles.
- `../js/react` publishes `nanocodex-react`, the wagmi-like headless React owner. Its provider and
  hooks manage the module Worker lifecycle, readiness, commands, and event
  subscriptions without imposing presentation policy.
- `../js/terminal` publishes `nanocodex-terminal`, the controlled transcript
  and composer presentation used by the site.
- `nanocodex/tools` owns the framework-independent live React document,
  bounded workspace store, and typed artifact tool used by the web consumer.
- `AgentTerminal` is the thin website policy wrapper around the package-owned
  `AgentTerminalView`. It supplies voice, artifact, account, runtime, and
  credential policy without introducing another conversation controller.

The module Worker loads the generated `nanocodex-wasm` package, and the Rust
engine owns the persistent Responses session, typed history, event stream, and
tool loop. Each thread opens one OPFS workspace shared by just-bash, Rust
`apply_patch`, isomorphic-git, the file viewer, commit history, uploads,
downloads, and the artifact dock. The model receives the standard
`exec_command` and Rust `apply_patch` tools rather than separate list/read/write
or Git tools. Shell commands include normal virtual Unix commands plus `git`
and `gh`; `git push origin nanocodex` publishes the same objects the
Commits view reads from the Cloudflare thread remote. Files survive agent,
Worker, and page restarts without being copied into conversation snapshots.
The Cloudflare Worker upgrades `/api/responses` and proxies OpenAI
tool calls. It accepts a user-provided OpenAI key into a one-hour Durable Object
session and returns only an opaque `HttpOnly`, `SameSite=Strict` cookie. The key
is never placed in a URL, local storage, React state, or WASM configuration.

Custom interfaces use the typed `render_artifact` tool composed by
`nanocodex/tools/browser`, alongside `exec_command`, `web__run`, and
`image_gen__imagegen`. The tool accepts JavaScript source defining a real React
`App`; `React`, an `html` tagged-template helper, and `sendPrompt` are supplied by the isolated iframe
runtime. Published documents live under `.nanocodex/artifacts` in the same Git
working tree and open in a fullscreen dock. Reusing an artifact ID replaces the
interface in place, so voice or text turns can continuously retheme and extend
it. Generated code has no imports, network access, or access to the parent page;
explicit `sendPrompt` actions re-enter the normal queued prompt lifecycle.
The browser agent requires an explicit user OpenAI key or ChatGPT session. A
presented session that cannot be read fails explicitly instead of falling back
to another credential.

The reusable `browser(...)` tool bundle gives the browser agent a bounded
`dataset` tool. It can inspect public
Parquet URLs, Hugging Face dataset/config/split exports, and uncompressed JSONL
URLs without downloading whole datasets into memory. Parquet reads use HTTP
ranges and filter/projection pushdown where possible; JSONL reads incrementally
scan the response stream. Dataset handles are scoped to an agent session. Query
limits and offsets accept any nonnegative safe range; input-byte and output-byte
budgets remain bounded. Partial results report `complete: false` and an opaque
`nextCursor` that retains projection and filters while resuming at the physical
Parquet row batch or JSONL byte position. The implementation and Parquet codecs
are lazy chunks, so ordinary agent sessions do not download them. Direct sources
must permit browser CORS. Parquet sources must honor byte-range requests; JSONL
sources must honor them when continuing from a cursor.

For example, ask the web agent to “inspect the `main` config’s `train` split of
`openai/gsm8k`, show its schema, and find five examples containing arithmetic.”
The resulting tool flow is equivalent to:

```json
{"operation":"open","source":{"kind":"huggingface","dataset":"openai/gsm8k","config":"main","split":"train"}}
{"operation":"query","dataset_id":"<returned id>","columns":["question","answer"],"filters":[{"column":"question","op":"contains","value":"how many"}],"limit":5}
{"operation":"query","dataset_id":"<returned id>","cursor":"<returned nextCursor>","limit":5}
{"operation":"close","dataset_id":"<returned id>"}
```

Run `npm run bench:dataset` in `js/bindings` for the deterministic 100,000-row
Snappy Parquet/JSONL browser-path benchmark. It reports cold and repeated query
latency, pulled bytes, range requests, scanned rows, and cache hits.

Development runs on the isolated `.nanocodex.localhost` origin printed at
startup. Provider credentials remain behind Worker Service Bindings and never
enter that browser origin.

Local development reads the optional ignored `.env` from the main Git worktree
through the repository workflow. BYOK uses the `BYOK_SESSIONS` Durable Object
binding; ChatGPT login uses its separate server-owned session boundary.

The browser agent does not use JavaScript Promise Integration (JSPI). Its
consumer startup gate checks only the platform APIs used by the shipped path:
a secure context, module Worker support, WebAssembly, WebSocket,
`crypto.randomUUID`, OPFS, and Web Locks. These are normal current stable
Safari/iPhone Safari capabilities; the real wasm-bindgen initialization remains
the authority for the shipped module and reports an actionable failure instead
of requiring Safari Technology Preview or a beta-only JSPI API.

Streaming events are coalesced once per animation frame before updating the
semantic transcript, and each independently scrolling transcript is
virtualized. `npm test` keeps the
event accumulator bounded under a 20,000-delta burst and covers assistant,
reasoning, and tool lifecycle updates.

The homepage also exposes the release contract: the checksum-verifying install
command, in-place `nanocodex update`, the crates.io SDK entry point, and links
to the latest GitHub Release and grouped conventional-commit changelog. GitHub
release notes also credit each pull request contributor.

Navigation stays available whenever an input is not active: `H`, `T`, `C`, `R`,
and `E` switch between Home, Code, Commits, Requests, and Evals. The repository
homepage is the root route. In Code, `Ctrl+P` searches the left tree and `Ctrl+F` opens the
fuzzy all-file jumper. In Commits, `F` searches history. Code and commit
scrolling are left to Pierre CodeView and the browser's native input behavior.

## Production

`master` CI can own production deployment after the `CLOUDFLARE_API_TOKEN`
repository secret, `CLOUDFLARE_ACCOUNT_ID` repository variable, and
`CLOUDFLARE_DEPLOY_ENABLED=true` repository variable are configured. The
existing `NANOCODEX_GIT_TOKEN` publishes the matching repository generation.
Without that explicit enablement, CI still validates the complete production
graph but does not mutate the hosted Worker. Local commands build and preview
it:

```bash
npm run build
npm run preview
```

For a break-glass production deployment, start from a clean commit and preserve
the same attestation contract before running `publish:repository`:

```bash
npm run deploy
```

The deploy command requires `HEAD` to equal the fetched `origin/master`, binds
that full commit SHA into the Worker version, rolls only that version to 100%
without rebuilding unchanged containers, and does not return successfully until
the live health endpoint attests the same revision.

Do not publish repository data until the hosted `/api/health` reports that
exact `deployment_sha`. The publisher enforces this ordering independently. An
authenticated operator can publish the already-deployed master revision with:

```bash
gh workflow run mirror-cloudflare-git.yml --ref master -f revision="$revision"
```

For the one-time invalid-publication repair, add
`-f repair_invalid_publication=true` to that dispatch.
