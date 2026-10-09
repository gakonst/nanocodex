// Passive, content-free observations. Timers report silence; they never cancel
// inference. Each socket retains only one active request and 32 response links.
export function createSocketObservations(observe) {
  if (observe === undefined) return;
  if (typeof observe !== "function") throw new TypeError("host socket event hook must be a function");
  const turns = new Map();
  const calls = new Map();
  const starts = new Map();
  const sockets = new Set();
  const emit = (value) => {
    try {
      const result = observe(value);
      if (result?.then) void Promise.resolve(result).catch(() => {});
    } catch { /* Observation cannot change transport behavior. */ }
  };
  const retain = (map, key, value, limit = 128) => {
    if (!map.has(key) && map.size >= limit) map.delete(map.keys().next().value);
    map.set(key, value);
  };
  return {
    runtime(encoded, agentId) {
      if (typeof encoded === "string" && !/"(?:input\.accepted|model\.(?:call|compaction|warmup)\.|run\.)/.test(encoded.slice(0, 512))) return;
      let event;
      try { event = typeof encoded === "string" ? JSON.parse(encoded) : encoded; } catch { return; }
      const p = event?.payload;
      if (!p || typeof event.type !== "string") return;
      if (event.type === "input.accepted" && typeof p.turn_id === "string" && typeof p.session_id === "string") {
        retain(turns, p.turn_id, p.session_id);
      }
      const sessionId = turns.get(p.turn_id) ?? event.request_id;
      const correlation = {
        turnId: p.turn_id,
        agentId,
        index: p.call_index ?? p.after_model_call_index,
        phase: event.type.startsWith("model.compaction.") ? "compaction" : event.type.startsWith("model.warmup.") ? "warmup" : "generation",
      };
      if (event.type === "model.call.started" || event.type === "model.compaction.started" || event.type === "model.warmup.started") {
        const key = JSON.stringify([sessionId, correlation.turnId, correlation.phase, correlation.index]);
        retain(starts, key, { sessionId, count: (starts.get(key)?.count ?? 0) + 1 });
        retain(calls, sessionId, { ...correlation, key, requests: 0 });
      } else if (/^model\.(call|compaction|warmup)\.(completed|failed)$/.test(event.type)) {
        // A response ID is evidence only when already seen on that socket.
        // Without it, finish only a uniquely attributable indexed operation.
        const candidates = [...sockets].filter(socket => socket.sessionId === sessionId
          && socket.matches(correlation, p.response_id));
        if (candidates.length === 1) {
          const operation = candidates[0].finished(event.type.endsWith(".failed") ? "failed" : "completed");
          if (calls.get(sessionId) === operation) calls.delete(sessionId);
        }
      }
    },
    release(sessionId) {
      calls.delete(sessionId);
      for (const [key, start] of starts) if (start.sessionId === sessionId) starts.delete(key);
      for (const [turn, session] of turns) if (session === sessionId) turns.delete(turn);
    },
    connect(sessionId, snapshot, requestId = sessionId) {
      const measure = () => { try { return snapshot(); } catch { return {}; } };
      const socketId = crypto.randomUUID();
      const began = performance.now();
      const responses = new Map();
      let handshake = {};
      let active, last, timer, ordinal = 0, closed = false;
      const record = (event, fields = {}, request = active ?? last) => emit({
        event, socket_id: socketId, request_id: requestId, runtime_session_id: sessionId,
        ...(uuid(handshake.egressRequestId) ? { egress_request_id: handshake.egressRequestId } : {}),
        ...(identifier(handshake.requestId, "req_") || uuid(handshake.requestId) ? { provider_request_id: handshake.requestId } : {}),
        ...(request ? { socket_request_index: request.index, ...request.context,
          ...(request.responseId ? { response_id: request.responseId } : {}) } : {}),
        ...fields,
      });
      const watch = (event, started) => {
        clearTimeout(timer);
        let after = 1_000;
        const waiting = () => {
          if (closed) return;
          const now = performance.now();
          record(event, { elapsed_ms: now - started,
            ...(active ? { received_message_count: active.messages,
              ...(active.firstMessage === undefined ? {} : { first_message_ms: active.firstMessage - active.started }),
              ...milestones(active),
              last_message_age_ms: now - (active.lastMessage ?? active.started), ...measure() } : {}) });
          after = Math.min(after * 2, 5_000);
          timer = setTimeout(waiting, after);
        };
        timer = setTimeout(waiting, after);
      };
      const responseLink = (request, id) => {
        if (!request || !identifier(id, "resp_") || (request.responseId && request.responseId !== id)) return;
        request.responseId = id;
        retain(responses, id, request, 32);
      };
      const finish = (outcome) => {
        if (!active) return;
        clearTimeout(timer);
        record("request.finished", { outcome, elapsed_ms: performance.now() - active.started,
          received_message_count: active.messages, ...milestones(active), ...measure() });
        last = active;
        active = undefined;
      };
      const socket = {
        sessionId,
        connected(value) {
          handshake = value;
          clearTimeout(timer);
          record("socket.opened", { elapsed_ms: performance.now() - began });
        },
        sendStarted() {
          finish("superseded");
          const operation = calls.get(sessionId);
          if (operation) operation.requests++;
          active = { index: ++ordinal, operation, context: operation ? {
            phase: operation.phase,
            ...(Number.isSafeInteger(operation.agentId) ? { agent_id: operation.agentId } : {}),
            ...(typeof operation.turnId === "string" ? { runtime_turn_id: operation.turnId } : {}),
            ...(Number.isSafeInteger(operation.index) ? { model_call_index: operation.index } : {}),
          } : {}, started: performance.now(), messages: 0 };
          record("request.send_started", measure());
          watch("request.send_waiting", active.started);
        },
        sent() {
          if (!active) return;
          const now = performance.now();
          record("request.sent", { send_wait_ms: now - active.started, ...measure() });
          if (active.firstMessage === undefined) active.started = now;
          active.sentAt = now;
          watch("request.waiting", active.started);
        },
        sendFailed() { finish("send_failed"); },
        message(text) {
          const now = performance.now();
          if (!active) return;
          active.messages++;
          active.lastMessage = now;
          // Only parse bounded envelopes while a relevant milestone is missing.
          // A large/unrecognized envelope remains unclassified, never copied to logs.
          const candidates = typeof text === "string" && text.length <= 16_384
            && (active.responseId === undefined || active.firstMessage === undefined || active.firstOutput === undefined
              || DELTA_STAGES.some(([field, , types]) => active[field] === undefined
                && types.some(type => text.includes(`"${type}"`))));
          let frame;
          if (candidates) {
            try { frame = JSON.parse(text); } catch { /* Invalid frames remain the runtime's responsibility. */ }
          }
          const type = EVENT_TYPES.has(frame?.type) ? frame.type : "unclassified";
          const classification = { provider_event_type: type };
          if (type !== "unclassified" && type !== "responsesapi.websocket_timing") responseLink(active, frame?.response?.id ?? frame?.response_id);
          // A new frame ends the current silence interval. Streaming calls
          // produce no waiting records while frames keep arriving.
          if (active.sentAt !== undefined) watch("request.waiting", active.started);
          if (active.firstMessage === undefined) {
            active.firstMessage = now;
            record("request.first_message", { elapsed_ms: now - active.started, ...classification, ...measure() });
          }
          // Keep the historical output-item milestone, but identify what caused
          // it. Empty item announcements are not evidence of generated text.
          if (active.firstOutput === undefined && OUTPUT_EVENTS.has(type)) {
            active.firstOutput = now;
            record("request.first_output", { elapsed_ms: now - active.started, ...classification,
              output_kind: OUTPUT_KINDS.get(type) ?? "item", ...measure() });
          }
          for (const [field, stage, types] of DELTA_STAGES) {
            if (active[field] === undefined && types.includes(type) && typeof frame.delta === "string" && frame.delta.length > 0) {
              active[field] = now;
              record(stage, { elapsed_ms: now - active.started, ...classification, ...measure() });
            }
          }
        },
        provider(timing) {
          const request = timing.response_id ? responses.get(timing.response_id) : active ?? last;
          if (request?.timingReported) return;
          if (request) request.timingReported = true;
          record("provider.timing", timing, request ?? null);
        },
        matches(operation, responseId) {
          if (!active || !sameOperation(active.operation, operation)) return false;
          if (active.responseId !== undefined && responseId !== undefined && responseId !== null) {
            return identifier(responseId, "resp_") && active.responseId === responseId;
          }
          // Large provider envelopes may leave the wire response ID unobserved.
          // Do not bind the completion ID; fall back to operation evidence.
          // Index/turn can be reused by retries. Only a single observed start
          // with a single send can support completion without a response ID.
          return typeof operation.turnId === "string" && Number.isSafeInteger(operation.index)
            && active.operation.requests === 1 && starts.get(active.operation.key)?.count === 1;
        },
        finished(outcome) { const operation = active?.operation; finish(outcome); return operation; },
        close(event, intentional = false) {
          if (closed) return;
          closed = true;
          clearTimeout(timer);
          record("socket.closed", { elapsed_ms: performance.now() - began, intentional,
            ...(Number.isInteger(event?.code) ? { close_code: event.code } : {}),
            ...(typeof event?.wasClean === "boolean" ? { close_clean: event.wasClean } : {}),
            ...(active ? { received_message_count: active.messages,
              last_message_age_ms: performance.now() - (active.lastMessage ?? active.started) } : {}), ...measure() });
          active = undefined;
          sockets.delete(socket);
        },
        error() { record("socket.error", { elapsed_ms: performance.now() - began, ...measure() }); },
      };
      sockets.add(socket);
      record("socket.connecting", {}, undefined);
      watch("socket.connect_waiting", began);
      return socket;
    },
  };
}

function uuid(value) { return typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value); }
function identifier(value, prefix) { return typeof value === "string" && value.startsWith(prefix) && /^[A-Za-z0-9_-]{1,160}$/.test(value); }
const OUTPUT_EVENTS = new Set(["response.output_text.delta", "response.reasoning_summary_text.delta",
  "response.reasoning_summary.delta", "response.reasoning_content.delta", "response.output_item.added", "response.output_item.done"]);

