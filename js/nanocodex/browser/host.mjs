import { createBeforeCompaction } from "../runtime/before-compaction.mjs";
import { createResponsesHttp, responsesHttpHeaders } from "../runtime/responses-http.mjs";
import { createCodeRuntime, toolResult } from "../runtime/code-runtime.mjs";
import {
  toolRouterBrand,
  toolRouterRuntime,
  toolRuntimeLifecycle,
} from "../runtime/tool-router.mjs";
import { utf8ByteLength } from "../runtime/utf8.mjs";
import { createWorkerEvaluator } from "../runtime/worker-evaluator.mjs";
import { openHostManagedWebSocket } from "./hostManagedWebSocket.mjs";

const DEFAULT_MAX_QUEUED_MESSAGES = 4_096;
const DEFAULT_MAX_QUEUED_BYTES = 32 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_SEND_BYTES = 16 * 1024 * 1024;
const SEND_BUFFER_WAIT_MS = 5_000;
const SEND_BUFFER_POLL_MS = 10;
const MPP_CLIENT_PROTOCOL_ERROR_CLOSE_CODE = 3008;
const WEBSOCKET_OPEN = 1;

export function createBrowserHost(options = {}) {
  const onSocketTiming = options.onSocketTiming;
  if (onSocketTiming !== undefined && typeof onSocketTiming !== "function") {
    throw new TypeError("host socket timing hook must be a function");
  }
  const preservation = createBeforeCompaction(options.beforeCompaction);
  const toolMode = options.toolMode ?? "code";
  if (toolMode !== "code" && toolMode !== "direct") {
    throw new TypeError("toolMode must be code or direct");
  }
  const toolsRouter = options.tools?.[toolRouterBrand]
    ? options.tools[toolRouterRuntime]
    : undefined;
  const toolsMcp = toolsRouter?.hasSourceKind("mcp") === true;
  if (toolsRouter?.hasSource("workspace") && options.filesystem) {
    throw new TypeError("workspace is already configured in Tools");
  }
  if (toolsMcp && options.mcp) {
    throw new TypeError("MCP is already configured in Tools");
  }
  if ((toolsMcp || options.mcp) && toolMode !== "code") {
    throw new TypeError("remote MCP requires Code Mode");
  }
  const toolsLifecycle = options.tools?.[toolRuntimeLifecycle];
  toolsLifecycle?.available();
  const WebSocketImpl = options.WebSocketImpl ?? globalThis.WebSocket;
  const createWebSocket = options.createWebSocket
    ?? (options.hostManagedProtocol && ((endpoint, sessionId, request) =>
      openHostManagedWebSocket(endpoint, sessionId, {
        WebSocketImpl,
        threadId: request.threadId,
      })))
    ?? (WebSocketImpl && ((endpoint) => new WebSocketImpl(endpoint)));
  if (!options.mpp && !createWebSocket) {
    throw new Error("WebSocket is unavailable in this runtime");
  }
  const http = createResponsesHttp((endpoint, apiKey, sessionId, metadata, body, signal) => {
    if (disposal) throw new Error("Nanocodex host is already disposed");
    if (options.mpp) throw JSON.stringify({ kind: "transport", detail: "MPP HTTPS transport is unavailable", reconnectable: false });
    if (options.createResponse) {
      const authorization = options.hostAuth
        ? { authorization: "host_managed" }
        : { authorization: "bearer", bearerToken: apiKey };
      return options.createResponse(endpoint, sessionId, { ...metadata, ...authorization, body, signal });
    }
    if (options.hostAuth) throw JSON.stringify({ kind: "transport", detail: "host-managed HTTPS requires createResponse", reconnectable: false });
    return fetch(endpoint, { method: "POST", headers: responsesHttpHeaders(apiKey, sessionId, metadata),
      body, signal, redirect: "error" });
  });
  const connections = new Map();
  const openingAttempts = new Set();
  const connectingConnections = new Set();
  const codeEvaluator = options.codeEvaluator
    ?? (typeof globalThis.Worker === "function"
      ? createWorkerEvaluator()
      : () => Promise.reject(new Error(
          "browser Code Mode requires a child Worker or an explicit codeEvaluator",
        )));
  const code = createCodeRuntime(options.tools, {
    evaluate: codeEvaluator,
    subagentSessions: options.subagentSessions,
  });
  const toolProviders = options.toolProviders ?? [];
  if (!Array.isArray(toolProviders)) {
    throw new TypeError("toolProviders must be an array");
  }
  for (const [index, provider] of toolProviders.entries()) {
    const sourceOptions = {
      id: provider.sourceId ?? `attached:${String(index).padStart(8, "0")}`,
      kind: "attached",
      mode: "attached-over-cloud",
      deferred: true,
    };
    const sourceId = code.addProvider(provider, sourceOptions);
    provider.setCatalogValidator?.((definitions) =>
      code.validateProviderDefinitions(sourceId, definitions, sourceOptions));
  }
  const toolProvidersReady = Promise.all(
    toolProviders.map((provider) => provider.settled?.()),
  );
  if (options.filesystem && options.filesystemTools === false) {
    code.addTools({
      apply_patch: {
        description: "Apply a Rust-verified patch to the browser workspace.",
        parameters: { type: "object", additionalProperties: false },
        async handler(input, context) {
          if (typeof options.applyPatch !== "function") {
            throw new Error("the Rust browser apply_patch planner is unavailable");
          }
          const summary = await options.applyPatch(input, context.sessionId);
          return toolResult(summary, {});
        },
      },
    });
  }
  const filesystemReady = options.filesystem && options.filesystemTools !== false
    ? import("../runtime/workspace.mjs")
        .then(({ tools }) => code.addTools(tools(options.filesystem)))
    : undefined;
  const mcp = options.mcp
    ? import("../runtime/mcp-runtime.mjs").then(({ createMcpRuntime }) =>
        createMcpRuntime(options.mcp, { clientName: "nanocodex-browser" }))
    : undefined;
  const mcpInstalled = mcp?.then(async (provider) => {
    if (disposal) return provider.close();
    try { code.addProvider(provider, { id: "mcp", kind: "mcp" }); }
    catch (error) {
      try { await provider.close(); }
      catch (cleanupError) {
        throw new AggregateError([error, cleanupError], "MCP installation and cleanup failed");
      }
      throw error;
    }
    return provider;
  });
  const onEvent = options.onEvent || (() => {});
  const maxQueuedMessages = options.maxQueuedMessages ?? DEFAULT_MAX_QUEUED_MESSAGES;
  const maxQueuedBytes = options.maxQueuedBytes ?? DEFAULT_MAX_QUEUED_BYTES;
  const maxBufferedSendBytes = options.maxBufferedSendBytes ?? DEFAULT_MAX_BUFFERED_SEND_BYTES;
  let nextHandle = 1;
  let references = 0;
  let disposal;
  let disposalError;
  let preconnected;

  function preconnect(endpoint, sessionId) {
    if (disposal) return Promise.reject(new Error("Nanocodex host is already disposed"));
    if (preconnected?.endpoint === endpoint && preconnected.sessionId === sessionId) {
      return preconnected.ownership.promise.then(() => undefined);
    }
    void closePreconnected().catch(() => {});
    const ownership = openOwned(() => createWebSocket(endpoint, sessionId, {
      authorization: "preconnect",
    }));
    const entry = { endpoint, sessionId, ownership };
    preconnected = entry;
    void ownership.promise.catch(() => {
      if (preconnected === entry) preconnected = undefined;
    });
    return ownership.promise.then(() => undefined);
  }

  function takePreconnected(endpoint, sessionId) {
    if (preconnected?.endpoint !== endpoint || preconnected.sessionId !== sessionId) {
      return undefined;
    }
    const ownership = preconnected.ownership;
    preconnected = undefined;
    return ownership;
  }

  function closePreconnected(error = new Error("WebSocket preconnection was closed")) {
    const entry = preconnected;
    preconnected = undefined;
    if (!entry) return Promise.resolve();
    return entry.ownership.dispose(error);
  }

  async function connect(endpoint, apiKey, sessionId, metadata = {}) {
    if (disposal) throw new Error("Nanocodex host is already disposed");
    if (options.mpp) return connectMpp(endpoint);
    const authorization = options.hostAuth
      ? { authorization: "host_managed" }
      : { authorization: "bearer", bearerToken: apiKey };
    const request = { ...metadata };
    delete request.authorization;
    delete request.bearerToken;
    Object.assign(request, authorization);
    const threadId = metadata.threadId ?? sessionId;
    let ownership = threadId === sessionId ? takePreconnected(endpoint, sessionId) : undefined;
    if (ownership === undefined) {
      if (preconnected !== undefined) await closePreconnected();
      ownership = openOwned(() => createWebSocket(endpoint, sessionId, request));
    }
    const opened = await ownership.promise;
    const { socket, ...handshake } = normalizeWebSocketConnection(opened);
    return new Promise((resolve, reject) => {
      let settled = false;
      const connection = {
        socket,
        timing: socketTiming(),
        queue: [],
        queuedBytes: 0,
        waiter: undefined,
        intentionallyClosed: false,
        overflowed: false,
      };
      const rejectConnection = (error) => {
        if (settled) return;
        settled = true;
        connectingConnections.delete(connection);
        finishSocketTiming(connection);
        reject(error);
      };
      connection.reject = rejectConnection;
      ownership.transfer();
      connectingConnections.add(connection);
      const resolveOpen = () => {
        if (settled) return;
        if (disposal) {
          connection.intentionallyClosed = true;
          settled = true;
          reject(disposalError);
          return;
        }
        settled = true;
        connectingConnections.delete(connection);
        delete connection.reject;
        const handle = nextHandle++;
        connections.set(handle, connection);
        resolve(JSON.stringify({
          handle,
          status: handshake.status ?? 101,
          request_id: handshake.requestId,
          server_model: handshake.serverModel,
          reasoning_included: handshake.reasoningIncluded ?? false,
          turn_state: handshake.turnState,
        }));
      };
      socket.addEventListener("open", resolveOpen, { once: true });
      socket.addEventListener("message", (event) => {
        enqueue(connection, typeof event.data === "string"
          ? { kind: "text", text: event.data }
          : { kind: "binary" });
      });
      socket.addEventListener("close", (event) => {
        connection.wakeSend?.();
        if (!settled) {
          rejectConnection(new Error(`WebSocket closed during connection with code ${event.code}`));
        } else if (!connection.intentionallyClosed && !connection.overflowed) {
          enqueue(connection, { kind: "closed", detail: `with code ${event.code}` });
        }
      });
      socket.addEventListener("error", () => {
        if (!settled) {
          rejectConnection(new Error("WebSocket connection failed"));
        } else {
          enqueue(connection, { kind: "error", detail: "WebSocket connection failed" });
        }
      });
      if (socket.readyState === WEBSOCKET_OPEN) resolveOpen();
      else if (socket.readyState > WEBSOCKET_OPEN) {
        rejectConnection(new Error("WebSocket closed during connection"));
      }
    });
  }

  async function connectMpp(endpoint) {
    if (disposal) throw new Error("Nanocodex host is already disposed");
    if (typeof options.mpp.ws !== "function") {
      throw new TypeError("mpp must provide ws(endpoint)");
    }
    const ownership = openOwned(() => options.mpp.ws(endpoint));
    const socket = await ownership.promise;
    if (!socket || typeof socket.addEventListener !== "function") {
      throw new TypeError("mpp.ws(endpoint) must return a WebSocket");
    }
    const handle = nextHandle++;
    const connection = {
      socket,
      timing: socketTiming(),
      queue: [],
      queuedBytes: 0,
      waiter: undefined,
      intentionallyClosed: false,
      overflowed: false,
      managed: true,
    };
    ownership.transfer();
    connections.set(handle, connection);
    socket.addEventListener("message", (event) => {
      enqueue(connection, typeof event.data === "string"
        ? { kind: "text", text: event.data }
        : { kind: "binary" });
    });
    socket.addEventListener("close", (event) => {
      if (!connection.intentionallyClosed && !connection.overflowed) {
        const code = event.code ?? 1000;
        const suffix = event.reason ? `: ${event.reason}` : "";
        enqueue(connection, code === MPP_CLIENT_PROTOCOL_ERROR_CLOSE_CODE
          ? {
              kind: "error",
              detail: `MPP WebSocket payment flow failed with code ${code}${suffix}`,
              reconnectable: false,
            }
          : { kind: "closed", detail: `with code ${code}${suffix}` });
      }
    });
    socket.addEventListener("error", () => {
      enqueue(connection, { kind: "error", detail: "MPP WebSocket connection failed" });
    });
    return JSON.stringify({ handle, status: 101, reasoning_included: false });
  }

  async function send(handle, message) {
    const connection = connections.get(handle);
    const closed = () => disposal || !connection || connections.get(handle) !== connection
      || connection.socket.readyState !== WEBSOCKET_OPEN;
    const closedResult = () => JSON.stringify({
      ok: false,
      reconnectable: true,
      error: "WebSocket is no longer open",
    });
    if (closed()) return closedResult();
    // The runtime sends one request at a time. Do not retain an unbounded queue
    // of request bodies while the underlying socket is under pressure.
    if (connection.sending) {
      return JSON.stringify({ ok: false, reconnectable: false,
        error: "concurrent WebSocket sends are unsupported" });
    }
    connection.sending = true;
    try {
      if (connection.managed) {
        connection.socket.send(JSON.stringify({ mpp: "message", data: message }));
        return JSON.stringify({ ok: true });
      }
      const frameBytes = utf8ByteLength(message);
      if (frameBytes > maxBufferedSendBytes) {
        return JSON.stringify({
          ok: false,
          reconnectable: false,
          error: `WebSocket frame size ${frameBytes} bytes exceeds ${maxBufferedSendBytes} bytes (buffered ${connection.socket.bufferedAmount} bytes)`,
        });
      }
      const deadline = Date.now() + SEND_BUFFER_WAIT_MS;
      while (connection.socket.bufferedAmount + frameBytes > maxBufferedSendBytes) {
        if (closed()) return closedResult();
        if (Date.now() >= deadline) {
          // Nothing was sent. The transport may reconnect, but this invocation
          // must never send later after reporting a retryable failure.
          return JSON.stringify({ ok: false, reconnectable: true,
            error: `WebSocket send buffer did not drain within ${SEND_BUFFER_WAIT_MS} ms (frame ${frameBytes} bytes, buffered ${connection.socket.bufferedAmount} bytes, limit ${maxBufferedSendBytes} bytes)` });
        }
        await new Promise((resolve) => {
          const timer = setTimeout(wake, SEND_BUFFER_POLL_MS);
          function wake() {
            clearTimeout(timer);
            connection.wakeSend = undefined;
            resolve();
          }
          connection.wakeSend = wake;
        });
      }
      if (closed()) return closedResult();
      connection.socket.send(message);
      return JSON.stringify({ ok: true });
    } catch (error) {
      return JSON.stringify({
        ok: false,
        reconnectable: connection.socket.readyState !== WEBSOCKET_OPEN,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      connection.sending = false;
    }
  }

  function next(handle) {
    const connection = connections.get(handle);
    if (!connection) {
      return Promise.resolve(JSON.stringify({ kind: "closed", detail: "before the next frame" }));
    }
    if (connection.queue.length) {
      const entry = connection.queue.shift();
      connection.queuedBytes -= entry.bytes;
      if (entry.enqueuedAt !== undefined && connection.timing) {
        const metrics = connection.timing.metrics;
        const residenceMs = Math.max(0, performance.now() - entry.enqueuedAt);
        metrics.delivered_message_count += 1;
        metrics.queue_residence_total_ms += residenceMs;
        metrics.queue_residence_max_ms = Math.max(metrics.queue_residence_max_ms, residenceMs);
      }
      return Promise.resolve(JSON.stringify(entry.message));
    }
    if (connection.waiter) return Promise.reject(new Error("concurrent reads are unsupported"));
    return new Promise((resolve) => {
      connection.waiter = (message) => {
        connection.waiter = undefined;
        resolve(JSON.stringify(message));
      };
    });
  }

  function close(handle) {
    const connection = connections.get(handle);
    if (!connection) return;
    connections.delete(handle);
    connection.intentionallyClosed = true;
    connection.wakeSend?.();
    connection.waiter?.({ kind: "closed", detail: "by the WASM runtime" });
    finishSocketTiming(connection);
    connection.queue.length = 0;
    connection.queuedBytes = 0;
    return connection.socket.close();
  }

  function enqueue(connection, message) {
    if (connection.overflowed || connection.intentionallyClosed) return;
    const timing = connection.timing;
    const dataMessage = message.kind === "text" || message.kind === "binary";
    const enqueuedAt = timing && dataMessage && !connection.waiter ? performance.now() : undefined;
    if (timing && dataMessage) {
      timing.metrics.message_count += 1;
      // Only the exact metadata event can produce diagnostics; deltas stay opaque.
      if (message.kind === "text" && timing.provider.size < 32) {
        const metadata = providerSocketTiming(message.text);
        if (metadata) {
          const key = metadata.response_id ?? timing.provider.size;
          if (!timing.provider.has(key)) timing.provider.set(key, metadata);
        }
      }
    }
    if (connection.waiter) {
      if (timing && dataMessage) timing.metrics.delivered_message_count += 1;
      connection.waiter(message);
      return;
    }
    const bytes = utf8ByteLength(message.kind === "text" ? message.text : JSON.stringify(message));
    if (connection.queue.length >= maxQueuedMessages || connection.queuedBytes + bytes > maxQueuedBytes) {
      connection.queue.length = 0;
      connection.queuedBytes = 0;
      connection.overflowed = true;
      const error = {
        kind: "error",
        detail: `receive queue exceeded ${maxQueuedMessages} messages or ${maxQueuedBytes} bytes`,
      };
      const errorBytes = utf8ByteLength(JSON.stringify(error));
      connection.queue.push({ message: error, bytes: errorBytes });
      connection.queuedBytes = errorBytes;
      connection.socket.close(1009, "receive queue exceeded configured bounds");
      return;
    }
    const entry = { message, bytes };
    if (enqueuedAt !== undefined) {
      entry.enqueuedAt = enqueuedAt;
      timing.metrics.buffered_message_count += 1;
    }
    connection.queue.push(entry);
    connection.queuedBytes += bytes;
  }

  // No counters, timestamps, metadata parsing or observations without the internal hook.
  function socketTiming() {
    return onSocketTiming === undefined ? undefined : {
      metrics: {
        message_count: 0,
        delivered_message_count: 0,
        buffered_message_count: 0,
        queue_residence_total_ms: 0,
        queue_residence_max_ms: 0,
      },
      provider: new Map(),
    };
  }

  function finishSocketTiming(connection) {
    const timing = connection.timing;
    if (!timing) return;
    // Finalize owned consumption, not the remote close event: buffered frames may
    // still be drained after that event. Clear first for reentrant close/dispose.
    connection.timing = undefined;
    const metrics = timing.metrics;
    try {
      const result = onSocketTiming({
        ...metrics,
        discarded_message_count: metrics.message_count - metrics.delivered_message_count,
        provider_timings: [...timing.provider.values()],
      });
      // Diagnostics must never fail transport cleanup, including an async hook.
      if (result?.then) void Promise.resolve(result).catch(() => {});
    } catch { /* Passive observations cannot fail transport cleanup. */ }
  }

  function dispose() {
    if (disposal) return disposal;
    preservation.dispose();
    http.dispose();
    disposalError = new Error("Nanocodex host was disposed during WebSocket connection");
    disposal = Promise.resolve().then(async () => {
      const cleanups = [];
      const cleanup = (action) => {
        try { cleanups.push(Promise.resolve(action())); }
        catch (failure) { cleanups.push(Promise.reject(failure)); }
      };
      const preconnectedOwnership = preconnected?.ownership;
      for (const attempt of [...openingAttempts]) {
        if (attempt !== preconnectedOwnership) cleanup(() => attempt.dispose(disposalError));
      }
      for (const connection of [...connectingConnections]) {
        connection.intentionallyClosed = true;
        cleanup(() => connection.reject(disposalError));
        cleanup(() => connection.socket.close());
      }
      for (const handle of [...connections.keys()]) cleanup(() => close(handle));
      cleanup(() => closePreconnected(disposalError));
      cleanup(() => code.reset());
      cleanup(() => mcpInstalled?.then(() => {}));
      cleanup(() => toolsLifecycle?.close());
      cleanup(() => options.onDispose?.());
      const settled = await Promise.allSettled(cleanups);
      const errors = settled
        .filter((result) => result.status === "rejected")
        .map((result) => result.reason);
      if (errors.length === 1) throw errors[0];
      if (errors.length > 1) {
        throw new AggregateError(errors, "Nanocodex host disposal failed");
      }
    });
    return disposal;
  }

  function openOwned(factory) {
    if (disposal) throw new Error("Nanocodex host is already disposed");
    let rejectDisposed;
    const disposed = new Promise((_, reject) => { rejectDisposed = reject; });
    const ownership = {
      disposed: false,
      opened: undefined,
      closePromise: undefined,
      error: undefined,
      dispose(error) {
        if (ownership.disposed) return ownership.closePromise ?? Promise.resolve();
        ownership.disposed = true;
        ownership.error = error;
        openingAttempts.delete(ownership);
        rejectDisposed(error);
        return closeOpened();
      },
      transfer() {
        if (disposal || ownership.disposed) {
          throw disposalError ?? ownership.error
            ?? new Error("Nanocodex host was disposed during WebSocket connection");
        }
        openingAttempts.delete(ownership);
      },
    };
    openingAttempts.add(ownership);
    let opening;
    try {
      opening = Promise.resolve(factory());
    } catch (error) {
      opening = Promise.reject(error);
    }
    opening = opening.then((opened) => {
      ownership.opened = opened;
      if (!ownership.disposed) return opened;
      void closeOpened().catch(() => {});
      throw ownership.error;
    });
    ownership.promise = Promise.race([opening, disposed]);
    void ownership.promise.catch(() => {
      openingAttempts.delete(ownership);
    });
    return ownership;

    function closeOpened() {
      if (ownership.closePromise) return ownership.closePromise;
      if (ownership.opened === undefined) return Promise.resolve();
      try {
        ownership.closePromise = Promise.resolve(
          normalizeWebSocketConnection(ownership.opened).socket.close(),
        );
      } catch (error) {
        ownership.closePromise = Promise.reject(error);
      }
      return ownership.closePromise;
    }
  }

  toolsLifecycle?.claim();
  return Object.freeze({
    ready: async () => {
      await Promise.all([filesystemReady, mcpInstalled, toolProvidersReady]);
    },
    retain() {
      if (disposal) throw new Error("Nanocodex host is already disposed");
      references += 1;
    },
    release() {
      if (references > 0) references -= 1;
      return references === 0 ? dispose() : Promise.resolve();
    },
    beforeCompaction: preservation.preserve,
    cancelBeforeCompaction: preservation.cancel,
    httpOpen: http.httpOpen,
    httpReady: http.httpReady,
    httpNext: http.httpNext,
    httpClose: http.httpClose,
    connect,
    preconnect,
    send,
    next,
    close,
    sleep: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    executeCode: code.executeCodeObserved,
    waitCode: code.waitCodeObserved,
    beginCodeTurn: code.beginTurn,
    cancelCodeTurn: code.cancelTurn,
    nextCodeUpdate: code.nextCodeUpdate,
    executeTool: code.executeTool,
    routeSubagent: (request) => {
      if (!options.subagentRouting) throw new Error("subagent routing is not configured");
      return options.subagentRouting.resolve(request);
    },
    bindSubagentRoute: (request) => {
      if (!options.subagentRouting) throw new Error("subagent routing is not configured");
      return options.subagentRouting.bind(request);
    },
    bindSubagentSession: code.bindSubagentSession,
    cancelCode: code.cancel,
    readWorkspaceFile: async (path) => {
      if (!options.filesystem) throw new Error("browser workspace is unavailable");
      const contents = await options.filesystem.readFile(path);
      if (!(contents instanceof Uint8Array)) {
        throw new TypeError("browser workspace readFile() must return Uint8Array");
      }
      return contents;
    },
    listWorkspace: async (path) => {
      if (!options.filesystem) throw new Error("browser workspace is unavailable");
      if (typeof options.filesystem.list !== "function") {
        throw new Error("browser workspace does not expose list()");
      }
      return options.filesystem.list(path, { maxEntries: 2_000 });
    },
    writeWorkspaceFile: async (path, contents) => {
      if (!options.filesystem) throw new Error("browser workspace is unavailable");
      await options.filesystem.writeFile(path, contents);
    },
    removeWorkspaceFile: async (path) => {
      if (!options.filesystem) throw new Error("browser workspace is unavailable");
      await options.filesystem.remove(path);
    },
    toolMode: () => toolMode,
    toolDefinitions: code.toolDefinitions,
    releaseSession: code.releaseSession,
    emitEvent: onEvent,
    reset: code.reset,
    dispose,
  });
}

function normalizeWebSocketConnection(opened) {
  if (opened?.socket && typeof opened.socket.addEventListener === "function") {
    return opened;
  }
  if (!opened || typeof opened.addEventListener !== "function") {
    throw new TypeError("createWebSocket must return a WebSocket or a connection descriptor");
  }
  return { socket: opened };
}

// Provider-reported fields are nested spans, not additive measurements. Bound the
// diagnostic parser and retain only three finite durations plus a response ID.
function providerSocketTiming(text) {
  if (text.length > 16_384 || !text.includes('"responsesapi.websocket_timing"')) return;
  let event;
  try { event = JSON.parse(text); } catch { return; }
  if (event?.type !== "responsesapi.websocket_timing"
    || (event.response_id !== undefined && (typeof event.response_id !== "string"
      || !/^resp_[A-Za-z0-9_-]{1,128}$/.test(event.response_id)))
    || !event.timing_metrics || typeof event.timing_metrics !== "object"
    || Array.isArray(event.timing_metrics)) return;
  const timing = {};
  for (const key of ["pre_inference_ms", "engine_queue_max_ms", "engine_service_ttft_total_ms"]) {
    const value = event.timing_metrics[key];
    if (typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 86_400_000) {
      timing[key] = value;
    }
  }
  if (Object.keys(timing).length) return {
    ...(event.response_id === undefined ? {} : { response_id: event.response_id }), ...timing,
  };
}
