import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Activity,
  AlertTriangle,
  Check,
  ChevronRight,
  Clock3,
  Cpu,
  Database,
  ShieldCheck,
  Terminal,
  X,
  Zap,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

type TrialStatus = "passed" | "failed" | "error";
type EvalView = "overview" | "verifier" | "trajectory";
type Runner = "harness" | "codex";

type TrialSummary = {
  id: string;
  taskName: string;
  trialName: string;
  source: string | null;
  taskRef: string | null;
  status: TrialStatus;
  reward: number | null;
  startedAt: string | null;
  finishedAt: string | null;
  durationMs: number | null;
  model: string | null;
  effort: string | null;
  runner: Runner;
  branch: string;
  modelCalls: number;
  toolCalls: number;
  tokens: {
    input: number;
    cached: number;
    output: number;
    reasoning: number;
  };
  verifier: {
    tests: number;
    passed: number;
    failed: number;
    skipped: number;
  } | null;
  detailUrl: string;
};

type HarborJob = {
  key: string;
  id: string;
  name: string;
  runner: Runner;
  branch: string;
  source: string;
  startedAt: string;
  finishedAt: string;
  durationMs: number | null;
  totalTrials: number;
  completedTrials: number;
  erroredTrials: number;
  retries: number;
  mean: number | null;
  tokens: {
    input: number;
    cached: number;
    output: number;
  };
  trials: TrialSummary[];
};

type ComparisonJob = {
  key: string;
  name: string;
  branch: string;
  finishedAt: string;
  durationMs: number | null;
  agentDurationMs: number;
  passed: number;
  score: number | null;
  model: string | null;
  effort: string | null;
  modelCalls: number;
  tokens: {
    input: number;
    cached: number;
    output: number;
  };
};

type ComparisonTask = {
  taskName: string;
  outcome: Runner | "tie";
  harness: {
    id: string;
    status: TrialStatus;
    reward: number | null;
    durationMs: number | null;
  };
  codex: {
    id: string | null;
    status: TrialStatus;
    reward: number | null;
    durationMs: number | null;
  };
};

export type EvalComparison = {
  taskCount: number;
  harness: ComparisonJob;
  codex: ComparisonJob;
  delta: number;
  headToHead: { harness: number; codex: number; ties: number };
  tasks: ComparisonTask[];
};

type HarborIndex = {
  generatedAt: string;
  source: string;
  jobCount: number;
  trialCount: number;
  comparison: EvalComparison | null;
  jobs: HarborJob[];
};

type TrialDetail = TrialSummary & {
  instruction: string | null;
  finalMessage: string | null;
  exception: { type: string; message: string } | null;
  phases: Array<{ name: string; durationMs: number | null }>;
  transport: string | null;
  orchestration: string | null;
  reconnects: number;
  compactions: number;
  trajectory: Array<{
    index: number;
    modelCall: number | null;
    tool: string;
    summary: string;
    workdir: string | null;
    status: string;
    durationMs: number | null;
    exitCode: number | null;
  }>;
  verifierTests: Array<{
    name: string;
    status: string;
    durationMs: number | null;
    filePath: string | null;
  }>;
};

const dateFormatter = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

