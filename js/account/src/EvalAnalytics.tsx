import { memo, useMemo } from "react";
import {
  Legend,
  ResponsiveContainer,
  Scatter,
  ScatterChart,
  Tooltip,
  XAxis,
  YAxis,
  ZAxis,
} from "recharts";
import type { EvalAnalyticsPoint, EvalResultPoint } from "./evalApi";

type AxisKey = "output" | "latency" | "cost";
type RunAxisKey = "input" | "latency" | "cost";
type ChartPoint = {
  x: number;
  score: number;
  effort: string;
  harness: string;
  model: string;
  passed: number;
  completed: number;
  sample: number;
};
type Series = {
  key: string;
  name: string;
  harness: string;
  values: ChartPoint[];
};
type RunChartPoint = {
  id: string;
  repetition: number;
  outputTokens: number;
  value: number;
  harness: string;
  model: string;
  thinking: string;
  result: string;
  passed: boolean;
};
type RunSeries = {
  key: string;
  name: string;
  color: string;
  passed: boolean;
  values: RunChartPoint[];
};
type RunLegendItem = {
  key: string;
  label: string;
  color: string;
};

const effortOrder = ["low", "medium", "high", "xhigh"];
const palette = [
  "var(--eval-series-1)",
  "var(--eval-series-2)",
  "var(--eval-series-3)",
  "var(--eval-series-4)",
  "var(--eval-series-5)",
  "var(--eval-series-6)",
];
const harnessColors: Record<string, string> = {
  codex: "var(--eval-codex)",
  nanocodex: "var(--eval-nanocodex)",
};
const runColors: Record<string, Record<string, string>> = {
  codex: {
    low: "var(--eval-codex-low)",
    medium: "var(--eval-codex-medium)",
    high: "var(--eval-codex-high)",
    xhigh: "var(--eval-codex-xhigh)",
  },
  nanocodex: {
    low: "var(--eval-nanocodex-low)",
    medium: "var(--eval-nanocodex-medium)",
    high: "var(--eval-nanocodex-high)",
    xhigh: "var(--eval-nanocodex-xhigh)",
  },
};

