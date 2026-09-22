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
// Routing is a foreground admission step. Do not spend a generation-sized
// deadline retrying a classifier: an unavailable result has an explicit fallback.
export const JEV_ROUTING_BUDGET_MS = 2_000;
/** One remote classification attempt. A timeout never launches another binding
 * call because the binding cannot be cancelled. Error text is never retained. */
export async function runJev(ai: RoutingAi, input: unknown, diagnostics: JevDiagnostics, budgetMs = JEV_ROUTING_BUDGET_MS, signal?: AbortSignal): Promise<unknown> {
  // A cancelled request did not invoke the binding; do not record an attempt.
  signal?.throwIfAborted();
  const started = Date.now();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let abort: (() => void) | undefined;
  try {
    const cancelled = new Promise<never>((_resolve, reject) => {
      if (signal) {
        abort = () => reject(signal.reason);
        signal.addEventListener("abort", abort, { once: true });
      }
    });
    const result = await Promise.race([
      // Register the cancellation race before invoking a binding that may abort
      // synchronously. A queued cancellation must not start another request.
      Promise.resolve().then(() => { signal?.throwIfAborted(); return ai.run("typesafe/jev", input); }),
      cancelled,
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(Object.assign(new Error("Jev deadline"), { name: "TimeoutError" })), budgetMs); }),
    ]);
    signal?.throwIfAborted();
    diagnostics.attempts.push({ duration_ms: Date.now() - started, outcome: "success" });
    diagnostics.outcome = "success";
    return result;
  } catch (error) {
    // Cancellation is propagated, never published as a classifier failure or a
    // fallback admission. The underlying binding still cannot be cancelled.
    signal?.throwIfAborted();
    const failure = jevFailure(error);
    diagnostics.attempts.push({ duration_ms: Date.now() - started, outcome: failure });
    diagnostics.outcome = failure;
    throw error;
  } finally {
    clearTimeout(timer);
    if (abort) signal?.removeEventListener("abort", abort);
  }
}
