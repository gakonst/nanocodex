# Nanocodex for JavaScript

The Node, browser, and Web API host entrypoints expose the same viem-v3-style
API. A `Transport` owns authentication, placement, and socket setup;
`Agent.create(...)` owns tools and the common Agent/Turn lifecycle. Generated
WASM handles, managed control-plane handles, and host routing remain private.

```js
import { Actions, Agent, Transport } from "nanocodex/node";

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  model: "gpt-5.6-luna",
  instructions: "You are a Rust coding agent. Preserve unrelated work and run relevant tests.",
  reasoningMode: "pro",
  thinking: "high",
  tools,
  workspace: process.cwd(),
});

const turn = agent.turn.prompt({ input: "Build the thing." });
const result = await turn.result();
turn.dispose();
console.log(result.finalMessage);
const usage = await result.usage();
console.log(usage);
console.log(usage.estimated_cost?.usd);
console.log(usage.cost_status);

await agent.session.setThinking("high");
await agent.session.setFastMode(true);
await agent.session.compact();

const branch = await agent.session.fork({ at: result });
const branchTurn = branch.turn.prompt({ input: "Try another approach." });
const branchResult = await branchTurn.result();
branchTurn.dispose();
console.log(branchResult.finalMessage);
branchResult.dispose();

const followOn = Actions.turn.prompt(agent, { input: "Now explain it." });
const followResult = await Actions.turn.getResult(followOn);
console.log(followResult.finalMessage);
followOn.dispose();
followResult.dispose();
result.dispose();
await branch.session.shutdown();
await agent.session.shutdown();
```

Transports are explicit, immutable configurations, like viem v3 transports:

```js
Transport.openAi({ apiKey, websocketUrl });
Transport.chatGpt({ subscription });
Transport.mpp({ session: paymentSession });
Transport.managed({ agent: { create: true } });
Transport.managed({ agent: { id: retainedAgentId } });
```

Managed identity is always explicit. `{ create: true }` provisions one new
account-owned durable Agent; `{ id }` eagerly verifies and opens that existing
Agent. Omitting `agent` never creates a durable resource. Both return the same
`sessionId`, `events.watch()`, `turn.prompt()` / Turn, `dispose()`, and
`session.shutdown()` lifecycle used by local transports. Managed shutdown
closes this client and any reverse tool attachment; it does not delete the
durable Agent.

Choose the entrypoint by execution owner:

- `nanocodex/browser` creates and owns a package module Worker. Its options are
  structured-clone-safe and its default harness includes the browser workspace.
- `nanocodex/host` runs in the current Web API isolate. Use it inside a
  caller-owned browser Worker, Cloudflare Worker, Vercel Function, or similar
  host when transports, tools, filesystems, or durability contain functions.
- `nanocodex/node` runs in the current Node process with Node host adapters.

The browser transports additionally expose `Transport.hostManaged(...)` for a
Worker, Durable Object, or application proxy that owns rotating credentials.
Authentication modes are constructors rather than a union of mutually
exclusive fields on `Agent.create`.

### Compose and place tools

`createTools` owns one deterministic tool recipe. Custom functions, a portable
workspace, and MCP are composed once; placement is selected afterward. Pass the
recipe to an in-process Node or Web API host, or reverse-attach it to a managed
agent target:

```js
import { createTools } from "nanocodex";
import { Agent, Transport, Workspace } from "nanocodex/node";
import WebSocket from "ws";

const workspace = await Workspace.open({ path: process.cwd() });
const tools = await createTools({
  workspace,
  tools: {
    lookup_issue: {
      description: "Read one issue from the application database.",
      parameters: {
        type: "object",
        properties: { id: { type: "string" } },
        required: ["id"],
        additionalProperties: false,
      },
      handler: ({ id }) => issues.get(id),
    },
  },
  mcp: {
    docs: { url: "https://mcp.example.test" },
  },
});

const agent = await Agent.create({
  transport: Transport.managed({
    agent: { id: agentId },
    baseUrl: managedOrigin,
    apiKey,
    toolsTransport: (target, options) => new WebSocket(target, {
      headers: options.headers,
    }),
  }),
  tools,
});

// On shutdown:
await agent.session.shutdown();
```

The managed target retains credentials in a private transport closure; the API
key is not embedded in the endpoint or serializable target data. While the
attachment is live, an exact same-name attached tool wins over the cloud tool.
After detach, the cloud definition is immediately eligible again. Definition
parity is validated before the attached catalog becomes active, and calls
already admitted retain their pinned placement.

`Tools` has one Agent owner and owns the lifecycle of its MCP runtime and
reverse attachments. Local transports host the recipe in process; a managed
transport starts a bounded reverse-attachment supervisor while the durable
Agent remains available through its cloud tools. A successful catalog
acknowledgement upgrades later admissions to the attached placement. A second
Agent host rejects the same value. Do not also supply legacy top-level
workspace or MCP configuration to an Agent that already receives them through
`Tools`.

### Use the embedding website through WebMCP

`nanocodex/browser` can turn the live host page's WebMCP registry into dynamic
agent tools. Handlers remain in the window so they use the website's existing
signed-in session and UI state; only structured definitions and bounded calls
cross into the package-owned Agent Worker.