function paletteColor(key: string) {
  let hash = 2166136261;
  for (let index = 0; index < key.length; index++) {
    hash ^= key.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return palette[(hash >>> 0) % palette.length];
}

function harnessLabel(harness: string) {
  if (harness === "codex") return "Codex";
  if (harness === "nanocodex") return "Nanocodex";
  return harness;
}

function runColor(harness: string, thinking: string) {
  return runColors[harness]?.[thinking] ?? harnessColors[harness] ?? paletteColor(`${harness}\u0000${thinking}`);
}

function effortRank(effort: string) {
  const rank = effortOrder.indexOf(effort);
  return rank < 0 ? effortOrder.length : rank;
}

const metrics = {
  output: {
    title: "Score by output tokens",
    label: "Median output tokens",
    runTitle: "Output tokens by run",
    runLabel: "Output tokens",
    value: (point: EvalResultPoint) => point.outputTokens,
    tick: (value: number) => Intl.NumberFormat(undefined, { notation: "compact" }).format(value),
  },
  latency: {
    title: "Score by agent execution time",
    label: "Median agent execution time",
    runTitle: "Agent execution time by run",
    runLabel: "Agent execution time",
    value: (point: EvalResultPoint) => point.durationMs,
    tick: (value: number) => value < 60_000 ? `${Math.round(value / 1_000)}s` : `${(value / 60_000).toFixed(1)}m`,
  },
  cost: {
    title: "Score by cost",
    label: "Median estimated cost",
    runTitle: "Cost by run",
    runLabel: "Estimated cost",
    value: (point: EvalResultPoint) => point.costUsd,
    tick: (value: number) => `$${value < 0.01 ? value.toFixed(3) : value.toFixed(2)}`,
  },
} satisfies Record<AxisKey, {
  title: string;
  label: string;
  runTitle: string;
  runLabel: string;
  value: (point: EvalResultPoint) => number | null;
  tick: (value: number) => string;
}>;

const analyticsMetrics = {
  output: {
    value: (point: EvalAnalyticsPoint) => point.medianOutputTokens,
    sample: (point: EvalAnalyticsPoint) => point.outputSamples,
  },
  latency: {
    value: (point: EvalAnalyticsPoint) => point.medianDurationMs,
    sample: (point: EvalAnalyticsPoint) => point.durationSamples,
  },
  cost: {
    value: (point: EvalAnalyticsPoint) => point.medianCostUsd,
    sample: (point: EvalAnalyticsPoint) => point.costSamples,
  },
} satisfies Record<AxisKey, {
  value: (point: EvalAnalyticsPoint) => number | null;
  sample: (point: EvalAnalyticsPoint) => number;
}>;

const runMetrics = {
  input: {
    title: "Input vs output tokens",
    label: "Input tokens",
    value: (point: EvalResultPoint) => point.inputTokens,
    tick: (value: number) => Intl.NumberFormat(undefined, { notation: "compact" }).format(value),
  },
  latency: {
    title: "Agent execution time vs output tokens",
    label: "Agent execution time",
    value: (point: EvalResultPoint) => point.durationMs,
    tick: metrics.latency.tick,
  },
  cost: {
    title: "Cost vs output tokens",
    label: "Estimated cost",
    value: (point: EvalResultPoint) => point.costUsd,
    tick: metrics.cost.tick,
  },
} satisfies Record<RunAxisKey, {
  title: string;
  label: string;
  value: (point: EvalResultPoint) => number | null;
  tick: (value: number) => string;
}>;

function passed(point: EvalResultPoint) {
  if (point.status !== null) return point.status === "passed";
  if (point.outcome !== null) return point.outcome === "passed";
  return point.state === "success";
}

function seriesFor(points: EvalAnalyticsPoint[], axisKey: AxisKey): Series[] {
  const axis = analyticsMetrics[axisKey];
  const lines = new Map<string, ChartPoint[]>();
  for (const point of points) {
    const x = axis.value(point);
    if (x === null) continue;
    const key = `${point.harness}\u0000${point.model}`;
    const line = lines.get(key) ?? [];
    line.push({
      x,
      score: point.completed > 0 ? point.passed / point.completed * 100 : 0,
      effort: point.thinking,
      harness: point.harness,
      model: point.model,
      passed: point.passed,
      completed: point.completed,
      sample: axis.sample(point),
    });
    lines.set(key, line);
  }
  return [...lines.entries()].map(([key, values]) => ({
    key,
    name: key.split("\u0000").join(" · "),
    harness: values[0]?.harness ?? "",
    values: values.sort((left, right) => {
      const leftRank = effortOrder.indexOf(left.effort);
      const rightRank = effortOrder.indexOf(right.effort);
      return (leftRank < 0 ? 99 : leftRank) - (rightRank < 0 ? 99 : rightRank);
    }),
  }));
}

function runSeriesFor(points: EvalResultPoint[], axisKey: RunAxisKey): RunSeries[] {
  const axis = runMetrics[axisKey];
  const grouped = new Map<string, RunSeries>();
  for (const point of points) {
    const value = axis.value(point);
    if (value === null || point.outputTokens === null) continue;
    const didPass = passed(point);
    const key = `${point.harness}\u0000${point.thinking}\u0000${didPass ? "passed" : "failed"}`;
    const series = grouped.get(key) ?? {
      key,
      name: `${harnessLabel(point.harness)} · ${point.thinking} · ${didPass ? "passed" : "failed"}`,
      color: runColor(point.harness, point.thinking),
      passed: didPass,
      values: [],
    };
    series.values.push({
      id: point.id,
      repetition: point.repetition,
      outputTokens: point.outputTokens,
      value,
      harness: point.harness,
      model: point.model,
      thinking: point.thinking,
      result: point.status ?? point.outcome ?? point.state,
      passed: didPass,
    });
    grouped.set(key, series);
  }
  return [...grouped.values()].sort((left, right) =>
    left.name.localeCompare(right.name),
  );
}

function runLegendFor(points: EvalResultPoint[]): RunLegendItem[] {
  const items = new Map<string, RunLegendItem>();
  for (const point of points) {
    const key = `${point.harness}\u0000${point.thinking}`;
    if (!items.has(key)) {
      items.set(key, {
        key,
        label: `${harnessLabel(point.harness)} · ${point.thinking}`,
        color: runColor(point.harness, point.thinking),
      });
    }
  }
  return [...items.values()].sort((left, right) => {
    const [leftHarness, leftEffort] = left.key.split("\u0000");
    const [rightHarness, rightEffort] = right.key.split("\u0000");
    return leftHarness.localeCompare(rightHarness)
      || effortRank(leftEffort) - effortRank(rightEffort)
      || leftEffort.localeCompare(rightEffort);
  });
}

function ChartTooltip({
  active,
  payload,
  axis,
}: {
  active?: boolean;
  payload?: ReadonlyArray<{ payload?: unknown }>;
  axis: (typeof metrics)[AxisKey];
}) {
  if (!active || !payload?.length) return null;
  const point = payload[0]?.payload as ChartPoint | undefined;
  if (!point) return null;
  return (
    <div className="eval-chart-tooltip">
      <strong>{harnessLabel(point.harness)} · {point.model}</strong>
      <span>{point.effort} thinking</span>
      <span>{Math.round(point.score)}% · {point.passed}/{point.completed} passed</span>
      <span>{axis.label}: {axis.tick(point.x)} · median of {point.sample}</span>
    </div>
  );
}

function FrontierChart({ points, axisKey }: { points: EvalAnalyticsPoint[]; axisKey: AxisKey }) {
  const axis = metrics[axisKey];
  const series = useMemo(() => seriesFor(points, axisKey), [axisKey, points]);
  return (
    <article className="eval-frontier">
      <header><h3>{axis.title}</h3><p>Thinking effort increases along each line.</p></header>
      {series.length ? (
        <div className="eval-chart-canvas">
          <ResponsiveContainer width="100%" height="100%">
            <ScatterChart margin={{ top: 12, right: 24, bottom: 46, left: 18 }} accessibilityLayer>
              <XAxis
                type="number"
                dataKey="x"
                domain={[0, "auto"]}
                tickFormatter={axis.tick}
                label={{ value: axis.label, position: "insideBottom", offset: -32 }}
              />
              <YAxis
                type="number"
                dataKey="score"
                domain={[0, 100]}
                width={48}
                tickFormatter={(value) => `${value}%`}
              />
              <Tooltip content={({ active, payload }) => (
                <ChartTooltip active={active} payload={payload} axis={axis} />
              )} />
              <Legend verticalAlign="top" align="right" iconType="line" />
              {series.map((line) => {
                const color = paletteColor(line.key);
                return (
                  <Scatter
                    key={line.key}
                    name={line.name}
                    data={line.values}
                    fill={color}
                    line={{ stroke: color, strokeWidth: 2.5 }}
                    isAnimationActive={false}
                  />
                );
              })}
            </ScatterChart>
          </ResponsiveContainer>
        </div>
      ) : (
        <div className="eval-chart-empty">
          <strong>No retained {axis.label.toLowerCase()} points yet.</strong>
          <span>This chart fills as completed runs are indexed.</span>
        </div>
      )}
    </article>
  );
}

function RunTooltip({
  active,
  payload,
  axis,
}: {
  active?: boolean;
  payload?: ReadonlyArray<{ payload?: unknown }>;
  axis: (typeof runMetrics)[RunAxisKey];
}) {
  if (!active || !payload?.length) return null;
  const point = payload[0]?.payload as RunChartPoint | undefined;
  if (!point) return null;
  return (
    <div className="eval-chart-tooltip">
      <strong>{point.harness} · {point.model}</strong>
      <span>{point.thinking} thinking · k={point.repetition}</span>
      <span>{point.passed ? "○ passed" : "× failed"} · {point.result}</span>
      <span>Output tokens: {metrics.output.tick(point.outputTokens)}</span>
      <span>{axis.label}: {axis.tick(point.value)}</span>
    </div>
  );
}

function RunChart({ points, axisKey }: { points: EvalResultPoint[]; axisKey: RunAxisKey }) {
  const axis = runMetrics[axisKey];
  const series = useMemo(() => runSeriesFor(points, axisKey), [axisKey, points]);
  return (
    <article className="eval-frontier eval-run-chart">
      <header><h3>{axis.title}</h3><p>One mark per retained treatment run.</p></header>
      {series.length ? (
        <div className="eval-chart-canvas">
          <ResponsiveContainer width="100%" height="100%">
            <ScatterChart margin={{ top: 12, right: 24, bottom: 46, left: 24 }} accessibilityLayer>
              <XAxis
                type="number"
                dataKey="outputTokens"
                domain={[0, "auto"]}
                tickFormatter={metrics.output.tick}
                label={{ value: "Output tokens", position: "insideBottom", offset: -32 }}
              />
              <YAxis
                type="number"
                dataKey="value"
                domain={[0, "auto"]}
                width={64}
                tickFormatter={axis.tick}
              />
              <ZAxis range={[70, 70]} />
              <Tooltip content={({ active, payload }) => (
                <RunTooltip active={active} payload={payload} axis={axis} />
              )} />
              {series.map((runSeries) => (
                <Scatter
                  key={runSeries.key}
                  name={runSeries.name}
                  data={runSeries.values}
                  fill={runSeries.color}
                  isAnimationActive={false}
                  shape={({ cx, cy }) => runSeries.passed ? (
                    <circle cx={cx} cy={cy} r={5} fill="none" stroke={runSeries.color} strokeWidth={2} />
                  ) : (
                    <g stroke={runSeries.color} strokeWidth={2} strokeLinecap="round">
                      <line x1={(cx ?? 0) - 5} y1={(cy ?? 0) - 5} x2={(cx ?? 0) + 5} y2={(cy ?? 0) + 5} />
                      <line x1={(cx ?? 0) + 5} y1={(cy ?? 0) - 5} x2={(cx ?? 0) - 5} y2={(cy ?? 0) + 5} />
                    </g>
                  )}
                />
              ))}
            </ScatterChart>
          </ResponsiveContainer>
        </div>
      ) : (
        <div className="eval-chart-empty">
          <strong>No retained {axis.label.toLowerCase()} points yet.</strong>
          <span>This chart fills as this task completes runs.</span>
        </div>
      )}
    </article>
  );
}

export const EvalAnalytics = memo(function EvalAnalytics({
  points,
  view = "frontier",
  taskCount = 0,
}: {
  points: EvalResultPoint[] | EvalAnalyticsPoint[];
  view?: "frontier" | "runs";
  taskCount?: number;
}) {
  const runView = view === "runs";
  const runPoints = runView ? points as EvalResultPoint[] : [];
  const frontierPoints = runView ? [] : points as EvalAnalyticsPoint[];
  const passedCount = runView
    ? runPoints.filter(passed).length
    : frontierPoints.reduce((total, point) => total + point.passed, 0);
  const completedCount = runView
    ? runPoints.length
    : frontierPoints.reduce((total, point) => total + point.completed, 0);
  const runLegend = useMemo(() => runLegendFor(runPoints), [runPoints]);
  return (
    <section className="eval-artifact" aria-labelledby="eval-artifact-title">
      <header className="eval-artifact-head">
        <div>
          <p className="rail-label">{runView ? "Task evidence" : "Benchmark artifact"}</p>
          <h2 id="eval-artifact-title">{runView ? "Runs" : "Score frontiers"}</h2>
          <p>{runView
            ? "Actual retained runs positioned by output-token volume. Circle means pass; × means failure."
            : "Codex and Nanocodex across model and thinking effort, using medians over retained repetitions."}</p>
          {runView ? (
            <div className="eval-run-legend" role="group" aria-label="Run chart legend">
              {runLegend.map((item) => (
                <span key={item.key}><i style={{ backgroundColor: item.color }} /> {item.label}</span>
              ))}
              <span><b>○</b> pass</span>
              <span><b>×</b> failure</span>
            </div>
          ) : null}
        </div>
        <dl>
          <div><dt>Runs</dt><dd>{completedCount}</dd></div>
          <div><dt>{runView ? "Failed" : "Tasks"}</dt><dd>{runView ? completedCount - passedCount : taskCount}</dd></div>
          <div><dt>Passed</dt><dd>{passedCount}</dd></div>
        </dl>
      </header>
      <div className="eval-chart-grid">
        {runView ? (
          <>
            <RunChart points={runPoints} axisKey="input" />
            <RunChart points={runPoints} axisKey="latency" />
            <RunChart points={runPoints} axisKey="cost" />
          </>
        ) : (
          <>
            <FrontierChart points={frontierPoints} axisKey="output" />
            <FrontierChart points={frontierPoints} axisKey="latency" />
            <FrontierChart points={frontierPoints} axisKey="cost" />
          </>
        )}
      </div>
    </section>
  );
});
