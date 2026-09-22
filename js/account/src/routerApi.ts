export type RouterDecision = {
  timestamp: number; clientIngressColo: string | null; chosen: string;
  decision: string; durationMs: number; confidence: number | null;
  probabilities: Record<string, number> | null;
  classifier: { outcome: string; attempts: Array<{ duration_ms: number; outcome: string }> };
};
export type RouterProvider = {
  source: "live" | "probe"; backend: string; model: string; effort: string;
  scope: string; clientIngressColo: string | null; workerColo: string | null;
  sampleCount: number; successCount: number; censoredCount: number;
  generationTtftSampleCount: number; generationTtftP50Ms: number | null;
  generationTtftP95Ms: number | null; fullResponseP50Ms: number | null;
  lastObservedAt: number | null; lastTtftObservedAt: number | null;
  httpErrorCount: number; networkErrorCount: number; protocolErrorCount: number;
  timeoutCount: number; cancelledCount: number;
};
export type RouterSnapshot = { version: 1; capturedAt: number; windowMs: number; retentionLimit: number;
  probesEnabled: boolean; probeIntervalMs: number; providers: RouterProvider[]; decisions: RouterDecision[] };
export function summarizeRouter(decisions: RouterDecision[]) {
  const attempts = decisions.flatMap(d => d.classifier.attempts);
  return { decisions: decisions.length, attempts: attempts.length,
    bindingFailures: attempts.filter(a => a.outcome !== "success").length,
    recovered: decisions.filter(d => d.classifier.outcome === "success" && d.classifier.attempts.length > 1).length,
    low: decisions.filter(d => d.decision === "low").length,
    accepted: decisions.filter(d => d.decision === "accepted").length,
    bypassed: decisions.filter(d => d.decision === "not_requested").length };
}

export function routeProvider(candidate: string): string {
  if (candidate.startsWith("@cf/")) return "workers_ai";
  if (candidate.startsWith("gpt-")) return "chatgpt";
  return candidate.split(":")[0];
}
