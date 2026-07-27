# Nanocodex for JavaScript

The Node and browser entrypoints expose the same viem-v3-style API over the
same Rust/WASM agent. Runtime-specific host options are flattened into
`Agent.create(...)`; generated WASM handles and host routing remain private.

```js
import { Actions, Agent } from "nanocodex/node";

const agent = await Agent.create({
  apiKey: process.env.OPENAI_API_KEY,
  reasoningMode: "pro",
  thinking: "high",
  tools,
});

const turn = agent.turn.prompt({ input: "Build the thing." });
console.log(await turn.result());

await agent.session.setThinking("high");
await agent.session.setFastMode(true);

const branch = await agent.session.fork({ at: turn });
console.log(await branch.turn.prompt({ input: "Try another approach." }).result());

const followOn = Actions.turn.prompt(agent, { input: "Now explain it." });
console.log(await Actions.turn.getResult(followOn));
```

Node and browser applications can instead pay through MPP without an OpenAI
API key. Pass an MPP session with a `ws(endpoint)` method; an `mppx` Tempo
session manager has this shape. Nanocodex defaults the socket to
`wss://openai.mpp.tempo.xyz/v1/responses` when `mpp` is present.

```js
import { Agent } from "nanocodex/node";
import { Expiry } from "accounts";
import { Provider } from "accounts/cli";
import { tempo } from "mppx/client";
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
const mpp = tempo.session.manager({
  account,
  autoSwap: { tokenIn: [pathUsd], slippage: 1 },
  bootstrap: true,
  client: provider.getClient(),
  webSocket: WebSocket,
  maxDeposit: "0.05",
  topUpAmount: "0.05",
});

const agent = await Agent.create({ mpp, thinking: "none", fastMode: true, tools });
const events = agent.events.watch();
const unwatch = events.onEvent((event) => {
  process.stdout.write(`${JSON.stringify(event)}\n`);
});
try {
  console.error(await agent.turn.prompt({ input: "Build the thing." }).result());
} finally {
  await mpp.close();
  unwatch();
  events.off();
  agent.dispose();
}
```

The application still owns its wallet, deposit policy, persisted payment
channel store, and final settlement. Keep the manager alive to reuse its channel
across agents, and supply mppx `channelStore` for reuse after a process or page
restart. Nanocodex never closes a caller-owned MPP session. `apiKey` and `mpp`
are mutually exclusive.

Completed turns can be persisted and resumed by a fresh Node or browser agent:

```js
const snapshot = turn.snapshot();
agent.dispose();

const resumed = await Agent.create({
  apiKey: process.env.OPENAI_API_KEY,
  resume: snapshot,
  tools,
});
```

The snapshot contains authoritative typed history but no provider response ID,
so the first resumed request safely replays the committed conversation. Resume
with the same instructions and tool definitions, and release the original
agent before handing its snapshot to another writer.

Node embedders whose bundler relocates package assets may compile and pass the
web-target artifact explicitly. The runtime still uses the Node host for
WebSockets and Code Mode:

```js
const module = await WebAssembly.compile(await readFile(wasmAssetPath));
const agent = await Agent.create({ apiKey, module });
```

A Codex-compatible rollout can also be resumed by materializing its committed
`response_item` history into a snapshot with no `request_prefix`. Nanocodex
rebuilds the current prefix from the supplied instructions and JavaScript tools
while preserving the rollout's workspace, lineage, cache key, canonical user
context, and typed history.

`Agent` and `Actions` are module namespaces, not classes. `Agent.create` returns
an owned client decorated with matching domain actions:

- `agent.turn.prompt(...)` / `Actions.turn.prompt(agent, ...)`
- `turn.snapshot()` / `Actions.turn.getSnapshot(turn)`
- `agent.session.fork(...)` / `Actions.session.fork(agent, ...)`
- `agent.session.setThinking(...)` / `Actions.session.setThinking(agent, ...)`
- `agent.session.setFastMode(...)` / `Actions.session.setFastMode(agent, ...)`
- `agent.session.spawn()` / `Actions.session.spawn(agent)`
- `agent.events.watch(...)` / `Actions.events.watch(agent, ...)`

Every action owns its types, for example `Actions.turn.prompt.Options`,
`Actions.turn.prompt.ReturnType`, and `Actions.events.watch.Watcher`.

Event watches are lazy, terminal handles:

```js
const watch = agent.events.watch();
const unlisten = watch.onEvent(console.log);

unlisten();
watch.off();
```

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

Browser Workers use the identical shape:

```js
import { Agent } from "nanocodex/browser";

const agent = await Agent.create({
  websocketUrl: signedOrCookieAuthorizedEndpoint,
  createWebSocket(endpoint, sessionId) {
    const url = new URL(endpoint);
    url.searchParams.set("session_id", sessionId);
    return new WebSocket(url);
  },
  tools,
});
```

After publication, a browser can load the same entrypoint without a package
manager or build step:

```html
<script type="module">
  import { Agent } from "https://cdn.jsdelivr.net/npm/nanocodex@0.2.0/browser/index.mjs";
  const agent = await Agent.create({ websocketUrl: "/api/responses" });
  console.log(await agent.turn.prompt({ input: "Hello." }).result());
</script>
```

Pin the package version in production. The adjacent WASM file is part of the
npm package and is resolved relative to the browser module. The endpoint must
be authorized by the embedding application because browser WebSockets cannot
attach OpenAI's upgrade authorization header.

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
