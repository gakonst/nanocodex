import { LatencyPlot, OutcomePlot } from "./RouterPlots";
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
  const [model, setModel] = useState("gpt-5.6-luna");
  const [effort, setEffort] = useState("low");
  const [source, setSource] = useState("live");
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
    && (provider === "all" || p.backend === provider) && (model === "all" || p.model === model) && p.effort === effort && (source === "all" || p.source === source)) ?? [];
  const decisions = snapshot?.decisions.filter(d => (colo === "all" || d.clientIngressColo === colo)
    && (provider === "all" || routeProvider(d.chosen) === provider) && (model === "all" || d.chosen.includes(model)) && d.chosen.endsWith(`:${effort}`)) ?? [];
  const summary = summarizeRouter(decisions);
  const livePlotRows = providers.filter(p => p.source === (source === "probe" ? "probe" : "live"));
  const unresolved = decisions.filter(d => d.decision === "unavailable_or_invalid" || d.decision === "unsupported_input").length;
  const liveSegments = [
    {label:"Accepted",count:summary.accepted,color:"#5b9b91"},
    {label:"Low confidence",count:summary.low,color:"#d9a05b"},
    {label:"Unavailable / invalid",count:unresolved,color:"#c96f6b"},
    {label:"Explicit · no Jev",count:summary.bypassed,color:"#8896c5"},
  ];
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
          <label>Model<select value={model} onChange={e => setModel(e.target.value)}><option value="all">All models</option>{[...new Set([model,...models])].filter(m=>m!=="all").map(m => <option key={m}>{m}</option>)}</select></label>
          <label>Effort<select value={effort} onChange={e=>setEffort(e.target.value)}>{["low","medium","high"].map(e=><option key={e}>{e}</option>)}</select></label>
          <label>Provider samples<select value={source} onChange={e => setSource(e.target.value)}><option value="live">Live</option><option value="probe">Probes</option></select></label>
        </div>
        <div className="router-overview">
          <article className="router-chart-card"><div className="router-chart-title"><h2>Time to first token</h2><span>Lower is faster</span></div>
            <p className="router-muted">{source === "probe" ? "Synthetic probes" : "Live generations"} · bars p50 · whiskers p95 · {colo === "all" ? "deployment global" : colo}</p>
            {livePlotRows.length ? <LatencyPlot label="Provider generation TTFT, median with p95 whisker" rows={livePlotRows.map(p=>({label:model === "all" ? `${p.backend} / ${p.model.replace("gpt-", "")}` : p.backend,provider:p.backend,p50:p.generationTtftP50Ms,p95:p.generationTtftP95Ms,count:p.sampleCount,failures:p.censoredCount}))}/> : <p className="router-empty">No {source === "probe" ? "probe" : "live"} samples for this selection.</p>}
          </article>
          <article className="router-chart-card"><div className="router-chart-title"><h2>Routing health</h2><span>{summary.decisions} decisions</span></div>
            <OutcomePlot label="Routing decision outcomes" segments={liveSegments}/>
            <div className="router-health-line"><strong className={summary.bindingFailures?"is-warning":""}>{summary.bindingFailures} / {summary.attempts}</strong><span>Jev binding attempts failed<br/>{summary.recovered} decisions recovered by retry</span></div>
            <div className="router-health-line"><strong>{providers.reduce((n,p)=>n+p.censoredCount,0)} / {providers.reduce((n,p)=>n+p.sampleCount,0)}</strong><span>Generation attempts failed</span></div>
          </article>
        </div>
        <p className="router-muted">Failures never count as fast responses. Fewer than three TTFT samples is sparse. Ingress is not Worker execution location.</p>
        <details className="router-inspect"><summary>Inspect samples, failures & choice probabilities</summary>
        <h2>Provider responsiveness</h2><p className="router-muted">Generation TTFT is measured inside the service, not at the client. Failures are excluded from latency statistics. Fewer than three TTFT samples is insufficient for routing. Ingress is not Worker execution location.</p>
        <div className="router-table-wrap"><table><thead><tr>{["Provider / model", "Effort", "Source", "Samples", "Failures", "TTFT p50", "TTFT p95", "Full response p50", "Last sample"].map(h => <th key={h}>{h}</th>)}</tr></thead><tbody>{providers.map(p => <tr key={[p.backend,p.model,p.effort,p.source].join(":")}><td><strong>{p.backend}</strong><small>{p.model}</small></td><td>{p.effort}</td><td><span className="router-badge">{p.source}</span></td><td>{p.sampleCount}<small>{p.generationTtftSampleCount} TTFT</small></td><td title={`HTTP ${p.httpErrorCount}, network ${p.networkErrorCount}, protocol ${p.protocolErrorCount}, timeout ${p.timeoutCount}, cancelled ${p.cancelledCount}`}>{p.censoredCount}</td><td>{ms(p.generationTtftP50Ms)}{p.generationTtftSampleCount < 3 && <small>Insufficient samples</small>}</td><td>{ms(p.generationTtftP95Ms)}</td><td>{ms(p.fullResponseP50Ms)}</td><td>{p.lastObservedAt ? new Date(p.lastObservedAt).toLocaleTimeString() : "—"}</td></tr>)}</tbody></table></div>
        {!providers.length && <p className="router-empty">No recent provider samples match these filters.</p>}
        <h2>Recent routing decisions</h2><p className="router-muted">Newly resolved direct routes only; pinned continuation turns do not call Jev again. Provider sample filters do not filter decisions. No prompts or account identifiers are stored here.</p>
        <div className="router-decisions">{decisions.slice(0,100).map((d,i) => <details key={`${d.timestamp}:${i}`}><summary><span className="router-decision-time">{new Date(d.timestamp).toLocaleTimeString()} · {d.clientIngressColo ?? "unknown"}</span><strong>{d.chosen}</strong><span className={`router-badge ${d.decision === "unavailable_or_invalid" ? "is-warning" : ""}`}>{label(d.decision)}</span><span>{ms(d.durationMs)}</span></summary><div className="router-decision-body"><p>Classifier: {label(d.classifier.outcome)} · confidence {percent(d.confidence)}</p><p>{d.classifier.attempts.length ? d.classifier.attempts.map((a,n) => `Attempt ${n+1}: ${label(a.outcome)} (${ms(a.duration_ms)})`).join(" · ") : "No classifier call."}</p><Probabilities values={d.probabilities} /></div></details>)}</div>
        {!decisions.length && <p className="router-empty">No routing decisions in this window. Existing pinned conversations do not create new decisions.</p>}
      </details></>}
    </> : <>
      <p className="router-muted">Historical snapshot · {benchmark.date} · {benchmark.model} / {benchmark.effort} · retained unchanged for comparison.</p>
      <div className="router-benchmark-top">
        <div><strong className="router-headline-number">9<span> / 18</span></strong><h2>Jev binding calls failed</h2><p className="router-muted">All 18 generations completed; 17 routes used fallback.</p></div>
        <div><h2>What the router actually did</h2><OutcomePlot label="Historical routing outcomes" segments={[{label:"Accepted",count:1,color:"#5b9b91"},{label:"Low confidence",count:8,color:"#d9a05b"},{label:"Binding failed",count:9,color:"#c96f6b"}]}/></div>
      </div>
      <div className="router-chart-title"><h2>First output, by region & provider</h2><span>45 / 45 streaming calls completed</span></div>
      <p className="router-muted">Faint bars: buffered baseline · solid bars: streaming · same axis · lower is faster</p>
      <div className="router-region-plots">{["IAD","LHR","NRT"].map(region=><article className="router-chart-card" key={region}><h3>{region}<small>{{IAD:"US East",LHR:"London",NRT:"Tokyo"}[region]}</small></h3><LatencyPlot label={`${region} baseline versus streaming time to first public output`} compare max={2500} rows={benchmark.cells.filter(c=>c.colo===region).map(c=>({label:c.provider,provider:c.provider,baseline:c.baseline,p50:c.streaming,count:c.streamingCount,failures:0}))}/></article>)}</div>
      <p className="router-chart-caveat">No proven regional-routing speedup yet. Five mixed prompts per cell; separate time windows and cache differences. Includes API and network time.</p>
      <details className="router-inspect"><summary>Inspect the 18 decisions & Jev probabilities</summary>
      <h2>All 18 Jev decisions</h2><p className="router-muted">Location labels below describe the harness Worker execution placement, when reported.</p><div className="router-decisions">{benchmark.jev.rows.map((r,i) => <details key={i}><summary><span>{(r.placement.replace("remote-", "") || "execution unknown")} · {r.arm}</span><strong>{r.selected}</strong><span className={`router-badge ${!r.binding_ok ? "is-warning" : ""}`}>{r.binding_ok ? label(r.confidence_status) : "binding failed"}</span><span>{ms(r.outer_router_ms)}</span></summary><div className="router-decision-body"><p>Confidence {percent(r.confidence)} · {r.binding_ok ? "Valid binding response" : "Binding failed; fallback used"}</p><Probabilities values={r.probabilities} /></div></details>)}</div>
    </details></>}
  </section>;
}
