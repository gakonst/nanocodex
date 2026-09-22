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

const thinkingColors: Record<string, string> = {
  none: "#87949e",
  minimal: "#549ac5",
  low: "#3f9b8c",
  medium: "#c78a38",
  high: "#9d79c2",
  xhigh: "#c56d9a",
  max: "#d16b6b",
};
function ProviderThinkingPlot({
  rows,
  backend,
  scale,
  selectedRow,
  onSelect,
}: {
  rows: ReturnType<typeof atlasRows>;
  backend: string;
  scale: ReturnType<typeof latencyScale>;
  selectedRow?: string;
  onSelect: (row: string) => void;
}) {
  const entries = rows.flatMap<{
    row: (typeof rows)[number];
    sample: RouterProvider | null;
  }>((row) => {
    const samples = row.samples
      .filter((p) => p.backend === backend && p.scope === "deployment_global")
      .sort((a, b) => a.source.localeCompare(b.source));
    return samples.length
      ? samples.map((sample) => ({ row, sample }))
      : [{ row, sample: null }];
  });
  const step = 23,
    height = entries.length * step;
  const x = (ms: number) => 8 + scale.x(ms) * 5.84;
  return (
    <>
      <td className="atlas-thinking-chart">
        <svg
          viewBox={`0 0 600 ${height}`}
          preserveAspectRatio="none"
          style={{ height }}
          role="group"
          aria-label={`${rows[0].model}, ${human(backend)}: all thinking levels on one TTFT plot`}
        >
          {scale.ticks.map((t) => (
            <line
              key={t}
              x1={x(t)}
              x2={x(t)}
              y1={0}
              y2={height}
              className="atlas-gridline"
            />
          ))}
          {entries.map(({ row, sample: p }, i) => {
            const y = i * step + step / 2,
              color = thinkingColors[row.effort] ?? "#549ac5";
            if (!p) return null;
            return (
              <g
                key={`${row.key}:${p.source}:${i}`}
                role="button"
                tabIndex={0}
                aria-label={sampleDescription(p)}
                aria-pressed={selectedRow === row.key}
                onClick={() => onSelect(row.key)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    onSelect(row.key);
                  }
                }}
                className="atlas-thinking-mark"
              >
                <title>{sampleDescription(p)}</title>
                <rect
                  x={0}
                  y={i * step}
                  width={600}
                  height={step}
                  fill={p.censoredCount ? "#d16b6b" : "currentColor"}
                  opacity={
                    p.censoredCount ? 0.09 : selectedRow === row.key ? 0.06 : 0
                  }
                />
                {p.generationTtftP50Ms !== null && (
                  <g stroke={color} strokeWidth={2}>
                    <line
                      x1={x(p.generationTtftP50Ms)}
                      x2={x(p.generationTtftP95Ms ?? p.generationTtftP50Ms)}
                      y1={y}
                      y2={y}
                      strokeDasharray={p.source === "probe" ? "4 3" : undefined}
                    />
                    {p.generationTtftP95Ms !== null && (
                      <line
                        x1={x(p.generationTtftP95Ms)}
                        x2={x(p.generationTtftP95Ms)}
                        y1={y - 4}
                        y2={y + 4}
                      />
                    )}
                    <path
                      d={`M ${x(p.generationTtftP50Ms)} ${y} h 0`}
                      strokeWidth={7}
                      strokeLinecap={p.source === "live" ? "round" : "square"}
                    />
                    {isSparse(p) && (
                      <path
                        d={`M ${x(p.generationTtftP50Ms)} ${y} h 0`}
                        stroke="var(--atlas-bg)"
                        strokeWidth={3}
                        strokeLinecap={p.source === "live" ? "round" : "square"}
                      />
                    )}
                  </g>
                )}
              </g>
            );
          })}
        </svg>
      </td>
      <td className="atlas-thinking-readings">
        {entries.map(({ row, sample: p }, i) => (
          <button
            key={`${row.key}:${i}`}
            disabled={!row.samples.some((p) => p.backend === backend)}
            onClick={() => onSelect(row.key)}
            aria-pressed={selectedRow === row.key}
            aria-label={
              p
                ? `Inspect ${sampleDescription(p)}`
                : `${row.effort}: no global observations`
            }
            className={p?.censoredCount ? "atlas-reading-failed" : ""}
            style={{ height: step }}
          >
            <span
              className="atlas-thinking-level"
              style={{ color: thinkingColors[row.effort] ?? "#549ac5" }}
            >
              {row.effort}
              <small>{p ? (p.source === "live" ? "L" : "P") : ""}</small>
            </span>
            {p ? (
              <>
                <span>
                  <b>{formatMs(p.generationTtftP50Ms)}</b> /{" "}
                  {formatMs(p.generationTtftP95Ms)}
                </span>
                <small>
                  n{p.generationTtftSampleCount}/{p.sampleCount}
                  {p.censoredCount > 0 && <em> · {p.censoredCount}×</em>}
                </small>
              </>
            ) : (
              <span className="atlas-unknown">— no observations</span>
            )}
          </button>
        ))}
      </td>
    </>
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
  const models = [...new Set(rows.map((row) => row.model))].map((model) => ({
    model,
    rows: rows.filter((row) => row.model === model),
  }));
  const effortOrder = [
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
  ];
  const efforts = [...new Set(rows.map((row) => row.effort))].sort(
    (a, b) =>
      effortOrder.indexOf(a) - effortOrder.indexOf(b) || a.localeCompare(b),
  );
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
          <p>
            Grouped by model. All thinking levels share one plot per provider.
          </p>
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
      <div className="atlas-legend atlas-thinking-legend">
        {efforts.map((e) => (
          <span key={e}>
            <i style={{ background: thinkingColors[e] ?? "#549ac5" }} />
            {e}
          </span>
        ))}
        <span>● live · ■ probe · marker p50 ━┫ p95</span>
        <span>Hollow = fewer than 3 TTFT samples</span>
        <span className="atlas-danger">Red = failed or cancelled</span>
        <span>n = TTFT samples / attempts</span>
      </div>
      <div
        className="atlas-scroll"
        role="region"
        aria-label="Models and providers with all thinking levels on each TTFT plot; scroll horizontally on small screens"
        tabIndex={0}
      >
        <table className="atlas-matrix atlas-by-model atlas-thinking-matrix">
          <caption>
            Global generation TTFT · shared logarithmic milliseconds axis ·
            lower is faster · click a marker or reading for all regions and
            error types
          </caption>
          <thead>
            <tr>
              <th scope="col">Provider</th>
              <th scope="col">
                <span className="atlas-effort">TTFT · all thinking levels</span>
                <div
                  className="atlas-thinking-axis"
                  role="img"
                  aria-label="Shared logarithmic milliseconds axis"
                >
                  {scale.ticks.map((t) => (
                    <span
                      key={t}
                      style={{
                        left: `${1.333333 + scale.x(t) * 0.9733333}%`,
                        transform:
                          t === 0
                            ? "none"
                            : t === scale.ceiling
                              ? "translateX(-100%)"
                              : "translateX(-50%)",
                      }}
                    >
                      {t === 0 ? "0" : formatMs(t)}
                    </span>
                  ))}
                </div>
              </th>
              <th scope="col">Thinking · source · p50 / p95 · samples</th>
            </tr>
          </thead>
          {models.map((group) => (
            <tbody
              key={group.model}
              aria-label={`${group.model} provider comparisons`}
            >
              <tr className="atlas-model-heading">
                <th colSpan={3} scope="rowgroup">
                  <strong>{group.model}</strong>
                  <span>
                    {group.rows.length} thinking levels · same axis for every
                    provider
                  </span>
                </th>
              </tr>
              {backends
                .filter((backend) =>
                  group.rows.some((row) =>
                    row.samples.some((p) => p.backend === backend),
                  ),
                )
                .map((backend) => (
                  <tr key={backend}>
                    <th
                      scope="row"
                      aria-label={`${group.model}, ${human(backend)}`}
                    >
                      <strong
                        className="atlas-provider"
                        style={{ color: providerColors[backend] }}
                      >
                        {human(backend)}
                      </strong>
                    </th>
                    <ProviderThinkingPlot
                      rows={group.rows}
                      backend={backend}
                      scale={scale}
                      selectedRow={
                        selection?.backend === backend
                          ? selection.row
                          : undefined
                      }
                      onSelect={(row) => setSelection({ row, backend })}
                    />
                  </tr>
                ))}
            </tbody>
          ))}
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
            Click a marker or reading to compare all ingress and execution
            cohorts, full-response latency, freshness and error categories.
          </p>
        )}
      </div>
      <DecisionRibbon decisions={snapshot.decisions} />
    </div>
  );
}
