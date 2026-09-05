import { isIP } from "node:net";

import WebSocket, { WebSocketServer } from "ws";

import { defaultCodexAuthFile, readCodexSubscription } from "./codex-auth-file.mjs";

const CHATGPT_RESPONSES_URL = "wss://chatgpt.com/backend-api/codex/responses";
const DEFAULT_RESPONSES_PATH = "/api/responses";
const DEFAULT_STATUS_PATH = "/api/auth/chatgpt";
const MAX_BUFFERED_BYTES = 32 * 1024 * 1024;
const MAX_HEADER_BYTES = 16 * 1024;
const SESSION_ID_PATTERN = /^[A-Za-z0-9._:-]{1,200}$/;
const FORBIDDEN_DOWNSTREAM_HEADERS = new Set([
  "authorization",
  "chatgpt-account-id",
  "openai-beta",
  "originator",
  "proxy-authorization",
  "session-id",
  "thread-id",
  "x-client-request-id",
  "x-openai-fedramp",
  "x-openai-internal-codex-responses-lite",
  "x-responsesapi-include-timing-metrics",
]);

export function chatGptSubscription(options = {}) {
  return createChatGptSubscriptionPlugin(options, {
    WebSocketImpl: WebSocket,
    readAuth: readCodexSubscription,
    upstreamUrl: CHATGPT_RESPONSES_URL,
  });
}

/** Internal dependency seam used by provider-free tests. Not package-exported. */
export function createChatGptSubscriptionPlugin(options = {}, dependencies) {
  const responsesPath = exactPath(options.responsesPath ?? DEFAULT_RESPONSES_PATH, "responsesPath");
  const statusPath = options.statusPath === false
    ? false
    : exactPath(options.statusPath ?? DEFAULT_STATUS_PATH, "statusPath");
  const authFile = options.authFile === undefined
    ? defaultCodexAuthFile()
    : resolveAuthFile(options.authFile);
  const minimumTokenTtlMs = options.minimumTokenTtlMs ?? 5 * 60_000;
  if (!Number.isSafeInteger(minimumTokenTtlMs) || minimumTokenTtlMs < 0) {
    throw new TypeError("minimumTokenTtlMs must be a non-negative integer");
  }
  if (options.onEvent !== undefined && typeof options.onEvent !== "function") {
    throw new TypeError("onEvent must be a function");
  }

  return {
    name: "nanocodex-chatgpt-subscription",
    apply: "serve",
    enforce: "pre",
    async configureServer(vite) {
      const server = vite.httpServer;
      if (!server) throw new Error("Nanocodex local ChatGPT auth requires a Vite HTTP server");
      const downstreamServer = new WebSocketServer({
        noServer: true,
        maxPayload: MAX_BUFFERED_BYTES,
        perMessageDeflate: false,
      });
      const downstreams = new Set();
      const upstreams = new Set();
      let closed = false;
      const emit = (type, detail = {}) => {
        try { options.onEvent?.(Object.freeze({ type, ...detail })); } catch (error) {
          vite.config.logger.warn(`[nanocodex] local ChatGPT observer failed: ${errorMessage(error)}`);
        }
      };
      const loadAuth = () => dependencies.readAuth(authFile, { minimumTtlMs: minimumTokenTtlMs });

      if (statusPath !== false) {
        vite.middlewares.use((request, response, next) => {
          let url;
          try {
            url = new URL(request.url ?? "/", "http://127.0.0.1");
          } catch {
            next();
            return;
          }
          if (url.pathname !== statusPath || url.search || request.method !== "GET") {
            next();
            return;
          }
          if (!safeSameOriginRequest(request)) {
            response.writeHead(403, noStoreHeaders("text/plain; charset=utf-8"));
            response.end("Forbidden\n");
            return;
          }
          void loadAuth().then(
            (auth) => {
              const body = JSON.stringify({
                state: "authenticated",
                expiresAt: auth.expiresAt,
                managedLocally: true,
              });
              response.writeHead(200, {
                ...noStoreHeaders("application/json; charset=utf-8"),
                "content-length": Buffer.byteLength(body),
              });
              response.end(body);
            },
            () => next(),
          );
        });
      }

      const onUpgrade = (request, socket, head) => {
        const parsed = parseUpgrade(request, responsesPath);
        if (parsed === undefined) return;
        if (parsed instanceof Error) {
          rejectHttpUpgrade(socket, errorStatus(parsed), errorMessage(parsed));
          return;
        }
        if (closed) {
          rejectHttpUpgrade(socket, 503, "Development server is closing");
          return;
        }
        try {
          downstreamServer.handleUpgrade(request, socket, head, (downstream) => {
            downstreams.add(downstream);
            downstream.once("close", () => downstreams.delete(downstream));
            void connectUpstream({
              authFile,
              dependencies,
              downstream,
              emit,
              loadAuth,
              sessionId: parsed.sessionId,
              upstreams,
            });
          });
        } catch {
          rejectHttpUpgrade(socket, 502, "WebSocket upgrade failed");
        }
      };
      server.prependListener("upgrade", onUpgrade);

      const close = () => {
        if (closed) return;
        closed = true;
        server.removeListener("upgrade", onUpgrade);
        for (const socket of downstreams) socket.terminate();
        for (const socket of upstreams) socket.terminate();
        downstreamServer.close();
      };
      server.once("close", close);

      try {
        const auth = await loadAuth();
        emit("auth.ready", { expiresAt: auth.expiresAt });
        vite.config.logger.info(
          `[nanocodex] local ChatGPT subscription ready from ${authFile}`,
        );
      } catch (error) {
        emit("auth.unavailable", { kind: authErrorKind(error) });
        vite.config.logger.info(
          `[nanocodex] local ChatGPT subscription unavailable; run \`codex login\` (${errorMessage(error)})`,
        );
      }
    },
  };
}

