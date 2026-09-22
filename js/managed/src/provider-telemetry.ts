/** Content-free measurements. HTTP headers are NOT generation TTFT. */
export interface ProviderObservation {
  timestamp: number;
  source: "live" | "probe";
  workerColo: string | null;
  clientIngressColo: string | null;
  backend: string;
  model: string;
  effort: string | null;
  outcome: "success" | "http_error" | "network_error" | "timeout" | "cancelled";
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
export function summarizeProviderObservations(samples: ProviderObservation[], now: number,
  options = { windowMs: 300_000, minimumSamples: 5, alpha: 0.3 }) {
  const fresh = samples.filter(x => x.timestamp <= now && now - x.timestamp <= options.windowMs)
    .sort((a, b) => a.timestamp - b.timestamp);
  const successes = fresh.filter(x => x.outcome === "success" && x.fullResponseMs !== null);
  const values = successes.map(x => x.fullResponseMs!);
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return {
    signalKind: "context_only_not_completion_probability" as const,
    sampleCount: fresh.length, successCount: successes.length,
    censoredCount: fresh.filter(x => x.outcome !== "success").length,
    successRate: fresh.length ? successes.length / fresh.length : null,
    lastObservedAt: fresh.at(-1)?.timestamp ?? null,
    usable: successes.length >= options.minimumSamples,
    fullResponseP50Ms: sorted.length ? (sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2) : null,
    fullResponseEwmaMs: values.length ? values.slice(1).reduce((a, x) => options.alpha * x + (1 - options.alpha) * a, values[0]) : null,
  };
}
/** Group by source+Worker colo+backend+model+effort before summarizing. Never pool regional claims. */
export function providerObservationKey(x: ProviderObservation): string {
  return JSON.stringify([x.source, x.workerColo, x.backend, x.model, x.effort]);
}
/** Attach to an existing sharded DO SQLite storage (tenant/thread or regional probe shard). */
export class SqliteProviderTelemetryStore implements ProviderTelemetryStore {
  constructor(private sql: { exec(query: string, ...bindings: any[]): any }) {
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
    if (!Number.isInteger(limit) || limit < 1 || limit > 100) return false;
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
