import { toolRouterRuntime } from "../runtime/tool-router.mjs";
import { utf8ByteLength } from "../runtime/utf8.mjs";
import { hostedCatalog } from "./hostedCatalog.mjs";
import { normalizeHostedMachines } from "./hostedMachine.mjs";

// heartbeatMs is the deprecated compatibility name for the control ping interval.
const DEFAULT_HEARTBEAT_MS = 30_000;
const DEFAULT_HANDSHAKE_TIMEOUT_MS = 10_000;
const DEFAULT_DRAIN_TIMEOUT_MS = 10_000;
const OPEN = 1;
const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
const ATTACHMENT_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$/;

export class AttachmentRejectedError extends Error {
  constructor(reason) {
    super(`tool attachment rejected: ${reason || "policy violation"}`);
    this.name = "AttachmentRejectedError";
  }
}

class AttachmentTransportError extends Error {
  constructor(error) {
    super(errorMessage(error));
    this.name = "AttachmentTransportError";
  }
}

/** Creates the executor side of the socket-owned reverse tool protocol. */
export function createAttachment(owner, target, options = {}) {
  const router = owner?.[toolRouterRuntime];
  if (!router || typeof router.execute !== "function") {
    throw new TypeError("attach requires a Tools runtime");
  }
  validateAttachmentOptions(target, options);
  const machines = normalizeHostedMachines(options.machines);
  const attachmentId = options.attachmentId;
  if (machines.length > 0 && attachmentId !== machines[0].id) {
    throw new TypeError("a machine attachment requires one machine whose id equals attachmentId");
  }
  const endpoint = attachmentEndpoint(target);
  const transport = attachmentTransport(target);
  let client;
  let starting;
  let stopped = false;
  let resolveClosed;
  const closed = new Promise((resolve) => { resolveClosed = resolve; });
  return Object.freeze({
    async connect() {
      if (stopped) throw new Error("tool attachment connector is closed");
      if (!starting) starting = (async () => {
        await router.settled?.();
        if (stopped) throw new Error("tool attachment connector is closed");
        const admission = router.snapshot();
        if (stopped) {
          admission.release();
          throw new Error("tool attachment connector is closed");
        }
        const created = createClient(endpoint, transport, options, admission, machines, attachmentId);
        void created.public.closed().then(resolveClosed);
        return created;
      })();
      client = await starting;
      await client.ready;
      return client.public;
    },
    close() {
      stopped = true;
      if (client) return client.public.close();
      if (!starting) {
        resolveClosed();
        return closed;
      }
      resolveClosed();
      void starting.then(
        (created) => created.public.close(),
        () => {},
      );
      return closed;
    },
    closed() { return closed; },
  });
}

