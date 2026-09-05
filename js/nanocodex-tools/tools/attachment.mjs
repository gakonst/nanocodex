import { normalizeObservationProvider, normalizeObservationFrame, OBSERVATION_TIMEOUT_MS } from "./observation.mjs";
import { toolRouterRuntime } from "../runtime/tool-router.mjs";
import { utf8ByteLength } from "../runtime/utf8.mjs";
import { hostedCatalog } from "./hostedCatalog.mjs";
import { normalizeHostedMachines } from "./hostedMachine.mjs";

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
  options = { ...options, observation: normalizeObservationProvider(options.observation, machines) };
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
    observation: undefined,
    calls: new Map(),
    receipts: new Map(),
    heartbeat: undefined,
    handshakeTimer: undefined,
    drainTimer: undefined,
    reconnectTimer: undefined,
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
      state.observation?.controller.abort();
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
        else resolveClosed();
        return closed;
      }
      if (!state.draining) {
        state.draining = true;
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
    try {
      const socket = await openSocket(endpoint, transport);
      if (state.stopped) { socket.close(1000, "attachment stopped"); return; }
      state.socket = socket;
      state.catalogSent = false;
      state.readyReceived = false;
      state.draining = false;
      state.drainAcknowledged = false;
      bindSocket(socket, {
        open() {
          if (state.socket !== socket) return;
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
          if (!state.readySettled) {
            state.readySettled = true;
            state.stopped = true;
            releaseAdmission();
            rejectReady(error);
            closeSocket(socket, 1011, "tool attachment initial connection failed");
          }
        },
      });
      state.handshakeTimer = setTimeout(() => {
        if (state.socket !== socket || state.readyReceived) return;
        if (!state.readySettled) {
          state.readySettled = true;
          state.stopped = true;
          rejectReady(new Error("tool attachment handshake timed out before ready"));
        }
        closeSocket(socket, 1012, "tool attachment handshake timed out");
      }, positiveOption(options.handshakeTimeoutMs, DEFAULT_HANDSHAKE_TIMEOUT_MS, "handshakeTimeoutMs"));
      if (socket.readyState === OPEN) queueMicrotask(() => {
        if (state.socket !== socket) return;
        try { publishCatalog(socket); }
        catch (error) { transportFailure(socket, error); }
      });
    } catch (error) {
      if (!state.stopped && state.readySettled && options.reconnect !== false) {
        state.reconnectTimer = setTimeout(connectGeneration, options.reconnectDelayMs ?? 250);
      } else if (!state.readySettled) {
        state.readySettled = true;
        releaseAdmission();
        rejectReady(error);
      }
      if (state.stopped || options.reconnect === false) resolveClosed();
    }
  }

  function publishCatalog(socket) {
    if (state.catalogSent) return;
    state.catalogSent = true;
    send(socket, {
      type: "catalog",
      tools: state.catalog,
      ...(options.observation === undefined ? {} : { observation_surfaces: options.observation.surfaces }),
      ...(state.machines.length === 0 ? {} : { machines: state.machines }),
      ...(state.attachmentId === undefined ? {} : { attachment_id: state.attachmentId }),
    });
  }

  async function captureObservation(frame, socket) {
    const reply = (result) => {
      if (state.socket === socket && !state.stopped && !state.draining) {
        try { send(socket, { type: "observation", request_id: frame.request_id, result }); }
        catch (error) { transportFailure(socket, error); }
      }
    };
    if (state.observation || !options.observation?.surfaces.some((s) => s.id === frame.surface_id)) {
      reply({ status: "unavailable", message: "Screen capture unavailable" });
      return;
    }
    const capture = { requestId: frame.request_id, controller: new AbortController() };
    state.observation = capture;
    const timer = setTimeout(() => capture.controller.abort(), OBSERVATION_TIMEOUT_MS);
    try {
      const image = await options.observation.capture({ surfaceId: frame.surface_id, signal: capture.controller.signal });
      if (!capture.controller.signal.aborted) reply({ status: "frame", frame: normalizeObservationFrame(image) });
    } catch {
      if (!capture.controller.signal.aborted) reply({ status: "unavailable", message: "Screen capture unavailable" });
    } finally {
      clearTimeout(timer);
      if (state.observation === capture) state.observation = undefined;
    }
  }

  async function handleFrame(encoded, socket) {
    if (state.socket !== socket) return;
    const frame = parseFrame(encoded);
    switch (frame.type) {
      case "ready":
        if (!state.catalogSent || state.readyReceived) throw new Error("ready received outside the catalog handshake");
        state.readyReceived = true;
        state.connected = true;
        clearTimeout(state.handshakeTimer);
        state.handshakeTimer = undefined;
        startHeartbeat(socket);
        if (!state.readySettled) { state.readySettled = true; resolveReady(publicClient); }
        break;
      case "observe":
        if (!state.readyReceived || state.draining) throw new Error("observe outside routing-ready socket");
        void captureObservation(frame, socket);
        break;
      case "observe_cancel":
        if (state.observation?.requestId === frame.request_id) state.observation.controller.abort();
        break;
      case "call":
        if (!state.readyReceived || state.drainAcknowledged) throw new Error("call received outside a routing-ready socket");
        await handleCall(frame, socket);
        break;
      case "cancel":
        if (!state.readyReceived) throw new Error("cancel received outside a routing-ready socket");
        handleCancel(frame, socket);
        break;
      case "ack":
        handleAck(frame, socket);
        break;
      case "pong":
        if (state.pendingNonce === undefined || frame.nonce !== state.pendingNonce) {
          throw new Error("pong nonce did not match the outstanding ping");
        }
        state.pendingNonce = undefined;
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

  async function handleCall(frame, socket) {
    const callId = frame.call_id;
    const identity = callIdentity(frame);
    const receipt = state.receipts.get(callId);
    if (receipt) {
      if (receipt.identity !== undefined && receipt.identity !== identity) throw new Error("completed call ID was reused with different immutable fields");
      send(socket, receipt.frame);
      return;
    }
    const active = state.calls.get(callId);
    if (active) {
      if (active.identity !== identity) throw new Error("active call ID was reused with different immutable fields");
      return;
    }
    if (state.calls.size >= 64) {
      retainAndSend(callId, identity, { status: "unavailable", message: "tool attachment has 64 active calls" }, socket);
      return;
    }
    if (state.receipts.size >= 512) throw new Error("tool attachment retained receipt bound is exhausted");
    if (frame.deadline_at <= Date.now()) {
      retainAndSend(callId, identity, { status: "unavailable", message: "tool attachment call deadline elapsed before dispatch" }, socket);
      return;
    }
    const controller = new AbortController();
    const call = { controller, identity };
    state.calls.set(callId, call);
    let deadline;
    const deadlinePromise = new Promise((resolve) => {
      const arm = () => {
        const remaining = frame.deadline_at - Date.now();
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
        admission.invoke(frame.name, frame.input, {
          sessionId: frame.session_id,
          parentCallId: "",
          callId,
          model: frame.model,
          signal: controller.signal,
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
    if (state.calls.get(callId) !== call) return;
    state.calls.delete(callId);
    if (!outcome) {
      try {
        const output = wireOutput(value);
        outcome = utf8ByteLength(JSON.stringify(output)) > frame.output_byte_budget
          ? { status: "ambiguous", message: "tool attachment output exceeded the admitted byte budget after dispatch" }
          : { status: "completed", output };
      } catch {
        outcome = { status: "ambiguous", message: "tool attachment result was not valid bounded wire output after dispatch" };
      }
    }
    if (state.socket !== socket) return;
    retainAndSend(callId, identity, outcome, socket);
    maybeFinishDrain(socket);
  }

  function handleCancel(frame, socket) {
    const callId = frame.call_id;
    const retained = state.receipts.get(callId);
    if (retained) {
      send(socket, retained.frame);
      return;
    }
    const call = state.calls.get(callId);
    if (call) {
      state.calls.delete(callId);
      call.controller.abort(new Error("tool attachment call was cancelled"));
    }
    if (state.receipts.size >= 512) throw new Error("tool attachment retained receipt bound is exhausted");
    retainAndSend(callId, call?.identity, call
      ? { status: "ambiguous", message: "tool execution was cancelled after dispatch" }
      : { status: "cancelled", message: "tool attachment call was cancelled before dispatch" }, socket);
    maybeFinishDrain(socket);
  }

  function handleAck(frame, socket) {
    const receipt = state.receipts.get(frame.call_id);
    if (!receipt) throw new Error("ack did not match a retained terminal result");
    state.receipts.delete(frame.call_id);
    maybeFinishDrain(socket);
  }

  function retainAndSend(callId, identity, outcome, socket) {
    const result = { type: "result", call_id: callId, outcome };
    state.receipts.set(callId, { identity, frame: result });
    send(socket, result);
  }

  function startHeartbeat(socket) {
    clearInterval(state.heartbeat);
    state.heartbeat = setInterval(() => {
      if (state.socket !== socket) return;
      if (state.pendingNonce !== undefined) {
        closeSocket(socket, 1012, "tool attachment heartbeat timed out");
        return;
      }
      const nonce = randomNonce();
      state.pendingNonce = nonce;
      try { send(socket, { type: "ping", nonce }); }
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
    state.stopped = true;
    state.connected = false;
    clearTimers(state);
    abortGeneration(state, new Error(reason));
    releaseAdmission();
    closeSocket(socket, 1008, closeReason(reason));
  }

  function transportFailure(socket, error) {
    if (state.socket !== socket) return;
    state.connected = false;
    closeSocket(socket, 1011, closeReason(`tool attachment transport failed: ${errorMessage(error)}`));
  }

  function closeSocket(socket, code, reason) {
    try { socket.close(code, reason); }
    catch {}
    socketClosed(socket);
  }

  function socketClosed(socket, event) {
    if (state.socket !== socket) return;
    const policyRejected = event?.code === 1008;
    if (policyRejected) state.stopped = true;
    state.connected = false;
    state.socket = undefined;
    state.catalogSent = false;
    state.readyReceived = false;
    clearTimers(state);
    abortGeneration(state, new Error("tool attachment disconnected"));
    if (!state.stopped && state.readySettled && options.reconnect !== false) {
      state.reconnectTimer = setTimeout(connectGeneration, options.reconnectDelayMs ?? 250);
    } else if (!state.readySettled) {
      state.readySettled = true;
      releaseAdmission();
      rejectReady(policyRejected
        ? new AttachmentRejectedError(event?.reason)
        : new Error("tool attachment closed before ready"));
    } else releaseAdmission();
    if (state.stopped || options.reconnect === false) resolveClosed();
  }

  function releaseAdmission() {
    if (state.admissionReleased) return;
    state.admissionReleased = true;
    admission.release();
  }
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
  const allowed = new Set(["provider", "reconnect", "reconnectDelayMs", "heartbeatMs", "handshakeTimeoutMs", "drainTimeoutMs", "machines", "attachmentId", "observation"]);
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
  try { socket.send(JSON.stringify(frame)); }
  catch (error) { throw new AttachmentTransportError(error); }
}
function parseFrame(encoded) {
  if (typeof encoded !== "string") throw new TypeError("tool attachments require text frames");
  if (utf8ByteLength(encoded) > 256 * 1024) throw new Error("tool attachment frame exceeds 262144 bytes");
  const frame = JSON.parse(encoded);
  if (!frame || typeof frame !== "object" || Array.isArray(frame)) throw new TypeError("tool attachment frame must be an object");
  const keys = DO_KEYS[frame.type];
  if (!keys) throw new Error(`unsupported durable-object tool attachment frame: ${frame.type}`);
  exactKeys(frame, keys);
  if (frame.type === "call") {
    requiredIdentifier(frame.session_id, "session_id"); requiredIdentifier(frame.call_id, "call_id");
    requiredIdentifier(frame.model, "model"); requiredIdentifier(frame.name, "name");
    if (typeof frame.input !== "string" && (!frame.input || typeof frame.input !== "object" || Array.isArray(frame.input))) throw new Error("call input must be an object or string");
    if (utf8ByteLength(JSON.stringify(frame.input)) > 128 * 1024) throw new Error("call input exceeds 131072 bytes");
    if (positiveInteger(frame.output_token_budget, "output_token_budget") > 1_000_000) throw new Error("output_token_budget exceeds protocol bound");
    if (positiveInteger(frame.output_byte_budget, "output_byte_budget") > 128 * 1024) throw new Error("output_byte_budget exceeds protocol bound");
    positiveInteger(frame.deadline_at, "deadline_at");
  } else if (frame.type === "observe" || frame.type === "observe_cancel") {
    requiredIdentifier(frame.request_id, "request_id");
    if (frame.type === "observe") requiredIdentifier(frame.surface_id, "surface_id");
  } else if (frame.type === "cancel" || frame.type === "ack") requiredIdentifier(frame.call_id, "call_id");
  else if (frame.type === "pong") {
    if (typeof frame.nonce !== "string" || !frame.nonce || utf8ByteLength(frame.nonce) > 128) throw new Error("invalid pong nonce");
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
  state.observation?.controller.abort(reason);
  for (const call of state.calls.values()) call.controller.abort(reason);
  state.calls.clear(); state.receipts.clear(); state.pendingNonce = undefined;
}
function positiveInteger(value, name) { if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${name} must be a positive safe integer`); return value; }
function positiveOption(value, fallback, name) { return value === undefined ? fallback : positiveInteger(value, name); }
function requiredIdentifier(value, name) { if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) throw new TypeError(`${name} must be a safe ASCII identifier`); return value; }
function exactKeys(value, allowed) { for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new Error(`${value.type} contains unsupported field ${key}`); }
function snapshot(value) { return value === undefined ? null : JSON.parse(JSON.stringify(value)); }
function decode(value) { if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) return new TextDecoder().decode(value); return String(value); }
function errorMessage(error) { return error && (error.stack || error.message) || String(error); }
function callIdentity(frame) { return JSON.stringify([frame.session_id, frame.call_id, frame.model, frame.name, frame.input, frame.output_token_budget, frame.output_byte_budget, frame.deadline_at]); }
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
function randomNonce() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

const DO_KEYS = Object.freeze({
  ready: ["type"],
  observe: ["type", "request_id", "surface_id"],
  observe_cancel: ["type", "request_id"],
  call: ["type", "session_id", "call_id", "model", "name", "input", "output_token_budget", "output_byte_budget", "deadline_at"],
  cancel: ["type", "call_id"],
  ack: ["type", "call_id"],
  pong: ["type", "nonce"],
  draining: ["type"],
});
