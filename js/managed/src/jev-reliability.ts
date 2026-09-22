import type { RoutingAi } from "./thread-model-routing";
export type JevFailure = "timeout" | "rate_limited" | "unavailable" | "binding_error" | "invalid_result";
export type JevAttempt = { duration_ms: number; outcome: "success" | JevFailure };
export type JevDiagnostics = { outcome: "success" | JevFailure | "not_requested" | "unsupported_input"; attempts: JevAttempt[] };
/** Classify in memory, persist only this fixed vocabulary. Never retain error text. */
export function jevFailure(error: unknown): JevFailure {
  const value = error as { status?: unknown; statusCode?: unknown; message?: unknown; name?: unknown } | null;
  const status = Number(value?.status ?? value?.statusCode);
  const message = typeof value?.message === "string" ? value.message : "";
  if (value?.name === "TimeoutError") return "timeout";
  if (status === 429 || /\b429\b|rate.?limit/i.test(message)) return "rate_limited";
  if (status >= 400 && status < 500) return "binding_error";
  if ([500, 502, 503, 504].includes(status) || /\b(?:500|502|503|504)\b|internal server error|temporarily unavailable|network error|fetch failed|connection reset|overloaded/i.test(message)) return "unavailable";
  return "binding_error";
}
/** At most one retry of an explicitly transient binding failure, within a shared
 * deadline. No retry of timeouts (binding cancellation is unavailable), rate
 * limits, invalid payloads or unknown failures. Never retries generation. */
export async function runJev(ai: RoutingAi, input: unknown, diagnostics: JevDiagnostics, budgetMs = 10_000, signal?: AbortSignal): Promise<unknown> {
  const deadline = Date.now() + budgetMs;
  for (let i = 0; i < 2; i++) {
    const started = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const result = await Promise.race([
        ai.run("typesafe/jev", input),
        new Promise<never>((_, reject) => { timer = setTimeout(() => reject(Object.assign(new Error("Jev deadline"), { name: "TimeoutError" })), Math.max(0, deadline - Date.now())); }),
      ]);
      diagnostics.attempts.push({ duration_ms: Date.now() - started, outcome: "success" });
      diagnostics.outcome = "success";
      return result;
    } catch (error) {
      const failure = jevFailure(error);
      diagnostics.attempts.push({ duration_ms: Date.now() - started, outcome: failure });
      diagnostics.outcome = failure;
      if (signal?.aborted || failure !== "unavailable" || i !== 0 || deadline - Date.now() < 500) throw error;
    } finally { clearTimeout(timer); }
  }
  throw new Error("Jev retry budget exhausted");
}
