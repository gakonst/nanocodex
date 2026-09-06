import { randomBytes } from "node:crypto";
import { createServer } from "node:http";
import { Readable } from "node:stream";

import WebSocket, { WebSocketServer } from "ws";

const CHATGPT_HOST = "chatgpt.com";
const CHATGPT_PATH_PREFIX = "/backend-api/codex/";
const RELAY_HTTP_PATHS = new Map([
  ["codex-web-search", "/backend-api/codex/alpha/search"],
  ["codex-image-generation", "/backend-api/codex/images/generations"],
  ["codex-image-edit", "/backend-api/codex/images/edits"],
  ["codex-history-list-windows", "/backend-api/codex/alpha/history/v2/list_windows"],
  ["codex-history-list-items", "/backend-api/codex/alpha/history/v2/list_items"],
  ["codex-history-read-item", "/backend-api/codex/alpha/history/v2/read_item"],
  ["codex-history-search-contents", "/backend-api/codex/alpha/history/v2/search_contents"],
  ["codex-notes-list-files-by-prefix", "/backend-api/codex/alpha/notes/v2/list_files_by_prefix"],
  ["codex-notes-read-file", "/backend-api/codex/alpha/notes/v2/read_file"],
  ["codex-notes-search-contents", "/backend-api/codex/alpha/notes/v2/search_contents"],
  ["codex-notes-write-file", "/backend-api/codex/alpha/notes/v2/write_file"],
  ["codex-notes-append-to-file", "/backend-api/codex/alpha/notes/v2/append_to_file"],
  ["codex-notes-thread-hint", "/backend-api/codex/alpha/notes/v2/thread_hint"],
]);
const MAX_BUFFERED_BYTES = 32 * 1024 * 1024;

