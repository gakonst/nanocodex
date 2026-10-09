# Nanocodex for JavaScript

The Node, browser, and Web API host entrypoints expose the same viem-v3-style
API. A `Transport` owns authentication, placement, and socket setup;
`Agent.create(...)` owns tools and the common Agent/Turn lifecycle. Generated
WASM handles, managed control-plane handles, and host routing remain private.

```js
import { Actions, Agent, Transport } from "nanocodex/node";

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  model: "gpt-6-luna",
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

### Claude harness

`Agent.create({ harness: "claude", model, auth, tools })` runs the native Rust
Messages backend and returns the same Agent as a Codex session: identical
`session`, `turn`, and `events` actions, the same `SessionCheckpoint`/fork/resume
contract, and the same durability store. `Claude.create` is the equivalent
constructor for the Claude-only `worker` entry point. Claude sessions take
explicit host-owned auth and a Claude tool array.
Host tools can share the `exec_command` and `write_stdin` contracts with Codex.
It does not silently switch managed providers, install Codex tools, or supply
a subscription sign-in screen. Managed account connection is documented in the
[managed Claude guide](../../docs/CLAUDE_MANAGED.md). See the [Claude JavaScript guide](../../docs/CLAUDE_JAVASCRIPT.md)
for durable reopen, replay, and placement boundaries.

### Personal memories and caller context

Managed agents can keep user preferences separate from team knowledge:

```js
import { Agent } from "nanocodex/managed";

await Agent.memory({ operation: "scan", query: "my preferences" }, { scope: "personal" });
await Agent.memory({ operation: "put", content: "I prefer concise replies." }, { scope: "personal" });
```

The default scope remains `team`. Personal memories belong to the authenticated
user within their organization, across teams; use the same scope when reading,
replacing, or deleting a memory. Managed clients also accept descriptive
`requestOrigin: { client: "desktop", hand: "user:HAND_ID", cwd: "/HAND_ID" }`.
Caller context is included once at startup and never grants execution authority.

### Compose and place tools

`createTools` owns one deterministic tool recipe. Custom functions, a portable
workspace, and MCP are composed once; placement is selected afterward. Pass the
recipe to an in-process Node or Web API host, or reverse-attach it to a managed
agent target:

For a reverse machine attachment, `attachmentId` is its stable safe-ASCII source
identity (at most 123 bytes), and must equal the `id` of its sole non-secret
`machines` entry. Multiple machines may stay attached through independent
`Tools` runtimes; reconnect one runtime to replace that machine route while the
durable managed agent stays alive. Generic attachments may omit machine metadata.

```js
import { createTools } from "nanocodex";
import { Agent, Transport, Workspace } from "nanocodex/node";
import WebSocket from "ws";