async function connectUpstream({ dependencies, downstream, emit, loadAuth, sessionId, upstreams }) {
  let auth;
  try {
    auth = await loadAuth();
  } catch (error) {
    emit("auth.unavailable", { kind: authErrorKind(error) });
    rejectWebSocket(downstream, authErrorKind(error) === "expired" ? 401 : 503,
      "Local ChatGPT login unavailable; run `codex login` and retry");
    return;
  }
  if (downstream.readyState !== WebSocket.OPEN) return;

  const upstream = new dependencies.WebSocketImpl(dependencies.upstreamUrl, {
    handshakeTimeout: 8_000,
    headers: upstreamHeaders(auth, sessionId),
    maxPayload: MAX_BUFFERED_BYTES,
    perMessageDeflate: false,
  });
  upstreams.add(upstream);
  upstream.once("close", () => upstreams.delete(upstream));
  let opened = false;
  let readySent = false;
  let settled = false;

  const fail = (status, message) => {
    if (settled) return;
    settled = true;
    emit("upstream.rejected", { status });
    rejectWebSocket(downstream, status, message);
    if (upstream.readyState !== WebSocket.CLOSED) upstream.terminate();
  };
  upstream.once("unexpected-response", (_request, response) => {
    response.resume();
    fail(normalizeProviderStatus(response.statusCode), providerFailure(response.statusCode));
  });
  upstream.once("error", () => {
    if (!opened) fail(502, "ChatGPT WebSocket connection failed");
  });
  upstream.once("open", () => {
    if (settled || downstream.readyState !== WebSocket.OPEN) {
      upstream.terminate();
      return;
    }
    opened = true;
    settled = true;
    const pending = [];
    let pendingBytes = 0;
    const bufferUntilReady = (data, isBinary) => {
      pendingBytes += data.byteLength;
      if (pendingBytes > MAX_BUFFERED_BYTES) {
        downstream.close(1013, "relay backpressure limit");
        upstream.close(1013, "relay backpressure limit");
        return;
      }
      pending.push([data, isBinary]);
    };
    upstream.on("message", bufferUntilReady);
    downstream.send(JSON.stringify({ type: "nanocodex.proxy.ready" }), { compress: false }, (error) => {
      if (error) {
        downstream.terminate();
        upstream.terminate();
        return;
      }
      readySent = true;
      upstream.off("message", bufferUntilReady);
      bridge(downstream, upstream, emit, pending);
      emit("connection.ready");
    });
  });
  downstream.once("close", () => {
    if (!readySent && upstream.readyState !== WebSocket.CLOSED) upstream.terminate();
  });
}

