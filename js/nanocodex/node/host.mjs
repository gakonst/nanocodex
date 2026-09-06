import { historyNotesHost } from "../runtime/history-notes.mjs";
import { Console } from "node:console";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import WebSocket from "ws";
import packageMetadata from "../package.json" with { type: "json" };

import { createCodeRuntime } from "../runtime/code-runtime.mjs";
import { createMcpRuntime } from "../runtime/mcp-runtime.mjs";
import {
  settleCleanup,
  toolRouterBrand,
  toolRouterRuntime,
  toolRuntimeLifecycle,
} from "../runtime/tool-router.mjs";
import { utf8ByteLength } from "../runtime/utf8.mjs";

const RESPONSES_WEBSOCKETS_BETA = "responses_websockets=2026-02-06";
const USER_AGENT = `nanocodex-wasm/${packageMetadata.version}`;
const DEFAULT_MAX_QUEUED_MESSAGES = 4_096;
const DEFAULT_MAX_QUEUED_BYTES = 32 * 1024 * 1024;
const DEFAULT_MAX_FRAME_BYTES = 16 * 1024 * 1024;
const MPP_CLIENT_PROTOCOL_ERROR_CLOSE_CODE = 3008;

export function createNodeHost(options = {}) {
  const historyNotes = historyNotesHost({ direct: !options.mpp });
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
  if (toolsMcp && options.mcpServers) {
    throw new TypeError("MCP is already configured in Tools");
  }
  if ((toolsMcp || options.mcpServers) && toolMode !== "code") {
    throw new TypeError("remote MCP requires Code Mode");
  }
  const toolsLifecycle = options.tools?.[toolRuntimeLifecycle];
  toolsLifecycle?.available();
  const connections = new Map();
  const code = createCodeRuntime(options.tools, {
    require: createRequire(resolve(options.workspace ?? process.cwd(), ".nanocodex-code-mode.cjs")),
    console: new Console({ stdout: process.stderr, stderr: process.stderr }),
    evaluate: options.codeEvaluator,
  });
  const filesystem = options.filesystem
    ? import("../runtime/workspace.mjs")
        .then(({ tools }) => code.addTools(tools(options.filesystem)))
    : undefined;
  const mcp = options.mcpServers
    ? createMcpRuntime(options.mcpServers, { clientName: "nanocodex-node" })
    : undefined;
  let disposal;
  const mcpInstalled = mcp?.then(async (provider) => {
    if (disposal) {
      await provider.close();
      return;
    }
    try { code.addProvider(provider, { id: "mcp", kind: "mcp" }); }
    catch (error) {
      try { await provider.close(); }
      catch (cleanupError) {
        throw new AggregateError([error, cleanupError], "MCP installation and cleanup failed");
      }
      throw error;
    }
  });
  const onEvent = options.onEvent || (() => {});
  const connectTimeoutMs = options.connectTimeoutMs ?? 30_000;
  const sendTimeoutMs = options.sendTimeoutMs ?? 30_000;
  const maxQueuedMessages = options.maxQueuedMessages ?? DEFAULT_MAX_QUEUED_MESSAGES;
  const maxQueuedBytes = options.maxQueuedBytes ?? DEFAULT_MAX_QUEUED_BYTES;
  const maxFrameBytes = options.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES;
  let nextHandle = 1;
  let references = 0;

  function connect(endpoint, apiKey, sessionId, metadata = {}) {
    if (options.mpp) return connectMpp(endpoint);
    return new Promise((resolve, reject) => {
      let settled = false;
      let upgradeResponse;
      const threadId = metadata.threadId ?? sessionId;
      const headers = {
        Authorization: `Bearer ${apiKey}`,
        "OpenAI-Beta": RESPONSES_WEBSOCKETS_BETA,
        "x-openai-internal-codex-responses-lite": "true",
        "session-id": sessionId,
        "thread-id": threadId,
        "x-client-request-id": threadId,
        "x-responsesapi-include-timing-metrics": "true",
        "User-Agent": USER_AGENT,
      };
      if (metadata.accountId) headers["ChatGPT-Account-ID"] = metadata.accountId;
      if (metadata.fedramp) headers["X-OpenAI-Fedramp"] = "true";
      if (metadata.turnState) headers["x-codex-turn-state"] = metadata.turnState;
      const socket = new WebSocket(endpoint, {
        handshakeTimeout: connectTimeoutMs,
        maxPayload: maxFrameBytes,
        headers,
      });
      const handle = nextHandle++;
      const connection = queueState(socket);

      socket.on("upgrade", (response) => { upgradeResponse = response; });
      socket.on("unexpected-response", (_request, response) => {
        if (settled) return;
        settled = true;
        response.setEncoding("utf8");
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.on("end", () => {
          const error = new Error(`WebSocket handshake was rejected with HTTP ${response.statusCode}`);
          error.status = response.statusCode;
          error.body = chunks.length ? chunks.join("") : "empty response body";
          const retryAfter = Number(header(response.headers, "retry-after"));
          if (Number.isFinite(retryAfter) && retryAfter >= 0) error.retryAfter = retryAfter;
          reject(error);
        });
      });
      socket.on("open", () => {
        settled = true;
        connections.set(handle, connection);
        const headers = upgradeResponse?.headers || {};
        resolve(JSON.stringify({
          handle,
          status: upgradeResponse?.statusCode || 101,
          request_id: header(headers, "x-request-id"),
          server_model: header(headers, "openai-model"),
          reasoning_included: header(headers, "x-reasoning-included") !== undefined,
          turn_state: header(headers, "x-codex-turn-state"),
        }));
      });
      socket.on("message", (data, isBinary) => {
        enqueue(connection, isBinary
          ? { kind: "binary" }
          : { kind: "text", text: data.toString("utf8") });
      });
      socket.on("close", (status, reason) => {
        if (!connection.intentionallyClosed && !connection.overflowed) {
          const suffix = reason.length ? `: ${reason.toString("utf8")}` : "";
          enqueue(connection, { kind: "closed", detail: `with code ${status}${suffix}` });
        }
      });
      socket.on("error", (error) => {
        if (!settled) {
          settled = true;
          reject(error);
        } else {
          enqueue(connection, { kind: "error", detail: errorMessage(error) });
        }
      });
    });
  }

  async function connectMpp(endpoint) {
    if (typeof options.mpp.ws !== "function") {
      throw new TypeError("mpp must provide ws(endpoint)");
    }
    const socket = await options.mpp.ws(endpoint);
    if (!socket || typeof socket.addEventListener !== "function") {
      throw new TypeError("mpp.ws(endpoint) must return a WebSocket");
    }
    const handle = nextHandle++;
    const connection = queueState(socket);
    connection.managed = true;
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

  function send(handle, message) {
    const connection = connections.get(handle);
    if (!connection || connection.socket.readyState !== WebSocket.OPEN) {
      return Promise.resolve(JSON.stringify({
        ok: false,
        reconnectable: true,
        error: "WebSocket is no longer open",
      }));
    }
    if (connection.managed) {
      try {
        connection.socket.send(JSON.stringify({ mpp: "message", data: message }));
        return Promise.resolve(JSON.stringify({ ok: true }));
      } catch (error) {
        return Promise.resolve(JSON.stringify({
          ok: false,
          reconnectable: connection.socket.readyState !== WebSocket.OPEN,
          error: errorMessage(error),
        }));
      }
    }
    return new Promise((resolve) => {
      let completed = false;
      const timer = setTimeout(() => finish({
        ok: false,
        reconnectable: false,
        error: `sending a WebSocket frame exceeded ${sendTimeoutMs} milliseconds`,
      }), sendTimeoutMs);
      function finish(result) {
        if (completed) return;
        completed = true;
        clearTimeout(timer);
        resolve(JSON.stringify(result));
      }
      connection.socket.send(message, (error) => finish(error ? {
        ok: false,
        reconnectable: connection.socket.readyState !== WebSocket.OPEN,
        error: errorMessage(error),
      } : { ok: true }));
    });
  }

  function next(handle, timeoutMs) {
    const connection = connections.get(handle);
    if (!connection) {
      return Promise.resolve(JSON.stringify({ kind: "closed", detail: "before the next frame" }));
    }
    if (connection.queue.length) {
      const entry = connection.queue.shift();
      connection.queuedBytes -= entry.bytes;
      return Promise.resolve(JSON.stringify(entry.message));
    }
    if (connection.waiter) return Promise.reject(new Error("concurrent reads are unsupported"));
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        connection.waiter = undefined;
        resolve(JSON.stringify({ kind: "timeout" }));
      }, timeoutMs);
      connection.waiter = (message) => {
        clearTimeout(timer);
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
    connection.waiter?.({ kind: "closed", detail: "by the WASM runtime" });
    connection.socket.close();
  }

  function enqueue(connection, message) {
    if (connection.overflowed) return;
    if (connection.waiter) {
      connection.waiter(message);
      return;
    }
    const bytes = messageBytes(message);
    if (connection.queue.length >= maxQueuedMessages || connection.queuedBytes + bytes > maxQueuedBytes) {
      connection.queue.length = 0;
      connection.queuedBytes = 0;
      connection.overflowed = true;
      const error = {
        kind: "error",
        detail: `receive queue exceeded ${maxQueuedMessages} messages or ${maxQueuedBytes} bytes`,
      };
      connection.queue.push({ message: error, bytes: messageBytes(error) });
      if (typeof connection.socket.terminate === "function") connection.socket.terminate();
      else connection.socket.close(1009, "receive queue exceeded configured bounds");
      return;
    }
    connection.queue.push({ message, bytes });
    connection.queuedBytes += bytes;
  }

  function dispose() {
    if (disposal) return disposal;
    historyNotes.cancel();
    disposal = Promise.resolve().then(() => settleCleanup([
      ...[...connections.keys()].map((handle) => () => close(handle)),
      () => code.reset(),
      () => mcpInstalled,
      () => toolsLifecycle?.close(),
      () => options.onDispose?.(),
    ], "Nanocodex host disposal failed", disposal));
    return disposal;
  }

  toolsLifecycle?.claim();
  return Object.freeze({
    ready: async () => { await Promise.all([filesystem, mcpInstalled]); },
    retain() {
      if (disposal) throw new Error("Nanocodex host is already disposed");
      references += 1;
    },
    release() {
      if (references > 0) references -= 1;
      return references === 0 ? dispose() : Promise.resolve();
    },
    historyNotes,
    connect,
    send,
    next,
    close,
    sleep: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    executeCode: code.executeCodeObserved,
    waitCode: code.waitCodeObserved,
    beginCodeTurn: code.beginTurn,
    cancelCodeTurn: (sessionId) => { historyNotes.cancel(sessionId); return code.cancelTurn(sessionId); },
    nextCodeUpdate: code.nextCodeUpdate,
    executeTool: code.executeTool,
    bindSubagentSession: code.bindSubagentSession,
    cancelCode: (sessionId) => { historyNotes.cancel(sessionId); return code.cancel(sessionId); },
    toolMode: () => toolMode,
    toolDefinitions: code.toolDefinitions,
    releaseSession: (sessionId) => { historyNotes.cancel(sessionId); return code.releaseSession(sessionId); },
    emitEvent: onEvent,
    reset: () => { historyNotes.cancel(); return code.reset(); },
    dispose,
  });
}

function queueState(socket) {
  return {
    socket,
    queue: [],
    queuedBytes: 0,
    waiter: undefined,
    intentionallyClosed: false,
    overflowed: false,
    managed: false,
  };
}

function header(headers, name) {
  const value = headers[name];
  return Array.isArray(value) ? value[0] : value;
}

function messageBytes(message) {
  return utf8ByteLength(message.kind === "text" ? message.text : JSON.stringify(message));
}

function errorMessage(error) {
  return error && (error.stack || error.message) || String(error);
}