const workspace = await Workspace.open({ path: process.cwd() });
const tools = await createTools({
  attachmentId: "laptop",
  machines: [{
    id: "laptop",
    name: "My laptop",
    workspace: process.cwd(),
    capabilities: ["filesystem", "native-shell"],
  }],
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

Browser consumers can attach Codex's ChatGPT Realtime voice lifecycle to the
same retained Agent. The resource owns microphone, speaker, WebRTC control,
and delegation cleanup; stopping voice does not cancel an active coding turn.
Managed and Connect voice use the WebRTC data channel for live events and
commands. Speech and captions can flow while durable startup completes;
delegated work still waits for admission. Local Agents retain the sideband path.
Snapshots update each speaker's transcript row as speech arrives, using a stable
`id` and `isPartial` flag. Completion replaces that row. `transcript.delta` events
carry the current partial text; `transcript` events retain completed-turn semantics.
Internal Realtime envelopes are projected into spoken text before publication.
Transcript updates continue while a delegation waits for durable admission.
Snapshots retain the latest 200 rows across stop/start. Subscribe to events if
an application needs its own longer transcript history.

Both local and managed browser Agents use the shared Rust/WASM client-managed
handoff policy. Only a completed final answer from the current spoken request
is submitted for speech. Commentary stays private; superseded, oversized, or
unconfirmed answers remain visible as `recovered` transcript rows and
`answer.recovered` events. Workspace and conversation history are not injected
into call startup. Browser media uses WebRTC echo cancellation, noise suppression,
and gain control; the native audio helper is used by native clients.

`start()` resolves after the media peer and backend session are ready. A media
connection timeout gets one retry after the first call has been closed.

The one-operation-at-a-time action surface is the canonical imperative API:

```js
import { Actions } from "nanocodex/browser";

const voice = Actions.voice.create(agent);

await Actions.voice.start(voice); // defaults to Codex's `cove` voice
Actions.voice.setMuted(voice, true); // also works while connecting
Actions.voice.toggleMuted(voice);
const { microphoneLevel, speakerLevel } = Actions.voice.getSnapshot(voice);

// Fence old speech before submitting typed input. The shared terminal does this.
await Actions.voice.noteTypedInput(voice);
await agent.turn.prompt("Check the tests.");
await Actions.voice.stop(voice);
await Actions.voice.destroy(voice);
```

Subscription voice preferences use the same Rust policy in browsers and native
apps. `start` and `create` accept `voice`, `instructions`, `pace` (`slow`,
`natural`, `fast`), `updates` (`auto`, `results`, `silent`), and optional
`acknowledgements`. Pace and style are speaking instructions.
`updates: "silent"` retains coding results as text without automatic speech.
`handoffMode` remains accepted for compatibility; browser client-managed
handoffs deliver completed finals and do not stream intermediate commentary.
Apply changed settings by stopping and starting a call. The shared terminal provides a saved
Voice settings panel with an Apply and reconnect action.

During an active call, `Actions.voice.speak(voice, text)` queues explicit speech,
`appendText(voice, text, { role: "developer" })` adds text using Codex's
subscription adapter (which treats all roles as context), and
`appendContext(voice, text)` adds background commentary without
requesting speech. Context and speech are split into provider-sized messages.
These commands retain frames until sent and preserve them
across reconnects when using the sideband transport. They are also methods on
the resource and on
`useVoice` from `nanocodex-react`. Choose `outputProvider: "elevenlabs"` and `elevenLabsVoiceId` to synthesize
spoken output with an ElevenLabs account voice, including an instant clone.
The account voice settings panel connects the API key, lists voices, and uploads
cloning samples after explicit consent. Keys are encrypted on the server and
never included in saved voice settings. ChatGPT still owns live input and agent
handoffs; `voice` continues to select its built-in voice. The default output
provider is `openai`. Platform audio configuration is not accepted.

`Voice.create(...)` remains the equivalent namespaced resource constructor, and
`Voice.voices` is the exact ChatGPT V3 voice catalog. The constructor accepts a
normal browser Agent, an account-owned managed Agent, or a grant-scoped
`ConnectAgent`. Authentication stays in the owning host routes. An explicit
`sidebandUrl` override selects the sideband transport; Connect uses a fresh
one-use ticket for that path. The browser binding never receives ChatGPT
credentials or places its reusable grant bearer in a WebSocket URL.

### Content-free Worker tracing

`nanocodex/cloudflare/tracing` wraps native `tracing.enterSpan(name, callback)`
with sanitized exception events. Use static operation names: they become
exception codes on failure. The original error is rethrown; its message and
stack are never passed to `recordException`.

`setSpanAttributes(span, attributes)` sets bulk metadata, while
`annotateActiveSpan(attributes)` annotates the current invocation or active
span without changing nesting. `recordSpanException(span, code)` handles
failures represented by results rather than thrown errors. Supply only static
codes and content-free attributes. On older workerd versions, these retain
individual attributes and `error.type` without requiring the September APIs.

### Durable Cloudflare Agent

`nanocodex/cloudflare` is the standard Durable Object consumer. It keeps the
host transport, SQLite durable state, private runtime identity, event persistence,
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

All SDK runtimes expose only `exec` and `wait` to the model. Workspace,
application, discovery (`tools.tool_search`) and local subagent tools are callable
inside Code Mode. Discovery returns tool information as cell output; it does not
add direct schemas. Newly discovered tools are available in the next cell. The
legacy `"code"` and `"direct"` mode values are rejected.

Node uses sandboxed QuickJS by default; browsers use a child Worker. Other Web API
hosts, including Cloudflare Workers, require an explicit compatible `codeEvaluator`.
The same evaluator capability is inherited by alternate child harnesses. Runtime-owned
Subagents are installed by default, including on a durable root. A durable root
retains its child task tree, conversations, and results across runtime shutdown
and reconstruction. Cloudflare teardown preserves the children's authorization
and pinned routes so completed children can receive follow-up messages after
restoration. Explicitly closing a child releases its bindings and keeps it closed
after reconstruction. If three consecutive automatic resumes are lost before the
child's turn settles, its next reconstruction reports `subagent recovery exhausted`
and stops automatic recovery. The child retains its history and can receive an
explicit delegation after any uncertain tool effects have been reconciled.
Recovery also carries a bounded summary of observed tool calls from the
interrupted turn, including call identities and whether a result was observed.
The summary is evidence for reconciliation, not a receipt or permission to
repeat an effect. Calls may already appear in the restored history, and omitted
entries are counted; an empty summary does not prove that no tools ran.
Child messages remain in the separate task-tree journal,
not the root conversation stream. Children of roots without durability live only
for the lifetime of the runtime. Obsolete child checkpoint tables from older
versions are discarded; current child state belongs to the root journal. Use
`Subagents.create({ maxConcurrency })` in `tools` to set an explicit finite
concurrency limit. Active subagent turns are unlimited by default.

Each Durable Object persists a private runtime identity in its own SQLite
storage and derives its state identity from it, so multiple objects in one
isolate remain independent and eviction reuses the same identity. Before
replacing an Agent inside a still-live object, await `agent.session.shutdown()`;
deleting the Durable Object and its retained event/state rows remains an
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

Children call `submit_result({output})`; the runtime supplies the trusted
instruction revision. The response is `{accepted: true, status: "accepted"}` or
`{accepted: false, status: "superseded"}`. Superseded submissions are normal
continuations: incorporate the updated instructions and submit again.

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
`web(...)` posts `{ commands, session_id, model }`, where `model` is the
effective model of the invoking root or subagent; `imageGeneration(...)` posts
`{ images, prompt }`. The host owns model authorization and may ignore or
override this value. Pass `url` when the host route lives elsewhere.

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
  codeEvaluator: workerCodeEvaluator, // Explicit QuickJS evaluator for this Worker.
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
use the individual factories in server-side Cloudflare Workers. Vite integration
is provided separately by `nanocodex-vite`.

The browser composition includes native `browseX` public X browsing, advertised
by `environment().apis` without an X connector. The embedding app serves
`/api/tools/x/browse` and `/api/tools/x/convert`; Nanocodex's account app forwards
these requests to the private X Worker.

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
Code Mode; no
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
let tools = nanocodex_oai_tools::embedded::bind_host(tools, javascript_host);
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

The root owns the task tree. `agent.session.shutdown()` stops every child runtime
before stopping the root driver, preserving durable children for reconstruction.
Applications do not maintain a parallel JS
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

Tempo/MPP helpers live in `nanocodex/tempo`, whose optional peer dependencies
`mppx` and `viem` are needed only when you import it; the core entry points
never reference them.

```sh
npm install nanocodex mppx viem
```

```js
import { Agent, Transport } from "nanocodex/node";
import { createTempoProviderFromAccounts } from "nanocodex/tempo";
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
  thinking: "low",
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
In that mode Nanocodex automatically adds its built-in Mercator MCP at
`https://mercator.sh/mcp` and wraps it with the same wallet and payment policy. The provider also exposes an MPP-aware
`fetch`; Mercator's paid REST handoffs use that same method rather than a second
wallet or payment configuration. Its MCP transport remains wrapped at the MCP
protocol layer, so browser requests do not need an `Accept-Payment` CORS header.
Browser Connect consumers send paid REST handoffs through the Connect API's
fixed Mercator relay because Mercator's job endpoint is not itself CORS-enabled;
the relay preserves MPP challenges, credentials, and receipts but never signs.
Passing a generic `MppSession`, an OpenAI key, or ChatGPT host auth does not
initialize Mercator. Pass `mcp: false` to opt out explicitly.

Trusted hosts can set a server's `privateResult` policy to intercept raw MCP
results before model output, Code Mode values, and tool-result tracing.
`beforeCall({ name, arguments }, context)` runs after payment context validation
and before remote dispatch. It may reject the call or return `{ privateContext }`. An own
`result` property instead supplies an already-safe receipt and bypasses both
remote dispatch and `transformResult`, allowing durable policies to replay a
receipt without repeating an effect.
`transformResult({ name, arguments, result, privateContext }, context)` receives
that invocation's `privateContext` and must return only safe MCP content. State stays local
to each call, including concurrent calls; it is `undefined` without `beforeCall`.
Both hooks run within the tool deadline. Failed private requests return a fixed
failure receipt without the original error or cause. Discovery failures also use
a fixed message. Callbacks and caller-owned clients remain trusted: they must
not independently log private payloads or return private state. No notification
consumer is configured by this policy.

Remote Streamable HTTP MCP servers are configured directly on the agent. The
JavaScript binding uses the official MCP SDK transport, keeps remote tools
deferred: discover them with `tools.tool_search` inside `exec`, then invoke the
returned canonical `mcp__<server>__<tool>` name in a new cell. Search returns
schemas as data; remote tools never become top-level model-visible calls.

MPP-enabled MCP uses MPPx's in-place `McpClient.wrap`. Ordinary paid HTTP uses
`Mppx.create(...).fetch`. The public `tempo()` method is installed in both and
supports Tempo charge and session challenges, so paid services composed behind
Mercator use the same signer and spending policy as the model:

```js
import { tempo } from "mppx/client";
import { createTempoProvider } from "nanocodex/tempo";

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
A paid server configured directly must wrap its options with `mcpPayment()` from
`nanocodex/tempo`, which loads `mppx/mcp/client` on first connection; a plain
`payment` object is rejected with a `TypeError`:

```js
import { mcpPayment } from "nanocodex/tempo";

const mcp = {
  paid: { url: "https://paid.example/mcp", payment: mcpPayment({ methods: [mcpMethod] }) },
};
```

Each server also accepts `headers`, `fetch`, allow/deny tool lists, a timeout,
or an already initialized MCP SDK-compatible `client`. Nanocodex closes clients
it creates and leaves caller-owned clients open. Connection failures are
reported by `tool_search` so one unavailable server does not prevent the agent
from starting.

Code Mode is the default. Model-facing `exec` cells can yield with a first-line
`// @exec: {"yield_time_ms": 1000, "max_output_tokens": 1000}` directive or
`yield_control()`. The model resumes the returned cell ID through `wait`, which
returns only new output and can terminate the cell. Output is unbounded by default;
`max_output_tokens` on `exec` and `max_tokens` on `wait` set explicit observation
budgets. Cells belong to their agent
session and are invalidated when the host shuts down; a persisted `wait` never
restarts missing work. Embedded cells retain ownership of all nested tool calls
until they finish or are cancelled.

Opt into observer-only instant steering with `instantToolSteering: true` in
Node/browser or Cloudflare Agent creation. Managed creation uses
`configuration: { instant_tool_steering: true }`. The default is false. Accepted
steering yields a foreground `exec`/`wait` with its output and live cell ID,
without cancelling evaluation or nested effects. Later `wait` resumes that
same cell. Model streams are not interrupted by this option; see
[the runtime boundaries](../../docs/codex-code-mode.md#observer-only-instant-steering).

Owned `nanocodex/node` and `nanocodex/host` agents may opt into
`codeEffectJournal` alongside their durability store. This trusted host adapter
must durably acknowledge `begin(context)` before dispatch, validate the original
session/call identity plus source/tool/input fingerprint, and fence stale owners.
Journal keys must include
`[sessionId, operationId ?? "", modelCallIndex ?? 0, parentCallId, callId]`.
Owned SDK hosts synchronously observe original Rust request/idempotency identity
and model-call ordinal before dispatch; missing canonical metadata interrupts
rather than falling back to a projected turn/run ID. Provider call IDs may repeat
across model responses within one operation and across operations. Use an explicit
prompt operation `id` with a durable owned journal. Deliberately non-durable
prompts (including ephemeral child tasks) emit an explicit null Rust operation
ID; the host scopes those invocations to the unique trusted accepted input item.
That scope does not promise cold replay of a non-durable task. Absent or malformed
metadata still interrupts before effects. Generic explicit runtimes without
an identity resolver retain their existing session/parent identity semantics.
`turnId` is diagnostic metadata, not the effect replay key.
Child event forwarding may race the host invocation. The owned SDK rendezvous
with the exact accepted session/turn and fresh tool-call metadata before journal
admission (one-second deadline, at most 128 waiters, abort/release/dispose cleanup).
It never derives effect authority from a parent correlation envelope or reuses
consumed model-call metadata. Parallel nested calls share one identity promise;
missing or conflicting metadata fails closed without effect dispatch.
Application tools run inside Code Mode. Nested contexts use the exact guest source
and a stable cell/ordinal; internal host callbacks retain their invocation metadata.
Adapters can additionally implement `beginCell(context)` and
`commitStore(context, writes)` together to persist Code Mode `store()` state.
Before evaluation, `beginCell` durably pins the cell's immutable starting entries;
recovery returns those same entries even when later cells changed session state.
The context has `name: "code-cell"`, `callId: parentCallId`, and `input: null`.
`commitStore` atomically merges only the cell's writes, once per original cell
identity. A completed-cell replay must not overwrite newer committed writes.
Completed failed scripts also commit their writes; interrupted/aborted cells do
not. Session stores remain isolated and entry snapshots are bounded to 8 MiB and
32,768 nodes. Hosts must fail closed when prior effects exist but their original
starting snapshot is missing, corrupt, or cannot be proved during an upgrade.
Without these optional methods, stores remain local to a runtime instance.

Deterministic adapter conflicts and corruption should throw an error with
`code: "CODE_EFFECT_UNKNOWN"`. These errors and invalid replay receipts settle
as failed unknown outcomes, without admitting further effects. Storage/transport
exceptions remain host interruptions so genuinely transient failures can recover.

Return `execute` for a new retained intent, `replay` with its exact completed
receipt, or `unknown` for an intent without a durable outcome. `complete` must
acknowledge storage before either direct tool output or a nested guest result is
returned. Direct receipts use `value: null` and `thrown: false` (handler failures
remain failed encoded tool outputs, not guest throws). Unknown direct intents
return an explicit failed tool result with `outcome: "unknown"` without dispatch.
Unknown nested effects fail the recovered cell even if guest code catches the error; they are never silently
rerun. Reconcile external state using the original operation identity.

Receipts must contain plain JSON data and are bounded to an aggregate 8 MiB
and 32,768 entries before output/value copies. Undefined results and raw undefined
rejections have explicit markers. Identical output/structured/value payloads use
receipt-local references (or derived JSON text), so ordinary inline media does
not consume three copies. An oversized or unretainable post-effect receipt interrupts the host and
leaves its original intent unknown, rather than granting permission to redispatch. Route larger media through a bounded
artifact/reference tool instead of embedding it in the replay receipt.
The journal is opt-in for generic SDK hosts; durability alone does not make
direct or nested application effects safe to rerun after abrupt owner loss.
It does not journal arbitrary native JavaScript/evaluator effects, built-in
provider-native tools, or external operations outside the application-tool router.

Custom evaluators receive `audio`, `notify`, `yield_control`, `setTimeout`, and
`clearTimeout` alongside the existing globals in `CodeEvaluatorEnvironment`.
Forward those helpers into the guest environment to preserve the model-visible
contract. `image` accepts individual MCP image blocks and honors explicit detail
before MCP metadata; `audio` accepts MCP audio blocks. Both accept data URLs.

Web API hosts without a child Worker supply an isolated Code Mode evaluator.
`createQuickJsEvaluator` accepts an asyncified
`quickjs-emscripten-core` module, serializes Asyncify execution, and exposes only
the standard Nanocodex Code Mode globals across the interpreter boundary. This
keeps deferred MCP plus Code Mode functional in Cloudflare Workers:

```js
import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { Agent, createQuickJsEvaluator, Transport } from "nanocodex/host";
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

Every session, Codex or Claude, exposes the same harness-neutral contract.
A `SessionCheckpoint` is the one portable, family-tagged boundary used to
resume, fork, and restore. It is JSON-safe; its contents are opaque and only
the harness that produced it decodes them:

```js
const checkpoint = await result.checkpoint(); // or agent.session.checkpoint()
await store.put("session", JSON.stringify(checkpoint));
result.dispose();
await agent.session.shutdown();

const resumed = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  resume: JSON.parse(await store.get("session")),
  tools,
});
await resumed.session.shutdown();
```

The checkpoint contains the complete unredacted model-visible conversation but
no provider response ID, so the first resumed request safely replays the
committed conversation. Resume with the same instructions and tool definitions,
and release the original agent before handing its checkpoint to another writer.
`Agent.create({ harness: "claude", ..., resume })` resumes a Claude checkpoint
the same way; a checkpoint of another family rejects with
`code: "checkpoint_family_mismatch"`.

`agent.session.info()` returns `{ sessionId, harness, lineage: { rootSessionId,
parentSessionId, origin, depth } }`. `agent.session.capabilities()` states which
lifecycle operations the backend supports and when model, thinking, and service
tier may change (`setModel`, `setThinking`, `setServiceTier`/`setFastMode`); an
unsupported operation rejects with
`code: "unsupported_capability"` and a `capability` name.
`agent.session.persistence()` reports the durable state backing the session, or
`null`. `agent.session.fork({ at, origin })` forks the latest boundary, a
completed `TurnResult`, or a `SessionCheckpoint` of the same conversation;
`origin: "side_conversation"` records an ephemeral side exploration.

For crash recovery inside a turn, provide the generic durability host instead
of manually persisting checkpoints. The host stores one opaque Rust state value;
model replay, tool ambiguity, operation deduplication, and checkpoint recovery
remain in Rust/WASM:

```js
import { Agent, Transport } from "nanocodex/host";

const agent = await Agent.create({
  transport: Transport.openAi({ apiKey: process.env.OPENAI_API_KEY }),
  durability: {
    async load(stateId) {
      return database.loadState(stateId);
    },
    async acquire(stateId, { ownerId }) {
      return database.acquireState(stateId, ownerId);
    },
    async replace(stateId, { ownerId, fence, expectedRevision, payload }) {
      return database.compareAndReplace(
        stateId,
        ownerId,
        fence,
        expectedRevision,
        payload,
      );
      // { status: "replaced", revision: "8" }
      // or { status: "conflict", actualRevision: "8" }
      // or { status: "not_committed", message: "transaction rolled back" }
    },
  },
  durabilityId: "customer-agent-123",
});

// Every prompt is durable because the state store is configured. Supply `id`
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
opaque Rust state. See `js/managed`,
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

The built-in stores can move one stopped agent across providers without
decoding or rebasing its Rust state. Cloudflare owners should use the adapter's
lifecycle-safe export instead of reconstructing its private state ID:

```js
import { Agent as CloudflareAgent } from "nanocodex/cloudflare";
import { importDurabilityStatePages } from "nanocodex/durability";
import { createPostgresDurabilityStore } from "nanocodex/durability/postgres";

await cloudflareAgent.session.shutdown();
const pages = [];
let cursor;
let to;
do {
  const page = await CloudflareAgent.exportDurabilityState(durableObjectOwner, {
    from: "0", // exclusive destination revision
    to,        // omit once, then repeat the selected inclusive source revision
    cursor,
  });
  pages.push(page);
  to = page.to;
  cursor = page.nextCursor ?? undefined;
} while (cursor !== undefined);

// Send the pages through an authenticated, encrypted operator path.
const destination = createPostgresDurabilityStore(vercelPostgresPool);
await importDurabilityStatePages(destination, JSON.parse(JSON.stringify(pages)));

const vercelAgent = await Agent.create({
  module: wasmModule,
  transport,
  durability: destination,
  durabilityId: pages[0].stateId,
});
```

`from` is exclusive and `to` is inclusive. For a nonzero `from`, load the
destination once, hash that exact state with `durabilityStateDigest`, and repeat
the short `fromDigest` on every page request; revision zero's null-state digest
is implied. Each page carries that SHA-256 lineage digest, so import atomically
succeeds only if the destination still has the exact revision and payload
selected at `from`.
Because `to` is one complete Rust state, no intermediate revision log is
needed. Export fences the old source owner, and PostgreSQL reconciles lost
COMMIT responses internally by retrying the identical idempotent request, so
the API never reports an ambiguous write outcome. Stop source admission before
the first page and never resume it after cutover begins. Pages can contain
conversation and tool state, so handle them as secrets. The Vercel example
includes a WASM integration test that executes the
same agent Cloudflare → PostgreSQL → Cloudflare, replays committed turn IDs
without model calls, rebuilds the first new provider request from committed
history without a previous-response handle, and then continues with new turns
on each destination.

The managed Cloudflare service exposes the same offline cutover at `POST
/v1/agents/<agent-id>/durability`; the call permanently closes source admission.
Create a destination with `POST /v1/agents`, an `Idempotency-Key` header, and
`{ "durability": <archive> }`. The stable key owns resumable receipt adoption.
The Vercel example accepts that same body at `POST /api/sessions` and exports a
stopped PostgreSQL state through `POST /api/durability/export` with
`{ "state_id": <durability-id>, "from": <revision>,
"fromDigest": <required-for-nonzero-from>, "to": <optional-revision>,
"cursor": <optional-cursor> }`.

Node embedders whose bundler relocates package assets may compile and pass the
web-target artifact explicitly. The runtime still uses the Node host for
WebSockets and Code Mode:

```js
const module = await WebAssembly.compile(await readFile(wasmAssetPath));
const agent = await Agent.create({ transport: Transport.openAi({ apiKey }), module });
```

`Agent` and `Actions` are module namespaces, not classes. `Agent.create` returns
an owned client decorated with matching domain actions:

- `agent.turn.prompt(...)` / `Actions.turn.prompt(agent, ...)`
- `turn.accepted()` / `Actions.turn.accepted(turn)`
- `turn.result()` / `Actions.turn.getResult(turn)`
- `result.checkpoint()` / `Actions.turn.getCheckpoint(result)`
- `result.usage()` / `Actions.turn.getUsage(result)`
- `agent.session.info()` / `Actions.session.info(agent)`
- `agent.session.capabilities()` / `Actions.session.capabilities(agent)`
- `agent.session.persistence()` / `Actions.session.persistence(agent)`
- `agent.session.checkpoint()` / `Actions.session.checkpoint(agent)`
- `agent.session.fork(...)` / `Actions.session.fork(agent, ...)`
- `agent.session.setModel(...)` / `Actions.session.setModel(agent, ...)`
- `agent.session.cancel()` / `Actions.session.cancel(agent)`
- `agent.session.compact()` / `Actions.session.compact(agent)`
- `agent.session.setThinking(...)` / `Actions.session.setThinking(agent, ...)`
- `agent.session.setFastMode(...)` / `Actions.session.setFastMode(agent, ...)`
- `agent.session.setServiceTier(...)` / `Actions.session.setServiceTier(agent, ...)`
- `agent.session.shutdown()` / `Actions.session.shutdown(agent)`
- `agent.session.spawn()` / `Actions.session.spawn(agent)`
- `agent.events.watch(...)` / `Actions.events.watch(agent, ...)`

`turn.accepted()` resolves when Rust has admitted the prompt. A durable agent
returns its stable request ID; a custom runtime without durable admission
returns `undefined`. Managed HTTP hosts can await this narrow boundary before
acknowledging a request without waiting for model execution or materializing a
result.

`turn.result()` resolves to a frozen, opaque completed `TurnResult` handle. Its
`finalMessage` is eager. The async `usage()` and `checkpoint()` actions materialize
immutable values once and cache their promises. A package Worker completes a
turn with only the message and hidden result identity; Rust-produced checkpoint
JSON crosses the Worker boundary only on first demand and is parsed once in the
calling isolate. `fork({ at: result })` consumes the hidden identity directly,
never an unfinished turn, clone, or provider response ID.

The completed result owns its identity independently from the `Turn`, so
`turn.dispose()` does not invalidate a successful result. Call `result.dispose()`
after its last fork/materialization; this releases the retained Worker/native
checkpoint and invalidates future `checkpoint()`, `usage()`, and turn forks.
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
  import { Agent, Transport } from "https://cdn.jsdelivr.net/npm/nanocodex@0.6.7/host/index.mjs";
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

Managed clients can save `Agent.definitions` and `Agent.environments`, select them
with `Agent.create({ definitionId, environmentTemplateId, configuration })`, and
inspect `agent.configuration()`, `environment()`, `usage()`, `requests()`,
`artifacts`, `webhook`, and `requiredActions`. See the
[managed configuration and operations guide](../../docs/MANAGED_AGENT_CONFIGURATION.md)
for examples, authorization, delivery semantics, and runtime limits.


## Call connected services from your app

After Nanocodex Connect login, the app and its agents share the same approved
service capabilities and exact account selections. Request the services during
login and call them directly without creating an agent or running a turn:

```js
const connection = await client.connection.connect({
  capabilities: { cloudAccounts: { spotify: true, soundcloud: true } },
});
const response = await client.connectors.spotify.request({
  path: "/v1/me/playlists?limit=20",
  // Select an approved account when more than one is connected:
  connectionId: connection.grant.connectorConnections.spotify[0],
});
if (!response.ok) throw new Error(`Spotify returned ${response.status}`);
const playlists = await response.json();
```

`client` is a `Client.create(...)` instance from `nanocodex/connect`. For a dynamic service, use
`client.connectors.request({ connector: "spotify", path: "/v1/me/playlists" })`.
The standalone form is `Actions.connectors.request(client, options)`.
Every API service has a scoped entry point, including `client.connectors.gmail`
and `client.connectors.soundcloud`. The original singular `client.connector.request`
and `Actions.connector.request` remain supported. Requests support a provider
path, HTTP method, optional `connectionId`, JSON object `body`, and abort `signal`.
The response preserves provider HTTP status, body, and pagination/rate-limit
headers. Writes are never automatically retried.

The underlying API is `POST /v1/connectors/:connector/request` at the configured
Connect API origin, authenticated with the app's Connect grant bearer token and
`X-Nanocodex-App-Id`. Browser calls must come from the grant's approved app origin.
Its JSON body uses `path`, `method`, optional `connection_id`, and optional `body`.
It requires no conversation or thread ID. Both this endpoint and agent egress
check current grant revocation, expiry, approved service, and exact account IDs.
Connecting an account alone does not grant it to every app; the user must approve
that service in the app's Connect flow. Provider tokens remain in the broker.
ChatGPT is a model connector and has no generic HTTP endpoint here.


### Use a provider SDK with a custom base URL

Native SDKs can use normal HTTP through
`https://nanocodex.gakonst.workers.dev/connectors/<service>/<provider-path>`.
All 13 API capabilities share this route. Append the provider's existing path:

| Connector | Example route |
| --- | --- |
| Spotify | `/connectors/spotify/v1/me/playlists` |
| SoundCloud | `/connectors/soundcloud/me/playlists` |
| Gmail | `/connectors/gmail/gmail/v1/users/me/messages` |
| Google Drive | `/connectors/gdrive/drive/v3/files` |
| Google Calendar | `/connectors/gcalendar/calendar/v3/calendars/primary/events` |
| Google Tasks | `/connectors/gtasks/tasks/v1/users/@me/lists` |
| Google Docs | `/connectors/gdocs/v1/documents/{id}` |
| Google Sheets | `/connectors/gsheets/v4/spreadsheets/{id}` |
| Google Slides | `/connectors/gslides/v1/presentations/{id}` |
| Google Contacts | `/connectors/gcontacts/v1/people/me/connections?personFields=names` |
| GitHub | `/connectors/github/user` |
| Slack | `/connectors/slack/api/auth.test` |
| X | `/connectors/x/2/users/me` |

Provider path restrictions and upstream OAuth permissions still apply. Google
services are individually opted in even though they share a Google login.
ChatGPT and remote MCP connections use their existing separate protocols.
The same routes are available on the configured Connect API origin.

Send the app's **Nanocodex Connect grant token** as the bearer token. Native
requests need no extra app identity headers: the token identifies its approved
app and user. SDK routes also accept `Authorization: OAuth <grant-token>`
(SoundCloud) and `Authorization: token <grant-token>` (GitHub). Tokens must stay in
headers, not query parameters. Browser Origins and any explicit app ID must match that grant.
The grant must already include the requested service; this route cannot connect
accounts or enlarge the grant. If multiple accounts were approved, set
`X-Nanocodex-Connector-Connection` to the selected approved connection ID.

For example, with [Spotipy](https://github.com/spotipy-dev/spotipy):

```python
import spotipy

spotify = spotipy.Spotify(auth=nanocodex_grant_token, retries=0, status_retries=0)
spotify.prefix = "https://nanocodex.gakonst.workers.dev/connectors/spotify/v1/"
page = spotify.current_user_playlists(limit=20)
next_page = spotify.next(page)
```

For Gmail with Google's [Python API client](https://github.com/googleapis/google-api-python-client):

```python
from google.oauth2.credentials import Credentials
from googleapiclient.discovery import build

gmail = build(
    "gmail", "v1",
    credentials=Credentials(token=nanocodex_grant_token),
    client_options={
        "api_endpoint": "https://nanocodex.gakonst.workers.dev/connectors/gmail/",
    },
    cache_discovery=False,
)
messages = gmail.users().messages().list(userId="me").execute(num_retries=0)
```

Use `userId="me"`; Gmail routes are restricted to the connected user. These
credentials hold only the Connect grant; renew an expired grant through Nanocodex
Connect instead of asking the provider SDK to refresh it with Google or Spotify.
SoundCloud clients use `/connectors/soundcloud/` as their base URL and the same
Connect grant, using [SoundCloud's OAuth header format](https://developers.soundcloud.com/docs).

The proxy preserves request methods, query parameters, and body bytes. Provider
status codes, JSON errors, rate-limit headers, and non-JSON bodies are preserved.
API links in JSON, pagination Link headers, and same-provider redirects are
rewritten through the proxy; artwork and public share URLs stay unchanged.
Cross-provider redirects are rejected. JSON link rewriting is bounded to 16 MiB
per response; use provider pagination for larger collections. Spotify GETs share
one-second read results and the broker's registration-wide rate-limit cooldown.
The broker retries a rate-limited Spotify GET at most twice within a ten-second
cooldown-wait budget; longer limits return 429 with the remaining `Retry-After`.
No writes are automatically retried. Disable SDK write retries to avoid repeating
a successful write after an ambiguous network failure.

See the runnable [Python example](../../examples/python/spotify_proxy.py).
The integration test uses real Spotipy 2.26.0 and google-api-python-client 2.192.0
against the Connect Worker with fixture provider responses, covering reads,
writes, pagination, rate limits, and revoked grants. Worker tests cover routing
and service denial for all 13 API connectors. SDKs that hardcode their API host need a custom transport instead of only
a base URL setting.


## Connect dialog appearance

Pass visual tokens when creating an embedded or popup Connect dialog:

```js
import { Client, Dialog } from "nanocodex/connect";

const appearance = {
  theme: "system",
  accentColor: "#635bff",
  fontFamily: '"Open Sans", system-ui, sans-serif',
  borderRadius: 12,
};
const client = Client.create({
  appId: "your-app-id",
  dialog: Dialog.iframe({ appearance }),
});
// Dialog.popup({ appearance }) accepts the same options for account authorization.
```

All fields are optional. `theme` accepts `light`, `dark`, or `system`;
`accentColor` requires six hex digits; `fontFamily` accepts an installed or
self-hosted font list of at most 160 characters; `borderRadius` is a number from
0 to 24 pixels. Arbitrary CSS, CSS functions, and unknown options are rejected.
The SDK snapshots the configuration when the dialog is created and carries it
in the `nanocodex_appearance` URL parameter to wallet and funding iframes or the
account popup. The hosted dialog limits the JSON value to 1,024 characters and
uses native defaults for malformed values. Omit `appearance` for native defaults.

### Standalone account services

`nanocodex/services` accesses Vault and dedicated SMS numbers without an agent,
thread, model, or WASM runtime. Keep direct account API keys on your server:

```js
import { createServicesClient } from "nanocodex/services";

const services = createServicesClient({ apiKey: process.env.NANOCODEX_API_KEY });
const { vault } = await services.vault.list();
const { status, ok } = await services.vault.request({
  vault_id: vault[0].id,
  url: "https://example.com/verify",
  method: "POST",
  body_encoding: "json",
  body: '{"code":"{{NANOCODEX_VAULT_TOTP}}"}',
});
```

Direct calls use `https://nanocodex.gakonst.workers.dev/v1/services`. Reads require
`data:read`, Vault requests require `tools:use`, and phone intents require
`data:write` plus `tools:use`. Vault returns metadata and destination HTTP status
only. TOTP seeds and generated codes stay in the broker; the saved exact HTTPS
origin bounds code injection. There is no code-export method.

Browser apps use an existing Connect client and exact signed service capabilities:

```js
import { Client } from "nanocodex/connect";
import { createServicesClient } from "nanocodex/services";

const connect = Client.create({ appId: "example-app" });
const connection = await connect.connection.connect({
  capabilities: {
    services: {
      vault: { ids: ["selected-vault-id"], origins: ["https://example.com"], request: true },
      phone: { numberIds: [], read: false, provision: true, release: false },
    },
  },
});
const services = createServicesClient({ connect, grantId: connection.grant.id });
```

Service-only connections default to `services.use` and hosted authorization.
They contain no `agentId`, access key, or MPP budget. Scope is bound into the
signed approval and checked against the returned grant. Reconnect with the same
`capabilities.services` to require that exact scope. Service requests use
`/v1/grants/:grantId/services/...` and the Connect session. Number and Vault IDs
are explicit; they never expand to the entire account.

Use `phone.available({ country: "US", area_code: "202" })`, `phone.list()`,
`phone.get(id)`, and `phone.messages(id, { limit: 20, cursor })` for reads. SMS
messages expire according to the service's retention policy; support for
verification messages depends on the sender and carrier.

Provisioning and release create requests for human review:

```js
import { createHostedRequest, openHostedPopup } from "nanocodex/services";

// Persist this UUID before sending; keep it when reconciling an uncertain request.
const operation_id = crypto.randomUUID();
const { request } = await services.phone.provision({
  operation_id, phone_number: "+12025550101", country: "US",
});
const approval = createHostedRequest({
  service: "phone", operationId: request.approval_request_id ?? request.operation_id, appOrigin: location.origin,
});
// Open from a separate click handler after the request is ready.
await openHostedPopup(approval);
const result = await services.phone.requests.get(request.operation_id);
```

`phone.release(numberId, { operation_id })` also requires hosted human approval.
Poll with the **original** caller UUID, including after a dropped intent response.
Connect returns an additional `request.approval_request_id` for the hosted owner
approval; use `request.approval_request_id ?? request.operation_id` for its link.
Any deliberate identical intent replay must retain the original caller UUID.
A newly provisioned number needs a fresh exact-ID Connect approval before inbox
access. The SDK never automatically retries a write. `ServiceError` exposes
`status`, `code`, and `outcomeUnknown`; an unknown outcome requires reconciliation,
not a new operation ID. No SDK method approves purchases or releases.

TOTP enrollment uses a private hosted form:

```js
const enrollment = createHostedRequest({ appOrigin: location.origin });
// Call from a user click. Inputs stay on the account origin.
const metadata = await openHostedPopup(enrollment);
if (metadata.service === "vault") console.log(metadata.vault_id);
```

The popup helper checks the sender window, exact hosted origin, and a unique
state. Only fixed metadata fields reach the caller. `createHostedRequest`
also accepts `host` for a trusted HTTPS account deployment and `state` for a
caller-managed unique nonce. Secret entry and approvals require a top-level window; embedded frames cannot
perform them. React applications can open the same request with
`HostedServiceButton` from `nanocodex-react/services`.

To use an existing Vault item before granting access, open the account picker
from a user click. No service client or Connect grant is needed:

```js
const selection = createHostedRequest({
  service: "vault", action: "select", appOrigin: location.origin,
});
const selected = await openHostedPopup(selection);
if (selected.service === "vault" && selected.action === "select") {
  // The user sees the recipient origin and explicitly chooses this item.
  // Only its opaque vault_id, kind, and name are shared.
  const connection = await connect.connection.connect({
    capabilities: { services: { vault: {
      ids: [selected.vault_id], origins: ["https://example.com"], request: true,
    } } },
  });
}
```

The picker does not create or expand a grant and does not enroll the item again.
The separate Connect approval authorizes the exact selected ID and destination
origins. Closing the popup or aborting its `signal` cancels the local wait;
closing a phone approval window does not cancel a server-side operation. Poll
that operation with its original caller UUID to learn its outcome.

Account management is available through `createServicesClient({ apiKey }).account`
without an agent or WASM. Use `client.links({ connect: 'google', add: 'login' })`
for ordinary Connections/Vault URLs, or
`client.hosted({ service: 'vault', kind: 'card' })` for a private enrollment link.
OAuth, Cloudflare, MCP, model sign-in, Vault/SSH management, and captured-card
save/balance methods share the account REST contracts. See the
[REST, JavaScript and Rust guide](../../docs/STANDALONE_SERVICES.md#account-management-and-browser-links).

## Building the WASM package

The repository build caches the locked Rust inputs and generated bindings. An
unchanged build verifies its source attestation and reuses those outputs.
Node, browser, and Web API bindings are generated concurrently.

Release optimization prefers a checksum-pinned native Binaryen 132 download
on Linux and macOS (x64 and ARM64), cached outside the checkout. If native
Binaryen is unavailable, the build reports the reason and uses the pinned npm
optimizer. Set `NANOCODEX_BINARYEN_CACHE` to relocate that cache or
`NANOCODEX_WASM_OPT` to select a compatible local executable. Native
optimization defaults to three workers; `BINARYEN_CORES` overrides that
limit. `CARGO_BUILD_JOBS` controls Rust compilation independently.

Use a separate `CARGO_TARGET_DIR` for concurrently changing worktrees.
Production deployment still requires a clean checkout and a WASM attestation
matching the deployed revision; cached outputs do not bypass that check.