function bridge(downstream, upstream, emit, pendingUpstreamMessages = []) {
  let closed = false;
  const relay = (source, destination, type) => (data, isBinary) => {
    if (closed || destination.readyState !== WebSocket.OPEN) return;
    if (isBinary) {
      closed = true;
      source.close(1003, "text frames required");
      destination.close(1003, "text frames required");
      return;
    }
    const byteLength = data.byteLength;
    if (destination.bufferedAmount + byteLength > MAX_BUFFERED_BYTES) {
      closed = true;
      source.close(1013, "relay backpressure limit");
      destination.close(1013, "relay backpressure limit");
      return;
    }
    destination.send(data, { binary: false, compress: false }, (error) => {
      if (error && !closed) {
        closed = true;
        source.terminate();
        destination.terminate();
      }
    });
    emit(type, { byteLength });
  };
  const relayDownstream = relay(downstream, upstream, "request.forwarded");
  const relayUpstream = relay(upstream, downstream, "response.forwarded");
  downstream.on("message", relayDownstream);
  upstream.on("message", relayUpstream);
  for (const [data, isBinary] of pendingUpstreamMessages) relayUpstream(data, isBinary);
  downstream.once("close", (code) => {
    emit("connection.closed", { side: "browser", code });
    closePeer(upstream, code);
  });
  upstream.once("close", (code) => {
    emit("connection.closed", { side: "provider", code });
    closePeer(downstream, code);
  });
  downstream.once("error", () => upstream.terminate());
  upstream.once("error", () => downstream.terminate());
}

function parseUpgrade(request, responsesPath) {
  let url;
  try {
    url = new URL(request.url ?? "/", "http://127.0.0.1");
  } catch {
    return new Error("Bad request");
  }
  if (url.pathname !== responsesPath) return undefined;
  if (request.method !== "GET") return statusError(405, "Method not allowed");
  if (!safeSameOriginRequest(request)) return statusError(403, "Forbidden");
  if (headerBytes(request) > MAX_HEADER_BYTES) return statusError(431, "Request headers too large");
  if (request.headers["sec-websocket-protocol"] !== undefined) {
    return statusError(400, "WebSocket subprotocols are not supported");
  }
  for (const name of FORBIDDEN_DOWNSTREAM_HEADERS) {
    if (request.headers[name] !== undefined) return statusError(400, "Credential headers are not accepted");
  }
  const entries = [...url.searchParams.entries()];
  if (entries.length !== 1 || entries[0][0] !== "session_id" || !SESSION_ID_PATTERN.test(entries[0][1])) {
    return statusError(400, "Invalid session");
  }
  return { sessionId: entries[0][1] };
}

function safeSameOriginRequest(request) {
  if (!isLoopbackAddress(request.socket.remoteAddress)) return false;
  const hosts = rawHeaderValues(request, "host");
  const origins = rawHeaderValues(request, "origin");
  if (hosts.length !== 1 || origins.length !== 1) return false;
  let origin;
  try {
    origin = new URL(origins[0]);
  } catch {
    return false;
  }
  const protocol = request.socket.encrypted === true ? "https:" : "http:";
  return origin.protocol === protocol
    && origin.host === hosts[0]
    && origin.pathname === "/"
    && !origin.search
    && !origin.hash
    && isLoopbackHostname(origin.hostname);
}

function rawHeaderValues(request, expectedName) {
  const values = [];
  for (let index = 0; index < request.rawHeaders.length; index += 2) {
    if (request.rawHeaders[index]?.toLowerCase() === expectedName) {
      values.push(request.rawHeaders[index + 1] ?? "");
    }
  }
  return values;
}