```js
import { Agent, Transport } from "nanocodex/browser";

const agent = await Agent.create({
  transport: Transport.hostManaged({ websocketUrl: "/api/responses" }),
  webMcp: { fallback: "when-empty" },
});
```

Native `document.modelContext` tools update live on `toolchange`. When none are
available, the default bounded fallback can observe visible page text and
controls, fill fields, activate controls, and submit forms. It never exposes
arbitrary page JavaScript, hidden elements, raw selectors, HTML, or password
values. Set `fallback: "never"` for native-only behavior. Every mutating call
opens the bundled Nanocodex approval dialog. Its trusted chrome shows the
requesting website, destination, exact action, one-time scope, and expandable
payload. The dialog remains in `Applying…` until the website handler settles;
rejection never invokes it. Read-only tools do not prompt. A `confirm` callback
is available only as a headless/custom-host override.

Managed browser agents accept the same `webMcp` option. Nanocodex automatically
reverse-attaches the page provider, preserving website authentication in the
browser while the account-owned conversation remains hosted.

Existing Vite applications need no generation command or WebMCP specification.
The plugin derives bounded candidates from source and injects the normal
Accounts-backed Nanocodex embed in development so the authenticated Agent can
verify them against the live page:

```ts
// vite.config.ts
import { defineConfig } from "vite";
import { nanocodex } from "nanocodex/vite";

export default defineConfig({
  plugins: [nanocodex()],
});
```

```ts
// next.config.ts
import type { NextConfig } from "next";
import { withWebMcp } from "nanocodex/next";

const nextConfig: NextConfig = {};
export default withWebMcp(nextConfig);
```

Vite owns the complete generation and verification lifecycle when the page
opens in development. It reuses `tempoxyz/accounts` for remembered passkeys,
ChatGPT subscription, Tempo MPP, and provider selection. Credentials stay in
Accounts and the broker; Vite receives only the source revision and manifest.
The Agent may improve the public contract or remove false positives, but Vite
pins execution targets, source evidence, and read/write annotations. A source
change invalidates the verified revision. Production builds make no model call.

Next.js currently retains deterministic source generation through
`withWebMcp()`.

Lower-level explicit commands remain available for non-Vite build systems:

```sh
npx nanocodex-webmcp generate . --out webmcp.manifest.json
npx nanocodex-webmcp check webmcp.manifest.json
```

Every new or changed candidate is emitted with `approved: false`. After
reviewing its source evidence and marking intentional tools approved, publish
it from the website:

```js
import manifest from "./webmcp.manifest.json" with { type: "json" };
import { publish } from "nanocodex/webmcp";

const publication = await publish(manifest, {
  handlers: {
    get_account: (input, { signal }) => accountClient.get(input, { signal }),
  },
});
```

