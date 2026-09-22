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
  thinkingPoints,
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
  source,
  scale,
  selectedRow,
  onSelect,
}: {
  rows: ReturnType<typeof atlasRows>;
  backend: string;
  source: "live" | "probe";
  scale: ReturnType<typeof latencyScale>;
  selectedRow?: string;
  onSelect: (row: string) => void;
}) {
  const points = thinkingPoints(rows, backend, source);
  const x = (ms: number) => 14 + scale.x(ms) * 5.72;
  return (
    <td className="atlas-thinking-chart">
      <svg
        viewBox="0 0 600 44"
        preserveAspectRatio="none"
        role="group"
        aria-label={`${rows[0].model}, ${human(backend)}: one line, one median TTFT point per thinking level`}
        onClick={() => onSelect(rows[0].key)}
      >
        <line
          x1={14}
          x2={586}
          y1={22}
          y2={22}
          className="atlas-provider-line"
        />
        {scale.ticks.map((t) => (
          <line
            key={t}
            x1={x(t)}
            x2={x(t)}
            y1={17}
            y2={27}
            className="atlas-gridline"
          />
        ))}
        {points.map(({ row, sample: p }) =>
          p?.generationTtftP50Ms !== null &&
          p?.generationTtftP50Ms !== undefined ? (
            <g
              key={row.key}
              role="button"
              tabIndex={0}
              aria-label={sampleDescription(p)}
              aria-pressed={selectedRow === row.key}
              className="atlas-thinking-mark"
              onClick={(e) => {
                e.stopPropagation();
                onSelect(row.key);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  e.stopPropagation();
                  onSelect(row.key);
                }
              }}
            >
              <title>{sampleDescription(p)}</title>
              <path
                d={`M ${x(p.generationTtftP50Ms)} 22 h 0`}
                stroke="var(--atlas-bg)"
                strokeWidth={13}
                strokeLinecap="round"
              />
              <path
                d={`M ${x(p.generationTtftP50Ms)} 22 h 0`}
                stroke={thinkingColors[row.effort] ?? "#549ac5"}
                strokeWidth={9}
                strokeLinecap="round"
              />
              {isSparse(p) && (
                <path
                  d={`M ${x(p.generationTtftP50Ms)} 22 h 0`}
                  stroke="var(--atlas-bg)"
                  strokeWidth={4}
                  strokeLinecap="round"
                />
              )}
            </g>
          ) : null,
        )}
      </svg>
    </td>
  );
}

