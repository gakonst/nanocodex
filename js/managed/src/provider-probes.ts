import type { ProviderObservation, ProviderTelemetryStore } from "./provider-telemetry";
const ENDPOINTS = {
  openrouter: "https://openrouter.ai/api/v1/chat/completions",
  vercel: "https://ai-gateway.vercel.sh/v1/chat/completions",
} as const;
export interface ProviderProbeOptions {
  enabled: boolean;
  dailyRequestLimit: number;
  targets: { backend: keyof typeof ENDPOINTS; model: string; key: string }[];
  store: ProviderTelemetryStore;
  /** Actual executing Worker location, supplied from trusted runtime observation; null if unknown.
   * Client request.cf.colo does not establish a Smart Placement execution location. */
  workerColo: string | null;
  fetch?: typeof fetch;
  now?: () => number;
  timeoutMs?: number;
}
/** Call from scheduled(event,env,ctx) via ctx.waitUntil(runProviderProbes(...)).
 * No cron registration or paid requests are enabled by importing this module.
 * At most two sequential requests/tick and 100/day per durable budget shard.
 * Configure ONE budget owner per schedule; multiplying shards multiplies spend limits.
 */
export async function runProviderProbes(options: ProviderProbeOptions): Promise<number> {
  if (!options.enabled) return 0;
  if (!Number.isInteger(options.dailyRequestLimit) || options.dailyRequestLimit < 1 || options.dailyRequestLimit > 100) return 0;
  const now = options.now ?? Date.now;
  let attempted = 0;
  for (const target of options.targets.slice(0, 2)) {
    if (!Object.hasOwn(ENDPOINTS, target.backend) || !target.key || !/^[a-zA-Z0-9_./:@-]{1,160}$/.test(target.model)) continue;
    const timestamp = now();
    if (!await options.store.reserveProbe(new Date(timestamp).toISOString().slice(0, 10), options.dailyRequestLimit)) break;
    attempted++;
    const controller = new AbortController();
    const timeoutMs = Math.max(1, Math.min(options.timeoutMs ?? 10_000, 30_000));
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    const observation: ProviderObservation = { timestamp, source: "probe", workerColo: options.workerColo,
      clientIngressColo: null, backend: target.backend, model: target.model, effort: null,
      outcome: "network_error", status: null, headersMs: null, fullResponseMs: null,
      generationTtftMs: null, clientDeliveryMs: null, elapsedMs: 0 };
    try {
      const response = await (options.fetch ?? fetch)(ENDPOINTS[target.backend], {
        method: "POST", redirect: "error", signal: controller.signal,
        headers: { authorization: `Bearer ${target.key}`, "content-type": "application/json" },
        body: JSON.stringify({ model: target.model, messages: [{ role: "user", content: "Reply OK." }], max_tokens: 8, stream: false }),
      });
      observation.headersMs = Math.max(0, now() - timestamp);
      observation.status = response.status;
      // Drain with a bounded byte budget; never log/store provider content.
      const reader = response.body?.getReader();
      if (reader) {
        let bytes = 0;
        while (true) {
          const chunk = await reader.read();
          if (chunk.done) break;
          bytes += chunk.value.byteLength;
          if (bytes > 65_536) { await reader.cancel(); throw new Error("probe response exceeds bound"); }
        }
      }
      observation.outcome = response.ok ? "success" : "http_error";
      if (response.ok) observation.fullResponseMs = Math.max(0, now() - timestamp);
    } catch {
      observation.outcome = controller.signal.aborted ? "timeout" : "network_error";
    } finally {
      clearTimeout(timer);
      observation.elapsedMs = Math.max(0, now() - timestamp);
      await options.store.append(observation);
    }
  }
  return attempted;
}
