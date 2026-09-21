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
    && ((x.status !== null && x.status >= 200 && x.status < 300) || ((x.backend === "workers_ai" || x.backend === "cloudflare") && x.status === null)));
  const full = successes.filter(x => validDuration(x.fullResponseMs));
  const ttft = successes.filter(x => validDuration(x.generationTtftMs));
  const stats = (values: number[]) => {
    const sorted = [...values].sort((a, b) => a - b), middle = Math.floor(sorted.length / 2);
    return {
      p50: sorted.length ? (sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2) : null,
      // Nearest-rank p95; absent data remains unknown.
      p95: sorted.length ? sorted[Math.ceil(sorted.length * .95) - 1] : null,
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
    generationTtftP50Ms: ttftStats.p50, generationTtftP95Ms: ttftStats.p95, generationTtftEwmaMs: ttftStats.ewma,
    fullResponseSampleCount: full.length,
    fullResponseP50Ms: fullStats.p50, fullResponseP95Ms: fullStats.p95, fullResponseEwmaMs: fullStats.ewma,
  };
}
/** Trusted ingress context is captured at the public Worker boundary, never from
 * request JSON/headers. An ingress colo is not evidence of Worker execution. */
export type ProviderOriginContext = {
  clientIngressColo?: string | null;
  workerColo?: string | null;
};
export type ProviderTelemetryScope = "client_ingress" | "worker_colo" | "deployment_global";
export const normalizeProviderColo = (value: unknown): string | null =>
  typeof value === "string" && /^[A-Z]{3}$/.test(value) ? value : null;
/** The default key preserves execution geography for legacy callers. Aggregates
 * explicitly select a scope; probes can only describe the deployment. */
export function providerObservationKey(x: ProviderObservation, scope: ProviderTelemetryScope = x.source === "probe" ? "deployment_global" : "worker_colo"): string {
  return JSON.stringify([x.source, scope, scope === "client_ingress" ? x.clientIngressColo
    : scope === "worker_colo" ? x.workerColo : null, x.backend, x.model, x.effort]);
}
export function summarizeProviderObservationGroups(samples: ProviderObservation[], now: number, origin?: ProviderOriginContext) {
  const groups = new Map<string, { scope: ProviderTelemetryScope; samples: ProviderObservation[] }>();
  for (const sample of samples) {
    const scopes: ProviderTelemetryScope[] = ["deployment_global"];
    if (sample.source === "live") {
      if (normalizeProviderColo(sample.clientIngressColo) && (origin === undefined
        || sample.clientIngressColo === normalizeProviderColo(origin.clientIngressColo))) scopes.push("client_ingress");
      if (normalizeProviderColo(sample.workerColo) && (origin === undefined
        || sample.workerColo === normalizeProviderColo(origin.workerColo))) scopes.push("worker_colo");
    }
    for (const scope of scopes) {
      const key = providerObservationKey(sample, scope);
      const group = groups.get(key) ?? { scope, samples: [] };
      group.samples.push(sample);
      groups.set(key, group);
    }
  }
  return [...groups.values()].map(({ scope, samples: group }) => {
    const { source, backend, model, effort } = group[0];
    return {
      source, backend, model, effort, scope,
      workerColo: scope === "worker_colo" ? group[0].workerColo : null,
      clientIngressColo: scope === "client_ingress" ? group[0].clientIngressColo : null,
      ...summarizeProviderObservations(group, now),
    };
  });
}
/** Private RPC validation is still required: accept bounded measurements only,
 * never bodies, prompts, identities, arbitrary labels or caller-supplied aggregates.
 * Success timing must be internally consistent; failures remain censored. */
