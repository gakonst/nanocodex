/** Content-free measurements. HTTP headers are NOT generation TTFT. */
export interface ProviderObservation {
  timestamp: number;
  source: "live" | "probe";
  workerColo: string | null;
  clientIngressColo: string | null;
  backend: string;
  model: string;
  effort: string | null;
  outcome: "success" | "http_error" | "network_error" | "protocol_error" | "timeout" | "cancelled";
  status: number | null;
  headersMs: number | null;
  fullResponseMs: number | null;
  generationTtftMs: number | null;
  /** End-to-end delivery requires client acknowledgement; fetch completion cannot supply it. */
  clientDeliveryMs: number | null;
  elapsedMs: number;
}
export interface ProviderTelemetryStore {
  append(observation: ProviderObservation): void | Promise<void>;
  /** Must atomically reserve before fetch; durable shared per budget owner, including retries. */
  reserveProbe(day: string, limit: number): boolean | Promise<boolean>;
}
export const PROVIDER_TELEMETRY_WINDOW_MS = 2 * 60 * 60 * 1000;
export const PROVIDER_TTFT_MINIMUM_SAMPLES = 3;
const MAX_MEASUREMENT_MS = 86_400_000;
const validDuration = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= MAX_MEASUREMENT_MS;

export function summarizeProviderObservations(samples: ProviderObservation[], now: number,
  options = { windowMs: PROVIDER_TELEMETRY_WINDOW_MS, minimumSamples: PROVIDER_TTFT_MINIMUM_SAMPLES, alpha: 0.3 }) {
  const fresh = samples.filter(x => Number.isFinite(x.timestamp) && x.timestamp >= 0
    && x.timestamp <= now && now - x.timestamp <= options.windowMs)
    .sort((a, b) => a.timestamp - b.timestamp);
  // Workers AI bindings validate generation protocol without exposing HTTP headers/status.
  const successes = fresh.filter(x => x.outcome === "success"
    && ((x.status !== null && x.status >= 200 && x.status < 300) || (x.backend === "workers_ai" && x.status === null)));
  const full = successes.filter(x => validDuration(x.fullResponseMs));
  const ttft = successes.filter(x => validDuration(x.generationTtftMs));
  const stats = (values: number[]) => {
    const sorted = [...values].sort((a, b) => a - b), middle = Math.floor(sorted.length / 2);
    return {
      p50: sorted.length ? (sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2) : null,
      ewma: values.length ? values.slice(1).reduce((a, x) => options.alpha * x + (1 - options.alpha) * a, values[0]) : null,
    };
  };
  const fullStats = stats(full.map(x => x.fullResponseMs!)), ttftStats = stats(ttft.map(x => x.generationTtftMs!));
  const lastObservedAt = fresh.at(-1)?.timestamp ?? null;
  const lastTtftObservedAt = ttft.at(-1)?.timestamp ?? null;
  const failures = fresh.length - successes.length;
  return {
    signalKind: "context_only_not_completion_probability" as const,
    windowMs: options.windowMs, minimumSamples: options.minimumSamples,
    sampleCount: fresh.length, successCount: successes.length, censoredCount: failures,
    availabilityFailureCount: failures,
    httpErrorCount: fresh.filter(x => x.outcome === "http_error").length,
    networkErrorCount: fresh.filter(x => x.outcome === "network_error").length,
    protocolErrorCount: fresh.filter(x => x.outcome === "protocol_error").length,
    timeoutCount: fresh.filter(x => x.outcome === "timeout").length,
    cancelledCount: fresh.filter(x => x.outcome === "cancelled").length,
    successRate: fresh.length ? successes.length / fresh.length : null,
    lastObservedAt, ageMs: lastObservedAt === null ? null : now - lastObservedAt,
    lastTtftObservedAt, ttftAgeMs: lastTtftObservedAt === null ? null : now - lastTtftObservedAt,
    usable: ttft.length >= options.minimumSamples || full.length >= options.minimumSamples,
    generationTtftSampleCount: ttft.length,
    generationTtftP50Ms: ttftStats.p50, generationTtftEwmaMs: ttftStats.ewma,
    fullResponseSampleCount: full.length,
    fullResponseP50Ms: fullStats.p50, fullResponseEwmaMs: fullStats.ewma,
  };
}
/** Probes describe the deployment, even if the scheduler's actual colo is known.
 * Live attempts stay grouped by Worker execution colo. Never use client ingress as execution location. */