function headerBytes(request) {
  return request.rawHeaders.reduce((total, value) => total + Buffer.byteLength(value), 0);
}

function upstreamHeaders(auth, sessionId) {
  return {
    Authorization: `Bearer ${auth.accessToken}`,
    "ChatGPT-Account-ID": auth.accountId,
    "OpenAI-Beta": "responses_websockets=2026-02-06",
    originator: "codex_cli_rs",
    "x-openai-internal-codex-responses-lite": "true",
    "session-id": sessionId,
    "thread-id": sessionId,
    "x-client-request-id": sessionId,
    "x-responsesapi-include-timing-metrics": "true",
    "User-Agent": "codex_cli_rs/0.0.0",
    ...(auth.fedramp ? { "X-OpenAI-Fedramp": "true" } : {}),
  };
}

function providerFailure(status) {
  if (status === 401 || status === 403) {
    return "ChatGPT rejected the local login; run `codex login` and retry";
  }
  if (status === 429) return "ChatGPT rate limit reached";
  return "ChatGPT WebSocket upgrade failed";
}

function normalizeProviderStatus(status) {
  return Number.isInteger(status) && status >= 400 && status <= 599 ? status : 502;
}

function rejectWebSocket(socket, status, error) {
  if (socket.readyState !== WebSocket.OPEN) return;
  socket.send(JSON.stringify({ type: "nanocodex.proxy.rejected", status, error }), { compress: false }, () => {
    if (socket.readyState === WebSocket.OPEN) socket.close(status === 429 ? 1013 : 1011, "connection rejected");
  });
}

function closePeer(peer, code) {
  if (peer.readyState === WebSocket.OPEN) peer.close(safeCloseCode(code), "relay peer closed");
  else if (peer.readyState === WebSocket.CONNECTING) peer.terminate();
}

function safeCloseCode(code) {
  const standard = code >= 1000 && code <= 1014 && ![1004, 1005, 1006].includes(code);
  return standard || (code >= 3000 && code <= 4999) ? code : 1011;
}

function rejectHttpUpgrade(socket, status, message) {
  if (socket.destroyed || socket.writableEnded) return;
  const body = `${message}\n`;
  socket.once("error", () => {});
  socket.end(
    `HTTP/1.1 ${status} ${message}\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\n`
    + `Content-Length: ${Buffer.byteLength(body)}\r\nConnection: close\r\n\r\n${body}`,
  );
}

function noStoreHeaders(contentType) {
  return {
    "cache-control": "no-store",
    "content-type": contentType,
    "x-content-type-options": "nosniff",
  };
}

function exactPath(value, name) {
  if (typeof value !== "string" || !value.startsWith("/") || value.startsWith("//")) {
    throw new TypeError(`${name} must be an absolute URL path`);
  }
  const url = new URL(value, "http://nanocodex.invalid");
  if (url.pathname !== value || url.search || url.hash) {
    throw new TypeError(`${name} must not contain a query or fragment`);
  }
  return value;
}

function resolveAuthFile(value) {
  if (value instanceof URL) {
    if (value.protocol !== "file:") throw new TypeError("authFile URL must use file:");
    return decodeURIComponent(value.pathname);
  }
  if (typeof value !== "string" || !value.trim()) throw new TypeError("authFile must be a path or file URL");
  return value;
}

function isLoopbackAddress(address) {
  if (address === "::1") return true;
  const normalized = address?.startsWith("::ffff:") ? address.slice(7) : address;
  return isLoopbackHostname(normalized ?? "");
}

function isLoopbackHostname(hostname) {
  const normalized = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (normalized === "localhost" || normalized === "::1") return true;
  return isIP(normalized) === 4 && normalized.split(".", 1)[0] === "127";
}

function statusError(status, message) {
  return Object.assign(new Error(message), { status });
}

function errorStatus(error) {
  return Number.isInteger(error.status) ? error.status : 400;
}

function authErrorKind(error) {
  return typeof error?.kind === "string" ? error.kind : "invalid";
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
