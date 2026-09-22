import type { AgentEvent } from "nanocodex";

/** Fixed transport dimensions only. Never retain errors, provider frames or prompts. */
export function transportObservation(event: AgentEvent, turnId?: string): Record<string, unknown> {
  const p = event.payload;
  const detail: Record<string, unknown> = {
    message_type: event.type,
    request_id: event.request_id,
    ...(turnId === undefined ? {} : { turn_id: turnId }),
    outcome: event.type.endsWith(".failed") ? "failure"
      : event.type.endsWith(".completed") ? "success"
      : event.type.endsWith(".retrying") ? "retrying" : "observed",
  };
  for (const [source, target] of Object.entries({ transport: "transport", error_class: "error_kind",
    error_code: "error_code", failure_phase: "failure_phase", replay_mode: "replay_mode" })) {
    const value = p[source];
    if (typeof value === "string" && /^[A-Za-z0-9_.:-]{1,80}$/.test(value)) detail[target] = value;
  }
  const operation = [p.direction, p.phase ?? p.purpose]
    .filter((value): value is string => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,80}$/.test(value)).join(":");
  if (operation) detail.operation_kind = operation;
  for (const [source, target] of Object.entries({ attempt: "attempt_count", next_attempt: "next_attempt",
    max_attempts: "max_attempts", connection_generation: "connection_generation", model_call_index: "model_call_index",
    status_code: "status_code" })) {
    const value = p[source];
    if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) detail[target] = value;
  }
  for (const [source, target] of Object.entries({ delay_ns: "retry_delay_ms", duration_ns: "duration_ms" })) {
    const value = p[source];
    if (typeof value === "number" && Number.isFinite(value) && value >= 0) detail[target] = value / 1_000_000;
  }
  for (const key of ["opens_new_socket", "server_requested_delay"]) {
    if (typeof p[key] === "boolean") detail[key] = p[key];
  }
  return detail;
}