export function projectLiveProviderObservation(value: unknown, now: number): ProviderObservation | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const x = value as Record<string, unknown>;
  if (x.source !== "live" || !["workers_ai", "chatgpt", "openrouter", "vercel", "cloudflare"].includes(x.backend as string)
    || typeof x.model !== "string" || x.model.length > 256 || !/^[@a-zA-Z0-9][a-zA-Z0-9_./:@-]*$/.test(x.model)
    || !(x.effort === null || ["none", "minimal", "low", "medium", "high", "xhigh", "max"].includes(x.effort as string))
    || !["success", "http_error", "network_error", "protocol_error", "timeout", "cancelled"].includes(x.outcome as string)
    || !Number.isSafeInteger(x.timestamp) || (x.timestamp as number) < 0 || (x.timestamp as number) > now
    || now - (x.timestamp as number) > PROVIDER_TELEMETRY_WINDOW_MS
    || !(x.workerColo === null || normalizeProviderColo(x.workerColo))
    || !(x.clientIngressColo === null || normalizeProviderColo(x.clientIngressColo))
    || !(x.status === null || (Number.isInteger(x.status) && (x.status as number) >= 100 && (x.status as number) <= 599))
    || !validDuration(x.elapsedMs) || x.clientDeliveryMs !== null) return null;
  for (const key of ["headersMs", "fullResponseMs", "generationTtftMs"] as const) {
    if (x[key] !== null && (!validDuration(x[key]) || (x[key] as number) > x.elapsedMs)) return null;
  }
  const success = x.outcome === "success";
  if (success && !(typeof x.status === "number" && x.status >= 200 && x.status < 300
    || x.status === null && (x.backend === "workers_ai" || x.backend === "cloudflare"))) return null;
  if (success && (!validDuration(x.fullResponseMs)
    || typeof x.headersMs === "number" && x.headersMs > x.fullResponseMs)) return null;
  if (!success && (x.fullResponseMs !== null || x.generationTtftMs !== null)) return null;
  if (x.headersMs !== null && x.status === null) return null;
  if (typeof x.generationTtftMs === "number" && (typeof x.fullResponseMs !== "number"
    || x.generationTtftMs > x.fullResponseMs || typeof x.headersMs === "number" && x.generationTtftMs < x.headersMs)) return null;
  const { timestamp, source, workerColo, clientIngressColo, backend, model, effort, outcome, status,
    headersMs, fullResponseMs, generationTtftMs, clientDeliveryMs, elapsedMs } = x;
  return { timestamp, source, workerColo, clientIngressColo, backend, model, effort, outcome, status,
    headersMs, fullResponseMs, generationTtftMs, clientDeliveryMs, elapsedMs } as ProviderObservation;
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
  let generationTtftMs: number | null = null;
  const elapsed = () => Math.max(0, clock.monotonicNow() - started);
  return {
    headers(httpStatus: number) {
      if (finished || headersMs !== null) return;
      status = httpStatus;
      headersMs = elapsed();
    },
    /** Call only when the transport emits its first nonempty public text delta or
     * validated tool event. Headers, roles, reasoning and hidden fragments do not count. */
    firstToken() {
      if (!finished && generationTtftMs === null) generationTtftMs = elapsed();
    },
    async finish(outcome: ProviderObservation["outcome"]): Promise<boolean> {
      if (finished) return false;
      finished = true;
      const elapsedMs = elapsed();
      // Bindings expose no HTTP status; their consumed protocol establishes success.
      const bindingSuccess = status === null && (metadata.backend === "workers_ai" || metadata.backend === "cloudflare");
      const resolvedOutcome = outcome === "success" && !bindingSuccess && (status === null || status < 200 || status >= 300)
        ? "http_error" : outcome;
      const { workerColo, clientIngressColo, backend, model, effort } = metadata;
      try {
        await store.append({ timestamp, source: "live", workerColo, clientIngressColo, backend, model, effort,
          outcome: resolvedOutcome, status, headersMs, elapsedMs,
          fullResponseMs: resolvedOutcome === "success" ? elapsedMs : null,
          generationTtftMs: resolvedOutcome === "success" ? generationTtftMs : null, clientDeliveryMs: null });
        return true;
      } catch { return false; }
    },
  };
}