export function RouterAtlas({ snapshot }: { snapshot: RouterSnapshot }) {
  const [selection, setSelection] = useState<{
    row: string;
    backend: string;
  } | null>(null);
  const [source, setSource] = useState<"live" | "probe">("probe");
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
    active?.samples.filter(
      (p) => p.backend === selection?.backend && p.source === source,
    ) ?? [];
  const selectedModel = models.find((group) => group.model === active?.model);
  return (
    <div className="router-atlas">
      <div className="atlas-topline">
        <div>
          <h2>Inference atlas</h2>
          <p>One line per provider · median TTFT by thinking level.</p>
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
        <div
          className="atlas-source-switch"
          role="group"
          aria-label="Measurement source"
        >
          {(["probe", "live"] as const).map((value) => (
            <button
              key={value}
              aria-pressed={source === value}
              onClick={() => {
                setSource(value);
                setSelection(null);
              }}
            >
              {value === "probe" ? "Probes" : "Live"}
            </button>
          ))}
        </div>
        {efforts.map((e) => (
          <span key={e}>
            <i style={{ background: thinkingColors[e] ?? "#549ac5" }} />
            {e}
          </span>
        ))}
        <span className="atlas-sparse-key">○ sparse</span>
      </div>
      <div
        className="atlas-scroll"
        role="region"
        aria-label="Median TTFT by model and provider, one line per provider"
        tabIndex={0}
      >
        <table className="atlas-matrix atlas-by-model atlas-thinking-matrix">
          <caption>
            {source === "probe" ? "Synthetic probes" : "Live traffic"} · median
            TTFT · log axis · tap a provider for details
          </caption>
          <thead>
            <tr>
              <th scope="col">Provider</th>
              <th scope="col">
                <div
                  className="atlas-thinking-axis"
                  role="img"
                  aria-label="Shared logarithmic milliseconds axis"
                >
                  {scale.ticks
                    .filter((t) => t === 0 || t >= 100)
                    .map((t) => (
                      <span
                        key={t}
                        style={{
                          left: `${2.333333 + scale.x(t) * 0.9533333}%`,
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
            </tr>
          </thead>
          {models.map((group) => (
            <tbody
              key={group.model}
              aria-label={`${group.model} provider comparisons`}
            >
              <tr className="atlas-model-heading">
                <th colSpan={2} scope="rowgroup">
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
                .map((backend) => {
                  const points = thinkingPoints(group.rows, backend, source);
                  const samples = points.flatMap((p) => p.matches);
                  const failures = samples.reduce(
                    (n, p) => n + p.censoredCount,
                    0,
                  );
                  const hasPoint = points.some(
                    (p) => p.sample?.generationTtftP50Ms != null,
                  );
                  return (
                    <tr key={backend}>
                      <th scope="row">
                        <button
                          className="atlas-provider-button"
                          aria-label={`Inspect ${group.model}, ${human(backend)}, all thinking levels`}
                          onClick={() =>
                            setSelection({ row: group.rows[0].key, backend })
                          }
                        >
                          <span>{human(backend)}</span>
                          {failures > 0 ? (
                            <small className="atlas-danger">{failures}×</small>
                          ) : !hasPoint ? (
                            <small>—</small>
                          ) : null}
                        </button>
                      </th>
                      <ProviderThinkingPlot
                        rows={group.rows}
                        backend={backend}
                        source={source}
                        scale={scale}
                        selectedRow={
                          selection?.backend === backend
                            ? selection.row
                            : undefined
                        }
                        onSelect={(row) => setSelection({ row, backend })}
                      />
                    </tr>
                  );
                })}
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
        One dot per thinking level · hollow = fewer than 3 TTFT samples · × =
        failed/cancelled. Tap a provider for overlapping dots, exact values and
        p95. Missing or split-cohort values are not plotted.
      </div>
      {active && selection && selectedModel && (
        <div
          className="atlas-inspector atlas-detail-sheet"
          role="region"
          aria-label="Selected provider details"
          onKeyDown={(e) => {
            if (e.key === "Escape") setSelection(null);
          }}
        >
          <div className="atlas-section-heading">
            <h3>
              {active.model}
              <small>
                {human(selection.backend)} ·{" "}
                {source === "probe" ? "probes" : "live"}
              </small>
            </h3>
            <button
              onClick={() => setSelection(null)}
              aria-label="Close provider details"
            >
              Close
            </button>
          </div>
          <div
            className="atlas-effort-picker"
            role="group"
            aria-label="Thinking level details"
          >
            {thinkingPoints(selectedModel.rows, selection.backend, source).map(
              ({ row, sample, matches }) => (
                <button
                  key={row.key}
                  aria-pressed={row.key === active.key}
                  onClick={() =>
                    setSelection({ row: row.key, backend: selection.backend })
                  }
                >
                  <i
                    style={{
                      background: thinkingColors[row.effort] ?? "#549ac5",
                    }}
                  />
                  {row.effort}
                  <strong>
                    {sample
                      ? formatMs(sample.generationTtftP50Ms)
                      : matches.length > 1
                        ? "split cohorts"
                        : "—"}
                  </strong>
                </button>
              ),
            )}
          </div>
          <div className="atlas-cohort-cards">
            {inspected.map((p, i) => (
              <article key={i}>
                <h4>
                  {cohort(p)}
                  <small>{active.effort}</small>
                </h4>
                <dl>
                  <div>
                    <dt>TTFT p50 / p95</dt>
                    <dd>
                      {formatMs(p.generationTtftP50Ms)} /{" "}
                      {formatMs(p.generationTtftP95Ms)}
                    </dd>
                  </div>
                  <div>
                    <dt>Full response p50</dt>
                    <dd>{formatMs(p.fullResponseP50Ms)}</dd>
                  </div>
                  <div>
                    <dt>TTFT / attempts</dt>
                    <dd>
                      {p.generationTtftSampleCount} / {p.sampleCount}
                      {isSparse(p) ? " · sparse" : ""}
                    </dd>
                  </div>
                  <div>
                    <dt>Failed / cancelled</dt>
                    <dd className={p.censoredCount ? "atlas-danger" : ""}>
                      {p.censoredCount}
                    </dd>
                  </div>
                  <div>
                    <dt>Last sample</dt>
                    <dd>{time(p.lastObservedAt)}</dd>
                  </div>
                  <div>
                    <dt>Last TTFT</dt>
                    <dd>{time(p.lastTtftObservedAt)}</dd>
                  </div>
                </dl>
                <p>
                  HTTP {p.httpErrorCount} · network {p.networkErrorCount} ·
                  protocol {p.protocolErrorCount} · timeout {p.timeoutCount} ·
                  cancelled {p.cancelledCount}
                </p>
                <small className="atlas-model-id">{p.model}</small>
              </article>
            ))}
          </div>
          {!inspected.length && (
            <p className="router-muted">
              No {source} observations for this provider and thinking level.
            </p>
          )}
          <p className="router-muted">
            Global and regional cohorts overlap; counts are not added together.
            Ingress and execution locations differ. Failures are excluded from
            latency percentiles.
          </p>
        </div>
      )}
      <DecisionRibbon decisions={snapshot.decisions} />
    </div>
  );
}
