import { useCallback, useEffect, useState } from "react";
import {
  RefreshCw,
  ArrowUpRight,
  Activity,
  Route,
  Clock3,
  AlertTriangle,
} from "lucide-react";
import benchmark from "./routerBenchmark.json";
import { type RouterDecision, type RouterSnapshot } from "./routerApi";
import { RouterAtlas, DecisionRibbon } from "./RouterAtlas";
import { formatMs, providerColors } from "./routerAtlasData";
import "./router.css";

function BenchmarkAtlas() {
  const providers = ["cloudflare", "openrouter", "vercel"];
  const decisions: RouterDecision[] = benchmark.jev.rows.map((r, i) => ({
    timestamp: i,
    clientIngressColo: r.placement.replace("remote-", "") || null,
    chosen: r.selected,
    decision: r.binding_ok
      ? r.confidence_status === "accepted"
        ? "accepted"
        : "low"
      : "unavailable_or_invalid",
    durationMs: r.outer_router_ms,
    confidence: r.confidence,
    probabilities: r.probabilities,
    classifier: {
      outcome: r.binding_ok ? "success" : "unavailable",
      attempts: [
        {
          duration_ms: r.outer_router_ms,
          outcome: r.binding_ok ? "success" : "unavailable",
        },
      ],
    },
  }));
  return (
    <div className="router-atlas">
      <div className="atlas-topline">
        <div>
          <h2>Regional benchmark</h2>
          <p>
            {benchmark.model} · {benchmark.effort} · {benchmark.date}
          </p>
        </div>
        <div className="atlas-totals">
          <span>
            <b>90/90</b> generations <em>baseline + streaming</em>
          </span>
          <span>
            <b className="atlas-danger">9/18</b> Jev failures{" "}
            <em>17 routes used fallback</em>
          </span>
        </div>
      </div>
      <div className="atlas-legend">
        <span>○ buffered baseline → ● streaming median</span>
        <span>Shared linear axis · client time to first output</span>
        <span>5 mixed prompts per cell per arm</span>
      </div>
      <div
        className="atlas-scroll"
        role="region"
        aria-label="Historical regional latency comparison"
        tabIndex={0}
      >
        <table className="atlas-matrix atlas-benchmark">
          <caption>
            Historical evidence · includes network and API overhead · lower is
            faster
          </caption>
          <thead>
            <tr>
              <th scope="col">Region</th>
              {providers.map((p) => (
                <th scope="col" key={p} style={{ color: providerColors[p] }}>
                  {p}
                  <svg preserveAspectRatio="none" viewBox="0 0 250 22">
                    {[0, 1000, 2000, 2500].map((t) => (
                      <text
                        key={t}
                        x={8 + (t / 2500) * 234}
                        y={16}
                        textAnchor={
                          t === 0 ? "start" : t === 2500 ? "end" : "middle"
                        }
                      >
                        {formatMs(t)}
                      </text>
                    ))}
                  </svg>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {["IAD", "LHR", "NRT"].map((region) => (
              <tr key={region}>
                <th scope="row">
                  <strong>{region}</strong>
                  <small>
                    {{ IAD: "US East", LHR: "London", NRT: "Tokyo" }[region]}
                  </small>
                </th>
                {providers.map((p) => {
                  const cell = benchmark.cells.find(
                    (c) => c.colo === region && c.provider === p,
                  )!;
                  const x = (n: number) => 8 + (n / 2500) * 234;
                  return (
                    <td key={p}>
                      <svg
                        viewBox="0 0 250 38"
                        role="img"
                        aria-label={`${region} ${p}: baseline ${formatMs(cell.baseline)}, streaming ${formatMs(cell.streaming)}, ${cell.streamingCount} streaming samples`}
                      >
                        {[0, 1000, 2000, 2500].map((t) => (
                          <line
                            key={t}
                            x1={x(t)}
                            x2={x(t)}
                            y1={0}
                            y2={38}
                            className="atlas-gridline"
                          />
                        ))}
                        <g stroke={providerColors[p]} strokeWidth={2}>
                          <line
                            x1={x(cell.baseline)}
                            x2={x(cell.streaming)}
                            y1={19}
                            y2={19}
                          />
                          <circle
                            cx={x(cell.baseline)}
                            cy={19}
                            r={5}
                            fill="var(--atlas-bg)"
                          />
                          <circle
                            cx={x(cell.streaming)}
                            cy={19}
                            r={5}
                            fill={providerColors[p]}
                          />
                        </g>
                      </svg>
                      <div className="atlas-benchmark-values">
                        {formatMs(cell.baseline)} →{" "}
                        <b>{formatMs(cell.streaming)}</b>
                        <small>{cell.streamingCount} calls · 0 failed</small>
                      </div>
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="atlas-footnote">
        No proven regional-routing speedup: separate time windows, cache
        differences and only five mixed prompts per cell. These client
        measurements are distinct from live server generation TTFT.
      </div>
      <DecisionRibbon
        decisions={decisions}
        historical
        annotations={Object.fromEntries(
          benchmark.jev.rows.map((r, i) => [i, r.arm]),
        )}
      />
    </div>
  );
}
export default function RouterDashboard() {
  const [view, setView] = useState<"live" | "benchmark">("live");
  const [snapshot, setSnapshot] = useState<RouterSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const reload = useCallback(() => setRefresh((n) => n + 1), []);
  useEffect(() => {
    if (view !== "live") return;
    const controller = new AbortController();
    let pending = false;
    async function load() {
      if (pending || document.hidden) return;
      pending = true;
      setBusy(true);
      try {
        const response = await fetch("/api/router", {
          credentials: "same-origin",
          signal: controller.signal,
          cache: "no-store",
        });
        if (!response.ok)
          throw new Error(
            response.status === 401
              ? "Sign in through Connect to view live routing."
              : response.status === 403
                ? "Live routing data is restricted to the platform administrator."
                : "Live telemetry is unavailable. Retry to refresh.",
          );
        const data: RouterSnapshot = await response.json();
        if (
          data.version !== 1 ||
          !Array.isArray(data.providers) ||
          !Array.isArray(data.decisions)
        )
          throw new Error("Unexpected telemetry response.");
        if (!controller.signal.aborted) {
          setSnapshot(data);
          setError(null);
        }
      } catch (failure) {
        if (!controller.signal.aborted) {
          setError(
            failure instanceof Error
              ? failure.message
              : "Live telemetry is unavailable.",
          );
          setSnapshot(null);
        }
      } finally {
        pending = false;
        if (!controller.signal.aborted) setBusy(false);
      }
    }
    void load();
    const tick = setInterval(() => void load(), 30_000);
    const visible = () => {
      if (!document.hidden) void load();
    };
    document.addEventListener("visibilitychange", visible);
    return () => {
      controller.abort();
      clearInterval(tick);
      document.removeEventListener("visibilitychange", visible);
    };
  }, [view, refresh]);
  return (
    <section className="router-dashboard" aria-labelledby="router-title">
      <header className="router-header">
        <div>
          <p className="router-eyebrow">
            <Route size={15} /> INFERENCE OPERATIONS
          </p>
          <h1 id="router-title">Router</h1>
          <p>Supply, speed and reliability. One view.</p>
        </div>
        <a
          className="router-link"
          href={benchmark.source}
          target="_blank"
          rel="noreferrer"
        >
          Benchmark report <ArrowUpRight size={16} />
        </a>
      </header>
      <div className="router-toolbar">
        <div className="router-tabs" role="group" aria-label="Data view">
          <button
            aria-pressed={view === "live"}
            onClick={() => setView("live")}
          >
            <Activity size={15} /> Live atlas
          </button>
          <button
            aria-pressed={view === "benchmark"}
            onClick={() => setView("benchmark")}
          >
            <Clock3 size={15} /> Sep 21 benchmark
          </button>
        </div>
        {view === "live" && (
          <button className="router-refresh" onClick={reload} disabled={busy}>
            <RefreshCw size={15} /> {busy ? "Refreshing…" : "Refresh"}
          </button>
        )}
      </div>
      {view === "live" ? (
        <>
          <p className="router-muted">
            Last {snapshot ? snapshot.windowMs / 3600000 : 2} hours · up to{" "}
            {snapshot?.retentionLimit ?? 512} provider observations and routing
            decisions each · refreshes every 30 seconds while visible.
          </p>
          {error ? (
            <div className="router-notice" role="alert">
              <AlertTriangle size={18} />
              <span>
                {error} <a href="/connect">Connect</a>
              </span>
            </div>
          ) : !snapshot ? (
            <p role="status">Loading live telemetry…</p>
          ) : (
            <>
              <div className="router-meta">
                <span>
                  Updated {new Date(snapshot.capturedAt).toLocaleTimeString()}
                </span>
                <span>
                  Probes{" "}
                  {snapshot.probesEnabled
                    ? `every ${snapshot.probeIntervalMs / 60000} min`
                    : "disabled"}
                </span>
                <span>Provider/model pinned per conversation</span>
              </div>
              <RouterAtlas snapshot={snapshot} />
            </>
          )}
        </>
      ) : (
        <BenchmarkAtlas />
      )}
    </section>
  );
}