function createClient(endpoint, transport, options, admission, machines, attachmentId) {
  const state = {
    socket: undefined,
    catalog: hostedCatalog(admission.catalog(options.provider ?? "javascript")),
    machines,
    attachmentId,
    runtimeId: crypto.randomUUID(),
    connection: undefined,
    attempt: 0,
    stoppedObserved: false,
    calls: new Map(),
    receipts: new Map(),
    active: new Set(),
    heartbeat: undefined,
    handshakeTimer: undefined,
    drainTimer: undefined,
    reconnectTimer: undefined,
    retryDelay: options.reconnectDelayMs ?? 250,
    connectedAt: 0,
    pendingNonce: undefined,
    catalogSent: false,
    readyReceived: false,
    stopped: false,
    draining: false,
    drainAcknowledged: false,
    connected: false,
    readySettled: false,
    admissionReleased: false,
  };
  let resolveReady;
  let rejectReady;
  let resolveClosed;
  const ready = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
  const closed = new Promise((resolve) => { resolveClosed = resolve; });
  const publicClient = Object.freeze({
    get connected() { return state.connected; },
    closed() { return closed; },
    close() {
      if (state.stopped) return closed;
      state.stopped = true;
      clearTimeout(state.reconnectTimer);
      state.reconnectTimer = undefined;
      const socket = state.socket;
      if (!socket || !state.readySettled) {
        clearTimers(state);
        if (!state.readySettled) {
          state.readySettled = true;
          rejectReady(new Error("tool attachment detached"));
        }
        abortGeneration(state, new Error("tool attachment detached"));
        releaseAdmission();
        if (socket) closeSocket(socket, 1000, "tool attachment detached");
        else finishStopped();
        return closed;
      }
      if (!state.draining) {
        state.draining = true;
        observe("draining");
        try { send(socket, { type: "drain" }); }
        catch (error) {
          closeSocket(socket, 1011, closeReason(`tool attachment drain failed: ${errorMessage(error)}`));
          return closed;
        }
        if (state.socket !== socket) return closed;
        state.drainTimer = setTimeout(() => {
          if (state.socket === socket) closeSocket(socket, 1000, "tool attachment drain timed out");
        }, positiveOption(options.drainTimeoutMs, DEFAULT_DRAIN_TIMEOUT_MS, "drainTimeoutMs"));
      }
      maybeFinishDrain(socket);
      return closed;
    },
  });
  const client = { ready, public: publicClient };
  void connectGeneration();
  return client;

  async function connectGeneration() {
    if (state.stopped) return;
    state.reconnectTimer = undefined;
    const connection = { id: crypto.randomUUID(), attempt: ++state.attempt };
    state.connection = connection;
    if (connection.attempt > 1) observe("reconnect_started", {}, connection);
    observe("connection_start", {}, connection);
    try {
      const socket = await openSocket(endpoint, transport);
      if (state.stopped) {
        try { socket.close(1000, "attachment stopped"); } catch {}
        finishStopped();
        return;
      }
      state.socket = socket;
      state.catalogSent = false;
      state.readyReceived = false;
      state.draining = false;
      state.drainAcknowledged = false;
      bindSocket(socket, {
        open() {
          if (state.socket !== socket) return;
          if (!state.catalogSent) observe("open", {}, connection);
          try { publishCatalog(socket); }
          catch (error) { transportFailure(socket, error); }
        },
        message(encoded) {
          if (state.socket !== socket) return;
          if (typeof encoded !== "string") {
            rejectProtocol(socket, "tool attachments require text frames");
            return;
          }
          void handleFrame(encoded, socket).catch((error) => {
            if (error instanceof AttachmentTransportError) transportFailure(socket, error);
            else rejectProtocol(socket, errorMessage(error));
          });
        },
        close(event) { socketClosed(socket, event); },
        error(error) {
          if (state.socket !== socket) return;
          observe("error", {}, connection);
          if (!state.readySettled) {
            state.readySettled = true;
            state.stopped = true;
            releaseAdmission();
            rejectReady(error);
            closeSocket(socket, 1011, "tool attachment initial connection failed");
          } else transportFailure(socket, error, false);
        },
      });
      state.handshakeTimer = setTimeout(() => {
        if (state.socket !== socket || state.readyReceived) return;
        observe("handshake_timeout", {}, connection);
        if (!state.readySettled) {
          state.readySettled = true;
          state.stopped = true;
          rejectReady(new Error("tool attachment handshake timed out before ready"));
        }
        closeSocket(socket, 1012, "tool attachment handshake timed out");
      }, positiveOption(options.handshakeTimeoutMs, DEFAULT_HANDSHAKE_TIMEOUT_MS, "handshakeTimeoutMs"));
      if (socket.readyState === OPEN) queueMicrotask(() => {
        if (state.socket !== socket) return;
        if (!state.catalogSent) observe("open", {}, connection);
        try { publishCatalog(socket); }
        catch (error) { transportFailure(socket, error); }
      });
    } catch (error) {
      observe("error", {}, connection);
      if (error instanceof AttachmentRejectedError) state.stopped = true;
      if (!state.stopped && state.readySettled && options.reconnect !== false) {
        scheduleReconnect();
      } else if (!state.readySettled) {
        state.readySettled = true;
        state.stopped = true;
        releaseAdmission();
        rejectReady(error);
      }
      if (state.stopped || options.reconnect === false) { abortGeneration(state, error); releaseAdmission(); finishStopped(); }
    }
  }

  function scheduleReconnect() {
    if (state.stopped || options.reconnect === false || state.reconnectTimer !== undefined) return;
    if (state.connectedAt && Date.now() - state.connectedAt >= 30_000) state.retryDelay = options.reconnectDelayMs ?? 250;
    state.connectedAt = 0;
    const delayMs = Math.min(5_000, state.retryDelay) * (0.75 + Math.random() / 4);
    observe("reconnect_scheduled", { delay_ms: delayMs });
    state.reconnectTimer = setTimeout(connectGeneration, delayMs);
    state.retryDelay = Math.min(5_000, state.retryDelay * 2);
  }

  function publishCatalog(socket) {
    if (state.catalogSent) return;
    state.catalogSent = true;
    send(socket, {
      type: "catalog",
      capabilities: ["turn_metadata"],
      diagnostics: true,
      command_recovery: true,
      turn_lifecycle: true,
      connection_id: state.connection.id,
      runtime_id: state.runtimeId,
      tools: state.catalog,
      ...(state.machines.length === 0 ? {} : { machines: state.machines }),
      ...(state.attachmentId === undefined ? {} : { attachment_id: state.attachmentId }),
    });
    observe("catalog");
  }

  async function handleFrame(encoded, socket) {
    if (state.socket !== socket) return;
    const frame = parseFrame(encoded);
    const timing = frame.type === "call" ? { received: performance.now(), connection: state.connection } : undefined;
    if (timing) diagnostic(socket, frame.call_id, "received", timing);
    switch (frame.type) {
      case "ready":
        if (!state.catalogSent || state.readyReceived) throw new Error("ready received outside the catalog handshake");
        state.readyReceived = true;
        state.connected = true;
        state.connectedAt = Date.now();
        observe("ready");
        clearTimeout(state.handshakeTimer);
        state.handshakeTimer = undefined;
        startHeartbeat(socket);
        for (const callId of state.receipts.keys()) replayReceipt(callId, socket);
        if (!state.readySettled) { state.readySettled = true; resolveReady(publicClient); }
        break;
      case "call":
        if (!state.readyReceived || state.drainAcknowledged) throw new Error("call received outside a routing-ready socket");
        await handleCall(frame, socket, timing);
        break;
      case "turn_ended":
        if (!state.readyReceived || state.drainAcknowledged) throw new Error("turn_ended received outside a routing-ready socket");
        await admission.endTurn(frame.session_id, frame.turn_id, frame.hook_event_name);
        break;
      case "cancel":
        if (!state.readyReceived) throw new Error("cancel received outside a routing-ready socket");
        handleCancel(frame, socket);
        break;
      case "ack":
        handleAck(frame, socket);
        break;
      case "recover":
        if (!state.readyReceived) throw new Error("recover received outside a routing-ready socket");
        for (const callId of frame.call_ids) {
          const call = state.calls.get(callId);
          if (call?.encoded) replayReceipt(callId, socket);
          else send(socket, { type: "status", call_id: callId, state: call ? "running" : "missing" });
        }
        break;
      case "draining":
        if (!state.draining || state.drainAcknowledged) throw new Error("unexpected draining acknowledgement");
        state.drainAcknowledged = true;
        maybeFinishDrain(socket);
        break;
      default:
        throw new Error(`unsupported durable-object frame: ${frame.type}`);
    }
  }

  async function handleCall(frame, socket, timing) {
    const callId = frame.call_id;
    const identity = immutableIdentity(frame);
    const retained = state.calls.get(callId);
    if (retained) {
      if (retained.identity !== identity) throw new Error("call identity conflicts with retained command");
      if (retained.encoded) replayReceipt(callId, socket);
      else send(socket, { type: "status", call_id: callId, state: "running" });
      return;
    }
    const controller = new AbortController();
    const call = { controller, timing, identity, frame };
    state.calls.set(callId, call);
    // Dispatch calls concurrently through the retained ToolRouter snapshot,
    // checking cancellation before dispatch. A connection-local count must not
    // reject valid calls or disconnect a socket with unacknowledged results.
    if (frame.deadline_at <= Date.now()) {
      retainAndSend(callId, { status: "unavailable", message: "tool attachment call deadline elapsed before dispatch" }, socket, timing);
      return;
    }
    state.active.add(call);
    let deadline;
    const deadlineAt = frame.deadline_at;
    const deadlinePromise = new Promise((resolve) => {
      const arm = () => {
        const remaining = deadlineAt - Date.now();
        if (remaining <= 0) {
          controller.abort(new Error("tool attachment call deadline elapsed"));
          resolve({ deadline: true });
          return;
        }
        deadline = setTimeout(arm, Math.min(remaining, 2_147_483_647));
      };
      arm();
    });
    let outcome;
    let value;
    try {
      value = await Promise.race([
        Promise.resolve().then(() => {
          timing.taskStarted = performance.now();
          return admission.invoke(frame.name, frame.input, {
          sessionId: frame.session_id,
          ...(frame.turn_id === undefined ? {} : { turnId: frame.turn_id }),
          parentCallId: "",
          callId,
          model: frame.model,
          signal: controller.signal,
          }, stage => {
            if (stage === "execution_started") timing.executionStarted = performance.now();
            else if (stage === "execution_finished") timing.executionFinished = performance.now();
            else return;
            diagnostic(socket, callId, stage, timing);
          });
        }).finally(() => {
          state.active.delete(call);
          if (state.stopped) releaseAdmission();
        }),
        deadlinePromise,
      ]);
      if (value?.deadline) outcome = { status: "ambiguous", message: "tool attachment call crossed its admitted deadline after dispatch" };
    } catch (error) {
      outcome = controller.signal.aborted
        ? { status: "ambiguous", message: "tool attachment call ended after local deadline or transport cancellation" }
        : { status: "completed", output: failedOutput(error) };
    }
    clearTimeout(deadline);
    if (state.calls.get(callId) !== call || call.encoded) return;
    let outputJson;
    if (!outcome) {
      try {
        const output = wireOutput(value);
        // Serialize the output once: the budget applies to exactly these UTF-8
        // bytes, and the receipt embeds the same text.
        const encoded = JSON.stringify(output);
        if (exceedsUtf8Budget(encoded, frame.output_byte_budget)) {
          outcome = { status: "ambiguous", message: "tool attachment output exceeded the admitted byte budget after dispatch" };
        } else {
          outcome = { status: "completed", output };
          outputJson = encoded;
        }
      } catch {
        outcome = { status: "ambiguous", message: "tool attachment result was not valid bounded wire output after dispatch" };
      }
    }
    retainAndSend(callId, outcome, state.socket, timing, outputJson);
    if (state.socket) maybeFinishDrain(state.socket);
  }

  function handleCancel(frame, socket) {
    const callId = frame.call_id;
    const call = state.calls.get(callId);
    if (!call || call.encoded) return;
    call.controller.abort(new Error("tool attachment call was cancelled"));
    retainAndSend(callId, { status: "ambiguous", message: "tool execution was cancelled after dispatch" }, socket, call.timing);
    maybeFinishDrain(socket);
  }

  function handleAck(frame, socket) {
    if (!state.receipts.has(frame.call_id)) return;
    state.receipts.delete(frame.call_id);
    state.calls.delete(frame.call_id);
    maybeFinishDrain(socket);
  }

  function retainAndSend(callId, outcome, socket, clock, outputJson) {
    const result = { type: "result", call_id: callId, outcome };
    const encodeStarted = performance.now();
    const task = clock.taskStarted ?? encodeStarted;
    const execution = clock.executionStarted ?? encodeStarted;
    const finished = clock.executionFinished ?? encodeStarted;
    const timing = {
      scheduler_ms: task - clock.received,
      execution_gate_ms: clock.taskStarted === undefined ? 0 : execution - task,
      execution_ms: clock.executionStarted === undefined ? 0 : finished - execution,
      result_encode_ms: clock.executionFinished === undefined ? 0 : encodeStarted - finished,
      result_queue_ms: 0, // Node sends synchronously; there is no result channel.
      host_elapsed_ms: encodeStarted - clock.received,
    };
    try {
      // Encode the business receipt once. The small metadata append and socket
      // handoff remain in the broker's combined transit/return residual.
      // JSON.stringify of a plain object concatenates its members' encodings in
      // insertion order, so splicing the already-encoded output is exactly
      // JSON.stringify(result) without serializing the output again.
      const encoded = outputJson === undefined
        ? JSON.stringify(result)
        : `{"type":"result","call_id":${JSON.stringify(callId)},"outcome":{"status":"completed","output":${outputJson}}}`;
      const encodingMs = performance.now() - encodeStarted;
      timing.result_encode_ms += encodingMs;
      timing.host_elapsed_ms += encodingMs;
      const receipt = `${encoded.slice(0, -1)},"timing":${JSON.stringify(timing)}}`;
      state.calls.get(callId).encoded = receipt;
      state.receipts.set(callId, receipt);
      observe("result_retained", { transport_call_id: callId }, clock.connection);
      if (socket && state.socket === socket && state.readyReceived) {
        diagnostic(socket, callId, "result_prepared", clock);
        socket.send(receipt);
      }
    } catch (error) {
      if (socket && state.socket === socket) transportFailure(socket, error);
    }
  }

  function replayReceipt(callId, socket) {
    const encoded = state.receipts.get(callId);
    if (encoded === undefined) return;
    try { socket.send(encoded); }
    catch (error) { throw new AttachmentTransportError(error); }
    observe("result_replayed", { transport_call_id: callId });
  }

  function startHeartbeat(socket) {
    clearInterval(state.heartbeat);
    state.pendingNonce = undefined;
    // Browser-standard WebSockets cannot send or observe control frames. Their
    // close/error events and admitted call deadlines provide failure detection.
    if (typeof socket.ping !== "function" || typeof socket.on !== "function") return;
    socket.on("pong", bytes => {
      if (state.socket === socket && equalBytes(bytes, state.pendingNonce)) state.pendingNonce = undefined;
    });
    state.heartbeat = setInterval(() => {
      if (state.socket !== socket) return;
      if (state.pendingNonce !== undefined) {
        observe("heartbeat_timeout");
        closeSocket(socket, 1012, "tool attachment heartbeat timed out");
        return;
      }
      state.pendingNonce = crypto.getRandomValues(new Uint8Array(16));
      try { socket.ping(state.pendingNonce); }
      catch (error) { transportFailure(socket, error); }
    }, positiveOption(options.heartbeatMs, DEFAULT_HEARTBEAT_MS, "heartbeatMs"));
  }

  function maybeFinishDrain(socket) {
    if (!state.stopped || !state.drainAcknowledged) return;
    if (state.calls.size || state.receipts.size) return;
    clearTimeout(state.drainTimer);
    state.drainTimer = undefined;
    releaseAdmission();
    closeSocket(socket, 1000, "tool attachment drained");
  }

  function rejectProtocol(socket, reason) {
    if (state.socket !== socket) return;
    observe("error");
    state.stopped = true;
    state.connected = false;
    clearTimers(state);
    abortGeneration(state, new Error(reason));
    releaseAdmission();
    closeSocket(socket, 1008, closeReason(reason));
  }

  function transportFailure(socket, error, report = true) {
    if (state.socket !== socket) return;
    if (report) observe("error");
    state.connected = false;
    closeSocket(socket, 1011, closeReason(`tool attachment transport failed: ${errorMessage(error)}`));
  }

  function closeSocket(socket, code, reason) {
    try { socket.close(code, reason); }
    catch {}
    socketClosed(socket, { code });
  }

  function socketClosed(socket, event) {
    if (state.socket !== socket) return;
    const policyRejected = event?.code === 1008;
    observe("close", Number.isInteger(event?.code) ? { close_code: event.code } : {});
    if (policyRejected) state.stopped = true;
    state.connected = false;
    state.socket = undefined;
    state.catalogSent = false;
    state.readyReceived = false;
    clearTimers(state);
    if (state.stopped || options.reconnect === false) abortGeneration(state, new Error("tool attachment stopped"));
    if (!state.stopped && state.readySettled && options.reconnect !== false) {
      scheduleReconnect();
    } else if (!state.readySettled) {
      state.readySettled = true;
      state.stopped = true;
      releaseAdmission();
      rejectReady(policyRejected
        ? new AttachmentRejectedError(event?.reason)
        : new Error("tool attachment closed before ready"));
    } else releaseAdmission();
    if (state.stopped || options.reconnect === false) finishStopped();
  }

  function diagnostic(socket, callId, stage, clock) {
    if (state.socket !== socket) return;
    // Passive telemetry must never alter business outcomes or retry execution.
    try {
      const elapsed = performance.now() - clock.received;
      if (!Number.isFinite(elapsed)) return;
      socket.send(JSON.stringify({
        type: "diagnostic", call_id: callId, stage,
        elapsed_ms: Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, elapsed)),
      }));
    } catch {}
  }

  function observe(event, extra = {}, connection = state.connection) {
    try {
      console.info({
        type: "hand.attachment", event,
        client_connection_id: connection?.id,
        attempt: connection?.attempt,
        runtime_id: state.runtimeId,
        active_calls: state.active.size,
        retained_calls: state.receipts.size,
        ...extra,
      });
    } catch {}
  }

  function finishStopped() {
    if (!state.stoppedObserved) {
      state.stoppedObserved = true;
      observe("stopped");
    }
    resolveClosed();
  }

  function releaseAdmission() {
    if (state.admissionReleased || state.active.size) return;
    state.admissionReleased = true;
    admission.release();
  }
}

