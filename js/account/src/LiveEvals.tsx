import {
  AlertTriangle,
  ChevronLeft,
  ChevronRight,
  Cpu,
  MemoryStick,
  Radio,
  Search,
  Server,
  X,
} from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { startTransition, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { EvalAnalytics } from "./EvalAnalytics";
import {
  evalApi,
  type EvalAnalyticsPoint,
  type EvalCase,
  type EvalCluster,
  type EvalClusterCapacity,
  type EvalClusterNode,
  type EvalCoordinate,
  type EvalOverview,
  type EvalResultPoint,
  type EvalSummary,
  type EvalTask,
  type EvalTaskOverview,
  type EvalTaskSnapshot,
  type EvalTreatment,
  type EvalWorksetAnalytics,
  type EvalWorksetDetail,
} from "./evalApi";
import "./evals.css";

type MatrixFilter = "all" | "active" | "issues" | "complete";
type AnalyticsView = "frontier" | "runs";

const initialTaskRows = 50;

function formatDuration(milliseconds: number | null) {
  if (milliseconds === null) return "—";
  if (milliseconds < 1_000) return `${Math.round(milliseconds)}ms`;
  const seconds = milliseconds / 1_000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
}

function formatWorksetDate(milliseconds: number) {
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(milliseconds));
}

function formatObservedAt(milliseconds: number) {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(milliseconds));
}