The publisher ignores unapproved entries and unregisters approved tools when
`publication.close()` aborts their registrations. Same-origin fetch and form
candidates can execute directly; framework-client candidates such as GraphQL,
tRPC, and server actions require an explicit handler. Cross-origin frames also
require `allow="tools"`, exact `exposedTo` origins when publishing, and matching
`fromOrigins` when consuming. See the
[WebMCP capability guide](https://nanocodex.ai/capabilities/webmcp).

Browser consumers can attach Codex's ChatGPT Realtime voice lifecycle to the
same retained Agent. The resource owns microphone, speaker, WebRTC, sideband,
and delegation cleanup; stopping voice does not cancel an active coding turn.

The one-operation-at-a-time action surface is the canonical imperative API:

```js
import { Actions } from "nanocodex/browser";

const voice = Actions.voice.create(agent);

await Actions.voice.start(voice); // defaults to Codex's `cove` voice
await Actions.voice.stop(voice);
await Actions.voice.destroy(voice);
```

`Voice.create(...)` remains the equivalent namespaced resource constructor, and
`Voice.voices` is the exact ChatGPT V3 voice catalog. The constructor accepts a
normal browser Agent, an account-owned managed Agent, or a grant-scoped
`ConnectAgent`. Authentication stays in the owning host routes; Connect uses a
fresh one-use sideband ticket, and the browser binding never receives ChatGPT
credentials or places its reusable grant bearer in a WebSocket URL.

### Durable Cloudflare Agent

`nanocodex/cloudflare` is the standard Durable Object consumer. It keeps the
host transport, SQLite journal, private runtime identity, event persistence,
hibernatable socket fan-out, and cursor replay inside the adapter:

```js
import { DurableObject } from "cloudflare:workers";
import { Agent } from "nanocodex/cloudflare";

export class CodingAgent extends DurableObject {
  #ready;

  constructor(context, env) {
    super(context, env);
    this.#ready = Agent.create(this, {
      instructions: "You are a focused coding agent.",
    });
  }

  async prompt(input) {
    const agent = await this.#ready;
    const turn = agent.turn.prompt({ input });
    let result;
    try {
      result = await turn.result();
      return result.finalMessage;
    } finally {
      try {
        result?.dispose();
      } finally {
        turn.dispose();
      }
    }
  }

  async fetch(request) {
    return (await this.#ready).events.connect(request);
  }
}
```

The returned value is the normal typed Agent: follow-on prompts reuse its owned
history, and results remain independently awaitable. `events.connect(request)`
is only a read-only AgentEvent WebSocket surface; it does not define prompt,
membership, room, quota, or application routing policy. Event frames are
`{ cursor, event }`. Replay is bounded; a far-behind client can receive
`{ type: "replay_paused", cursor, latest_cursor }` followed by close code
`1013`, then continues by reconnecting with that pause cursor as
`?cursor=<decimal>`.

Cloudflare Agents default to direct tool mode because Workers prohibit dynamic
`eval`/`new Function`. Caller-defined tools therefore work without a code
evaluator. Select `toolMode: "code"` only when also supplying an evaluator that
is explicitly compatible with the deployed Worker runtime. Runtime-owned
Subagents are installed by default, including on a durable root. Clean children
use the existing in-memory Rust task tree and are closed with the live root;
their lifecycles are not reconstructed from the durable journal. Use
`Subagents.create({ maxConcurrency })` in `tools` only to override the default
maximum concurrency of 32.

Each Durable Object persists a private runtime identity in its own SQLite
storage and derives its journal identity from it, so multiple objects in one
isolate remain independent and eviction reuses the same identity. Before
replacing an Agent inside a still-live object, await `agent.session.shutdown()`;
deleting the Durable Object and its retained event/journal rows remains an
application-owned lifecycle operation.

Internally this constructor uses `Transport.hostManaged` and an exact brokered
Responses WebSocket. `authMode` is required and accepts only `"api_key"` or
`"chatgpt"`; URLs and non-secret placeholders are fixed. `Agent.create` awaits
the private binding's WebSocket upgrade, so a missing binding or a broker whose
single policy does not match the selected mode rejects startup. The managed
Worker API deliberately has no provider-key, token, transport, or durability
option.

The managed Worker needs only the Durable Object and private broker bindings;
the broker's separate Wrangler configuration owns the real provider secret:

```jsonc
{
  "services": [{ "binding": "EGRESS", "service": "my-private-egress-broker" }],
  "durable_objects": {
    "bindings": [{ "name": "AGENTS", "class_name": "CodingAgent" }]
  },
  "migrations": [{ "tag": "v1", "new_sqlite_classes": ["CodingAgent"] }],
  "vars": { "NANOCODEX_AUTH_MODE": "chatgpt" }
}
```

Do not put `OPENAI_API_KEY`, OAuth material, account IDs, or relay capabilities
in this managed Worker configuration. A private Service Binding is a
controlled-code boundary, so the separately deployed broker must still enforce
one exact destination, one matching credential policy, placeholder replacement,
header allowlisting, and no public route.

Task-tree orchestration is an optional extension over the core agent. Both
native and WASM consumers run the same Rust implementation and receive the
same seven tools: `spawn_agent`, `submit_result`, `send_agent_message`,
`list_agents`, `wait_agent`, `interrupt_agent`, and `close_agent`.

Inside a caller-owned Worker or server isolate, host capabilities stay as
ordinary functions without crossing another compatibility protocol:

```js
import { Agent, Transport } from "nanocodex/host";
import nanocodexWasm from "./nanocodex.wasm";

const myApplicationTool = {
  name: "lookup_order",
  description: "Look up one order.",
  parameters: {
    type: "object",
    properties: { id: { type: "string" } },
    required: ["id"],
    additionalProperties: false,
  },
  handler: ({ id }) => orders.get(id),
};

const agent = await Agent.create({
  module: nanocodexWasm,
  transport: Transport.hostManaged({
    websocketUrl: "/api/responses",
    createWebSocket: (endpoint) => new WebSocket(endpoint),
  }),
  tools: [myApplicationTool],
});
```

`parameters` is optional and defaults to an open object. TypeScript types are
erased at runtime, so provide JSON Schema only when the model needs a precise
argument contract, as `lookup_order` does above.

## Standard web and browser tools

`nanocodex/tools` contains composable named tools rather than another agent or
runtime. Each factory returns an entry that can sit beside application tools
and Rust/WASM extensions in the same array:

```js
import { Agent, Transport } from "nanocodex/host";
import {
  dataset,
  imageGeneration,
  updatePlan,
  web,
} from "nanocodex/tools";

const agent = await Agent.create({
  transport: Transport.hostManaged({
    websocketUrl: "/api/responses",
    createWebSocket: (endpoint) => new WebSocket(endpoint),
  }),
  tools: [
    web(),
    dataset(),
    imageGeneration({
      recentImages: (sessionId, count) => images.get(sessionId).slice(-count),
      rememberImage: (sessionId, imageUrl) => images.get(sessionId).push(imageUrl),
    }),
    updatePlan(),
    myApplicationTool,
  ],
});
```

The web and image factories use the canonical OpenAI/Codex tool names, argument
schemas, bounds, and image-edit modes, and normalize common malformed model
arguments before dispatch. In a browser, they default to the same-origin
`/api/tools/web-search` and `/api/tools/image-generation` routes. The host owns
only a bounded JSON endpoint, credentials, authorization, and persistence.
`web(...)` posts `{ commands, session_id }`; `imageGeneration(...)` posts
`{ images, prompt }`. Pass `url` when the host route lives elsewhere.

`dataset()` runs entirely in the caller and inspects public HTTPS Parquet,
uncompressed JSONL, and Hugging Face datasets. It opens a session-scoped handle,
returns schema metadata, and supports projection and filtering queries without
hard row or offset ceilings. Input and output bytes remain bounded; partial
results return an opaque `nextCursor` that retains the query and resumes from a
physical Parquet row batch or JSONL byte position. Parquet uses HTTP range reads
and predicate pushdown where possible; JSONL scans incrementally and requires
byte-range support for cursor continuation. The implementation, Parquet reader,
and non-Snappy codecs load only after the model first calls the tool. Direct URLs
must allow browser CORS, and Parquet servers must support byte ranges.
Consumers that only need this capability can import `dataset` from the smaller
`nanocodex/tools/dataset` leaf entry.

```js
const datasets = dataset();
const opened = await datasets.handler({
  operation: "open",
  source: {
    kind: "huggingface",
    dataset: "openai/gsm8k",
    config: "main",
    split: "train",
  },
}, { sessionId: "thread-1" });

const page = await datasets.handler({
  operation: "query",
  dataset_id: opened.datasetId,
  columns: ["question", "answer"],
  filters: [{ column: "question", op: "contains", value: "how many" }],
  limit: 5,
}, { sessionId: "thread-1" });

if (page.nextCursor) {
  await datasets.handler({
    operation: "query",
    dataset_id: opened.datasetId,
    cursor: page.nextCursor,
    limit: 5,
  }, { sessionId: "thread-1" });
}
```

This same adapter works inside a Cloudflare Worker or Durable Object:

```js
import { Agent, Transport } from "nanocodex/host";
import { web } from "nanocodex/tools";

const agent = await Agent.create({
  module: env.NANOCODEX_WASM,
  transport: Transport.hostManaged({
    websocketUrl: env.RESPONSES_WEBSOCKET_URL,
    createWebSocket: (endpoint) => new WebSocket(endpoint),
  }),
  toolMode: "direct",
  tools: [
    web({
      url: env.WEB_TOOL_URL,
      headers: { authorization: `Bearer ${env.WEB_TOOL_TOKEN}` },
    }),
  ],
});
```

For a caller-owned browser Worker, `browser(...)` composes the same tools with
one persistent OPFS workspace and a lazy WASM-backed shell (Python through
Pyodide, C/C++ through wasm-clang, plus browser Git and bounded commands):

```js
import { Agent } from "nanocodex/host";
import { browser } from "nanocodex/tools/browser";

const runtime = await browser({
  threadId,
  recentImages,
  rememberImage,
});

const agent = await Agent.create({
  transport,
  filesystem: runtime.filesystem,
  instructions: runtime.instructions,
  executionEnvironment: {
    currentDate,
    timezone,
    projectInstructions: runtime.projectInstructions,
  },
  tools: runtime.tools,
});
```

`browser(...)` runs in a browser Worker because OPFS is a browser capability;
use the individual factories in server-side Cloudflare Workers. An ordinary
Vite browser app needs one plugin. `vite dev` reads the current ChatGPT
subscription from the developer's Codex auth file on the server, owns the
same-origin `/api/responses` socket, and installs compatibility in both page
and nested Worker graphs:

```js
import { nanocodex } from "nanocodex/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [nanocodex()],
  worker: { format: "es" },
});
```

Cloudflare applications use the combined entry instead of installing a second
Cloudflare plugin:

```js
import { nanocodex } from "nanocodex/vite/cloudflare";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), nanocodex()],
});
```

In development, the application Worker remains the sole browser credential
broker. Nanocodex gives workerd only the current access-token snapshot and a
capability-scoped loopback egress; it never imports the host process
environment. `vite build` does not read local auth or include those bindings,
so the application's production authentication and agent ownership remain
unchanged. The refresh token, ID token, full auth document, and credentials
never enter browser code or responses. `nanocodex()` already includes the
legacy `nanocodexTools()` compatibility plugin; do not install both.

The browser composition includes `render_artifact` as a normal typed tool. For
other hosts, compose the same factory with any workspace implementing the
Nanocodex workspace contract:

```js
import { artifact, web } from "nanocodex/tools";

const tools = [
  web({ url: env.WEB_TOOL_URL }),
  artifact({ workspace }),
];
```

The artifact factory performs no dynamic evaluation and is safe to load in a
Cloudflare Worker. Browser hosts additionally install the exact iframe syntax
validator. The model calls `tools.render_artifact({ id, title, source })` from
Code Mode, or `render_artifact` directly when the host selects direct mode; no
artifact CLI is installed. Artifact capacity is host-owned: the binding adds no
byte, source-length, ID-length, or document-count policy limits.

Application tools may provide `outputSchema` alongside `parameters`. The
binding serializes it to Rust's `output_schema`, so Code Mode receives the same
generated TypeScript return declaration as native Codex tools instead of
guessing result fields:

```js
const execCommand = {
  name: "exec_command",
  description: "Run a command.",
  parameters: { type: "object", properties: { cmd: { type: "string" } }, required: ["cmd"] },
  outputSchema: {
    type: "object",
    properties: { output: { type: "string" }, wall_time_seconds: { type: "number" } },
    required: ["output", "wall_time_seconds"],
    additionalProperties: false,
  },
  handler: runCommand,
};
```

This is what loading a Rust-written tool from JavaScript looks like here.
`nanocodex-subagents` is statically linked into `nanocodex.wasm`; every JS
`Agent.create(...)` installs it by default. Spreading `Subagents.create()` into
`tools` overrides its maximum concurrency and contributes one opaque extension
entry, not seven JavaScript handlers. Inside the binding, Rust creates one
shared registry and installs fresh tools for every root, spawn, and fork:

```rust,ignore
let (registry, control, updates) = nanocodex_subagents::channel(max_concurrency);
let tools = Tools::builder().without_defaults().build()?;
let tools = nanocodex_tools::embedded::bind_host(tools, javascript_host);
let (agent, events) = Nanocodex::builder(openai)
    .tools_factory(move |handle| {
        nanocodex_subagents::install_tools(tools.clone(), handle, registry.clone())
    })
    .build()?;
```

This is deliberately static composition, not a generic runtime loader for an
arbitrary second `.wasm` plugin. A custom Rust extension is linked into the
binding crate at build time and exposed by a small branded JS configuration;
adding a dynamic component ABI would be a separate feature with a much larger
contract and runtime cost.

The root owns the task tree. `agent.session.shutdown()` closes every child
before stopping the root driver; applications do not maintain a parallel JS
scheduler or reimplement the communication tools.

## Persistent workspaces

Runtime-specific `Workspace` adapters give an embedding application one file
contract for both local browser kernels and Node kernels. The browser adapter
uses the origin-private file system (OPFS), so reopening the same stable name
after a Worker, page, or agent-session restart reuses its files. The Node
adapter roots the same operations in an ordinary directory and refuses path
traversal and symbolic-link escapes.

```js
import { Workspace } from "nanocodex/browser/workspace";
import { Agent, Transport } from "nanocodex/host";

const workspace = await Workspace.open({ name: "my-notebook" });
const agent = await Agent.create({
  transport: Transport.hostManaged({
    websocketUrl: "/api/responses",
    createWebSocket: (endpoint) => new WebSocket(endpoint),
  }),
  filesystem: workspace,
});

await workspace.writeFile("README.md", "# Durable browser workspace\n");
console.log(await workspace.list(".", { recursive: true }));
```

The returned handle is application-owned and remains usable by a file browser,
editor, upload/download surface, or another agent session. `Workspace.tools`
exposes bounded `list_files`, `read_file`, `write_file`, `make_directory`, and
`delete_file` operations through the normal caller-defined tool boundary. It
does not add a fake browser shell.

Node uses the same shape with a real directory:

```js
import { Agent, Transport, Workspace } from "nanocodex/node";

const workspace = await Workspace.open({ path: process.cwd() });
const agent = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  filesystem: workspace,
});
```

Node and browser applications can instead pay through MPP without an OpenAI
API key. Pass an MPP session with a `ws(endpoint)` method; an `mppx` Tempo
session manager has this shape. Nanocodex defaults the socket to
`wss://openai.mpp.tempo.xyz/v1/responses` when `mpp` is present.

```js
import { Agent, createTempoProviderFromAccounts, Transport } from "nanocodex/node";
import { Expiry } from "accounts";
import { Provider } from "accounts/cli";
import { parseUnits } from "viem";
import { connect } from "viem/experimental/erc7846";
import WebSocket from "ws";

const pathUsd = "0x20c0000000000000000000000000000000000000";
const provider = Provider.create({ mpp: false });
if (!provider.store.persist.hasHydrated()) {
  await new Promise((resolve) => provider.store.persist.onFinishHydration(resolve));
}
const status = await provider.getAccessKeyStatus();
if (status === "missing" || status === "expired") {
  await connect(provider.getClient(), {
    capabilities: { authorizeAccessKey: {
      expiry: Expiry.days(1),
      limits: [{ token: pathUsd, limit: parseUnits("25", 6) }],
    } },
  });
}
const root = provider.getAccount();
const account = await provider.store.accessKeys.select({
  account: root.address,
  chainId: provider.getClient().chain.id,
});
if (!account) throw new Error("Tempo account has no usable access key");
console.error(`Tempo access-key signer: ${account.accessKeyAddress}`);
const tempoProvider = await createTempoProviderFromAccounts({
  wallet: provider,
  accessKey: account.accessKeyAddress,
  policy: {
    autoSwap: { tokenIn: [pathUsd], slippage: 1 },
    maxDeposit: "0.05",
    topUpAmount: "0.05",
  },
  session: { bootstrap: true, webSocket: WebSocket },
});
const mpp = tempoProvider.session;

const agent = await Agent.create({
  transport: Transport.mpp({ session: tempoProvider }),
  thinking: "none",
  fastMode: true,
  tools,
});
const events = agent.events.watch();
const unwatch = events.onEvent((event) => {
  process.stdout.write(`${JSON.stringify(event)}\n`);
});
let turn;
let result;
try {
  turn = agent.turn.prompt({ input: "Build the thing." });
  result = await turn.result();
  console.error(result.finalMessage);
} finally {
  try {
    result?.dispose();
  } finally {
    turn?.dispose();
  }
  unwatch();
  events.off();
  const cleanupErrors = [];
  try {
    await agent.session.shutdown();
  } catch (error) {
    cleanupErrors.push(error);
  }
  try {
    await mpp.close();
  } catch (error) {
    cleanupErrors.push(error);
  }
  if (cleanupErrors.length === 1) throw cleanupErrors[0];
  if (cleanupErrors.length > 1) {
    throw new AggregateError(cleanupErrors, "agent shutdown and MPP settlement both failed");
  }
}
```

The application still owns its wallet, deposit policy, persisted payment
channel store, and final settlement. Keep the manager alive to reuse its channel
across agents, and supply mppx `channelStore` for reuse after a process or page
restart. Nanocodex never closes a caller-owned MPP session.
`createTempoProviderFromAccounts({ wallet, ... })`
accepts any provider returned by Accounts SDK `Provider.create(...)`, regardless
of its wallet adapter, and constructs both payment paths from that provider's
adapter-neutral `getMppxParameters()` contract. The lower-level
`createTempoProvider({ session, payment })` remains available when the
application constructs MPPx itself. Both explicitly select Tempo provider mode.
In that mode Nanocodex automatically adds its built-in Mercator MCP and wraps it
with the same wallet and payment policy. The provider also exposes an MPP-aware
`fetch`; Mercator's paid REST handoffs use that same method rather than a second
wallet or payment configuration. Its MCP transport remains wrapped at the MCP
protocol layer, so browser requests do not need an `Accept-Payment` CORS header.
Browser Connect consumers send paid REST handoffs through the Connect API's
fixed Mercator relay because Mercator's job endpoint is not itself CORS-enabled;
the relay preserves MPP challenges, credentials, and receipts but never signs.
Passing a generic `MppSession`, an OpenAI key, or ChatGPT host auth does not
initialize Mercator. Pass `mcp: false` to opt out explicitly.

Remote Streamable HTTP MCP servers are configured directly on the agent. The
JavaScript binding uses the official MCP SDK transport, keeps remote tools
deferred, and mirrors native Nanocodex exposure: the initial Responses request
contains provider-native `tool_search`, while canonical `mcp__<server>__<tool>`
functions are callable only below Code Mode. Code Mode also exposes
`tools.tool_search`, so one cell can discover a deferred tool and invoke the
returned canonical name. Search results return loadable namespaces for the next
model request; remote tools never become a flat set of top-level model-visible
calls.

MPP-enabled MCP uses MPPx's in-place `McpClient.wrap`. Ordinary paid HTTP uses
`Mppx.create(...).fetch`. The public `tempo()` method is installed in both and
supports Tempo charge and session challenges, so paid services composed behind
Mercator use the same signer and spending policy as the model:

```js
const mcpMethod = tempo({
  account,
  channelStore,
  getClient: () => provider.getClient(),
  maxDeposit: "0.05",
  topUpAmount: "0.05",
});

const agent = await Agent.create({
  transport: Transport.mpp({
    session: createTempoProvider({
      session: mpp,
      payment: { methods: [mcpMethod] },
    }),
  }),
});
```

Explicit `mcp` entries are merged over the Tempo defaults, so an application
can replace `mercator` or add other servers without rebuilding the provider.

Each server also accepts `headers`, `fetch`, allow/deny tool lists, a timeout,
or an already initialized MCP SDK-compatible `client`. Nanocodex closes clients
it creates and leaves caller-owned clients open. Connection failures are
reported by `tool_search` so one unavailable server does not prevent the agent
from starting.

Runtimes whose content-security policy rejects `eval`/`new Function` can supply
a Code Mode evaluator. `createQuickJsEvaluator` accepts an asyncified
`quickjs-emscripten-core` module, serializes Asyncify execution, and exposes only
the standard Nanocodex Code Mode globals across the interpreter boundary. This
keeps deferred MCP plus Code Mode functional in Cloudflare Workers:

```js
import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { Agent, createQuickJsEvaluator, createTempoProvider, Transport } from "nanocodex/host";
import { newQuickJSAsyncWASMModuleFromVariant } from "quickjs-emscripten-core";

const quickJs = await newQuickJSAsyncWASMModuleFromVariant(asyncVariant);
const agent = await Agent.create({
  transport: Transport.mpp({ session: tempoProvider }),
  // module and mcp omitted here
  codeEvaluator: createQuickJsEvaluator(quickJs),
});
```

Cloudflare requires the QuickJS `.wasm` file to be statically imported and
passed with `newVariant(..., { wasmModule })`; the complete deployment is in
`examples/cloudflare-fetch-mcp`.

Completed results can be persisted and resumed by a fresh Node or browser
agent:

```js
const snapshot = await result.snapshot();
result.dispose();
await agent.session.shutdown();

const resumed = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  resume: snapshot,
  tools,
});
await resumed.session.shutdown();
```

The snapshot contains authoritative typed history but no provider response ID,
so the first resumed request safely replays the committed conversation. Resume
with the same instructions and tool definitions, and release the original
agent before handing its snapshot to another writer.

For crash recovery inside a turn, provide the generic durability host instead
of manually persisting snapshots. The host stores opaque Rust journal batches;
model replay, tool ambiguity, operation deduplication, and checkpoint recovery
remain in Rust/WASM:

```js
import { Agent, Transport } from "nanocodex/host";

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  durability: {
    async load(journalId) {
      return database.loadJournal(journalId);
    },
    async append(journalId, { expectedRevision, payload }) {
      return database.compareAndAppend(journalId, expectedRevision, payload);
      // { status: "appended", revision: "8" }
      // or { status: "conflict", actualRevision: "8" }
      // or { status: "not_committed", message: "transaction rolled back" }
    },
  },
  durabilityId: "customer-agent-123",
});

// Every prompt is journaled because durability is configured. Supply `id`
// only when an external retry must identify the same logical operation.
const turn = agent.turn.prompt({ input: "Build the thing." });
// const turn = agent.turn.prompt({ id: "request-7", input: "Build the thing." });
let result;
try {
  result = await turn.result();
  console.log(result.finalMessage);
} finally {
  try {
    result?.dispose();
  } finally {
    turn.dispose();
    await agent.session.shutdown();
  }
}
```

Revisions are unsigned decimal strings so JavaScript preserves Rust's full
`u64` range. Import `durabilityRevision`, `createMemoryDurabilityStore`,
`createSqliteDurabilityStore`, and `sqliteDurabilitySchema` from the small
`nanocodex/durability` leaf. Durable step hosts can carry the memory store's
`snapshot()` into the next step. SQLite hosts provide one transaction query
adapter and execute the canonical schema; the platform never interprets the
opaque Rust journal. See `services/managed`,
`examples/vercel-workflows`, and `examples/rivet-actors` for all three host
shapes.

Cloudflare Durable Objects can bind their colocated SQLite and initialize the
canonical schema in one call. The adapter is structural and adds no Workers
runtime dependency:

```js
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";

const durability = createCloudflareDurabilityStore(this.ctx.storage);
const agent = await Agent.create({
  module: env.NANOCODEX_WASM,
  transport,
  durability,
  durabilityId: sessionId,
});
```

Vercel and other PostgreSQL hosts use `createPostgresDurabilityStore(pool)`
from `nanocodex/durability/postgres`; connection ownership and secret policy
remain in the application.

Node embedders whose bundler relocates package assets may compile and pass the
web-target artifact explicitly. The runtime still uses the Node host for
WebSockets and Code Mode:

```js
const module = await WebAssembly.compile(await readFile(wasmAssetPath));
const agent = await Agent.create({ transport: Transport.openAi({ apiKey }), module });
```

A Codex-compatible rollout can also be resumed by materializing its committed
`response_item` history into a snapshot with no `request_prefix`. Nanocodex
rebuilds the current prefix from the supplied instructions and JavaScript tools
while preserving the rollout's workspace, lineage, cache key, canonical user
context, and typed history.

`Agent` and `Actions` are module namespaces, not classes. `Agent.create` returns
an owned client decorated with matching domain actions:

- `agent.turn.prompt(...)` / `Actions.turn.prompt(agent, ...)`
- `turn.accepted()` / `Actions.turn.accepted(turn)`
- `turn.result()` / `Actions.turn.getResult(turn)`
- `result.snapshot()` / `Actions.turn.getSnapshot(result)`
- `result.usage()` / `Actions.turn.getUsage(result)`
- `agent.session.fork(...)` / `Actions.session.fork(agent, ...)`
- `agent.session.compact()` / `Actions.session.compact(agent)`
- `agent.session.setThinking(...)` / `Actions.session.setThinking(agent, ...)`
- `agent.session.setFastMode(...)` / `Actions.session.setFastMode(agent, ...)`
- `agent.session.shutdown()` / `Actions.session.shutdown(agent)`
- `agent.session.spawn()` / `Actions.session.spawn(agent)`
- `agent.events.watch(...)` / `Actions.events.watch(agent, ...)`

`turn.accepted()` resolves when Rust has admitted the prompt. A durable agent
returns its stable request ID; a custom runtime without durable admission
returns `undefined`. Managed HTTP hosts can await this narrow boundary before
acknowledging a request without waiting for model execution or materializing a
result.

`turn.result()` resolves to a frozen, opaque completed `TurnResult` handle. Its
`finalMessage` is eager. The async `usage()` and `snapshot()` actions materialize
immutable values once and cache their promises. A package Worker completes a
turn with only the message and hidden result identity; Rust-produced snapshot
JSON crosses the Worker boundary only on first demand and is parsed once in the
calling isolate. Historical `fork({ at })` consumes the hidden identity directly,
never an unfinished turn, clone, snapshot, or provider response ID.

The completed result owns its identity independently from the `Turn`, so
`turn.dispose()` does not invalidate a successful result. Call `result.dispose()`
after its last fork/materialization; this releases the retained Worker/native
checkpoint and invalidates future `snapshot()`, `usage()`, and historical forks.
An undisposed result intentionally keeps its package Worker alive after the last
Agent shuts down so its lazy values remain available. Garbage collection is only
a fallback for forgotten handles, not deterministic cleanup.

`turn.dispose()` only releases the JavaScript/WASM handle; like dropping the
Rust `Turn`, it does not cancel accepted work. Await `turn.cancel()` before
disposing unfinished work. At an application or session boundary,
`agent.session.shutdown()` cancels unfinished turns and joins driver, model,
tool, and transport cleanup.

Every action owns its types, for example `Actions.turn.prompt.Options`,
`Actions.turn.prompt.ReturnType`, and `Actions.events.watch.Watcher`.

Event watches are lazy, terminal handles:

```js
const watch = agent.events.watch();
const unlisten = watch.onEvent(console.log);

unlisten();
watch.off();
```

A throwing callback is reported through the host's `reportError` hook (or
`console.error` when that hook is unavailable) without interrupting later
listeners or the owned agent lifecycle.