// Exact test of utf8ByteLength(text) > budget. Each UTF-16 code unit encodes
// to one to three UTF-8 bytes, so only lengths between those bounds need the
// exact count.
function exceedsUtf8Budget(text, budget) {
  if (text.length > budget) return true;
  if (text.length * 3 <= budget) return false;
  return utf8ByteLength(text) > budget;
}
function wireOutput(value) {
  if (value?.[TOOL_RESULT]) return {
    output: outputBody(value.output), success: value.success,
    structured_result: snapshot(value.structuredResult),
    metadata: value.metadata == null ? null : snapshot(value.metadata), process_trace: null,
  };
  return { output: outputBody(value), success: true, structured_result: snapshot(value), metadata: null, process_trace: null };
}
function failedOutput(error) { return { output: errorMessage(error), success: false, structured_result: null, metadata: null, process_trace: null }; }
function outputBody(value) {
  if (Array.isArray(value) && value.every((item) => ["input_text", "input_image", "input_audio"].includes(item?.type))) return snapshot(value);
  if (typeof value === "string") return value;
  return value === undefined ? "undefined" : JSON.stringify(value);
}

async function openSocket(endpoint, transport) {
  let socket;
  if (transport !== undefined) socket = await transport.connect(endpoint);
  else {
    if (typeof globalThis.window === "object" && !isSameOriginWebSocket(endpoint)) {
      throw new Error("browser tool attachments require a same-origin URL or an injected authenticated transport");
    }
    const WebSocketImpl = globalThis.WebSocket;
    if (typeof WebSocketImpl !== "function") throw new Error("WebSocket is unavailable; inject transport.connect() in this runtime");
    socket = new WebSocketImpl(endpoint);
  }
  if (!socket || typeof socket.send !== "function" || typeof socket.close !== "function") {
    throw new TypeError("attachment transport must return a WebSocket-compatible object");
  }
  return socket;
}

