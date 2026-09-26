/** Passive, per-call Hand timings. Never log input, output, paths, IDs of machines,
 * credentials, error messages, or a provider-supplied tool name. Clocks measure
 * the local awaited boundary, not time before a Worker starts or after it returns.
 */
export type HandCallOutcome = "ok" | "failed" | "unavailable" | "ambiguous" | "cancelled";
export type HandCallStage = "namespace.route" | "namespace.invoke" | "namespace.cua.queue"
  | "account.ownership" | "account.resolve" | "account.handler" | "account.fetch"
  | "account.decode" | "sandbox.preflight";

export function handToolKind(name: string): string {
  switch (name) {
    case "exec_command": case "write_stdin": case "preview":
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
): void {
  try {
    const duration = performance.now() - started;
    console.info({ type: "hand.tool.stage", stage, tool: handToolKind(name), outcome,
      duration_ms: Number.isFinite(duration) ? Math.max(0, duration) : 0,
      // Tool contexts normally issue opaque IDs. Do not emit arbitrary strings.
      ...(callId && /^[A-Za-z0-9_./:-]{1,128}$/.test(callId) ? { call_id: callId } : {}),
    });
  } catch { /* Observation must not alter the tool's outcome. */ }
}
