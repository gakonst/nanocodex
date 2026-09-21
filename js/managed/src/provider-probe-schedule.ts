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
    if (backend === "cloudflare") return available.cloudflare === true && env.AI ? [{ backend, model, effort }] : [];
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
