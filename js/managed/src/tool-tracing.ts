import { AsyncLocalStorage } from "node:async_hooks";
import { annotateActiveSpan, setSpanAttributes, tracing, type SpanAttributes, type TraceSpan } from "nanocodex/cloudflare/tracing";
import { recordDiagnostic } from "./diagnostic-journal";

type ToolTraceContext = Readonly<{ sessionId?: string; callId?: string; parentCallId?: string; turnId?: string }>;
const toolSpan = new AsyncLocalStorage<TraceSpan>();
const toolCorrelation = new AsyncLocalStorage<Readonly<Record<string, string>>>();

/** Trusted invocation IDs shared by nested operational diagnostics. */
export function currentToolCorrelation(): Readonly<Record<string, string>> {
  return toolCorrelation.getStore() ?? {};
}

/** Keep the actual entered span on pinned runtimes without getActiveSpan.
 * Separate WebSocket invocations can annotate only their own native context. */
export function annotateToolSpan(attributes: SpanAttributes): void {
  const span = toolSpan.getStore();
  if (span) setSpanAttributes(span, attributes);
  else annotateActiveSpan(attributes);
}

/** Native async parents join binding/DO subrequests. Logical IDs join fresh
 * WebSocket invocations and external Hands, which cannot share a native parent.
 * The original operation is memoized: a tracing failure must never resend it.
 */
export function traceToolInvocation<T>(
  operation: "nanocodex.tool" | "hand.account.invoke" | "hand.provider.invoke",
  threadId: string | undefined, name: string, context: ToolTraceContext,
  run: () => Promise<T>, managedTurnId?: string,
): Promise<T> {
  const ids = Object.fromEntries(Object.entries({ thread_id: threadId, runtime_session_id: context.sessionId,
    tool_call_id: context.callId, parent_call_id: context.parentCallId, host_turn_id: context.turnId,
    managed_turn_id: managedTurnId }).filter((entry): entry is [string, string] => safeId(entry[1])));
  const tool = safeId(name) ? name : "unknown";
  const attributes = Object.fromEntries(Object.entries({ ...ids, tool }).map(([key, value]) => [`nanocodex.${key}`, value]));
  let original: Promise<T> | undefined;
  const execute = () => {
    if (original) return original;
    const started = performance.now();
    const log = (stage: string, outcome?: string) => {
      const record = { type: "managed.tool.invocation", operation, ...ids, tool, stage,
        ...(outcome === undefined ? {} : { outcome, duration_ms: performance.now() - started }) };
      recordDiagnostic(record);
      try { console.info(record); }
      catch { /* Diagnostics cannot change tool behavior. */ }
    };
    log("started");
    // Code Mode registers its update queue synchronously before Rust's next
    // host callback. Enter the operation now, even though its result is async.
    try { original = Promise.resolve(toolCorrelation.run(Object.freeze(ids), run)); }
    catch (error) { original = Promise.reject(error); }
    original = original.then(result => { log("finished", "completed"); return result; },
      error => { log("finished", "failed"); throw error; });
    return original;
  };
  try {
    const traced = tracing.enterSpan(operation, span => {
      try { setSpanAttributes(span, attributes); } catch { /* Keep the original operation. */ }
      return toolSpan.run(span, execute);
    });
    void Promise.resolve(traced).catch(() => undefined);
  } catch { /* Missing tracing must not replace the original operation. */ }
  return execute();
}

function safeId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_./:-]{1,160}$/.test(value);
}