function validateAttachmentOptions(target, options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) throw new TypeError("tool attachment options must be an object");
  const allowed = new Set(["provider", "reconnect", "reconnectDelayMs", "heartbeatMs", "handshakeTimeoutMs", "drainTimeoutMs", "machines", "attachmentId"]);
  for (const name of Object.keys(options)) if (!allowed.has(name)) throw new TypeError(`unsupported tool attachment option: ${name}`);
  if (options.provider !== undefined && (typeof options.provider !== "string" || !options.provider.trim())) throw new TypeError("tool attachment provider must be a non-empty string");
  if (options.reconnect !== undefined && typeof options.reconnect !== "boolean") throw new TypeError("tool attachment reconnect must be boolean");
  if (options.attachmentId !== undefined && (typeof options.attachmentId !== "string" || !ATTACHMENT_ID.test(options.attachmentId))) {
    throw new TypeError("tool attachment attachmentId must be a safe identifier of at most 123 bytes");
  }
  for (const name of ["reconnectDelayMs", "heartbeatMs", "handshakeTimeoutMs", "drainTimeoutMs"]) {
    if (options[name] !== undefined) positiveInteger(options[name], name);
  }
  if (typeof target === "object" && !(target instanceof URL)) {
    if (!target || Array.isArray(target)) throw new TypeError("invalid tool attachment target");
    for (const name of Object.keys(target)) if (!new Set(["endpoint", "transport"]).has(name)) throw new TypeError(`unsupported tool attachment target field: ${name}`);
    if (target.endpoint === undefined) throw new TypeError("tool attachment target requires endpoint");
    if (!target.transport || typeof target.transport.connect !== "function") throw new TypeError("tool attachment target transport must implement connect(endpoint)");
  }
}

