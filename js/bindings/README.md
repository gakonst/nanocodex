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

const branch = await agent.session.fork({ at: turn });
console.log(await branch.turn.prompt({ input: "Try another approach." }).result());

const followOn = Actions.turn.prompt(agent, { input: "Now explain it." });
console.log(await Actions.turn.getResult(followOn));
```

`Agent` and `Actions` are module namespaces, not classes. `Agent.create` returns
an owned client decorated with matching domain actions:

- `agent.turn.prompt(...)` / `Actions.turn.prompt(agent, ...)`
- `agent.session.fork(...)` / `Actions.session.fork(agent, ...)`
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
  import { Agent } from "https://cdn.jsdelivr.net/npm/nanocodex@0.1.0/browser/index.mjs";
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