export function providerObservationKey(x: ProviderObservation): string {
  return JSON.stringify([x.source, x.source === "probe" ? null : x.workerColo, x.backend, x.model, x.effort]);
}
export function summarizeProviderObservationGroups(samples: ProviderObservation[], now: number) {
  const groups = new Map<string, ProviderObservation[]>();
  for (const sample of samples) {
    const key = providerObservationKey(sample);
    const group = groups.get(key) ?? [];
    group.push(sample);
    groups.set(key, group);
  }
  return [...groups.values()].map(group => {
    const { source, backend, model, effort } = group[0];
    return {
      source, backend, model, effort,
      scope: source === "probe" ? "deployment_global" as const : "worker_colo" as const,
      workerColo: source === "probe" ? null : group[0].workerColo,
      ...summarizeProviderObservations(group, now),
    };
  });
}
/** Attach to an existing sharded DO SQLite storage (tenant/thread or regional probe shard). */
export class SqliteProviderTelemetryStore implements ProviderTelemetryStore {
  private sql: { exec(query: string, ...bindings: any[]): any };
  constructor(sql: { exec(query: string, ...bindings: any[]): any }) {
    this.sql = sql;
    sql.exec("CREATE TABLE IF NOT EXISTS provider_observations (id INTEGER PRIMARY KEY, timestamp INTEGER NOT NULL, sample TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS provider_probe_budget (day TEXT PRIMARY KEY, count INTEGER NOT NULL)");
  }
  append(x: ProviderObservation): void {
    // Explicit field projection prevents accidental prompt, error-body, or credential persistence.
    const { timestamp, source, workerColo, clientIngressColo, backend, model, effort, outcome, status,
      headersMs, fullResponseMs, generationTtftMs, clientDeliveryMs, elapsedMs } = x;
    this.sql.exec("INSERT INTO provider_observations(timestamp,sample) VALUES (?,?)", timestamp,
      JSON.stringify({ timestamp, source, workerColo, clientIngressColo, backend, model, effort, outcome, status,
        headersMs, fullResponseMs, generationTtftMs, clientDeliveryMs, elapsedMs }));
    this.sql.exec("DELETE FROM provider_observations WHERE id NOT IN (SELECT id FROM provider_observations ORDER BY id DESC LIMIT 512)");
  }
  reserveProbe(day: string, limit: number): boolean {
    if (!Number.isInteger(limit) || limit < 1 || limit > 4096) return false;
    const rows = [...this.sql.exec("INSERT INTO provider_probe_budget(day,count) VALUES (?,1) ON CONFLICT(day) DO UPDATE SET count=count+1 WHERE count < ? RETURNING count", day, limit)];
    this.sql.exec("DELETE FROM provider_probe_budget WHERE day < ?", day);
    return rows.length === 1;
  }
  read(): ProviderObservation[] {
    return [...this.sql.exec("SELECT sample FROM provider_observations ORDER BY id")].map(row => JSON.parse(row.sample));
  }
}

export type LiveProviderMetadata = Pick<ProviderObservation,
  "workerColo" | "clientIngressColo" | "backend" | "model" | "effort">;
/** One instance per actual outbound attempt. Call headers after fetch resolves and
 * finish after body consumption, or finish with a failure outcome on catch.
 * Persistence failure returns false and must not fail a user's generation.
 */
export function beginLiveProviderObservation(
  metadata: LiveProviderMetadata,
  store: Pick<ProviderTelemetryStore, "append">,
  clock: { wallNow: () => number; monotonicNow: () => number } = {
    wallNow: Date.now, monotonicNow: () => performance.now(),
  },
) {
  const timestamp = clock.wallNow();
  const started = clock.monotonicNow();
  let headersMs: number | null = null;
  let status: number | null = null;
  let finished = false;
  const elapsed = () => Math.max(0, clock.monotonicNow() - started);
  return {
    headers(httpStatus: number) {
      if (finished || headersMs !== null) return;
      status = httpStatus;
      headersMs = elapsed();
    },
    async finish(outcome: ProviderObservation["outcome"]): Promise<boolean> {
      if (finished) return false;
      finished = true;
      const elapsedMs = elapsed();
      // A success requires an observed successful HTTP status and consumed body.
      const resolvedOutcome = outcome === "success" && (status === null || status < 200 || status >= 300)
        ? "http_error" : outcome;
      const { workerColo, clientIngressColo, backend, model, effort } = metadata;
      try {
        await store.append({ timestamp, source: "live", workerColo, clientIngressColo, backend, model, effort,
          outcome: resolvedOutcome, status, headersMs, elapsedMs,
          fullResponseMs: resolvedOutcome === "success" ? elapsedMs : null,
          generationTtftMs: null, clientDeliveryMs: null });
        return true;
      } catch { return false; }
    },
  };
}
