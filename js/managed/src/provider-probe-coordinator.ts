import { RouterTelemetryStore } from "./router-telemetry";
import { DurableObject } from "cloudflare:workers";
import { ROUTING_CANDIDATES } from "./thread-model-routing";
import { runProviderProbes } from "./provider-probes";
import { SqliteProviderTelemetryStore, summarizeProviderObservationGroups, projectLiveProviderObservation, type ProviderOriginContext } from "./provider-telemetry";
import { configuredProbeTargets, claimProbeSlot, probeDailyLimit, probeSlotAllocation, PROBE_INTERVAL_MS,
  type ProviderProbeEnvironment } from "./provider-probe-schedule";

/** Private service binding only: no public fetch route, credential setter or
 * user-controlled probe prompt. One deployment owner bounds aggregate spend. */
export class ProviderProbeCoordinator extends DurableObject<ProviderProbeEnvironment> {
  #running?: Promise<number>;
  #store: SqliteProviderTelemetryStore;
  #router: RouterTelemetryStore;
  constructor(ctx: DurableObjectState, env: ProviderProbeEnvironment) {
    super(ctx, env);
    this.#store = new SqliteProviderTelemetryStore(ctx.storage.sql);
    this.#router = new RouterTelemetryStore(ctx.storage.sql);
  }
  async tick(scheduledTime: number): Promise<number> {
    if (this.env.NANOCODEX_PROVIDER_PROBES !== "true" || !probeDailyLimit(this.env)) return 0;
    // Late replayed events must not spend today's budget or change freshness.
    if (Math.abs(Date.now() - scheduledTime) > PROBE_INTERVAL_MS) return 0;
    if (this.#running) return 0;
    if (!claimProbeSlot(this.ctx.storage.sql, scheduledTime)) return 0;
    const targets = configuredProbeTargets(this.env);
    const allocation = probeSlotAllocation(scheduledTime, probeDailyLimit(this.env), targets.length);
    if (!allocation.maxTargetsPerRun) return 0;
    const pending = runProviderProbes({ enabled: true, dailyRequestLimit: probeDailyLimit(this.env),
      targets, store: this.#store, workerColo: null,
      ai: this.env.AI, ...allocation, timeoutMs: 10_000 });
    this.#running = pending;
    try { return await pending; } finally { if (this.#running === pending) this.#running = undefined; }
  }
  /** Private service-binding RPC only. No public route accepts observations. */
  observe(value: unknown): boolean {
    const sample = projectLiveProviderObservation(value, Date.now());
    if (!sample) return false;
    const candidate = ROUTING_CANDIDATES.find(candidate => candidate.backend === sample.backend
      && candidate.thinking === sample.effort && (candidate.model === sample.model || candidate.provider_model === sample.model));
    if (!candidate) return false;
    // Aliases describe the same candidate; retain one canonical live cohort.
    try { this.#store.append({ ...sample, model: candidate.model }); return true; } catch { return false; }
  }
  observeRoute(value: unknown): boolean { return this.#router.append(value); }
  dashboardSnapshot() {
    const now = Date.now();
    const rows = this.#store.read().filter(row => row.timestamp <= now && now - row.timestamp <= 7_200_000);
    return { version: 1, capturedAt: now, windowMs: 7_200_000, retentionLimit: 512,
      probesEnabled: this.env.NANOCODEX_PROVIDER_PROBES === "true", probeIntervalMs: PROBE_INTERVAL_MS,
      providers: summarizeProviderObservationGroups(rows, now), decisions: this.#router.read(now) };
  }
  snapshot(origin?: ProviderOriginContext) {
    // Explicit field projection also bounds malformed private callers.
    const context = origin && typeof origin === "object" ? {
      clientIngressColo: origin.clientIngressColo, workerColo: origin.workerColo,
    } : {};
    return summarizeProviderObservationGroups(this.#store.read(), Date.now(), context);
  }
}