// These names are a fixed protocol allowlist, never provider-supplied log text.
const DELTA_STAGES = [
  ["firstReasoning", "request.first_reasoning_delta", ["response.reasoning_text.delta", "response.reasoning_summary_text.delta",
    "response.reasoning_summary.delta", "response.reasoning_content.delta"]],
  ["firstAnswer", "request.first_answer_delta", ["response.output_text.delta"]],
  ["firstTool", "request.first_tool_delta", ["response.function_call_arguments.delta", "response.custom_tool_call_input.delta"]],
];
const OUTPUT_KINDS = new Map(DELTA_STAGES.flatMap(([, stage, types]) => types.map(type => [type,
  stage === "request.first_reasoning_delta" ? "reasoning" : stage === "request.first_answer_delta" ? "answer" : "tool"])));
const EVENT_TYPES = new Set([...OUTPUT_EVENTS, ...OUTPUT_KINDS.keys(), "response.created", "response.in_progress", "response.queued",
  "response.completed", "response.failed", "response.incomplete", "error", "responsesapi.websocket_timing"]);
function milestones(request) {
  return Object.fromEntries([["firstOutput", "first_output_ms"], ["firstReasoning", "first_reasoning_delta_ms"],
    ["firstAnswer", "first_answer_delta_ms"], ["firstTool", "first_tool_delta_ms"]]
    .filter(([field]) => request[field] !== undefined).map(([field, key]) => [key, request[field] - request.started]));
}

function sameOperation(a, b) {
  return a !== undefined && a.phase === b.phase && a.turnId === b.turnId && a.index === b.index;
}
