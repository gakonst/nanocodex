import { AsyncLocalStorage } from "node:async_hooks";
import { setSpanAttributes, tracing } from "nanocodex/cloudflare/tracing";

type Context = { trace_id: string; scope: string; closed?: boolean; reads: Record<string, { count: number; duration_ms: number }> };
const contexts = new AsyncLocalStorage<Context>();

export function performanceSyncScope<T>(traceId: string, scope: string, run: () => T): T {
  const context: Context = { trace_id: traceId, scope, reads: {} };
  return tracing.enterSpan("managed.operation", span => contexts.run(context, () => {
    setSpanAttributes(span, scopeAttributes(traceId, scope));
    const began = performance.now();
    try { return run(); }
    finally { context.closed = true; console.info({ type: "managed.performance", trace_id: traceId, stage: scope,
      duration_ms: performance.now() - began, reads: context.reads }); }
  }));
}

/** Timings only: never record query arguments, tool results or credentials. */
export async function performanceScope<T>(traceId: string, scope: string, run: () => Promise<T>): Promise<T> {
  const context: Context = { trace_id: traceId, scope, reads: {} };
  return tracing.enterSpan("managed.operation", span => contexts.run(context, async () => {
    setSpanAttributes(span, scopeAttributes(traceId, scope));
    const began = performance.now();
    try { return await run(); }
    finally {
      context.closed = true;
      console.info({ type: "managed.performance", trace_id: traceId, stage: scope,
        duration_ms: performance.now() - began, reads: context.reads });
    }
  }));
}

export async function performanceStage<T>(stage: string, run: () => Promise<T>): Promise<T> {
  const context = contexts.getStore();
  return tracing.enterSpan("managed.stage", async span => {
    setSpanAttributes(span, scopeAttributes(context?.trace_id, stage));
    if (!context) return run();
    const began = performance.now();
    let success = false;
    try { const result = await run(); success = true; return result; }
    finally { console.info({ type: "managed.performance", trace_id: context.trace_id,
      stage, started_at: Date.now() - (performance.now() - began), duration_ms: performance.now() - began, success }); }
  });
}

function scopeAttributes(operationId: string | undefined, operation: string) {
  // Some legacy scope labels contain request paths. Native span names and
  // attributes use only fixed labels, never those paths or their capabilities.
  return { "nanocodex.operation_id": operationId,
    "nanocodex.operation": /^[a-z][a-z0-9_.]{0,63}$/.test(operation) ? operation : "request" };
}

/** One bounded discovery record; no owner, authority key or metadata payload. */
export function performanceCache(stage: string, cacheState: "hit" | "miss", ageMs: number, remainingMs: number): void {
  const context = contexts.getStore();
  if (!context) return;
  try {
    console.info({ type: "managed.performance", trace_id: context.trace_id, stage,
      cache_state: cacheState, cache_age_ms: Math.max(0, ageMs), remaining_ttl_ms: Math.max(0, remainingMs) });
  } catch { /* Passive cache observations cannot fail admission. */ }
}

export function performanceRead<T>(table: string, run: () => T): T {
  const context = contexts.getStore();
  if (!context) return run();
  const began = performance.now();
  try { return run(); }
  finally {
    const read = context.reads[table] ??= { count: 0, duration_ms: 0 };
    read.count++;
    read.duration_ms += performance.now() - began;
  }
}

