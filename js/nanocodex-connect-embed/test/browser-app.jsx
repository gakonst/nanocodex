import React, { useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { Agent } from "nanocodex/managed";
import { ConnectFixture } from "./browser-connect.jsx";
import { createManagedAgentSource } from "nanocodex-connect-embed/managed";
import {
  AgentProvider, AgentMessages, AgentComposer, AgentStatus, AgentActivity,
  AgentPendingPrompts, AgentLoadOlder, AgentEmbed,
} from "nanocodex-connect-embed/primitives";

const ids = {
  alpha: "0198d3f0-8844-7000-8000-000000000001",
  beta: "0198d3f0-8844-7000-8000-000000000002",
};
const params = new URLSearchParams(location.search);
function App() {
  const [name, setName] = useState("alpha");
  const [history, setHistory] = useState(true);
  const [connected, setConnected] = useState(true);
  const source = useMemo(() => connected
    ? createManagedAgentSource(Agent.open(ids[name], { baseUrl: location.origin }), { history })
    : undefined, [name, history, connected]);
  return <>
    <h1>Embed browser journey</h1>
    <aside id="host"><button>Host button</button><textarea aria-label="Host notes" defaultValue="Host text" /><p>Host paragraph</p></aside>
    <nav aria-label="Fixture controls">
      <label>Agent <select aria-label="Agent" value={name} onChange={event => setName(event.target.value)}>
        <option value="alpha">Alpha</option><option value="beta">Beta</option>
      </select></label>
      <label><input type="checkbox" checked={history} onChange={event => setHistory(event.target.checked)} /> Include history</label>
      <button onClick={() => setConnected(!connected)}>{connected ? "Disconnect" : "Connect"}</button>
    </nav>
    {params.get("assembled") === "true"
      ? <AgentEmbed agent={source} theme={params.get("theme") || undefined} />
      : <AgentProvider agent={source}>
        <section data-agent-embed="" data-theme={params.get("theme") || undefined} aria-label="Embedded agent">
          <AgentStatus /><AgentLoadOlder /><AgentMessages showActivity={false} />
          <AgentActivity /><AgentPendingPrompts /><AgentComposer />
        </section>
      </AgentProvider>}
  </>;
}
createRoot(document.getElementById("root")).render(params.get("rich") === "true" ? <ConnectFixture /> : <App />);
