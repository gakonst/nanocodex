import { createServer } from "node:http";
import { Readable } from "node:stream";
import { connect as connectTls } from "node:tls";
import { pathToFileURL } from "node:url";

const DEFAULT_UPSTREAM_ORIGIN = "https://chatgpt.com";
const MAX_UPSTREAM_HEADER_BYTES = 64 * 1024;
const UPSTREAM_HANDSHAKE_TIMEOUT_MS = 15_000;
const ALLOWED_HTTP_PATHS = new Set([
  "/backend-api/codex/alpha/search",
  "/backend-api/codex/images/edits",
  "/backend-api/codex/images/generations",
  "/backend-api/codex/realtime/calls",
]);
const RESPONSES_PATH = "/backend-api/codex/responses";
const FORWARDED_HEADERS = [
  "authorization",
  "chatgpt-account-id",
  "content-type",
  "openai-alpha",
  "openai-beta",
  "originator",
  "session-id",
  "thread-id",
  "user-agent",
  "x-client-request-id",
  "x-codex-turn-state",
  "x-oai-attestation",
  "x-openai-fedramp",
  "x-openai-internal-codex-responses-lite",
  "x-responsesapi-include-timing-metrics",
  "x-session-id",
];
const RETURNED_HEADERS = [
  "content-type",
  "location",
  "openai-model",
  "retry-after",
  "x-codex-turn-state",
  "x-reasoning-included",
  "x-request-id",
];

export function startRelay({
  host = "0.0.0.0",
  port = Number(process.env.PORT ?? 8080),
  upstreamOrigin = DEFAULT_UPSTREAM_ORIGIN,
} = {}) {
  const upstream = new URL(upstreamOrigin);
  if (upstream.protocol !== "https:" && upstream.hostname !== "127.0.0.1") {
    throw new Error("upstream must use HTTPS");
  }
  const server = createServer((request, response) => {
    void proxyHttp(request, response, upstream).catch((error) => {
      if (response.headersSent) response.destroy(error);
      else {
        response.writeHead(502, { "cache-control": "no-store", "content-type": "text/plain" });
        response.end("upstream request failed\n");
      }
    });
  });
  server.on("upgrade", (request, socket, head) => proxyWebSocket(request, socket, head, upstream));
  server.on("clientError", (_error, socket) => rejectSocket(socket, 400, "bad request"));
  server.headersTimeout = 10_000;
  server.requestTimeout = 120_000;
  server.listen(port, host);
  return server;
}

async function proxyHttp(request, response, upstreamOrigin) {
  const incoming = new URL(request.url ?? "/", "http://relay.internal");
  if (request.method === "GET" && incoming.pathname === "/health") {
    response.writeHead(204, { "cache-control": "no-store" });
    response.end();
    return;
  }
  if (request.method !== "POST" || !ALLOWED_HTTP_PATHS.has(incoming.pathname)) {
    response.writeHead(404, { "cache-control": "no-store", "content-type": "text/plain" });
    response.end("not found\n");
    return;
  }
  if (!hasBearer(request.headers.authorization)) {
    response.writeHead(401, { "cache-control": "no-store", "content-type": "text/plain" });
    response.end("missing authorization\n");
    return;
  }

  const headers = forwardedHeaders(request.headers);
  headers.set("accept-encoding", "identity");
  const target = new URL(`${incoming.pathname}${incoming.search}`, upstreamOrigin);
  const controller = new AbortController();
  request.once("aborted", () => controller.abort());
  const upstream = await fetch(target, {
    method: "POST",
    headers,
    body: request,
    duplex: "half",
    redirect: "manual",
    signal: controller.signal,
  });
  const returned = { "cache-control": "no-store" };
  for (const name of RETURNED_HEADERS) {
    const value = upstream.headers.get(name);
    if (value !== null) returned[name] = value;
  }
  response.writeHead(upstream.status, returned);
  if (!upstream.body) {
    response.end();
    return;
  }
  Readable.fromWeb(upstream.body).once("error", (error) => response.destroy(error)).pipe(response);
}

