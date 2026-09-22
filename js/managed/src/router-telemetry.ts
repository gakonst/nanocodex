import { z } from "zod";
import { ROUTING_CANDIDATES, projectThreadRouteDiagnostics, type ThreadRoute } from "./thread-model-routing";
import { PROVIDER_TELEMETRY_WINDOW_MS } from "./provider-telemetry";
const candidate = z.string().refine(id => ROUTING_CANDIDATES.some(c => c.id === id));
const failure = z.enum(["success", "timeout", "rate_limited", "unavailable", "binding_error", "invalid_result"]);
const schema = z.object({
  timestamp: z.number().int().nonnegative(),
  clientIngressColo: z.string().regex(/^[A-Z]{3}$/).nullable(),
  chosen: candidate,
  decision: z.enum(["accepted", "low", "unavailable_or_invalid", "not_requested", "unsupported_input"]),
  durationMs: z.number().nonnegative().max(86_400_000),
  classifier: z.object({ outcome: z.union([failure, z.enum(["not_requested", "unsupported_input"])]),
    attempts: z.array(z.object({ duration_ms: z.number().nonnegative().max(86_400_000), outcome: failure })).max(2) }),
  confidence: z.number().min(0).max(1).nullable(),
  probabilities: z.record(candidate, z.number().min(0).max(1)).nullable(),
});
export type RouterObservation = z.infer<typeof schema>;
/** Explicit projection before RPC and again before persistence. No prompts,
 * policies, account IDs, session IDs, raw errors or provider bodies. */
export function routeObservation(route: ThreadRoute, clientIngressColo: string | null): RouterObservation | null {
  if (!route.classifier) return null; // Historical/legacy routes lack attempt telemetry.
  const diagnostics = projectThreadRouteDiagnostics(route);
  const chosen = ROUTING_CANDIDATES.find(c => c.backend === route.backend && c.model === route.model && c.thinking === route.thinking);
  return parseRouterObservation({ timestamp: Date.now(), clientIngressColo, chosen: chosen?.id,
    decision: route.classifier.outcome === "not_requested" || route.classifier.outcome === "unsupported_input"
      ? route.classifier.outcome : diagnostics?.confidence_status ?? "unavailable_or_invalid",
    durationMs: route.router_duration_ms, classifier: route.classifier,
    confidence: diagnostics?.candidate_confidence ?? null, probabilities: diagnostics?.candidate_probabilities ?? null }, Date.now());
}
export function parseRouterObservation(value: unknown, now: number): RouterObservation | null {
  const parsed = schema.safeParse(value);
  if (!parsed.success || parsed.data.timestamp > now || now - parsed.data.timestamp > PROVIDER_TELEMETRY_WINDOW_MS) return null;
  return parsed.data;
}
export class RouterTelemetryStore {
  constructor(private sql: { exec(query: string, ...bindings: any[]): any }) {
    sql.exec("CREATE TABLE IF NOT EXISTS router_observations (id INTEGER PRIMARY KEY, timestamp INTEGER NOT NULL, sample TEXT NOT NULL)");
  }
  append(value: unknown, now = Date.now()): boolean {
    const sample = parseRouterObservation(value, now);
    if (!sample) return false;
    this.sql.exec("INSERT INTO router_observations(timestamp,sample) VALUES (?,?)", sample.timestamp, JSON.stringify(sample));
    this.sql.exec("DELETE FROM router_observations WHERE timestamp < ? OR id NOT IN (SELECT id FROM router_observations ORDER BY id DESC LIMIT 512)", now - PROVIDER_TELEMETRY_WINDOW_MS);
    return true;
  }
  read(now = Date.now()): RouterObservation[] {
    return [...this.sql.exec("SELECT sample FROM router_observations WHERE timestamp >= ? ORDER BY id DESC LIMIT 512", now - PROVIDER_TELEMETRY_WINDOW_MS)]
      .flatMap(row => { const parsed = parseRouterObservation(JSON.parse(row.sample), now); return parsed ? [parsed] : []; });
  }
}
