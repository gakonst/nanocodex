import { useEffect, useState } from "react";
import { Agent } from "nanocodex/managed";
type UsagePage = Awaited<ReturnType<Agent.Agent["usage"]>>;
type ArtifactPage = Awaited<ReturnType<Agent.Agent["artifacts"]["list"]>>;
type RequestPage = Awaited<ReturnType<Agent.Agent["requests"]>>;
type EventHistoryPage = Awaited<ReturnType<Agent.Agent["events"]["page"]>>;
import "./ManagedAgentInspector.css";

/** Uses the same account-authorized durable history as the conversation. */
export function ManagedAgentInspector({ agentId, onClose }: { agentId: string; onClose(): void }) {
  const [data, setData] = useState<{ usage: UsagePage; requests: RequestPage; artifacts: ArtifactPage; events: EventHistoryPage }>();
  const [error, setError] = useState<string>();
  const [before, setBefore] = useState<string>();
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    const abort = new AbortController();
    const client = Agent.open(agentId, { fetch: (input, init) => fetch(input, { ...init, signal: abort.signal }) });
    setData(undefined); setError(undefined);
    void Promise.all([client.usage(), client.requests(), client.artifacts.list(), client.events.page({ before, limit: 128 })])
      .then(([usage, requests, artifacts, events]) => { if (!abort.signal.aborted) setData({ usage, requests, artifacts, events }); })
      .catch(e => { if (!abort.signal.aborted) setError(e instanceof Error ? e.message : "Could not load session"); });
    return () => abort.abort();
  }, [agentId, before, refresh]);
  const download = async (id: string, path: string) => {
    try {
      const bytes = await Agent.open(agentId).artifacts.download(id);
      const url = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
      const anchor = document.createElement("a"); anchor.href = url; anchor.download = path.split("/").at(-1) ?? "artifact";
      anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) { setError(e instanceof Error ? e.message : "Download failed"); }
  };
  return <section className="managed-inspector" aria-label="Session inspector">
    <header><strong>Session inspector</strong><button onClick={() => setRefresh(n => n + 1)}>Refresh</button><button onClick={onClose}>Close</button></header>
    {error && <p role="alert">{error}</p>}
    {!data && !error && <p role="status">Loading session…</p>}
    {data && <>
      <h3>Turn usage</h3>
      <p>Reported usage includes cache writes. Missing usage remains unknown. {data.usage.has_more ? "Showing the first 256 lifecycle records." : ""}</p>
      <table><thead><tr><th>Turn</th><th>State</th><th>Input / output tokens</th><th>Estimated cost</th></tr></thead><tbody>
        {data.usage.data.map(row => <tr key={row.cursor}><td>{row.turn_id}</td><td>{row.type.replace("turn_", "")}</td><td>{row.usage ? `${row.usage.input_tokens} / ${row.usage.output_tokens}` : "Unknown"}</td><td>{row.usage?.estimated_cost ? `$${row.usage.estimated_cost.usd}` : "Unknown"}</td></tr>)}
      </tbody></table>
      <h3>Model requests and child agents</h3>
      <p>Each response ID appears once. Request usage is detail within turn totals; do not add the two. {data.requests.has_more ? "Showing the first 256 requests; use the API cursor for more." : ""}</p>
      {data.requests.data.map(row => <details key={row.id}><summary>Agent {row.agent_id} · {row.type} · {typeof row.payload.duration_ns === "number" ? `${(row.payload.duration_ns / 1e6).toFixed(0)} ms` : "Duration unknown"}</summary><pre>{JSON.stringify(row.payload, null, 2)}</pre></details>)}
      <h3>Published outputs</h3>
      {data.artifacts.data.length === 0 && <p>No published files.</p>}
      {data.artifacts.data.map(file => <p key={file.id}><button onClick={() => void download(file.id, file.path)}>{file.path}</button> · {file.size.toLocaleString()} bytes · turn {file.turn_id}<br /><small>SHA-256 {file.digest}</small></p>)}
      {data.artifacts.publications.filter(p => p.state === "failed").map(p => <p key={p.turn_id} role="status">Publication failed for {p.turn_id}: {p.error}</p>)}
      <h3>Event timeline</h3>
      <p>Durable events, including model, tool, and child-agent detail. Open an event to inspect its recorded payload.</p>
      {data.events.data.map(event => <details key={event.cursor}><summary>{event.createdAt === undefined ? "Time unknown" : new Date(event.createdAt).toLocaleTimeString()} · {event.data.type === "event" && event.data.event && typeof event.data.event === "object" && "type" in event.data.event ? String(event.data.event.type) : event.type} · {event.turnId ?? "session"}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>)}
      {data.events.hasMore && <button onClick={() => setBefore(data.events.data[0]?.cursor)}>Older events</button>}
      {before && <button onClick={() => setBefore(undefined)}>Latest events</button>}
    </>}
  </section>;
}
