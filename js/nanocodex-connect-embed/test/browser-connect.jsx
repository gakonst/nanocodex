import React, { useMemo, useState } from "react";
import { Agent } from "nanocodex/managed";
import { ConnectConversation } from "nanocodex-connect-embed";

/** The fixture adapts the real managed HTTP handle to the structural Connect
 * handle. Grant authorization itself is outside this presentation journey. */
export function ConnectFixture() {
  const params = new URLSearchParams(location.search);
  const [authorization, setAuthorization] = useState(params.get("auth") || "active");
  const [rawTraces, setRawTraces] = useState(true);
  const [showToolCalls, setShowToolCalls] = useState(true);
  const [history, setHistory] = useState(true);
  const [state, setState] = useState();
  const agent = useMemo(() => {
    const managed = Agent.open("0198d3f0-8844-7000-8000-000000000001", { baseUrl: location.origin });
    return { ...managed, type: "connect", sessionId: managed.id };
  }, []);
  const connection = {
    agentId: authorization === "mismatch" ? "0198d3f0-8844-7000-8000-000000000002" : agent.id,
    grant: { id: "synthetic-browser-grant", status: authorization === "revoked" ? "revoked" : "active",
      visibility: { conversationHistory: history, rawTraces, finalMessages: true } },
  };
  return <>
    <label>Authorization <select aria-label="Authorization" value={authorization} onChange={event => setAuthorization(event.target.value)}>
      <option value="active">Active</option><option value="revoked">Revoked</option><option value="mismatch">Mismatched agent</option>
    </select></label>
    <label><input type="checkbox" checked={rawTraces} onChange={event => setRawTraces(event.target.checked)} /> Grant raw traces</label>
    <label><input type="checkbox" checked={showToolCalls} onChange={event => setShowToolCalls(event.target.checked)} /> Show tool calls</label>
    <label><input type="checkbox" checked={history} onChange={event => setHistory(event.target.checked)} /> Grant history</label>
    <output aria-label="Connect state">{state?.status}</output>
    <section aria-label="Connect conversation">
      <ConnectConversation agent={agent} connection={connection} showToolCalls={showToolCalls}
        inactiveMessage={({ agentError }) => agentError || ""} onStateChange={setState} />
    </section>
  </>;
}