The same watcher can instead be consumed as an ordered async iterable; breaking
the loop releases that iterator, while `watch.off()` terminates the whole watch.

```js
const watch = agent.events.watch();
for await (const event of watch) {
  console.log(event);
  if (done) break;
}
watch.off();
```

Applications add typed action domains with decorators:

```js
const extended = agent.extend((client) => ({
  inspect: {
    session: () => client.sessionId,
  },
}));

extended.inspect.session();
```

The package-owned browser Worker accepts the same transport policy without
function-valued callbacks:

```js
import { Agent, Transport } from "nanocodex/browser";

const agent = await Agent.create({
  transport: Transport.hostManaged({
    websocketUrl: signedOrCookieAuthorizedEndpoint,
  }),
  threadId,
});
```

Caller-owned browser Workers and server isolates import `nanocodex/host` when
they need function-valued tools or socket construction. Server-side runtimes
can await a `fetch()`-based WebSocket upgrade. The third callback argument is a
discriminated authorization request plus connection metadata, including the
eager `preconnect` request. With `Transport.openAi`, `authorization` is
`"bearer"` and `bearerToken` is present. With `Transport.hostManaged`, it is
`"host_managed"`; the host must resolve credentials without exposing them to
WASM. Do not retain or log bearer tokens. Return the socket alone or a
descriptor containing response metadata:

