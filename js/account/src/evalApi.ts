export type EvalCoordinateState = "unclaimed" | "running" | "success" | "failed";

export type EvalSummary = {
  total: number;
  unclaimed: number;
  running: number;
  success: number;
  failed: number;
};

export type EvalWorkset = {
  id: string;
  profile: string;
  digest: string;
  createdAtMs: number;
  taskCount: number;
  summary: EvalSummary;
};

export type EvalOverview = {
  schemaVersion: number;
  observedAtMs: number;
  summary: EvalSummary;
  worksets: EvalWorkset[];
};

export type EvalClusterCapacity = {
  totalBytes: number;
  availableBytes: number;
};

export type EvalClusterNode = {
  id: string;
  observedAtMs: number;
  uptimeSeconds: number;
  claimedTasks: number;
  workerProcesses: number;
  vmProcesses: number;
  cpuCores: number;
  cpuUsagePercent: number;
  loadAverage: { one: number; five: number; fifteen: number };
  memory: EvalClusterCapacity;
  swap: EvalClusterCapacity;
  pressure: {
    cpuSomeAvg10: number | null;
    memorySomeAvg10: number | null;
    memoryFullAvg10: number | null;
  };
};

export type EvalCluster = {
  schemaVersion: number;
  observedAtMs: number;
  nodes: EvalClusterNode[];
};

export type EvalCoordinate = {
  id: string;
  repetition: number;
  state: EvalCoordinateState;
  status: string | null;
  outcome: string | null;
  updatedAtMs: number | null;
  durationMs: number | null;
  message: string | null;
  detailId: string | null;
};

export type EvalTreatment = {
  id: string;
  label: string;
  harness: string;
  model: string;
  thinking: string;
  cells: EvalCoordinate[];
};

export type EvalTaskOverview = {
  id: string;
  name: string;
  label: string;
  digest: string;
  treatmentCount: number;
  summary: EvalSummary;
};

export type EvalTask = {
  id: string;
  name: string;
  label: string;
  digest: string;
  treatments: EvalTreatment[];
};

export type EvalWorksetDetail = {
  schemaVersion: number;
  observedAtMs: number;
  workset: EvalWorkset;
  tasks: EvalTaskOverview[];
};

export type EvalResultPoint = {
  id: string;
  taskId: string;
  taskName: string;
  taskLabel: string;
  state: "success" | "failed";
  harness: string;
  model: string;
  thinking: string;
  repetition: number;
  status: string | null;
  outcome: string | null;
  durationMs: number | null;
  inputTokens: number | null;
  cachedInputTokens: number | null;
  outputTokens: number | null;
  reasoningOutputTokens: number | null;
  totalTokens: number | null;
  costUsd: number | null;
};

export type EvalAnalyticsPoint = {
  harness: string;
  model: string;
  thinking: string;
  passed: number;
  completed: number;
  medianOutputTokens: number | null;
  outputSamples: number;
  medianDurationMs: number | null;
  durationSamples: number;
  medianCostUsd: number | null;
  costSamples: number;
};

export type EvalWorksetAnalytics = {
  schemaVersion: number;
  observedAtMs: number;
  worksetId: string;
  taskCount: number;
  points: EvalAnalyticsPoint[];
};

export type EvalTaskSnapshot = {
  schemaVersion: number;
  observedAtMs: number;
  worksetId: string;
  workset: EvalWorkset;
  taskSummary: EvalTaskOverview;
  task: EvalTask;
  points: EvalResultPoint[];
};

export type EvalCoordinateOutcome = {
  id: string;
  status: string | null;
  outcome: string | null;
};

export type EvalTaskOutcomesPage = {
  schemaVersion: number;
  observedAtMs: number;
  worksetId: string;
  taskId: string;
  total: number;
  nextCursor: number | null;
  outcomes: EvalCoordinateOutcome[];
};

export type EvalCase = {
  schemaVersion: number;
  taskName: string | null;
  prompt: string | null;
  status: string | null;
  outcome: string | null;
  environment: string | null;
  model: string | null;
  effort: string | null;
  finalMessage: string | null;
  toolCalls: number | null;
  usage: Record<string, unknown> | null;
  verifier: Record<string, unknown> | null;
  exception: Record<string, unknown> | null;
  timing: Record<string, unknown> | null;
  verifierStdout: string | null;
  verifierStderr: string | null;
};

export class EvalApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "EvalApiError";
    this.status = status;
  }
}

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(path, {
    headers: { accept: "application/json" },
    signal,
  });
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: unknown } | null;
    const detail = typeof body?.error === "string" ? body.error : `HTTP ${response.status}`;
    throw new EvalApiError(`Evaluation API request failed: ${detail}`, response.status);
  }
  return response.json() as Promise<T>;
}

export class EvalApiClient {
  overview(signal?: AbortSignal) {
    return getJson<EvalOverview>("/api/evals", signal);
  }

  cluster(signal?: AbortSignal) {
    return getJson<EvalCluster>("/api/evals/cluster", signal);
  }

  workset(id: string, signal?: AbortSignal) {
    return getJson<EvalWorksetDetail>(`/api/evals/worksets/${encodeURIComponent(id)}`, signal);
  }

  worksetAnalytics(id: string, signal?: AbortSignal) {
    return getJson<EvalWorksetAnalytics>(
      `/api/evals/worksets/${encodeURIComponent(id)}/analytics`,
      signal,
    );
  }

  task(worksetId: string, taskId: string, signal?: AbortSignal) {
    return getJson<EvalTaskSnapshot>(
      `/api/evals/worksets/${encodeURIComponent(worksetId)}/tasks/${encodeURIComponent(taskId)}`,
      signal,
    );
  }

  taskOutcomes(worksetId: string, taskId: string, cursor: number, signal?: AbortSignal) {
    return getJson<EvalTaskOutcomesPage>(
      `/api/evals/worksets/${encodeURIComponent(worksetId)}/tasks/${encodeURIComponent(taskId)}/outcomes?cursor=${cursor}&limit=8`,
      signal,
    );
  }

  evalCase(id: string, signal?: AbortSignal) {
    return getJson<EvalCase>(`/api/evals/cases/${encodeURIComponent(id)}`, signal);
  }
}

export const evalApi = new EvalApiClient();