function isSameOriginWebSocket(endpoint) {
  if (!globalThis.location?.href) return false;
  const websocket = new URL(endpoint); const page = new URL(globalThis.location.href);
  const pageProtocol = page.protocol === "https:" ? "wss:" : page.protocol === "http:" ? "ws:" : "";
  return websocket.protocol === pageProtocol && websocket.host === page.host;
}
function bindSocket(socket, handlers) {
  if (typeof socket.addEventListener === "function") {
    socket.addEventListener("open", handlers.open);
    socket.addEventListener("message", (event) => handlers.message(event?.data));
    socket.addEventListener("close", handlers.close);
    socket.addEventListener("error", (event) => handlers.error(event?.error ?? new Error("tool attachment WebSocket failed")));
    return;
  }
  if (typeof socket.on === "function") {
    socket.on("open", handlers.open);
    socket.on("message", (data, isBinary) => handlers.message(isBinary === true ? data : typeof data === "string" ? data : decode(data)));
    socket.on("close", (code, reason) => handlers.close({ code, reason: decode(reason) }));
    socket.on("error", (error) => handlers.error(error ?? new Error("tool attachment WebSocket failed")));
    return;
  }
  throw new TypeError("attachment WebSocket must support addEventListener() or on()");
}
function send(socket, frame) {
  try {
    const encoded = JSON.stringify(frame);
    // Results already obey the admitted output byte budget. Let the transport
    // own its frame limits and send queue; buffered bytes are not a failed call.
    socket.send(encoded);
  }
  catch (error) { throw new AttachmentTransportError(error); }
}
function parseFrame(encoded) {
  if (typeof encoded !== "string") throw new TypeError("tool attachments require text frames");
  const frame = JSON.parse(encoded);
  if (!frame || typeof frame !== "object" || Array.isArray(frame)) throw new TypeError("tool attachment frame must be an object");
  const keys = DO_KEYS[frame.type];
  if (!keys) throw new Error(`unsupported durable-object tool attachment frame: ${frame.type}`);
  exactKeys(frame, keys);
  if (frame.type === "call") {
    requiredIdentifier(frame.session_id, "session_id"); requiredIdentifier(frame.call_id, "call_id");
    if (frame.turn_id !== undefined && (typeof frame.turn_id !== "string" || !frame.turn_id || utf8ByteLength(frame.turn_id) > 256)) throw new TypeError("turn_id must be 1-256 UTF-8 bytes");
    if (typeof frame.model !== "string") throw new TypeError("model must be a string");
    requiredIdentifier(frame.name, "name");
    if (typeof frame.input !== "string" && (!frame.input || typeof frame.input !== "object" || Array.isArray(frame.input))) throw new Error("call input must be an object or string");
    positiveInteger(frame.output_token_budget, "output_token_budget");
    positiveInteger(frame.output_byte_budget, "output_byte_budget");
    positiveInteger(frame.deadline_at, "deadline_at");
  } else if (frame.type === "turn_ended") {
    requiredIdentifier(frame.session_id, "session_id");
    if (typeof frame.turn_id !== "string" || !frame.turn_id || utf8ByteLength(frame.turn_id) > 256) throw new TypeError("invalid turn_id");
    if (!["Stop", "Interrupt", "SubagentStop"].includes(frame.hook_event_name)) throw new TypeError("invalid lifecycle event");
  } else if (frame.type === "cancel" || frame.type === "ack") requiredIdentifier(frame.call_id, "call_id");
  else if (frame.type === "recover") {
    if (!Array.isArray(frame.call_ids) || frame.call_ids.length > 100 || new Set(frame.call_ids).size !== frame.call_ids.length) throw new Error("recover requires at most 100 unique call ids");
    for (const callId of frame.call_ids) requiredIdentifier(callId, "call_id");
  }
  return frame;
}
function clearTimers(state) {
  clearInterval(state.heartbeat); clearTimeout(state.handshakeTimer);
  clearTimeout(state.drainTimer); clearTimeout(state.reconnectTimer);
  state.heartbeat = state.handshakeTimer = state.drainTimer = state.reconnectTimer = undefined;
  state.pendingNonce = undefined;
}
function abortGeneration(state, reason) {
  for (const call of state.active) call.controller.abort(reason);
  state.calls.clear(); state.receipts.clear(); state.pendingNonce = undefined;
}
function positiveInteger(value, name) { if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${name} must be a positive safe integer`); return value; }
function positiveOption(value, fallback, name) { return value === undefined ? fallback : positiveInteger(value, name); }
function requiredIdentifier(value, name) { if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) throw new TypeError(`${name} must be a safe ASCII identifier`); return value; }
function exactKeys(value, allowed) { for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new Error(`${value.type} contains unsupported field ${key}`); }
function snapshot(value) { return value === undefined ? null : JSON.parse(JSON.stringify(value)); }
function decode(value) { if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) return new TextDecoder().decode(value); return String(value); }
function errorMessage(error) { return error && (error.stack || error.message) || String(error); }
function attachmentEndpoint(target) {
  const raw = typeof target === "object" && !(target instanceof URL) ? target.endpoint : target;
  let url; try { url = new URL(raw); } catch { throw new TypeError("tool attachment target must be a valid WebSocket URL"); }
  if (url.protocol !== "ws:" && url.protocol !== "wss:") throw new TypeError("tool attachment target must use ws: or wss:");
  if (url.username || url.password) throw new TypeError("tool attachment target must not contain credentials");
  if (url.hash) throw new TypeError("tool attachment target must not contain a fragment");
  if (url.protocol === "ws:" && !isLoopback(url.hostname)) throw new TypeError("plaintext ws: tool attachments are limited to loopback hosts");
  return url.href;
}
function attachmentTransport(target) { return typeof target === "object" && !(target instanceof URL) ? target.transport : undefined; }
function isLoopback(hostname) {
  return hostname === "localhost"
    || hostname === "[::1]"
    || /^127(?:\.[0-9]{1,3}){3}$/.test(hostname)
    || hostname === "nanocodex.localhost"
    || /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/.test(hostname);
}
function closeReason(reason) {
  if (utf8ByteLength(reason) <= 123) return reason;
  let bounded = ""; for (const scalar of reason) { if (utf8ByteLength(bounded + scalar) > 123) break; bounded += scalar; }
  return bounded;
}
function equalBytes(value, expected) {
  if (expected === undefined || !ArrayBuffer.isView(value)) return false;
  const bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  return bytes.length === expected.length && bytes.every((byte, index) => byte === expected[index]);
}
function immutableIdentity(frame) {
  const stable = value => Array.isArray(value) ? value.map(stable)
    : value && typeof value === "object" ? Object.fromEntries(Object.keys(value).sort().map(key => [key, stable(value[key])])) : value;
  return JSON.stringify(stable(frame));
}

const DO_KEYS = Object.freeze({
  ready: ["type"],
  turn_ended: ["type", "session_id", "turn_id", "hook_event_name"],
  call: ["type", "session_id", "turn_id", "call_id", "model", "name", "input", "output_token_budget", "output_byte_budget", "deadline_at"],
  cancel: ["type", "call_id"],
  ack: ["type", "call_id"],
  recover: ["type", "call_ids"],
  draining: ["type"],
});
