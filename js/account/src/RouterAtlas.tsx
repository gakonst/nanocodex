import { useState } from "react";
import {
  routeProvider,
  summarizeRouter,
  type RouterDecision,
  type RouterProvider,
  type RouterSnapshot,
} from "./routerApi";
import {
  atlasRows,
  formatMs,
  globalTotals,
  isSparse,
  latencyScale,
  providerColors,
  providerNames,
} from "./routerAtlasData";

const pct = (n: number | null) =>
  n === null ? "—" : `${(n * 100).toFixed(1)}%`;
const time = (n: number | null) =>
  n === null ? "—" : new Date(n).toLocaleTimeString();
const human = (s: string) => s.replaceAll("_", " ");
const outcomes: Record<string, string> = {
  accepted: "#3f9b8c",
  low: "#c78a38",
  not_requested: "#8797d7",
};
const outcomeColor = (d: RouterDecision) => outcomes[d.decision] ?? "#d16b6b";
const cohort = (p: RouterProvider) =>
  p.scope === "deployment_global"
    ? "Global"
    : p.scope === "client_ingress"
      ? `Ingress ${p.clientIngressColo ?? "unknown"}`
      : `Execution ${p.workerColo ?? "unknown"}`;
function sampleDescription(p: RouterProvider) {
  return `${p.backend} · ${p.model} · ${p.effort} · ${p.source} · ${cohort(p)}. TTFT median ${formatMs(p.generationTtftP50Ms)}, p95 ${formatMs(p.generationTtftP95Ms)}. ${p.generationTtftSampleCount} TTFT measurements / ${p.sampleCount} attempts; ${p.censoredCount} failed or cancelled. Full response median ${formatMs(p.fullResponseP50Ms)}.${isSparse(p) ? " Sparse TTFT evidence." : ""}`;
}
export function DecisionRibbon({
  decisions,
  historical = false,
  annotations,
}: {
  decisions: RouterDecision[];
  historical?: boolean;
  annotations?: Record<number, string>;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const ordered = [...decisions].sort((a, b) => a.timestamp - b.timestamp);
  const summary = summarizeRouter(decisions);
  const decisionKey = (item: RouterDecision) =>
    `${item.timestamp}:${item.chosen}`;
  const d =
    ordered.find((item) => decisionKey(item) === selected) ??
    [...ordered].reverse().find((item) => item.probabilities !== null) ??
    ordered.at(-1);
  const maxDuration = Math.max(1, ...ordered.map((d) => d.durationMs));
  const counts = ["accepted", "low", "not_requested"].map((name) => ({
    name,
    count: decisions.filter((d) => d.decision === name).length,
  }));
  counts.push({
    name: "unavailable / invalid",
    count: decisions.filter(
      (d) => !["accepted", "low", "not_requested"].includes(d.decision),
    ).length,
  });
  return (
    <div className="atlas-decisions">
      <div className="atlas-section-heading">
        <h3>
          Jev decisions <small>{decisions.length} retained</small>
        </h3>
        <span>
          <b className={summary.bindingFailures ? "atlas-danger" : ""}>
            {summary.bindingFailures}/{summary.attempts}
          </b>{" "}
          binding attempts failed · {summary.recovered} recovered
        </span>
      </div>
      <div className="atlas-decision-key">
        {counts.map((c) => (
          <span key={c.name}>
            <i style={{ background: outcomes[c.name] ?? "#d16b6b" }} />
            <b>{c.count}</b>{" "}
            {c.name === "not_requested" ? "explicit · no Jev" : human(c.name)}
          </span>
        ))}
      </div>
      <div
        className="atlas-ribbon"
        aria-label={
          historical
            ? "Recorded routing decisions; bar height is outer routing duration"
            : "Routing decisions, oldest to newest; bar height is routing duration"
        }
      >
        {ordered.map((item, i) => (
          <button
            key={`${item.timestamp}:${i}`}
            aria-label={`${historical ? `Decision ${i + 1}` : time(item.timestamp)}: ${human(item.decision)}, ${item.chosen}, ${formatMs(item.durationMs)}. Inspect probabilities.`}
            aria-pressed={d === item}
            title={`${item.chosen}\n${human(item.decision)} · ${formatMs(item.durationMs)}`}
            onClick={() => setSelected(decisionKey(item))}
          >
            <i
              style={{
                height: `${(item.durationMs / maxDuration) * 100}%`,
                background: outcomeColor(item),
              }}
            />
          </button>
        ))}
        {!ordered.length && (
          <p>
            No new route decisions. Pinned continuation turns do not call Jev.
          </p>
        )}
      </div>
      {!!ordered.length && (
        <div className="atlas-ribbon-axis">
          <span>
            {historical ? "Recorded order" : "Oldest → newest"} · click a bar
          </span>
          <span>Height: route latency · max {formatMs(maxDuration)}</span>
        </div>
      )}
      {d && (
        <div className="atlas-decision-detail">
          <div className="atlas-choice-heading">
            <strong>{d.chosen}</strong>
            <span>
              {historical ? annotations?.[d.timestamp] : time(d.timestamp)} ·{" "}
              {historical ? "Harness placement" : "Ingress"}{" "}
              {d.clientIngressColo ?? "unknown"} · {human(d.decision)} ·{" "}
              {formatMs(d.durationMs)} · confidence {pct(d.confidence)}
            </span>
          </div>
          <div
            className="atlas-probability-track"
            aria-label="Jev candidate choice probabilities"
          >
            {d.probabilities ? (
              Object.entries(d.probabilities)
                .sort((a, b) => b[1] - a[1])
                .map(([name, value]) => (
                  <div
                    key={name}
                    style={{
                      width: `${value * 100}%`,
                      background:
                        providerColors[routeProvider(name)] ?? "#929292",
                    }}
                    title={`${name}: ${pct(value)}`}
                  >
                    {value >= 0.12 ? pct(value) : ""}
                  </div>
                ))
            ) : (
              <span>
                {d.classifier.outcome === "not_requested"
                  ? "Explicit route · classifier bypassed"
                  : "No valid probabilities returned"}
              </span>
            )}
          </div>
          {d.probabilities && (
            <div className="atlas-probability-list">
              {Object.entries(d.probabilities)
                .sort((a, b) => b[1] - a[1])
                .map(([name, value]) => (
                  <span key={name}>
                    <i
                      style={{
                        background:
                          providerColors[routeProvider(name)] ?? "#929292",
                      }}
                    />
                    <b>{pct(value)}</b> {name}
                  </span>
                ))}
            </div>
          )}
          <p className="router-muted">
            {historical
              ? "Historical outer routing latency shown; individual binding timing was not retained."
              : d.classifier.attempts.length
                ? d.classifier.attempts
                    .map(
                      (a, n) =>
                        `Attempt ${n + 1}: ${human(a.outcome)} (${formatMs(a.duration_ms)})`,
                    )
                    .join(" · ")
                : "No classifier call."}{" "}
            Choice probabilities are preferences, not task success rates.
          </p>
        </div>
      )}
    </div>
  );
}

export function RouterAtlas({ snapshot }: { snapshot: RouterSnapshot }) {
  const [selection, setSelection] = useState<{
    row: string;
    backend: string;
  } | null>(null);
  const rows = atlasRows(snapshot.providers);
  const backends = [
    ...new Set([...providerNames, ...snapshot.providers.map((p) => p.backend)]),
  ];
  const scale = latencyScale(snapshot.providers);
  const routing = summarizeRouter(snapshot.decisions);
  const live = globalTotals(snapshot.providers, "live"),
    probe = globalTotals(snapshot.providers, "probe");
  const active = rows.find((r) => r.key === selection?.row);
  const inspected =
    active?.samples.filter((p) => p.backend === selection?.backend) ?? [];
  return (
    <div className="router-atlas">
      <div className="atlas-topline">
        <div>
          <h2>Inference atlas</h2>
          <p>Every observed model. Every effort. Every provider.</p>
        </div>
        <div className="atlas-totals">
          <span>
            <b>{live.samples}</b> live <em>{live.failed} failed / cancelled</em>
          </span>
          <span>
            <b>{probe.samples}</b> probes{" "}
            <em>{probe.failed} failed / cancelled</em>
          </span>
          <span>
            <b className={routing.bindingFailures ? "atlas-danger" : ""}>
              {routing.bindingFailures}/{routing.attempts}
            </b>{" "}
            Jev failures
            <em>
              {routing.recovered} recovered · {routing.low} low confidence
            </em>
          </span>
          <span>
            <b>{new Set(rows.map((r) => r.model)).size}</b> models{" "}
            <em>{rows.length} effort pairs</em>
          </span>
        </div>
      </div>
      <div className="atlas-legend">
        <span>
          <b>L</b> live · <b>P</b> probe
        </span>
        <span>● median ━┫ p95 TTFT</span>
        <span>Hollow dot = fewer than 3 TTFT samples</span>
        <span className="atlas-danger">Red underline = failure share</span>
        <span>n = TTFT samples / attempts</span>
      </div>
      <div
        className="atlas-scroll"
        role="region"
        aria-label="All model and provider latency matrix; scroll horizontally on small screens"
        tabIndex={0}
      >
        <table className="atlas-matrix">
          <caption>
            Global generation TTFT · shared logarithmic milliseconds axis ·
            lower is faster · click a cell for all regions and error types
          </caption>
          <thead>
            <tr>
              <th scope="col">Model / effort</th>
              {backends.map((backend) => (
                <th scope="col" key={backend}>
                  <span
                    className="atlas-provider"
                    style={{ color: providerColors[backend] }}
                  >
                    {human(backend)}
                  </span>
                  <svg
                    preserveAspectRatio="none"
                    viewBox="0 0 180 22"
                    aria-label="Logarithmic latency scale"
                  >
                    {scale.ticks.map((t) => (
                      <text
                        key={t}
                        x={8 + scale.x(t) * 1.64}
                        y={16}
                        textAnchor={
                          t === 0
                            ? "start"
                            : t === scale.ceiling
                              ? "end"
                              : "middle"
                        }
                      >
                        {t === 0 ? "0" : formatMs(t)}
                      </text>
                    ))}
                  </svg>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, index) => {
              const newModel =
                index === 0 || rows[index - 1].model !== row.model;
              const sources = (["live", "probe"] as const).filter((source) =>
                row.samples.some((p) => p.source === source),
              );
              return (
                <tr
                  key={row.key}
                  className={newModel ? "atlas-model-start" : ""}
                >
                  <th scope="row" aria-label={`${row.model}, ${row.effort}`}>
                    {newModel && <strong>{row.model}</strong>}
                    <small>{row.effort}</small>
                  </th>
                  {backends.map((backend) => {
                    const samples = row.samples.filter(
                      (p) =>
                        p.backend === backend &&
                        p.scope === "deployment_global",
                    );
                    const hasCohorts = row.samples.some(
                      (p) => p.backend === backend,
                    );
                    return (
                      <td key={backend}>
                        <button
                          className="atlas-cell"
                          disabled={!hasCohorts}
                          aria-pressed={
                            selection?.row === row.key &&
                            selection.backend === backend
                          }
                          onClick={() =>
                            setSelection({ row: row.key, backend })
                          }
                          aria-label={`Inspect ${row.model}, ${row.effort}, ${human(backend)}: ${samples.length ? samples.map(sampleDescription).join(" ") : "No global observations"}`}
                        >
                          {sources.map((source) => {
                            const matches = samples.filter(
                              (p) => p.source === source,
                            );
                            return matches.length ? (
                              matches.map((p, i) => (
                                <div
                                  className={`atlas-lane ${p.censoredCount ? "atlas-lane-failed" : ""}`}
                                  key={`${source}:${i}`}
                                  title={sampleDescription(p)}
                                >
                                  <span className="atlas-source">
                                    {source === "live" ? "L" : "P"}
                                  </span>
                                  <div className="atlas-signal">
                                    <svg
                                      preserveAspectRatio="none"
                                      viewBox="0 0 180 16"
                                      aria-hidden="true"
                                    >
                                      {scale.ticks.map((t) => (
                                        <line
                                          key={t}
                                          x1={8 + scale.x(t) * 1.64}
                                          x2={8 + scale.x(t) * 1.64}
                                          y1={0}
                                          y2={16}
                                          className="atlas-gridline"
                                        />
                                      ))}
                                      {p.generationTtftP50Ms !== null && (
                                        <g
                                          stroke={
                                            providerColors[backend] ?? "#929292"
                                          }
                                          strokeWidth={2}
                                        >
                                          <line
                                            x1={
                                              8 +
                                              scale.x(p.generationTtftP50Ms) *
                                                1.64
                                            }
                                            x2={
                                              8 +
                                              scale.x(
                                                p.generationTtftP95Ms ??
                                                  p.generationTtftP50Ms,
                                              ) *
                                                1.64
                                            }
                                            y1={8}
                                            y2={8}
                                          />
                                          {p.generationTtftP95Ms !== null && (
                                            <line
                                              x1={
                                                8 +
                                                scale.x(p.generationTtftP95Ms) *
                                                  1.64
                                              }
                                              x2={
                                                8 +
                                                scale.x(p.generationTtftP95Ms) *
                                                  1.64
                                              }
                                              y1={4}
                                              y2={12}
                                            />
                                          )}
                                          <circle
                                            cx={
                                              8 +
                                              scale.x(p.generationTtftP50Ms) *
                                                1.64
                                            }
                                            cy={8}
                                            r={3}
                                            fill={
                                              isSparse(p)
                                                ? "var(--atlas-bg)"
                                                : (providerColors[backend] ??
                                                  "#929292")
                                            }
                                          />
                                        </g>
                                      )}
                                    </svg>
                                    <span className="atlas-values">
                                      <b>{formatMs(p.generationTtftP50Ms)}</b>
                                      <span>
                                        {" "}
                                        / {formatMs(p.generationTtftP95Ms)}
                                      </span>
                                      <small>
                                        n{p.generationTtftSampleCount}/
                                        {p.sampleCount}
                                        {p.censoredCount > 0 && (
                                          <em> · {p.censoredCount}×</em>
                                        )}
                                      </small>
                                    </span>
                                  </div>
                                  {p.censoredCount > 0 && (
                                    <i
                                      className="atlas-failure"
                                      style={{
                                        width: `${(p.censoredCount / Math.max(1, p.sampleCount)) * 100}%`,
                                      }}
                                    />
                                  )}
                                </div>
                              ))
                            ) : (
                              <div
                                className="atlas-lane atlas-missing"
                                key={source}
                              >
                                <span className="atlas-source">
                                  {source === "live" ? "L" : "P"}
                                </span>
                                <span>— no observations</span>
                              </div>
                            );
                          })}
                        </button>
                      </td>
                    );
                  })}
                </tr>
              );
            })}
          </tbody>
        </table>
        {!rows.length && (
          <p className="router-empty">
            No provider observations in this window. Missing cells do not
            indicate unsupported models.
          </p>
        )}
      </div>
      <div className="atlas-footnote">
        TTFT excludes failures; missing ≠ zero. Values: median / p95. × = failed
        or cancelled. Global totals count each observation once; regional
        cohorts overlap. Empty source lanes are omitted. Model aliases align
        visually; measurements stay separate.
      </div>
      <div className="atlas-inspector" aria-live="polite">
        {active && selection ? (
          <>
            <div className="atlas-section-heading">
              <h3>
                {active.model}{" "}
                <small>
                  {active.effort} · {human(selection.backend)}
                </small>
              </h3>
              <button onClick={() => setSelection(null)}>
                Clear selection
              </button>
            </div>
            <div className="router-table-wrap">
              <table>
                <thead>
                  <tr>
                    {[
                      "Cohort / source",
                      "Model ID",
                      "TTFT p50 / p95",
                      "Full p50",
                      "TTFT / attempts",
                      "Failures",
                      "Last sample / TTFT",
                    ].map((t) => (
                      <th key={t}>{t}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {inspected.map((p, i) => (
                    <tr key={i}>
                      <td>
                        <strong>{cohort(p)}</strong>
                        <small>{p.source}</small>
                      </td>
                      <td>{p.model}</td>
                      <td>
                        {formatMs(p.generationTtftP50Ms)} /{" "}
                        {formatMs(p.generationTtftP95Ms)}
                        {isSparse(p) && <small>Sparse TTFT evidence</small>}
                      </td>
                      <td>{formatMs(p.fullResponseP50Ms)}</td>
                      <td>
                        {p.generationTtftSampleCount} / {p.sampleCount}
                        <small>{p.successCount} completed</small>
                      </td>
                      <td className={p.censoredCount ? "atlas-danger" : ""}>
                        {p.censoredCount}
                        <small>
                          HTTP {p.httpErrorCount} · network{" "}
                          {p.networkErrorCount} · protocol{" "}
                          {p.protocolErrorCount}
                          <br />
                          timeout {p.timeoutCount} · cancelled{" "}
                          {p.cancelledCount}
                        </small>
                      </td>
                      <td>
                        {time(p.lastObservedAt)}
                        <small>TTFT {time(p.lastTtftObservedAt)}</small>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <p className="router-muted">
              Ingress is where the request entered Cloudflare. Execution is
              where the Worker ran. These are overlapping views of the same
              observations, not independent benchmarks.
            </p>
          </>
        ) : (
          <p>
            Click any populated cell to compare all ingress and execution
            cohorts, full-response latency, freshness and error categories.
          </p>
        )}
      </div>
      <DecisionRibbon decisions={snapshot.decisions} />
    </div>
  );
}
