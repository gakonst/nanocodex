import { ROUTING_CANDIDATES } from "./thread-model-routing";
import { gatewayAvailability, type GatewaySecrets } from "./gateway-runtime";
import type { RoutingAi } from "./thread-model-routing";
import type { ProviderProbeOptions } from "./provider-probes";

export const PROBE_INTERVAL_MS = 30 * 60_000;
export const PROBE_SCHEDULE = "*/30 * * * *";
export const PROBE_OWNER = "deployment-provider-probes-v1";
export interface ProviderProbeEnvironment extends GatewaySecrets {
  AI?: RoutingAi;
  NANOCODEX_PROVIDER_PROBES?: string;
  NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT?: string;
}
export function probeDailyLimit(env: ProviderProbeEnvironment): number {
  const value = Number(env.NANOCODEX_PROVIDER_PROBE_DAILY_LIMIT ?? 1600);
  return Number.isInteger(value) && value >= 1 && value <= 4096 ? value : 0;
}
/** This is deployment-owned API capacity. A ChatGPT subscription has no global
 * credential; it stays eligible in routing with unknown background-probe TTFT. */
export function configuredProbeTargets(env: ProviderProbeEnvironment): ProviderProbeOptions["targets"] {
  const available = gatewayAvailability(env);
  return ROUTING_CANDIDATES.flatMap<ProviderProbeOptions["targets"][number]>(candidate => {
    const { backend, provider_model: model, thinking: effort } = candidate;
    if (backend === "workers_ai") return env.AI ? [{ backend, model, effort }] : [];
    if (backend === "cloudflare") {
      if (available.cloudflare !== true) return [];
      if (env.CLOUDFLARE_AI_API_TOKEN !== undefined || env.NANOCODEX_CLOUDFLARE_ACCOUNT_ID !== undefined) {
        return [{ backend, model, effort, key: env.CLOUDFLARE_AI_API_TOKEN, accountId: env.NANOCODEX_CLOUDFLARE_ACCOUNT_ID }];
      }
      return [{ backend, model, effort }];
    }
    if (backend !== "openrouter" && backend !== "vercel" || !available[backend]) return [];
    return [{ backend, model, effort, key: backend === "openrouter" ? env.OPENROUTER_API_KEY! : env.AI_GATEWAY_API_KEY! }];
  });
}
/** SQL uniqueness makes duplicate cron events/restarts at-most-once per slot.
 * Failed probes still consume their already-reserved durable request budget. */
export function claimProbeSlot(sql: { exec(query: string, ...bindings: any[]): any }, scheduledTime: number): boolean {
  if (!Number.isSafeInteger(scheduledTime) || scheduledTime < 0) return false;
  const slot = Math.floor(scheduledTime / PROBE_INTERVAL_MS);
  sql.exec("CREATE TABLE IF NOT EXISTS provider_probe_ticks (slot INTEGER PRIMARY KEY)");
  const claimed = [...sql.exec("INSERT OR IGNORE INTO provider_probe_ticks(slot) VALUES (?) RETURNING slot", slot)].length === 1;
  sql.exec("DELETE FROM provider_probe_ticks WHERE slot < ?", slot - 96);
  return claimed;
}

/** Spread the daily cap over all cron slots and rotate the bounded target slice.
 * A 45-target catalog needs 1620 requests/day to guarantee three samples in every
 * two-hour/four-slot window. At the default 1600 cap a few cohorts stay sparse;
 * routing must retain its minimum-three gate rather than invent evidence. */
export function probeSlotAllocation(scheduledTime: number, dailyLimit: number, targetCount: number) {
  if (!Number.isSafeInteger(scheduledTime) || scheduledTime < 0
    || !Number.isInteger(dailyLimit) || dailyLimit < 1 || dailyLimit > 4096
    || !Number.isInteger(targetCount) || targetCount < 1) return { startIndex: 0, maxTargetsPerRun: 0 };
  const slotsPerDay = 86_400_000 / PROBE_INTERVAL_MS;
  const slot = Math.floor(scheduledTime / PROBE_INTERVAL_MS);
  const before = Math.floor(slot * dailyLimit / slotsPerDay);
  const after = Math.floor((slot + 1) * dailyLimit / slotsPerDay);
  return { startIndex: before % targetCount, maxTargetsPerRun: Math.min(targetCount, 45, after - before) };
}
