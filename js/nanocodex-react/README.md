# nanocodex-react

React hooks over the headless browser SDK. The vanilla config owns the package
Worker, Rust/WASM Agent, persistent workspace, and cleanup. React only reads
that external state and binds event subscriptions.

## Headless conversation controller

`nanocodex-react/agent` accepts a structural Agent contract and owns the full
conversation lifecycle without owning markup, Markdown rendering, scrolling,
or CSS. It projects ordered user, reasoning, assistant, nested tool, plan, and
error entries, and exposes stable controls for prompts, steering, cancellation,
history, and cleanup.

```tsx
import { useAgentController } from "nanocodex-react/agent";

function Conversation({ agent, visible }) {
  const conversation = useAgentController(agent, { visible });
  return (
    <section>
      {conversation.entries.map((entry) => (
        <TranscriptEntry key={entry.id} entry={entry} />
      ))}
      <Composer
        running={conversation.running}
        onCancel={conversation.cancel}
        onSubmit={conversation.submit}
      />
    </section>
  );
}
```

The default submit intent steers the latest active turn. Pass
`{ intent: "queue" }` to queue a separate root turn. `loadOlder()` delegates to
the Agent watcher and merges retained pages by durable turn identity. Streaming
bursts publish at most once per animation frame; while `visible` is false the
controller continues reducing events and publishes one catch-up snapshot after
becoming visible. `AgentController` provides the same API as a render-prop
component.

Reasoning summaries and assistant text arrive as separate Markdown-bearing
entries. Consumers can apply the same streaming renderer to both while keeping
their own visual policy:

```tsx
import { Streamdown } from "streamdown";

function TranscriptEntry({ entry }) {
  if (entry.kind !== "reasoning" && entry.kind !== "assistant") return null;
  return (
    <article data-kind={entry.kind}>
      {entry.kind === "reasoning" ? <span>thinking{entry.streaming ? "…" : ""}</span> : null}
      <Streamdown
        caret={entry.streaming ? "block" : undefined}
        isAnimating={entry.streaming}
        mode={entry.streaming ? "streaming" : "static"}
        skipHtml
      >
        {entry.text}
      </Streamdown>
    </article>
  );
}
```

Connect applications normalize their capability-bound agent through the
Connect entrypoint. The `history` choice is required and should come directly
from the signed grant; when false, the source never requests retained history
and exposes live events only for turns submitted through that source.

```tsx
import { createConnectAgentSource } from "nanocodex-react/connect";
import { useAgentController } from "nanocodex-react/agent";

const source = createConnectAgentSource(connectAgent, {
  history: connection.grant.visibility.conversationHistory,
});
const conversation = useAgentController(source);
```

```tsx
import { createConfig, useNanocodex } from "nanocodex-react";

const config = createConfig();

function App() {
  const { data: agent, error, isPending, refetch } = useNanocodex({ config });
  // `agent` is the normal headless Agent from `nanocodex/browser`.
}

root.render(<App />);
```

`useNanocodex({ config, threadId, enabled })` follows an external-store lifecycle: the
vanilla config creates one Agent for active subscribers, shares it, and shuts it
down after the last subscriber leaves. Disabled hooks stay idle and do not
prepare or create an Agent. Omitted and empty thread IDs resolve to one stable
config-owned default, including across React remounts. Server rendering always
observes the idle snapshot and then reconciles with the live client resource.

Components that need only part of the resource can select it without rerendering
for unrelated state changes:

```tsx
const sessionId = useNanocodex({
  config,
  selector: (resource) => resource.data?.sessionId,
  equalityFn: Object.is,
});
```

Without a selector, `useNanocodex` returns the full query-like resource shown above.
`useAgentEvents` is the narrow hook for ordered typed events.

`useVoice` is the thin React adapter over the Rust/WASM-owned Codex voice
resource. It accepts the same normal, managed, and Connect Agent handles as the
lower-level browser binding:

```tsx
const { data: agent } = useNanocodex({ config, threadId });
const voice = useVoice(agent);

return (
  <button disabled={!agent} onClick={() => void voice.toggle()}>
    {voice.isActive ? `Voice (${voice.voice})` : "Voice"}
  </button>
);
```

Unmounting or replacing the Agent closes voice media and the Realtime session.
Call `voice.cancel()` separately when the user intends to cancel the active
coding turn.

Create the config once, outside React. Applications with many consumers can put
it in `NanocodexProvider` and omit `config` from each hook. Agent defaults belong
in `createConfig({ agent: { ... } })`; React does not need a separate preload or
preparation lifecycle.

```ts
const config = createConfig({ agent: { /* tools and policy */ } });
```