function proxyWebSocket(request, socket, head, upstreamOrigin) {
  const incoming = new URL(request.url ?? "/", "http://relay.internal");
  const websocketKey = firstHeader(request.headers["sec-websocket-key"]);
  if (incoming.pathname !== RESPONSES_PATH) {
    rejectSocket(socket, 404, "not found");
    return;
  }
  if (!hasBearer(request.headers.authorization)) {
    rejectSocket(socket, 401, "missing authorization");
    return;
  }
  if (!websocketKey) {
    rejectSocket(socket, 400, "missing WebSocket key");
    return;
  }

  const upstream = connectTls({
    host: upstreamOrigin.hostname,
    port: Number(upstreamOrigin.port || 443),
    servername: upstreamOrigin.hostname,
  });
  socket.setNoDelay(true);
  upstream.setNoDelay(true);
  let header = Buffer.alloc(0);
  let upgraded = false;
  const timeout = setTimeout(() => {
    upstream.destroy();
    rejectSocket(socket, 504, "upstream timeout");
  }, UPSTREAM_HANDSHAKE_TIMEOUT_MS);

  upstream.once("secureConnect", () => {
    const lines = [
      `GET ${RESPONSES_PATH}${incoming.search} HTTP/1.1`,
      `Host: ${upstreamOrigin.host}`,
      "Connection: Upgrade",
      "Upgrade: websocket",
      "Sec-WebSocket-Version: 13",
      `Sec-WebSocket-Key: ${websocketKey}`,
    ];
    const headers = forwardedHeaders(request.headers);
    for (const [name, value] of headers) lines.push(`${name}: ${value}`);
    lines.push("", "");
    upstream.write(lines.join("\r\n"));
  });
  upstream.on("data", function onHandshake(chunk) {
    if (upgraded) return;
    header = Buffer.concat([header, chunk]);
    if (header.byteLength > MAX_UPSTREAM_HEADER_BYTES) {
      clearTimeout(timeout);
      upstream.destroy();
      rejectSocket(socket, 502, "upstream headers too large");
      return;
    }
    const headerEnd = header.indexOf("\r\n\r\n");
    if (headerEnd === -1) return;
    clearTimeout(timeout);
    upgraded = true;
    upstream.off("data", onHandshake);
    socket.write(header);
    if (head.byteLength > 0) upstream.write(head);
    const status = responseStatus(header);
    if (status !== 101) {
      upstream.pipe(socket);
      return;
    }
    socket.pipe(upstream);
    upstream.pipe(socket);
  });
  upstream.once("error", () => {
    clearTimeout(timeout);
    if (!upgraded) rejectSocket(socket, 502, "upstream WebSocket failed");
    else socket.destroy();
  });
  socket.once("error", () => upstream.destroy());
  socket.once("close", () => upstream.destroy());
}

export function responseStatus(header) {
  const lineEnd = header.indexOf("\r\n");
  if (lineEnd < 0) return Number.NaN;
  return Number(header.subarray(0, lineEnd).toString("ascii").split(" ")[1]);
}

function forwardedHeaders(source) {
  const headers = new Headers();
  for (const name of FORWARDED_HEADERS) {
    const value = firstHeader(source[name]);
    if (value) headers.set(name, value);
  }
  return headers;
}

function firstHeader(value) {
  return Array.isArray(value) ? value[0] : value;
}

function hasBearer(value) {
  return typeof firstHeader(value) === "string" && firstHeader(value).startsWith("Bearer ");
}

function rejectSocket(socket, status, message) {
  if (socket.destroyed || socket.writableEnded) return;
  const body = `${message}\n`;
  socket.end(
    `HTTP/1.1 ${status} ${message}\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nContent-Length: ${Buffer.byteLength(body)}\r\nConnection: close\r\n\r\n${body}`,
  );
}

const entry = process.argv[1] ? pathToFileURL(process.argv[1]).href : undefined;
if (import.meta.url === entry) startRelay();