function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** exponent).toFixed(exponent < 2 ? 0 : 1)} ${units[exponent]}`;
}

function formatUptime(seconds: number) {
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  return days > 0 ? `${days}d ${hours}h` : `${hours}h ${Math.floor((seconds % 3_600) / 60)}m`;
}

function usedPercent(capacity: EvalClusterCapacity) {
  if (capacity.totalBytes <= 0) return 0;
  return Math.max(0, Math.min(100,
    ((capacity.totalBytes - capacity.availableBytes) / capacity.totalBytes) * 100,
  ));
}

function formatPressure(value: number | null) {
  return value === null ? "n/a" : `${value.toFixed(1)}%`;
}

function NodeCapacity({ label, capacity }: { label: string; capacity: EvalClusterCapacity }) {
  const percent = usedPercent(capacity);
  return (
    <div className="eval-node-capacity">
      <span><strong>{label}</strong><small>{percent.toFixed(0)}%</small></span>
      <i aria-hidden="true"><b style={{ width: `${percent}%` }} /></i>
      <small>{formatBytes(capacity.totalBytes - capacity.availableBytes)} / {formatBytes(capacity.totalBytes)}</small>
    </div>
  );
}

function ClusterNode({ node }: { node: EvalClusterNode }) {
  const cpu = Math.max(0, Math.min(100, node.cpuUsagePercent));
  const lifecycleAligned = node.workerProcesses === node.claimedTasks &&
    node.claimedTasks === node.vmProcesses;
  return (
    <article className="eval-node-card">
      <header>
        <span className="eval-node-online" aria-hidden="true" />
        <div><strong>{node.id}</strong><small>online · up {formatUptime(node.uptimeSeconds)}</small></div>
        <Server aria-hidden="true" />
      </header>
      <div className="eval-node-counts">
        <div><span>Active evals</span><strong>{node.claimedTasks}</strong></div>
        <div><span>CPU cores</span><strong>{node.cpuCores}</strong></div>
        {!lifecycleAligned ? (
          <div className="eval-node-mismatch">
            <span>Lifecycle mismatch</span>
            <strong>{node.workerProcesses} / {node.claimedTasks} / {node.vmProcesses}</strong>
            <small>workers / claims / VMs</small>
          </div>
        ) : null}
      </div>
      <div className="eval-node-utilization">
        <div className="eval-node-capacity">
          <span><strong><Cpu aria-hidden="true" /> CPU</strong><small>{cpu.toFixed(0)}%</small></span>
          <i aria-hidden="true"><b style={{ width: `${cpu}%` }} /></i>
          <small>load {node.loadAverage.one.toFixed(1)} / {node.loadAverage.five.toFixed(1)} / {node.loadAverage.fifteen.toFixed(1)}</small>
        </div>
        <NodeCapacity label="Memory" capacity={node.memory} />
        <NodeCapacity label="Swap" capacity={node.swap} />
      </div>
      <footer>
        <MemoryStick aria-hidden="true" />
        <span>10s pressure</span>
        <span>CPU {formatPressure(node.pressure.cpuSomeAvg10)}</span>
        <span>memory some {formatPressure(node.pressure.memorySomeAvg10)}</span>
        <span>memory full {formatPressure(node.pressure.memoryFullAvg10)}</span>
      </footer>
    </article>
  );
}

function ClusterView({ cluster }: { cluster: EvalCluster }) {
  return (
    <section className="eval-cluster" aria-labelledby="cluster-heading">
      <header>
        <div><p className="rail-label">Runtime</p><h2 id="cluster-heading">Cluster</h2></div>
        <span>{cluster.nodes.length} node{cluster.nodes.length === 1 ? "" : "s"}</span>
      </header>
      {cluster.nodes.length ? (
        cluster.nodes.map((node) => <ClusterNode node={node} key={node.id} />)
      ) : (
        <p className="eval-cluster-state">No cluster nodes are reporting.</p>
      )}
    </section>
  );
}

function formatInteger(value: unknown) {
  return typeof value === "number" ? value.toLocaleString() : "—";
}

function taskMatchesFilter(task: EvalTaskOverview, filter: MatrixFilter) {
  const { summary } = task;
  if (filter === "all") return true;
  if (filter === "active") return summary.running > 0;
  if (filter === "issues") return summary.failed > 0;
  return summary.total > 0 && summary.success + summary.failed === summary.total;
}

function coordinateLabel(cell: EvalCoordinate) {
  if (cell.status === "passed" || cell.status === "failed") return cell.status;
  if (cell.outcome === "passed") return "passed";
  return cell.state;
}

function CellMark({ cell }: { cell: EvalCoordinate }) {
  const label = coordinateLabel(cell);
  return (
    <span className={`eval-cell-mark ${label}`} aria-hidden="true">
      {label === "passed" || label === "success" ? "✓" : label === "failed" ? "×" : cell.repetition}
    </span>
  );
}

function JsonEvidence({ title, value }: { title: string; value: unknown }) {
  if (!value) return null;
  return (
    <details className="live-evidence-block">
      <summary>{title}</summary>
      <pre>{JSON.stringify(value, null, 2)}</pre>
    </details>
  );
}

function CaseInspector({
  evidence,
  cell,
  treatment,
  onClose,
}: {
  evidence: EvalCase;
  cell: EvalCoordinate;
  treatment: EvalTreatment;
  onClose: () => void;
}) {
  return (
    <article className={`live-case-detail ${cell.state}`} aria-label="Selected evaluation case">
      <header>
        <div>
          <p className="eyebrow">{treatment.harness} · {treatment.model} · {treatment.thinking}</p>
          <h2>Repetition {cell.repetition}</h2>
        </div>
        <button type="button" className="icon-button" onClick={onClose} aria-label="Close case detail">
          <X aria-hidden="true" />
        </button>
      </header>
      <div className="live-case-detail-status">
        <strong>{evidence.status ?? coordinateLabel(cell)}</strong>
        <span>{formatDuration(cell.durationMs)}</span>
      </div>
      <dl className="live-case-metrics">
        <div><dt>Model</dt><dd>{evidence.model ?? treatment.model}</dd></div>
        <div><dt>Effort</dt><dd>{evidence.effort ?? treatment.thinking}</dd></div>
        <div><dt>Environment</dt><dd>{evidence.environment ?? "—"}</dd></div>
        <div><dt>Tool calls</dt><dd>{formatInteger(evidence.toolCalls)}</dd></div>
        <div><dt>Input tokens</dt><dd>{formatInteger(evidence.usage?.input_tokens)}</dd></div>
        <div><dt>Cached input</dt><dd>{formatInteger(evidence.usage?.cached_input_tokens)}</dd></div>
        <div><dt>Output tokens</dt><dd>{formatInteger(evidence.usage?.output_tokens)}</dd></div>
        <div><dt>Total tokens</dt><dd>{formatInteger(evidence.usage?.total_tokens)}</dd></div>
      </dl>
      {evidence.prompt ? (
        <section className="live-case-task">
          <p className="rail-label">Task instruction</p>
          <pre>{evidence.prompt}</pre>
        </section>
      ) : null}
      {evidence.verifierStdout ? (
        <details className="live-evidence-block" open={evidence.status !== "passed"}>
          <summary>Verifier stdout</summary>
          <pre>{evidence.verifierStdout}</pre>
        </details>
      ) : null}
      {evidence.verifierStderr ? (
        <details className="live-evidence-block" open>
          <summary>Verifier stderr</summary>
          <pre>{evidence.verifierStderr}</pre>
        </details>
      ) : null}
      {evidence.finalMessage ? (
        <details className="live-evidence-block">
          <summary>Final agent message</summary>
          <pre>{evidence.finalMessage}</pre>
        </details>
      ) : null}
      <JsonEvidence title="Verifier result" value={evidence.verifier} />
      <JsonEvidence title="Exception" value={evidence.exception} />
      <JsonEvidence title="Timing" value={evidence.timing} />
    </article>
  );
}

function progressRank(summary: EvalSummary) {
  if (summary.running > 0) return 0;
  if (summary.unclaimed > 0) return 1;
  return 2;
}

function ProgressBar({ summary, label }: { summary: EvalSummary; label: string }) {
  const denominator = Math.max(summary.total, 1);
  const finished = summary.success + summary.failed;
  const segments = [
    { key: "success", value: summary.success },
    { key: "failed", value: summary.failed },
    { key: "running", value: summary.running },
    { key: "unclaimed", value: summary.unclaimed },
  ];
  return (
    <div
      className="eval-progress"
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={summary.total}
      aria-valuenow={finished}
    >
      <div className="eval-progress-track">
        {segments.map((segment) => segment.value > 0 ? (
          <span
            className={segment.key}
            style={{ width: `${segment.value / denominator * 100}%` }}
            key={segment.key}
          />
        ) : null)}
      </div>
      <div className="eval-progress-copy">
        <strong>{finished} / {summary.total}</strong>
        <span>{summary.running} running</span>
        <span>{summary.unclaimed} unclaimed</span>
        {summary.failed > 0 ? <span>{summary.failed} execution failed</span> : null}
      </div>
    </div>
  );
}

function PageBack({ onClick, children }: { onClick: () => void; children: string }) {
  return (
    <button type="button" className="eval-back" onClick={onClick}>
      <ChevronLeft aria-hidden="true" />
      {children}
    </button>
  );
}

function Analytics({
  points,
  view = "frontier",
  taskCount,
}: {
  points: EvalResultPoint[] | EvalAnalyticsPoint[];
  view?: AnalyticsView;
  taskCount?: number;
}) {
  return <EvalAnalytics points={points} view={view} taskCount={taskCount} />;
}

export type LiveEvalsData =
  | {
      kind: "overview";
      overview: EvalOverview;
      cluster: EvalCluster;
    }
  | {
      kind: "workset";
      detail: EvalWorksetDetail;
      analytics: EvalWorksetAnalytics;
    }
  | {
      kind: "task";
      snapshot: EvalTaskSnapshot;
    };

export type EvalSurfaceStatus = {
  observedAtMs: number;
  error: string | null;
  retry: () => void;
};

function Freshness({ status }: { status: EvalSurfaceStatus }) {
  return (
    <div className={status.error ? "eval-freshness is-error" : "eval-freshness"}>
      <span>Updated {formatObservedAt(status.observedAtMs)}</span>
      {status.error ? (
        <span role="alert">
          Refresh failed · {status.error}
          <button type="button" onClick={status.retry}>Retry</button>
        </span>
      ) : null}
    </div>
  );
}

export function LiveEvals({ data, status }: { data: LiveEvalsData; status: EvalSurfaceStatus }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [selectedCell, setSelectedCell] = useState<{
    treatment: EvalTreatment;
    cell: EvalCoordinate;
    evidence: EvalCase;
  } | null>(null);
  const [caseError, setCaseError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<MatrixFilter>("all");
  const [shownTaskRows, setShownTaskRows] = useState(initialTaskRows);
  const detailRef = useRef<HTMLDivElement>(null);
  const caseRequestId = useRef(0);
  const detail = data.kind === "workset" ? data.detail : null;
  const selectedWorkset = data.kind === "task" ? data.snapshot.workset : detail?.workset ?? null;
  const selectedWorksetId = selectedWorkset?.id ?? null;
  const tasks = detail?.tasks ?? [];
  const normalizedQuery = query.trim().toLowerCase();
  const visibleTasks = useMemo(
    () => tasks
      .filter(
        (task) =>
          (!normalizedQuery ||
            task.name.toLowerCase().includes(normalizedQuery) ||
            task.label.toLowerCase().includes(normalizedQuery)) &&
          taskMatchesFilter(task, filter),
      )
      .sort((left, right) =>
        progressRank(left.summary) - progressRank(right.summary) || left.name.localeCompare(right.name)
      ),
    [filter, normalizedQuery, tasks],
  );
  const renderedTasks = visibleTasks.slice(0, shownTaskRows);
  const orderedWorksets = useMemo(
    () => [...(data.kind === "overview" ? data.overview.worksets : [])].sort((left, right) =>
      progressRank(left.summary) - progressRank(right.summary) ||
      right.createdAtMs - left.createdAtMs
    ),
    [data],
  );
  const selectedTaskOverview = data.kind === "task" ? data.snapshot.taskSummary : null;
  const selectedTask: EvalTask | null = data.kind === "task" ? data.snapshot.task : null;
  const repetitions = [
    ...new Set(selectedTask?.treatments.flatMap((treatment) =>
      treatment.cells.map((cell) => cell.repetition)) ?? []),
  ].sort((left, right) => left - right);
  useEffect(() => {
    if (!selectedCell) return;
    window.requestAnimationFrame(() =>
      detailRef.current?.scrollIntoView({ behavior: "smooth", block: "start" }),
    );
  }, [selectedCell?.cell.id]);
  useEffect(() => {
    setShownTaskRows(initialTaskRows);
  }, [filter, normalizedQuery, selectedWorksetId]);

  function chooseWorkset(id: string) {
    caseRequestId.current++;
    setSelectedCell(null);
    setCaseError(null);
    startTransition(() => navigate(`/evals/worksets/${encodeURIComponent(id)}`));
  }

  function chooseTask(id: string) {
    if (!selectedWorkset) return;
    caseRequestId.current++;
    setSelectedCell(null);
    setCaseError(null);
    startTransition(() => {
      navigate(
        `/evals/worksets/${encodeURIComponent(selectedWorkset.id)}/tasks/${encodeURIComponent(id)}`,
      );
    });
  }

  function chooseCell(treatment: EvalTreatment, cell: EvalCoordinate) {
    if (!cell.detailId) return;
    const requestId = ++caseRequestId.current;
    setCaseError(null);
    void queryClient.fetchQuery({
      queryKey: ["evals", "case", cell.detailId],
      queryFn: ({ signal }) => evalApi.evalCase(cell.detailId!, signal),
      staleTime: Infinity,
    }).then(
      (evidence) => {
        if (caseRequestId.current !== requestId) return;
        startTransition(() => setSelectedCell({ treatment, cell, evidence }));
      },
      (error: unknown) => {
        if (caseRequestId.current !== requestId) return;
        setCaseError(error instanceof Error ? error.message : "Case evidence is unavailable.");
      },
    );
  }

  function closeCell() {
    caseRequestId.current++;
    setSelectedCell(null);
    setCaseError(null);
  }

  if (data.kind === "overview") {
    return (
      <div className="live-evals">
        <section className="eval-page-head eval-overview-head">
          <div>
            <p className="eyebrow"><Radio aria-hidden="true" /> Coordinator evidence</p>
            <h1>Evals</h1>
            <p>Durable benchmark progress and retained result artifacts.</p>
          </div>
          <div className="eval-head-status">
            <ProgressBar summary={data.overview.summary} label="All retained evaluations progress" />
            <Freshness status={status} />
          </div>
        </section>
        <section className="eval-full-table" aria-labelledby="worksets-heading">
          <header><p className="rail-label">Benchmarks</p><h2 id="worksets-heading">Worksets</h2></header>
          <div className="eval-table-heading eval-workset-grid" aria-hidden="true">
            <span>Benchmark</span><span>Progress</span><span>Tasks</span><span>Created</span><span />
          </div>
          {orderedWorksets.map((workset) => (
            <button
              type="button"
              className="eval-table-row eval-workset-grid"
              onClick={() => chooseWorkset(workset.id)}
              key={workset.id}
            >
              <span className="eval-primary-cell"><strong>{workset.profile}</strong><small>{workset.digest.slice(0, 16)}</small></span>
              <ProgressBar summary={workset.summary} label={`${workset.profile} progress`} />
              <span className="eval-row-meta eval-workset-count"><span>Tasks · </span>{workset.taskCount}</span>
              <span className="eval-row-meta eval-workset-created"><span>Created · </span>{formatWorksetDate(workset.createdAtMs)}</span>
              <ChevronRight aria-hidden="true" />
            </button>
          ))}
          {!orderedWorksets.length ? <p className="eval-empty-list">No durable worksets yet.</p> : null}
        </section>
        <ClusterView cluster={data.cluster} />
      </div>
    );
  }

  if (data.kind === "workset") {
    const workset = data.detail.workset;
    return (
      <div className="live-evals">
        <section className="eval-page-head eval-detail-head">
          <div>
            <PageBack onClick={() => startTransition(() => navigate("/evals"))}>All evals</PageBack>
            <p className="eyebrow">Benchmark · {workset.digest.slice(0, 16)}</p>
            <h1>{workset.profile}</h1>
            <p>{workset.taskCount} tasks across the retained harness, model, thinking, and repetition sweep.</p>
          </div>
          <div className="eval-head-status">
            <ProgressBar summary={workset.summary} label={`${workset.profile} progress`} />
            <Freshness status={status} />
          </div>
        </section>
        <Analytics points={data.analytics.points} taskCount={data.analytics.taskCount} />
        <section className="eval-full-table" aria-labelledby="tasks-heading">
          <header className="eval-table-toolbar">
            <div><p className="rail-label">Progress</p><h2 id="tasks-heading">Tasks</h2></div>
            <label className="live-eval-search"><Search aria-hidden="true" /><input type="search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Filter tasks" aria-label="Filter evaluation tasks" /></label>
            <div className="live-filter" role="group" aria-label="Task filter">
              {(["all", "active", "issues", "complete"] as MatrixFilter[]).map((option) => (
                <button type="button" className={filter === option ? "is-active" : ""} onClick={() => setFilter(option)} key={option}>{option}</button>
              ))}
            </div>
          </header>
          <div className="eval-table-heading eval-task-grid" aria-hidden="true">
            <span>Task</span><span>Progress</span><span>Treatments</span><span />
          </div>
          {renderedTasks.map((task) => (
            <button type="button" className="eval-table-row eval-task-grid" onClick={() => chooseTask(task.id)} key={task.id}>
              <span className="eval-primary-cell"><strong>{task.label}</strong><small>{task.name}</small></span>
              <ProgressBar summary={task.summary} label={`${task.label} progress`} />
              <span className="eval-row-meta eval-task-treatments"><span>Treatments · </span>{task.treatmentCount}</span>
              <ChevronRight aria-hidden="true" />
            </button>
          ))}
          {renderedTasks.length < visibleTasks.length ? (
            <button
              type="button"
              className="eval-show-more"
              onClick={() => setShownTaskRows((count) => count + initialTaskRows)}
            >
              Show {Math.min(initialTaskRows, visibleTasks.length - renderedTasks.length)} more tasks
              <small>{visibleTasks.length - renderedTasks.length} remaining</small>
            </button>
          ) : null}
          {!visibleTasks.length ? <p className="eval-empty-list">No tasks match this filter.</p> : null}
        </section>
      </div>
    );
  }

  const taskWorkset = data.snapshot.workset;
  return (
    <div className="live-evals">
      <section className="eval-page-head eval-detail-head">
        <div>
          <PageBack onClick={() => chooseWorkset(taskWorkset.id)}>{taskWorkset.profile}</PageBack>
          <p className="eyebrow">Task · {(selectedTaskOverview?.digest ?? selectedTask?.digest)?.slice(0, 16)}</p>
          <h1>{selectedTaskOverview?.label ?? selectedTask?.label}</h1>
          <p>{selectedTaskOverview?.name ?? selectedTask?.name}</p>
        </div>
        {selectedTaskOverview ? (
          <div className="eval-head-status">
            <ProgressBar summary={selectedTaskOverview.summary} label={`${selectedTaskOverview.label} progress`} />
            <Freshness status={status} />
          </div>
        ) : null}
      </section>
      <Analytics points={data.snapshot.points} view="runs" />
      <section className="eval-run-section" aria-labelledby="treatments-heading">
        {selectedTask ? (
          <>
            <header className="eval-task-panel-header">
              <div><p className="rail-label">Runs</p><h2 id="treatments-heading">Treatments and repetitions</h2></div>
              <span>{selectedTask.treatments.length} treatments</span>
            </header>
            <div className="eval-matrix-legend" role="group" aria-label="Result legend">
              <span><i className="passed" /> verifier passed</span><span><i className="failed" /> verifier / execution failed</span><span><i className="running" /> running</span><span><i className="unclaimed" /> unclaimed</span>
            </div>
            <div className="eval-task-matrix-scroll">
              <table className="eval-task-matrix">
                <thead><tr><th>Treatment</th>{repetitions.map((repetition) => <th key={repetition}>#{repetition}</th>)}<th>Done</th></tr></thead>
                <tbody>
                  {selectedTask.treatments.map((treatment) => (
                    <tr key={treatment.id}>
                      <th scope="row"><span>{treatment.harness} · {treatment.model}</span><small>{treatment.thinking} thinking</small></th>
                      {repetitions.map((repetition) => {
                        const cell = treatment.cells.find((candidate) => candidate.repetition === repetition);
                        if (!cell) return <td className="is-unavailable" key={repetition} />;
                        return (
                          <td key={repetition}>
                            <button type="button" className={`eval-matrix-cell ${cell.state} ${cell.status ?? cell.outcome ?? ""}`} title={`${treatment.label}\nrepetition ${repetition} · ${coordinateLabel(cell)}\n${formatDuration(cell.durationMs)}`} aria-label={`${treatment.label}, repetition ${repetition}: ${coordinateLabel(cell)}`} aria-pressed={selectedCell?.cell.id === cell.id} disabled={!cell.detailId} onClick={() => chooseCell(treatment, cell)}>
                              <CellMark cell={cell} />
                            </button>
                          </td>
                        );
                      })}
                      <td className="live-row-total">{treatment.cells.filter((cell) => cell.state === "success" || cell.state === "failed").length}/{treatment.cells.length}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {caseError ? (
              <p className="live-case-error" role="alert"><AlertTriangle aria-hidden="true" /> {caseError}</p>
            ) : null}
            {selectedCell ? (
              <div className="live-case-slot" ref={detailRef}><CaseInspector evidence={selectedCell.evidence} cell={selectedCell.cell} treatment={selectedCell.treatment} onClose={closeCell} /></div>
            ) : null}
          </>
        ) : null}
      </section>
    </div>
  );
}