/** Opt-in SQL audit. Preserve native receivers and cursors, including transactions. */
export function performanceState<Props>(state: DurableObjectState<Props>): DurableObjectState<Props> {
  type Statement = { statement_id: string; operation?: string; tables: string[]; exec_ms: number; consume_ms: number; rows_read: number; rows_written: number; success: boolean; count: number };
  const pending: Array<{ trace_id?: string; stage: string; read: () => Statement }> = [];
  const flush = () => {
    const groups = new Map<string, { trace_id?: string; stage: string; statements: Map<string, Statement> }>();
    for (const item of pending.splice(0)) {
      const key = `${item.trace_id ?? ""}\n${item.stage}`;
      let group = groups.get(key);
      if (!group) { group = { trace_id: item.trace_id, stage: item.stage, statements: new Map() }; groups.set(key, group); }
      const statement = item.read();
      const current = group.statements.get(statement.statement_id);
      if (!current) group.statements.set(statement.statement_id, statement);
      else {
        current.count += statement.count;
        current.exec_ms += statement.exec_ms;
        current.consume_ms += statement.consume_ms;
        current.rows_read += statement.rows_read;
        current.rows_written += statement.rows_written;
        current.success &&= statement.success;
      }
    }
    for (const group of groups.values()) console.info({ type: "managed.sql_batch", object_id: state.id.toString(),
      trace_id: group.trace_id, stage: group.stage, statements: [...group.statements.values()] });
  };
  const sql = new Proxy(state.storage.sql, {
    get(target, property) {
      if (property !== "exec") {
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      }
      return (query: string, ...bindings: unknown[]) => {
        const began = performance.now();
        const inherited = contexts.getStore();
        const context = inherited?.closed ? undefined : inherited;
        const normalized = query.replace(/'(?:''|[^'])*'/g, "?").replace(/\b\d+\b/g, "?").replace(/\s+/g, " ").trim();
        let hash = 2166136261;
        for (let i = 0; i < normalized.length; i++) hash = Math.imul(hash ^ normalized.charCodeAt(i), 16777619);
        const tables = [...new Set([...normalized.matchAll(/\b(?:FROM|JOIN|INTO|UPDATE|TABLE(?: IF NOT EXISTS)?)\s+([a-z_][a-z_0-9]*)/gi)].map(match => match[1]!.toLowerCase()))].filter(table => table !== "set");
        let cursor: SqlStorageCursor<Record<string, SqlStorageValue>> | undefined;
        let execMs = 0;
        let consumeMs = 0;
        try {
          cursor = target.exec(query, ...bindings);
          execMs = performance.now() - began;
          const result = cursor;
          const proxy: typeof result = new Proxy(result, {
            get(target, property) {
              if (property === Symbol.iterator) return () => proxy;
              const value = Reflect.get(target, property, target);
              if (typeof value !== "function") return value;
              return (...args: unknown[]) => {
                const started = performance.now();
                try { return value.apply(target, args); }
                finally { consumeMs += performance.now() - started; }
              };
            },
          });
          return proxy;
        }
        finally {
          execMs = performance.now() - began;
          // SQL consumers are synchronous. Snapshot rows after toArray/one/iteration
          // in the current stack, without draining or replacing the native cursor.
          if (pending.length === 0) queueMicrotask(flush);
          pending.push({ trace_id: context?.trace_id, stage: context?.scope ?? "object.background", read: () => ({
            statement_id: (hash >>> 0).toString(16), operation: normalized.match(/^[A-Za-z]+/)?.[0]?.toUpperCase(),
            tables, exec_ms: execMs, consume_ms: consumeMs, rows_read: cursor?.rowsRead ?? 0,
            rows_written: cursor?.rowsWritten ?? 0, success: cursor !== undefined, count: 1 }) });
        }
      };
    },
  });
  const storage = new Proxy(state.storage, {
    get(target, property) {
      if (property === "sql") return sql;
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  return new Proxy(state, {
    get(target, property) {
      if (property === "storage") return storage;
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}

/** Observe the existing output-gate commit without delaying the response path.
 * sync waits for pending writes; it does not weaken durability or start an RPC.
 * CF handler clocks cannot show time spent behind the output gate after return.
 */
export function performanceCommit(state: Pick<DurableObjectState, "id" | "storage" | "waitUntil">,
  stage: "session.create.commit"): void {
  const began = performance.now();
  const traceId = state.id.toString();
  const record = (success: boolean) => {
    try { console.info({ type: "managed.performance", trace_id: traceId, stage,
      duration_ms: performance.now() - began, success }); }
    catch { /* Observation cannot alter a durable response. */ }
  };
  try { state.waitUntil(state.storage.sync().then(() => record(true), () => record(false))); }
  catch { /* Native output gates remain authoritative if observation fails. */ }
}

/** One owned-connection summary. Only fixed numeric fields and correlation IDs. */
export function performanceSocketTiming(sessionId: string, observation: unknown): void {
  if (!observation || typeof observation !== "object" || Array.isArray(observation)) return;
  const input = observation as Record<string, unknown>;
  const metrics: Record<string, number> = {};
  for (const key of ["message_count", "delivered_message_count", "buffered_message_count", "discarded_message_count",
    "queue_residence_total_ms", "queue_residence_max_ms"]) {
    const value = input[key];
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return;
    metrics[key] = value;
  }
  try {
    console.info({ type: "managed.performance", stage: "transport.socket_queue", session_id: sessionId, ...metrics });
    if (!Array.isArray(input.provider_timings)) return;
    for (const provider of input.provider_timings.slice(0, 32)) {
      if (!provider || typeof provider !== "object" || Array.isArray(provider)
        || (provider.response_id !== undefined && (typeof provider.response_id !== "string"
          || !/^resp_[A-Za-z0-9_-]{1,128}$/.test(provider.response_id)))) continue;
      const timing: Record<string, number> = {};
      for (const key of ["pre_inference_ms", "engine_queue_max_ms", "engine_service_ttft_total_ms"]) {
        const value = provider[key];
        if (typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 86_400_000) timing[key] = value;
      }
      if (Object.keys(timing).length) console.info({ type: "managed.performance", stage: "transport.provider_timing",
        session_id: sessionId, ...(provider.response_id === undefined ? {} : { response_id: provider.response_id }), ...timing });
    }
  } catch { /* Passive observations cannot fail transport cleanup. */ }
}

/** Live fixed lifecycle fields. No frame content, headers, reasons or errors. */
export function performanceSocketEvent(sessionId: string, observation: unknown, turnId?: string, record?: (value: unknown) => void): void {
  if (!observation || typeof observation !== "object" || Array.isArray(observation)) return;
  const input = observation as Record<string, unknown>;
  const event = input.event;
  if (typeof event !== "string" || !SOCKET_EVENTS.has(event) || !correlationId(input.socket_id)) return;
  const safe: Record<string, string | number | boolean> = { socket_id: input.socket_id };
  if (correlationId(input.request_id)) safe.request_id = input.request_id;
  if (correlationId(input.runtime_session_id)) safe.runtime_session_id = input.runtime_session_id;
  if (correlationId(input.runtime_turn_id)) safe.runtime_turn_id = input.runtime_turn_id;
  if (correlationId(input.egress_request_id)) safe.egress_request_id = input.egress_request_id;
  if (correlationId(turnId)) safe.turn_id = turnId;
  if (typeof input.provider_request_id === "string" && (correlationId(input.provider_request_id)
    || /^req_[A-Za-z0-9_-]{1,128}$/.test(input.provider_request_id))) safe.provider_request_id = input.provider_request_id;
  if (typeof input.response_id === "string" && /^resp_[A-Za-z0-9_-]{1,160}$/.test(input.response_id)) safe.response_id = input.response_id;
  for (const key of ["elapsed_ms", "send_wait_ms", "first_message_ms", "first_output_ms", "first_reasoning_delta_ms", "first_answer_delta_ms", "first_tool_delta_ms", "last_message_age_ms",
    "socket_queue_residence_max_ms", "pre_inference_ms", "engine_queue_max_ms", "engine_service_ttft_total_ms"]) {
    const value = input[key];
    if (typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 86_400_000) safe[key] = value;
  }
  for (const key of ["agent_id", "socket_request_index", "model_call_index", "received_message_count", "queued_message_count",
    "socket_delivered_message_count", "buffered_send_bytes"]) {
    const value = input[key];
    if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) safe[key] = value;
  }
  if (typeof input.close_code === "number" && Number.isInteger(input.close_code) && input.close_code >= 0 && input.close_code <= 4999) safe.close_code = input.close_code;
  for (const key of ["intentional", "close_clean"]) if (typeof input[key] === "boolean") safe[key] = input[key];
  if (typeof input.phase === "string" && ["generation", "compaction", "warmup"].includes(input.phase)) safe.phase = input.phase;
  if (typeof input.outcome === "string" && ["completed", "failed", "send_failed", "superseded"].includes(input.outcome)) safe.outcome = input.outcome;
  if (typeof input.provider_event_type === "string" && PROVIDER_EVENTS.has(input.provider_event_type)) safe.provider_event_type = input.provider_event_type;
  if (typeof input.output_kind === "string" && ["item", "reasoning", "answer", "tool"].includes(input.output_kind)) safe.output_kind = input.output_kind;
  const observationRecord = { type: "managed.performance", stage: `transport.${event}`, session_id: sessionId, thread_id: sessionId, ...safe };
  try { record?.(observationRecord); } catch { /* Persistence is optional diagnostics. */ }
  try { console.info(observationRecord); }
  catch { /* Passive diagnostics cannot alter an active model request. */ }
}

const SOCKET_EVENTS = new Set(["socket.connecting", "socket.connect_waiting", "socket.opened", "socket.closed", "socket.error",
  "request.send_started", "request.send_waiting", "request.sent", "request.waiting", "request.first_message", "request.first_output", "request.first_reasoning_delta", "request.first_answer_delta", "request.first_tool_delta", "request.finished", "provider.timing"]);
const PROVIDER_EVENTS = new Set(["unclassified", "response.created", "response.in_progress", "response.queued",
  "response.completed", "response.failed", "response.incomplete", "error", "responsesapi.websocket_timing",
  "response.output_item.added", "response.output_item.done", "response.output_text.delta", "response.reasoning_text.delta",
  "response.reasoning_summary_text.delta", "response.reasoning_summary.delta", "response.reasoning_content.delta",
  "response.function_call_arguments.delta", "response.custom_tool_call_input.delta"]);
function correlationId(value: unknown): value is string { return typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value); }

/** Post-policy WebSocket request controls; no input, tool schema, IDs or metadata. */
export function performanceRequestShape(sessionId: string, observation: unknown): void {
  if (!observation || typeof observation !== "object" || Array.isArray(observation)) return;
  const input = observation as Record<string, unknown>;
  const safe: Record<string, string | number | boolean> = {};
  const enums: Record<string, readonly string[]> = {
    model: ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"],
    reasoning_effort: ["none", "minimal", "low", "medium", "high", "xhigh", "max"],
    reasoning_context: ["all_turns", "last_turn"],
    service_tier: ["default", "auto", "priority", "flex", "fast"],
    text_verbosity: ["low", "medium", "high"],
    tool_choice: ["auto", "none", "required"],
  };
  for (const [key, allowed] of Object.entries(enums)) {
    const value = input[key];
    if (typeof value !== "string" || (!allowed.includes(value) && value !== "other_or_absent")) return;
    safe[key] = value;
  }
  for (const key of ["encoded_characters", "input_items", "tools_count"]) {
    const value = input[key];
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) return;
    safe[key] = value;
  }
  for (const key of ["cache_key_present", "previous_response_present", "encrypted_reasoning_included",
    "parallel_tool_calls", "store", "stream", "generate"]) {
    if (typeof input[key] === "boolean") safe[key] = input[key];
  }
  try { console.info({ type: "managed.performance", stage: "transport.request_controls", session_id: sessionId, ...safe }); }
  catch { /* Diagnostics cannot change a sent request. */ }
}