export async function startChatGptWorkerEgress(options = {}) {
  const capability = options.capability ?? randomBytes(32).toString("base64url");
  if (!/^[A-Za-z0-9_-]{43}$/.test(capability)) {
    throw new TypeError("ChatGPT development egress capability must be 43 base64url characters");
  }
  const localPrefix = `/${capability}`;
  const relayPrefix = `/v1/${capability}`;
  const sockets = new Set();
  const upstreams = new Set();
  const downstreamServer = new WebSocketServer({
    noServer: true,
    maxPayload: MAX_BUFFERED_BYTES,
    perMessageDeflate: false,
  });
  const server = createServer((request, response) => {
    void proxyHttpRequest(
      request,
      response,
      options.fetchImpl ?? fetch,
      localPrefix,
      relayPrefix,
    );
  });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  server.on("upgrade", (request, socket, head) => {
    proxyWebSocketUpgrade({
      downstreamServer,
      head,
      openUpstream: options.openUpstream,
      localPrefix,
      relayPrefix,
      request,
      socket,
      upstreams,
    });
  });
  server.on("clientError", (_error, socket) => {
    socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
  });

  await new Promise((resolve, reject) => {
    const onError = (error) => reject(error);
    server.once("error", onError);
    server.listen(options.port ?? 0, "127.0.0.1", () => {
      server.off("error", onError);
      resolve();
    });
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("ChatGPT development egress did not bind TCP");

  let closed = false;
  return Object.freeze({
    url: `http://127.0.0.1:${address.port}${localPrefix}/`,
    relayUrl: `http://127.0.0.1:${address.port}${relayPrefix}`,
    async close() {
      if (closed) return;
      closed = true;
      for (const socket of downstreamServer.clients) socket.terminate();
      for (const socket of upstreams) socket.terminate();
      for (const socket of sockets) socket.destroy();
      await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
    },
  });
}

async function proxyHttpRequest(request, response, fetchImpl, localPrefix, relayPrefix) {
  const upstreamPath = allowedUpstreamPath(request.url, localPrefix, relayPrefix, false);
  if (!safeLoopbackRequest(request) || upstreamPath === undefined) {
    response.writeHead(404, noStoreHeaders("text/plain; charset=utf-8"));
    response.end("Not found\n");
    return;
  }
  try {
    const method = request.method ?? "GET";
    const upstream = await fetchImpl(`https://${CHATGPT_HOST}${upstreamPath}`, {
      method,
      headers: upstreamHttpHeaders(request),
      body: method === "GET" || method === "HEAD"
        ? undefined
        : Readable.toWeb(request),
      duplex: "half",
      redirect: "manual",
    });
    const headers = new Headers(upstream.headers);
    headers.delete("content-encoding");
    headers.delete("content-length");
    headers.set("cache-control", "no-store");
    response.writeHead(upstream.status, upstream.statusText, Object.fromEntries(headers));
    if (upstream.body) Readable.fromWeb(upstream.body).pipe(response);
    else response.end();
  } catch {
    if (!response.headersSent) response.writeHead(502, noStoreHeaders("text/plain; charset=utf-8"));
    response.end("ChatGPT development egress failed\n");
  }
}

function proxyWebSocketUpgrade({
  downstreamServer,
  head,
  localPrefix,
  openUpstream,
  relayPrefix,
  request,
  socket,
  upstreams,
}) {
  socket.on("error", () => {});
  const upstreamPath = allowedUpstreamPath(request.url, localPrefix, relayPrefix, true);
  if (!safeLoopbackRequest(request) || upstreamPath === undefined) {
    rejectUpgrade(socket, 404, "Not found");
    return;
  }
  const upstreamOptions = {
    handshakeTimeout: 15_000,
    headers: upstreamWebSocketHeaders(request),
    maxPayload: MAX_BUFFERED_BYTES,
    perMessageDeflate: false,
  };
  const upstream = openUpstream
    ? openUpstream(`wss://${CHATGPT_HOST}${upstreamPath}`, upstreamOptions)
    : new WebSocket(`wss://${CHATGPT_HOST}${upstreamPath}`, upstreamOptions);
  upstreams.add(upstream);
  upstream.once("close", () => upstreams.delete(upstream));
  let settled = false;
  upstream.once("open", () => {
    if (settled || socket.destroyed) {
      upstream.terminate();
      return;
    }
    settled = true;
    try {
      downstreamServer.handleUpgrade(request, socket, head, (downstream) => {
        bridge(downstream, upstream);
      });
    } catch {
      upstream.terminate();
      rejectUpgrade(socket, 502, "WebSocket upgrade failed");
    }
  });
  upstream.once("unexpected-response", (_request, response) => {
    response.resume();
    if (settled || socket.destroyed) return;
    settled = true;
    rejectUpgrade(socket, safeStatus(response.statusCode), "ChatGPT WebSocket rejected");
    upstream.terminate();
  });
  upstream.once("error", () => {
    if (settled || socket.destroyed) return;
    settled = true;
    rejectUpgrade(socket, 502, "ChatGPT WebSocket failed");
  });
  socket.once("close", () => {
    if (!settled && upstream.readyState !== WebSocket.CLOSED) upstream.terminate();
  });
}

function bridge(left, right) {
  let closed = false;
  const relay = (source, destination) => (data, isBinary) => {
    if (closed || destination.readyState !== WebSocket.OPEN) return;
    if (destination.bufferedAmount + data.byteLength > MAX_BUFFERED_BYTES) {
      closed = true;
      source.close(1013, "relay backpressure limit");
      destination.close(1013, "relay backpressure limit");
      return;
    }
    destination.send(data, { binary: isBinary, compress: false }, (error) => {
      if (error && !closed) {
        closed = true;
        source.terminate();
        destination.terminate();
      }
    });
  };
  left.on("message", relay(left, right));
  right.on("message", relay(right, left));
  left.once("close", (code) => closePeer(right, code));
  right.once("close", (code) => closePeer(left, code));
  left.once("error", () => right.terminate());
  right.once("error", () => left.terminate());
}

function upstreamHttpHeaders(request) {
  const headers = new Headers();
  for (const name of [
    "accept",
    "authorization",
    "chatgpt-account-id",
    "content-type",
    "x-openai-tool-output-truncation-policy",
    "x-openai-encrypted-tool-arguments",
    "openai-alpha",
    "originator",
    "session-id",
    "thread-id",
    "user-agent",
    "x-oai-attestation",
    "x-openai-fedramp",
    "x-session-id",
  ]) {
    const value = request.headers[name];
    if (typeof value === "string") headers.set(name, value);
  }
  return headers;
}

function upstreamWebSocketHeaders(request) {
  const allowed = new Set([
    "authorization",
    "chatgpt-account-id",
    "openai-beta",
    "originator",
    "session-id",
    "thread-id",
    "user-agent",
    "x-client-request-id",
    "x-openai-fedramp",
    "x-openai-internal-codex-responses-lite",
    "x-responsesapi-include-timing-metrics",
  ]);
  return Object.fromEntries(
    Object.entries(request.headers).flatMap(([name, value]) =>
      value === undefined || !allowed.has(name) ? [] : [[name, value]]),
  );
}

function safeLoopbackRequest(request) {
  const address = request.socket.remoteAddress;
  return address === "127.0.0.1" || address === "::1" || address === "::ffff:127.0.0.1";
}

function allowedUpstreamPath(path, localPrefix, relayPrefix, websocket) {
  if (typeof path !== "string") return undefined;
  try {
    const url = new URL(path, "http://127.0.0.1");
    const expectedPrefix = `${localPrefix}${CHATGPT_PATH_PREFIX}`;
    if (url.pathname.startsWith(expectedPrefix)) {
      return `${url.pathname.slice(localPrefix.length)}${url.search}`;
    }
    if (websocket && url.pathname === relayPrefix && !url.search) {
      return "/backend-api/codex/responses";
    }
    if (!websocket) {
      const route = url.pathname.slice(`${relayPrefix}/http/`.length);
      const upstream = url.pathname.startsWith(`${relayPrefix}/http/`)
        ? RELAY_HTTP_PATHS.get(route)
        : undefined;
      if (upstream) return `${upstream}${url.search}`;
    }
    return undefined;
  } catch {
    return undefined;
  }
}

function closePeer(peer, code) {
  if (peer.readyState === WebSocket.OPEN) peer.close(safeCloseCode(code), "relay peer closed");
  else if (peer.readyState === WebSocket.CONNECTING) peer.terminate();
}

function safeCloseCode(code) {
  const standard = code >= 1000 && code <= 1014 && ![1004, 1005, 1006].includes(code);
  return standard || (code >= 3000 && code <= 4999) ? code : 1011;
}

function safeStatus(status) {
  return Number.isInteger(status) && status >= 400 && status <= 599 ? status : 502;
}

function rejectUpgrade(socket, status, message) {
  if (socket.destroyed || socket.writableEnded) return;
  const body = `${message}\n`;
  socket.end(
    `HTTP/1.1 ${status} ${message}\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\n`
    + `Content-Length: ${Buffer.byteLength(body)}\r\nConnection: close\r\n\r\n${body}`,
  );
}

function noStoreHeaders(contentType) {
  return { "cache-control": "no-store", "content-type": contentType };
}
