import { DurableObject } from "cloudflare:workers";
import { runProviderProbes } from "./provider-probes";
import { SqliteProviderTelemetryStore, summarizeProviderObservationGroups } from "./provider-telemetry";
import { configuredProbeTargets, claimProbeSlot, probeDailyLimit, PROBE_INTERVAL_MS,
  type ProviderProbeEnvironment } from "./provider-probe-schedule";

/** Private service binding only: no public fetch route, credential setter or
 * user-controlled probe prompt. One deployment owner bounds aggregate spend. */
export class ProviderProbeCoordinator extends DurableObject<ProviderProbeEnvironment> {
  #running?: Promise<number>;
  #store: SqliteProviderTelemetryStore;
  constructor(ctx: DurableObjectState, env: ProviderProbeEnvironment) {
    super(ctx, env);
    this.#store = new SqliteProviderTelemetryStore(ctx.storage.sql);
  }
  async tick(scheduledTime: number): Promise<number> {
    if (this.env.NANOCODEX_PROVIDER_PROBES !== "true" || !probeDailyLimit(this.env)) return 0;
    // Late replayed events must not spend today's budget or change freshness.
    if (Math.abs(Date.now() - scheduledTime) > PROBE_INTERVAL_MS) return 0;
    if (this.#running) return 0;
    if (!claimProbeSlot(this.ctx.storage.sql, scheduledTime)) return 0;
    const pending = runProviderProbes({ enabled: true, dailyRequestLimit: probeDailyLimit(this.env),
      targets: configuredProbeTargets(this.env), store: this.#store, workerColo: null,
      ai: this.env.AI, maxTargetsPerRun: 45, timeoutMs: 10_000 });
    this.#running = pending;
    try { return await pending; } finally { if (this.#running === pending) this.#running = undefined; }
  }
  snapshot() {
    return summarizeProviderObservationGroups(this.#store.read(), Date.now());
  }
}
