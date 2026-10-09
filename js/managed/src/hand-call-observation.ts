import { recordDiagnostic } from "./diagnostic-journal";

/** Passive, per-call Hand timings. Never log input, output, paths, IDs of machines,
 * credentials, error messages, or a provider-supplied tool name. Clocks measure
 * the local awaited boundary, not time before a Worker starts or after it returns.
 */
export type HandCallOutcome = "ok" | "failed" | "unavailable" | "ambiguous" | "cancelled";
export type HandCallStage = "namespace.prepare" | "namespace.host_readiness" | "namespace.account_discovery" | "namespace.selected_lookup" | "namespace.route" | "namespace.invoke" | "namespace.cua.queue"
  | "account.decode_input" | "account.ownership" | "account.resolve" | "account.handler" | "account.fetch"
  | "account.decode" | "account.receipt" | "sandbox.preflight" | "sandbox.invoke";

export function handToolKind(name: string): string {
  switch (name) {
    case "exec_command": case "write_stdin": case "preview": case "native_secure_input": case "validate_app":
      return name;
    case "mcp__cua_repl__js": return "cua";
    case "mcp__cua_repl__js_reset": return "cua_reset";
    default: return "other";
  }
}

/** A caller may emit several stages for the same call; call_id is an opaque
 * correlation key and never derived from user-supplied input or provider output. */
export function observeHandCall(
  stage: HandCallStage, name: string, started: number, outcome: HandCallOutcome,
  callId?: string,
  correlation?: Readonly<{ session_id?: string; thread_id?: string; turn_id?: string; parent_call_id?: string }>,
  endedAt = performance.now(),
): void {
  try {
    const duration = endedAt - started;
    const ids = handCallCorrelation({ ...correlation, ...(correlation ? { source_call_id: callId } : {}) });
    const record = { type: "hand.tool.stage", stage, tool: handToolKind(name), outcome,
      duration_ms: Number.isFinite(duration) ? Math.max(0, duration) : 0,
      // Tool contexts normally issue opaque IDs. Do not emit arbitrary strings.
      ...(callId && /^[A-Za-z0-9_./:-]{1,128}$/.test(callId) ? { call_id: callId } : {}),
      ...ids,
    };
    recordDiagnostic(record);
    console.info(record);
  } catch { /* Observation must not alter the tool's outcome. */ }
}

export function observeHandSummary(
  type: "hand.call.account" | "hand.call.provider", name: string,
  correlation: Parameters<typeof handCallCorrelation>[0],
  durations: Readonly<{ input_decode_ms?: number; ownership_ms?: number; resolve_ms?: number; handler_ms?: number; fetch_ms?: number; decode_ms?: number; total_ms: number }>,
  outcome: HandCallOutcome,
): void {
  try {
    const record = { type, tool: handToolKind(name), ...handCallCorrelation(correlation), ...durations, outcome };
    recordDiagnostic(record);
    console.info(record);
  } catch { /* Passive diagnostics cannot change a response. */ }
}

export function handCallCorrelation(ids: Readonly<{
  session_id?: string; thread_id?: string; source_call_id?: string; turn_id?: string; parent_call_id?: string;
}>): Record<string, string> {
  return Object.fromEntries(Object.entries(ids).filter((entry): entry is [string, string] =>
    typeof entry[1] === "string" && /^[A-Za-z0-9_./:-]{1,128}$/.test(entry[1])));
}