function formatDuration(milliseconds: number | null) {
  if (milliseconds === null) return "—";
  if (milliseconds < 1000) return `${Math.round(milliseconds)}ms`;
  const seconds = milliseconds / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 2 : 1)}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${Math.round(seconds % 60)}s`;
}

function formatTokens(value: number) {
  if (value < 1000) return String(value);
  if (value < 1_000_000) return `${(value / 1000).toFixed(value < 10_000 ? 1 : 0)}k`;
  return `${(value / 1_000_000).toFixed(2)}m`;
}

function taskLabel(taskName: string) {
  return taskName.includes("/") ? taskName.split("/").at(-1) ?? taskName : taskName;
}

function statusIcon(status: TrialStatus | string) {
  if (status === "passed") return <Check aria-hidden="true" />;
  if (status === "failed") return <X aria-hidden="true" />;
  return <AlertTriangle aria-hidden="true" />;
}

function score(job: HarborJob) {
  if (job.mean === null) return "—";
  return `${(job.mean * 100).toFixed(job.mean === 0 || job.mean === 1 ? 0 : 1)}%`;
}

function comparisonScore(value: number | null) {
  if (value === null) return "—";
  return `${(value * 100).toFixed(1)}%`;
}

function runnerLabel(runner: Runner) {
  return runner === "harness" ? "NanoCodex" : "Codex";
}

export function Harbor() {
  const [index, setIndex] = useState<HarborIndex | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [selectedJobKey, setSelectedJobKey] = useState<string | null>(null);
  const [selectedTrialId, setSelectedTrialId] = useState<string | null>(null);
  const [detail, setDetail] = useState<TrialDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [view, setView] = useState<EvalView>("overview");
  const jobListRef = useRef<HTMLDivElement>(null);
  const trialListRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const controller = new AbortController();
    fetch("/data/harbor/index.json", { signal: controller.signal })
      .then((response) => {
        if (!response.ok) throw new Error(`Harbor index failed: ${response.status}`);
        return response.json() as Promise<HarborIndex>;
      })
      .then(setIndex)
      .catch((error) => {
        if (error instanceof DOMException && error.name === "AbortError") return;
        setLoadError(true);
      });
    return () => controller.abort();
  }, []);

  const defaultGate = useMemo(
    () =>
      index?.jobs.find((job) => job.key === index.comparison?.harness.key) ??
      index?.jobs.find((job) => job.runner === "harness" && job.totalTrials > 1) ??
      index?.jobs[0] ??
      null,
    [index],
  );
  const selectedJob =
    index?.jobs.find((job) => job.key === selectedJobKey) ?? defaultGate ?? null;
  const trials = useMemo(() => {
    const statusOrder: Record<TrialStatus, number> = { failed: 0, error: 1, passed: 2 };
    return [...(selectedJob?.trials ?? [])].sort(
      (left, right) =>
        statusOrder[left.status] - statusOrder[right.status] ||
        left.taskName.localeCompare(right.taskName),
    );
  }, [selectedJob]);
  const selectedTrial =
    trials.find((trial) => trial.id === selectedTrialId) ?? trials[0] ?? null;

  useEffect(() => {
    if (!selectedTrial) return;
    setSelectedTrialId(selectedTrial.id);
  }, [selectedTrial]);

  useEffect(() => {
    if (!selectedTrial) {
      setDetail(null);
      return;
    }
    const controller = new AbortController();
    setDetail(null);
    setDetailLoading(true);
    fetch(selectedTrial.detailUrl, { signal: controller.signal })
      .then((response) => {
        if (!response.ok) throw new Error(`Trial detail failed: ${response.status}`);
        return response.json() as Promise<TrialDetail>;
      })
      .then(setDetail)
      .finally(() => setDetailLoading(false))
      .catch((error) => {
        if (error instanceof DOMException && error.name === "AbortError") return;
        setDetailLoading(false);
      });
    return () => controller.abort();
  }, [selectedTrial]);

  const jobVirtualizer = useVirtualizer({
    count: index?.jobs.length ?? 0,
    getScrollElement: () => jobListRef.current,
    estimateSize: () => 76,
    overscan: 8,
  });
  const trialVirtualizer = useVirtualizer({
    count: trials.length,
    getScrollElement: () => trialListRef.current,
    estimateSize: () => 82,
    overscan: 8,
  });

  if (loadError) {
    return (
      <section className="harbor-loading page-grid">
        <AlertTriangle aria-hidden="true" />
        <h1>Evaluation data unavailable</h1>
        <p>Run `npm run sync` to derive the eval index from the retained job artifacts.</p>
      </section>
    );
  }

  if (!index || !defaultGate) {
    return null;
  }

  const comparison = index.comparison;
  const decisiveTasks = comparison?.tasks.filter((task) => task.outcome !== "tie") ?? [];
  const phaseTotal = detail?.phases.reduce((total, phase) => total + (phase.durationMs ?? 0), 0) ?? 0;

  const openComparisonTrial = (runner: Runner, trialId: string | null) => {
    if (!comparison || !trialId) return;
    setSelectedJobKey(comparison[runner].key);
    setSelectedTrialId(trialId);
    setView("overview");
  };

  return (
    <>
      <section className="eval-hero page-grid">
        <div className="eval-hero-copy">
          <p className="eyebrow">
            <Activity aria-hidden="true" /> Development record
          </p>
          <h1>Evals</h1>
          <p>
            Same model, same effort, same {comparison?.taskCount ?? "retained"} public tasks.
            We run these constantly to decide what to build next. Codex is the baseline, and every
            result links to the verifier and trajectory that produced it.
          </p>
        </div>
        <div className="gate-score">
          <span>Matched development set</span>
          <strong>{comparison?.taskCount ?? defaultGate.completedTrials}</strong>
          <p>
            {comparison
              ? `public tasks · same model and effort`
              : `${defaultGate.completedTrials} completed trials`}
          </p>
        </div>
        <div className="eval-strip">
          <div>
            <span>Codex reference</span>
            <strong>{comparisonScore(comparison?.codex.score ?? null)}</strong>
          </div>
          <div>
            <span>NanoCodex experiment</span>
            <strong>{comparisonScore(comparison?.harness.score ?? null)}</strong>
          </div>
          <div>
            <span>Different outcomes</span>
            <strong>{comparison ? comparison.headToHead.harness + comparison.headToHead.codex : "—"}</strong>
          </div>
          <div>
            <span>Same outcome</span>
            <strong>{comparison?.headToHead.ties ?? "—"}</strong>
          </div>
        </div>
      </section>

      {comparison ? (
        <section className="comparison-board page-grid" aria-labelledby="comparison-title">
          <header>
            <div>
              <p className="rail-label">Matched task comparison</p>
              <h2 id="comparison-title">Tasks to investigate</h2>
            </div>
            <p>
              {comparison.harness.model} · {comparison.harness.effort} effort · public development set
            </p>
          </header>
          <table className="comparison-table">
            <thead>
              <tr>
                <th>Task</th>
                <th>Codex</th>
                <th>NanoCodex</th>
              </tr>
            </thead>
            <tbody>
              {decisiveTasks.map((task) => (
                <tr key={task.taskName}>
                  <th scope="row">{taskLabel(task.taskName)}</th>
                  <td>
                    <button
                      className={`comparison-result ${task.codex.status}`}
                      type="button"
                      onClick={() => openComparisonTrial("codex", task.codex.id)}
                    >
                      {statusIcon(task.codex.status)} {task.codex.status}
                    </button>
                  </td>
                  <td>
                    <button
                      className={`comparison-result ${task.harness.status}`}
                      type="button"
                      onClick={() => openComparisonTrial("harness", task.harness.id)}
                    >
                      {statusIcon(task.harness.status)} {task.harness.status}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <footer>
            The disagreement set is the useful part: {comparison.headToHead.harness + comparison.headToHead.codex} tasks
            differed and {comparison.headToHead.ties} had the same outcome. Click any result to inspect the retained trial.
          </footer>
        </section>
      ) : null}

      <section className="harbor-layout page-grid" id="evaluation-results" aria-label="Evaluation results">
        <aside className="job-column">
          <div className="eval-column-heading">
            <p className="rail-label">Runs</p>
            <span>{index.jobs.length}</span>
          </div>
          <div className="eval-scroll-list" ref={jobListRef}>
            <div
              className="eval-virtual-inner"
              style={{ height: `${jobVirtualizer.getTotalSize()}px` }}
            >
              {jobVirtualizer.getVirtualItems().map((virtualRow) => {
                const job = index.jobs[virtualRow.index];
                const isSelected = job.key === selectedJob?.key;
                return (
                  <button
                    className={isSelected ? "job-row is-selected" : "job-row"}
                    type="button"
                    key={job.id}
                    style={{ transform: `translateY(${virtualRow.start}px)` }}
                    onClick={() => {
                      setSelectedJobKey(job.key);
                      setSelectedTrialId(null);
                      setView("overview");
                    }}
                  >
                    <span className="eval-row-meta">
                      <span>{dateFormatter.format(new Date(job.finishedAt))}</span>
                      <span>{runnerLabel(job.runner)}</span>
                      <span>{job.totalTrials === 1 ? "Focused" : `${job.totalTrials} tasks`}</span>
                    </span>
                    <strong>
                      {job.totalTrials === 1
                        ? taskLabel(job.trials[0]?.taskName ?? job.name)
                        : `${runnerLabel(job.runner)} gate`}
                    </strong>
                    <span className="job-score">{score(job)}</span>
                  </button>
                );
              })}
            </div>
          </div>
        </aside>

        <label className="mobile-job-picker">
          <span>Evaluation run</span>
          <select
            value={selectedJob?.key ?? ""}
            onChange={(event) => {
              setSelectedJobKey(event.target.value);
              setSelectedTrialId(null);
              setView("overview");
            }}
          >
            {index.jobs.map((job) => (
              <option value={job.key} key={job.key}>
                {runnerLabel(job.runner)} · {dateFormatter.format(new Date(job.finishedAt))} · {score(job)}
                {job.totalTrials === 1 ? ` · ${taskLabel(job.trials[0]?.taskName ?? job.name)}` : ""}
              </option>
            ))}
          </select>
        </label>

        <section className="trial-column" aria-labelledby="trial-list-title">
          <div className="eval-column-heading">
            <div>
              <p className="rail-label">Tasks</p>
              <h2 id="trial-list-title">{selectedJob?.totalTrials === 1 ? "Focused run" : "Gate results"}</h2>
            </div>
            <span>{trials.length}</span>
          </div>
          <div className="eval-scroll-list" ref={trialListRef}>
            <div
              className="eval-virtual-inner"
              style={{ height: `${trialVirtualizer.getTotalSize()}px` }}
            >
              {trialVirtualizer.getVirtualItems().map((virtualRow) => {
                const trial = trials[virtualRow.index];
                const isSelected = trial.id === selectedTrial?.id;
                return (
                  <button
                    className={isSelected ? `trial-row is-selected ${trial.status}` : `trial-row ${trial.status}`}
                    type="button"
                    key={trial.id}
                    style={{ transform: `translateY(${virtualRow.start}px)` }}
                    onClick={() => {
                      setSelectedTrialId(trial.id);
                      setView("overview");
                    }}
                  >
                    <span className="trial-status">{statusIcon(trial.status)}</span>
                    <span className="trial-row-copy">
                      <strong>{taskLabel(trial.taskName)}</strong>
                      <small>
                        {formatDuration(trial.durationMs)} · {trial.modelCalls}/{trial.toolCalls} model/tool
                      </small>
                    </span>
                    <span className="trial-reward">{trial.reward ?? "—"}</span>
                    <ChevronRight aria-hidden="true" />
                  </button>
                );
              })}
            </div>
          </div>
        </section>

        <article className="eval-detail" aria-labelledby="eval-detail-title">
          {selectedTrial ? (
            <>
              <header className={`eval-detail-header ${selectedTrial.status}`}>
                <p className="eyebrow">
                  {statusIcon(selectedTrial.status)} {selectedTrial.status} · reward {selectedTrial.reward ?? "—"}
                </p>
                <h2 id="eval-detail-title">{taskLabel(selectedTrial.taskName)}</h2>
                <p>
                  {runnerLabel(selectedTrial.runner)} · {selectedTrial.model} · {selectedTrial.effort ?? "low"} effort · {selectedTrial.branch}
                </p>
                <div className="eval-detail-duration">{formatDuration(selectedTrial.durationMs)}</div>
              </header>

              <nav className="detail-tabs eval-tabs" aria-label="Evaluation details">
                <button
                  className={view === "overview" ? "is-active" : ""}
                  type="button"
                  onClick={() => setView("overview")}
                >
                  Overview <span>[1]</span>
                </button>
                <button
                  className={view === "verifier" ? "is-active" : ""}
                  type="button"
                  onClick={() => setView("verifier")}
                >
                  Verifier <span>[2]</span>
                </button>
                <button
                  className={view === "trajectory" ? "is-active" : ""}
                  type="button"
                  onClick={() => setView("trajectory")}
                >
                  Trajectory <span>[3]</span>
                </button>
              </nav>

              {!detailLoading && view === "overview" ? (
                <div className="eval-overview">
                  <section className="eval-metrics-grid">
                    <div>
                      <Cpu aria-hidden="true" />
                      <span>Model calls</span>
                      <strong>{selectedTrial.modelCalls}</strong>
                    </div>
                    <div>
                      <Terminal aria-hidden="true" />
                      <span>Tool calls</span>
                      <strong>{selectedTrial.toolCalls}</strong>
                    </div>
                    <div>
                      <Database aria-hidden="true" />
                      <span>Input / cached</span>
                      <strong>
                        {formatTokens(selectedTrial.tokens.input)} / {formatTokens(selectedTrial.tokens.cached)}
                      </strong>
                    </div>
                    <div>
                      <Zap aria-hidden="true" />
                      <span>Output / reasoning</span>
                      <strong>
                        {formatTokens(selectedTrial.tokens.output)} / {formatTokens(selectedTrial.tokens.reasoning)}
                      </strong>
                    </div>
                  </section>

                  {detail ? (
                    <>
                      <section className="phase-section">
                        <p className="rail-label">Wall-clock phases</p>
                        <div className="phase-bar" aria-label="Trial phase durations">
                          {detail.phases.map((phase) => (
                            <span
                              key={phase.name}
                              style={{ width: `${phaseTotal ? ((phase.durationMs ?? 0) / phaseTotal) * 100 : 0}%` }}
                              title={`${phase.name}: ${formatDuration(phase.durationMs)}`}
                            />
                          ))}
                        </div>
                        <div className="phase-legend">
                          {detail.phases.map((phase, index) => (
                            <div key={phase.name}>
                              <i data-phase={index} />
                              <span>{phase.name}</span>
                              <strong>{formatDuration(phase.durationMs)}</strong>
                            </div>
                          ))}
                        </div>
                      </section>

                      <section className="eval-text-record">
                        <p className="rail-label">Task</p>
                        <pre>{detail.instruction ?? "Task instruction unavailable."}</pre>
                      </section>

                      <section className="eval-text-record">
                        <p className="rail-label">Agent result</p>
                        <pre>{detail.finalMessage ?? "No final assistant message was retained."}</pre>
                      </section>
                    </>
                  ) : null}
                </div>
              ) : null}

              {!detailLoading && view === "verifier" ? (
                <div className="verifier-view">
                  <section className="verifier-summary">
                    <ShieldCheck aria-hidden="true" />
                    <div>
                      <p className="rail-label">Canonical verifier</p>
                      <h3>
                        {selectedTrial.verifier?.passed ?? 0} / {selectedTrial.verifier?.tests ?? 0} passed
                      </h3>
                    </div>
                  </section>
                  <div className="verifier-tests">
                    {(detail?.verifierTests ?? []).map((test) => (
                      <div className={`verifier-test ${test.status}`} key={test.name}>
                        <span>{test.status === "passed" ? <Check aria-hidden="true" /> : <X aria-hidden="true" />}</span>
                        <div>
                          <strong>{test.name}</strong>
                          <small>{test.filePath}</small>
                        </div>
                        <span>{formatDuration(test.durationMs)}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ) : null}

              {!detailLoading && view === "trajectory" ? (
                <div className="trajectory-view">
                  <div className="trajectory-heading">
                    <div>
                      <p className="rail-label">Tool trajectory</p>
                      <h3>{detail?.trajectory.length ?? 0} calls</h3>
                    </div>
                    <p>Commands are summarized; raw output and reasoning are not published.</p>
                  </div>
                  <div className="trajectory-list">
                    {(detail?.trajectory ?? []).map((step) => (
                      <div className="trajectory-step" key={`${step.index}-${step.tool}`}>
                        <span className="trajectory-index">{String(step.index).padStart(2, "0")}</span>
                        <div>
                          <span className="trajectory-tool">{step.tool}</span>
                          <strong>{step.summary}</strong>
                          <small>{step.workdir}</small>
                        </div>
                        <div className="trajectory-result">
                          <span className={step.exitCode === null || step.exitCode === 0 ? "passed" : "failed"}>
                            {step.exitCode === null ? step.status : `exit ${step.exitCode}`}
                          </span>
                          <small>{formatDuration(step.durationMs)}</small>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ) : null}
            </>
          ) : (
            <div className="eval-detail-loading">This run has no retained trial details.</div>
          )}
        </article>
      </section>
    </>
  );
}