```js
import { Agent, Transport } from "nanocodex/host";
import module from "nanocodex/wasm";

const agent = await Agent.create({
  transport: Transport.openAi({
    apiKey,
    async createWebSocket(endpoint, sessionId, request) {
      if (request.authorization !== "bearer") {
        throw new Error("this host requires Nanocodex bearer authorization");
      }
      const response = await fetch(endpoint.replace("wss:", "https:"), {
        headers: {
          Authorization: `Bearer ${request.bearerToken}`,
          Upgrade: "websocket",
          "session-id": sessionId,
        },
      });
      if (!response.webSocket) throw new Error(`upgrade failed: ${response.status}`);
      response.webSocket.accept();
      return { socket: response.webSocket, status: response.status };
    },
  }),
  module,
});
```

`Transport.hostManaged` is useful when the embedding runtime owns rotating credentials. The
callback can acquire a fresh token, attempt the upgrade, and refresh-and-retry
on 401. Bound and reject upgrade work in the callback: until it returns a
socket, there is no connection handle for Nanocodex to close. Selecting one
transport makes authentication modes mutually exclusive by construction.

After publication, a browser can load the current-isolate host without a
package manager or build step:

```html
<script type="module">
  import { Agent, Transport } from "https://cdn.jsdelivr.net/npm/nanocodex@0.5.0/host/index.mjs";
  const agent = await Agent.create({
    transport: Transport.hostManaged({
      websocketUrl: "/api/responses",
      createWebSocket: (endpoint) => new WebSocket(endpoint),
    }),
  });
  const turn = agent.turn.prompt({ input: "Hello." });
  let result;
  try {
    result = await turn.result();
    console.log(result.finalMessage);
  } finally {
    try {
      result?.dispose();
    } finally {
      turn.dispose();
      await agent.session.shutdown();
    }
  }
</script>
```

Pin the package version in production. The adjacent WASM file is part of the
npm package and is resolved relative to the host module. This no-build path
runs in the current page isolate; bundled applications should prefer the
package-owned Worker from `nanocodex/browser`. The endpoint must be authorized
by the embedding application because browser WebSockets cannot attach OpenAI's
upgrade authorization header.

The owned Rust session retains follow-on history, response state, tool output,
its WebSocket, and stable prompt-cache identity. Typed browser content accepts
ordered text, remote/data-URL image, and audio items. JavaScript tools are
ordinary async handlers described by JSON Schema and appear in the same ordered
agent event stream as built-in code mode.

Run the standalone Node proof with:

```sh
cd examples/node
npm install
OPENAI_API_KEY=... npm start
```
