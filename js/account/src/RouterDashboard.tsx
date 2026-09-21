import { useCallback, useEffect, useState } from "react";
import { RefreshCw, ArrowUpRight, Activity, Route, Clock3, AlertTriangle } from "lucide-react";
import benchmark from "./routerBenchmark.json";
import { routeProvider, summarizeRouter, type RouterSnapshot } from "./routerApi";
import "./router.css";
const ms = (value: number | null) => value === null ? "—" : `${Math.round(value).toLocaleString()} ms`;
const percent = (value: number | null) => value === null ? "—" : `${(value * 100).toFixed(1)}%`;
const label = (value: string) => value.replaceAll("_", " ");
function Probabilities({ values }: { values: Record<string, number> | null }) {
  if (!values) return <p className="router-muted">No valid choice probabilities were returned.</p>;
  return <div className="router-probabilities">{Object.entries(values).sort((a,b) => b[1]-a[1]).map(([name, value]) =>
    <div key={name}><span>{name}</span><meter min={0} max={1} value={value} aria-label={`${name} choice probability`} /><strong>{percent(value)}</strong></div>)}
    <p className="router-muted">Choice probabilities describe Jev’s preference among candidates. They are not task success probabilities.</p></div>;
}
export default function RouterDashboard() {
  const [view, setView] = useState<"live" | "benchmark">("live");
  const [snapshot, setSnapshot] = useState<RouterSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [colo, setColo] = useState("all");
  const [provider, setProvider] = useState("all");
  const [model, setModel] = useState("all");
  const [source, setSource] = useState("all");
  const reload = useCallback(() => setRefresh(n => n + 1), []);
  useEffect(() => {
    if (view !== "live") return;
    const controller = new AbortController();
    let pending = false;
    async function load() {
      if (pending || document.hidden) return;
      pending = true; setBusy(true);
      try {
        const response = await fetch("/api/router", { credentials: "same-origin", signal: controller.signal, cache: "no-store" });
        if (!response.ok) throw new Error(response.status === 401 ? "Sign in through Connect to view live routing." : response.status === 403 ? "Live routing data is restricted to the platform administrator." : "Live telemetry is unavailable. Retry to refresh.");
        const data: RouterSnapshot = await response.json();
        if (data.version !== 1 || !Array.isArray(data.providers) || !Array.isArray(data.decisions)) throw new Error("Unexpected telemetry response.");
        if (!controller.signal.aborted) { setSnapshot(data); setError(null); }
      } catch (failure) {
        if (!controller.signal.aborted) { setError(failure instanceof Error ? failure.message : "Live telemetry is unavailable."); setSnapshot(null); }
      } finally { pending = false; if (!controller.signal.aborted) setBusy(false); }
    }
    void load();
    const tick = setInterval(() => void load(), 30_000);
    const visible = () => { if (!document.hidden) void load(); };
    document.addEventListener("visibilitychange", visible);
    return () => { controller.abort(); clearInterval(tick); document.removeEventListener("visibilitychange", visible); };
  }, [view, refresh]);
  const colos = [...new Set(snapshot?.providers.flatMap(p => p.clientIngressColo ? [p.clientIngressColo] : []) ?? [])].sort();
  const models = [...new Set(snapshot?.providers.map(p => p.model) ?? [])].sort();
  const providers = snapshot?.providers.filter(p => p.sampleCount > 0 && (colo === "all" ? p.scope === "deployment_global" : p.scope === "client_ingress" && p.clientIngressColo === colo)
    && (provider === "all" || p.backend === provider) && (model === "all" || p.model === model) && (source === "all" || p.source === source)) ?? [];
  const decisions = snapshot?.decisions.filter(d => (colo === "all" || d.clientIngressColo === colo)
    && (provider === "all" || routeProvider(d.chosen) === provider) && (model === "all" || d.chosen.includes(model))) ?? [];
  const summary = summarizeRouter(decisions);
  return <section className="router-dashboard" aria-labelledby="router-title">
    <header className="router-header"><div><p className="router-eyebrow"><Route size={15} /> INFERENCE OPERATIONS</p><h1 id="router-title">Router</h1><p>Where requests go. How long they take. What fails.</p></div>
      <a className="router-link" href={benchmark.source} target="_blank" rel="noreferrer">Benchmark report <ArrowUpRight size={16} /></a></header>
    <div className="router-toolbar"><div className="router-tabs" role="group" aria-label="Data view">
      <button aria-pressed={view === "live"} onClick={() => setView("live")}><Activity size={15} /> Live</button>
      <button aria-pressed={view === "benchmark"} onClick={() => setView("benchmark")}><Clock3 size={15} /> Sep 21 benchmark</button></div>
      {view === "live" && <button className="router-refresh" onClick={reload} disabled={busy}><RefreshCw size={15} /> {busy ? "Refreshing…" : "Refresh"}</button>}</div>
    {view === "live" ? <>
      <p className="router-muted">Last two hours · up to 512 provider observations and 512 routing decisions · refreshes every 30 seconds while visible.</p>
      {error ? <div className="router-notice" role="alert"><AlertTriangle size={18} /><span>{error} <a href="/connect">Connect</a></span></div> : !snapshot ? <p role="status">Loading live telemetry…</p> : <>
        <div className="router-meta"><span>Updated {new Date(snapshot.capturedAt).toLocaleTimeString()}</span><span>Scheduled probes {snapshot.probesEnabled ? `every ${snapshot.probeIntervalMs / 60_000} min` : "disabled"}</span><span>Provider/model stays pinned per conversation</span></div>
        <div className="router-filters">
          <label>Ingress colo<select value={colo} onChange={e => setColo(e.target.value)}><option value="all">Deployment global</option>{colos.map(c => <option key={c}>{c}</option>)}</select></label>
          <label>Provider<select value={provider} onChange={e => setProvider(e.target.value)}><option value="all">All providers</option>{["cloudflare","openrouter","vercel","workers_ai","chatgpt"].map(p => <option key={p}>{p}</option>)}</select></label>
          <label>Model<select value={model} onChange={e => setModel(e.target.value)}><option value="all">All models</option>{models.map(m => <option key={m}>{m}</option>)}</select></label>
          <label>Provider samples<select value={source} onChange={e => setSource(e.target.value)}><option value="all">Live & probes (separate)</option><option value="live">Live</option><option value="probe">Probes</option></select></label>
        </div>
        <div className="router-stats"><Stat title="Routing decisions" value={summary.decisions} note={`${summary.accepted} accepted · ${summary.bypassed} explicit`} /><Stat title="Binding failures" value={`${summary.bindingFailures} / ${summary.attempts}`} note={`${summary.recovered} decisions recovered by retry`} warning={summary.bindingFailures > 0} /><Stat title="Low confidence" value={summary.low} note="Valid classifier output; fallback policy used" /><Stat title="Generation failures" value={providers.reduce((n,p) => n+p.censoredCount,0)} note={`Across ${providers.reduce((n,p) => n+p.sampleCount,0)} filtered provider samples`} /></div>
        <h2>Provider responsiveness</h2><p className="router-muted">Generation TTFT is measured inside the service, not at the client. Failures are excluded from latency statistics. Fewer than three TTFT samples is insufficient for routing. Ingress is not Worker execution location.</p>
        <div className="router-table-wrap"><table><thead><tr>{["Provider / model", "Effort", "Source", "Samples", "Failures", "TTFT p50", "TTFT p95", "Full response p50", "Last sample"].map(h => <th key={h}>{h}</th>)}</tr></thead><tbody>{providers.map(p => <tr key={[p.backend,p.model,p.effort,p.source].join(":")}><td><strong>{p.backend}</strong><small>{p.model}</small></td><td>{p.effort}</td><td><span className="router-badge">{p.source}</span></td><td>{p.sampleCount}<small>{p.generationTtftSampleCount} TTFT</small></td><td title={`HTTP ${p.httpErrorCount}, network ${p.networkErrorCount}, protocol ${p.protocolErrorCount}, timeout ${p.timeoutCount}, cancelled ${p.cancelledCount}`}>{p.censoredCount}</td><td>{ms(p.generationTtftP50Ms)}{p.generationTtftSampleCount < 3 && <small>Insufficient samples</small>}</td><td>{ms(p.generationTtftP95Ms)}</td><td>{ms(p.fullResponseP50Ms)}</td><td>{p.lastObservedAt ? new Date(p.lastObservedAt).toLocaleTimeString() : "—"}</td></tr>)}</tbody></table></div>
        {!providers.length && <p className="router-empty">No recent provider samples match these filters.</p>}
        <h2>Recent routing decisions</h2><p className="router-muted">Newly resolved direct routes only; pinned continuation turns do not call Jev again. Provider sample filters do not filter decisions. No prompts or account identifiers are stored here.</p>
        <div className="router-decisions">{decisions.slice(0,100).map((d,i) => <details key={`${d.timestamp}:${i}`}><summary><span className="router-decision-time">{new Date(d.timestamp).toLocaleTimeString()} · {d.clientIngressColo ?? "unknown"}</span><strong>{d.chosen}</strong><span className={`router-badge ${d.decision === "unavailable_or_invalid" ? "is-warning" : ""}`}>{label(d.decision)}</span><span>{ms(d.durationMs)}</span></summary><div className="router-decision-body"><p>Classifier: {label(d.classifier.outcome)} · confidence {percent(d.confidence)}</p><p>{d.classifier.attempts.length ? d.classifier.attempts.map((a,n) => `Attempt ${n+1}: ${label(a.outcome)} (${ms(a.duration_ms)})`).join(" · ") : "No classifier call."}</p><Probabilities values={d.probabilities} /></div></details>)}</div>
        {!decisions.length && <p className="router-empty">No routing decisions in this window. Existing pinned conversations do not create new decisions.</p>}
      </>}
    </> : <>
      <p className="router-muted">Historical snapshot · {benchmark.date} · {benchmark.model} / {benchmark.effort} · retained unchanged for comparison.</p>
      <div className="router-stats"><Stat title="Streaming completions" value="45 / 45" note="45 buffered baseline calls also completed" /><Stat title="Jev binding failures" value={`${benchmark.jev.bindingFailures} / ${benchmark.jev.cases}`} note="Original errors lacked cause details" warning /><Stat title="Low-confidence choices" value={benchmark.jev.lowConfidence} note={`${benchmark.jev.accepted} accepted at ≥ 0.75 confidence`} /><Stat title="Downstream completions" value={`${benchmark.jev.completed} / ${benchmark.jev.cases}`} note="Fallbacks still generated responses" /></div>
      <div className="router-notice"><AlertTriangle size={18} /><span>This run does not establish a regional-routing speedup. Nine early bindings failed, eight valid choices had low confidence, and the outer harness added another classifier call. Cache hits and generated token counts also changed.</span></div>
      <h2>Time to first public output</h2><p className="router-muted">Descriptive medians in milliseconds; five mixed short/long prompts per cell. Includes API routing and network time. Before/after windows were about 25 minutes apart.</p>
      <div className="router-table-wrap"><table><thead><tr><th>Ingress</th><th>Provider</th><th>Buffered</th><th>Streaming</th><th>Difference</th><th>Streaming time</th></tr></thead><tbody>{benchmark.cells.map(c => <tr key={`${c.colo}:${c.provider}`}><td>{c.colo}</td><td><strong>{c.provider}</strong></td><td>{ms(c.baseline)}</td><td>{ms(c.streaming)}</td><td>{c.streaming > c.baseline ? "+" : ""}{Math.round(c.streaming-c.baseline)} ms</td><td><div className="router-bar" aria-label={`${ms(c.streaming)} streaming median`}><i style={{width:`${c.streaming / 2500 * 100}%`}} /></div></td></tr>)}</tbody></table></div>
      <h2>All 18 Jev decisions</h2><p className="router-muted">Location labels below describe the harness Worker execution placement, when reported.</p><div className="router-decisions">{benchmark.jev.rows.map((r,i) => <details key={i}><summary><span>{(r.placement.replace("remote-", "") || "execution unknown")} · {r.arm}</span><strong>{r.selected}</strong><span className={`router-badge ${!r.binding_ok ? "is-warning" : ""}`}>{r.binding_ok ? label(r.confidence_status) : "binding failed"}</span><span>{ms(r.outer_router_ms)}</span></summary><div className="router-decision-body"><p>Confidence {percent(r.confidence)} · {r.binding_ok ? "Valid binding response" : "Binding failed; fallback used"}</p><Probabilities values={r.probabilities} /></div></details>)}</div>
    </>}
  </section>;
}
function Stat({title,value,note,warning=false}:{title:string;value:string|number;note:string;warning?:boolean}) {
  return <div className={`router-stat ${warning ? "is-warning" : ""}`}><span>{title}</span><strong>{value}</strong><small>{note}</small></div>;
}
