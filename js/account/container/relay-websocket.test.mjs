import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer as createHttpsServer } from "node:https";
import { request } from "node:http";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import tls from "node:tls";
import test, { before, after } from "node:test";
import { WebSocket, WebSocketServer } from "ws";
import { startRelay } from "./relay.mjs";

let directory, cert, key;
before(() => {
  directory = mkdtempSync(join(tmpdir(), "nanocodex-relay-tls-"));
  execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
    "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost",
    "-keyout", join(directory, "key.pem"), "-out", join(directory, "cert.pem")], { stdio: "ignore" });
  key = readFileSync(join(directory, "key.pem"));
  cert = readFileSync(join(directory, "cert.pem"));
});
after(() => { if (directory) rmSync(directory, { recursive: true, force: true }); });
const relayId = "22222222-2222-4222-8222-222222222222";
const parentId = "11111111-1111-4111-8111-111111111111";
const headers = {
  authorization: "Bearer synthetic-private-access-marker", "chatgpt-account-id": "synthetic-private-account-marker",
  "x-nanocodex-relay-id": relayId, "x-nanocodex-egress-request-id": parentId,
  "x-private": "synthetic-private-header-marker",
};

async function fixture(t, raw = false) {
  // Trust only this ephemeral fixture certificate at the connect boundary. The
  // production relay still performs a real authenticated TLS handshake.
  const connect = tls.connect;
  t.mock.method(tls, "connect", (options) => connect({ ...options, ca: cert }));
  syncBuiltinESMExports();
  t.after(() => { t.mock.restoreAll(); syncBuiltinESMExports(); });
  const upstream = raw ? tls.createServer({ cert, key }) : createHttpsServer({ cert, key });
  const sockets = new Set();
  upstream.on("connection", (socket) => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  upstream.listen(0, "127.0.0.1");
  await once(upstream, "listening");
  const relay = startRelay({ host: "127.0.0.1", port: 0, upstreamOrigin: `https://localhost:${upstream.address().port}` });
  await once(relay, "listening");
  relay.on("connection", (socket) => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  t.after(() => { for (const socket of sockets) socket.destroy(); relay.close(); upstream.close(); });
  return { upstream, url: `ws://127.0.0.1:${relay.address().port}/backend-api/codex/responses` };
}

function assertSafe(records) {
  const allowed = new Set(["type", "transport", "outcome", "relay_id", "status", "socket_reused", "dns_observed",
    "process_age_ms", "duration_ms", "socket_setup_ms", "dns_lookup_ms", "tcp_connect_ms", "tls_handshake_ms",
    "upgrade_send_ms", "upstream_first_byte_ms", "upstream_upgrade_ms", "header_read_ms"]);
  for (const record of records) {
    assert.equal(record.type, "responses.relay.upstream");
    for (const [name, value] of Object.entries(record)) {
      assert.ok(allowed.has(name), `unexpected telemetry field ${name}`);
      if (name.endsWith("_ms")) assert.ok(Number.isFinite(value) && value >= 0, name);
    }
  }
  assert.doesNotMatch(JSON.stringify(records), /synthetic-private-|11111111-1111-4111/);
}

for (const brokenLogger of [false, true]) test(`real TLS WebSocket roundtrip preserves headers and frames (throwing logger: ${brokenLogger})`, { timeout: 10_000 }, async (t) => {
  const records = [];
  const f = await fixture(t);
  t.mock.method(console, "info", (record) => { records.push(JSON.parse(record)); if (brokenLogger) throw new Error("synthetic-private-logger-marker"); });
  const wss = new WebSocketServer({ noServer: true });
  t.after(() => wss.close());
  let observed;
  f.upstream.on("upgrade", (incoming, socket, head) => {
    observed = incoming.headers;
    wss.handleUpgrade(incoming, socket, head, (ws) => {
      ws.on("message", (data) => ws.send(data.toString()));
    });
  });
  const client = new WebSocket(f.url, { headers });
  t.after(() => client.terminate());
  await once(client, "open");
  const received = once(client, "message");
  client.send("synthetic-private-prompt-marker");
  assert.equal((await received)[0].toString(), "synthetic-private-prompt-marker");
  assert.equal(observed.authorization, headers.authorization);
  assert.equal(observed["chatgpt-account-id"], headers["chatgpt-account-id"]);
  for (const name of ["x-nanocodex-relay-id", "x-nanocodex-egress-request-id", "x-private"]) assert.equal(observed[name], undefined);
  const closed = once(client, "close"); client.close(); await closed;
  assert.equal(records.length, 1);
  assert.equal(records[0].outcome, "upgraded"); assert.equal(records[0].status, 101);
  assert.equal(records[0].relay_id, relayId); assert.equal(records[0].socket_reused, false);
  for (const name of ["socket_setup_ms", "tcp_connect_ms", "tls_handshake_ms", "upstream_first_byte_ms", "upstream_upgrade_ms", "header_read_ms"]) {
    assert.ok(Number.isFinite(records[0][name]), name);
  }
  assertSafe(records);
});

test("real TLS fragmented rejection preserves provider bytes and omits invalid correlation", { timeout: 10_000 }, async (t) => {
  const f = await fixture(t, true);
  const records = [];
  t.mock.method(console, "info", (record) => records.push(JSON.parse(record)));
  const body = "synthetic-private-provider-body-marker";
  f.upstream.on("secureConnection", (socket) => {
    let requestBytes = "";
    socket.on("data", (chunk) => {
      requestBytes += chunk.toString();
      if (!requestBytes.includes("\r\n\r\n")) return;
      socket.removeAllListeners("data");
      assert.doesNotMatch(requestBytes, /x-nanocodex-|x-private:/i);
      socket.write("HTTP/1.1 403 Forbidden\r\nX-Request-Id: synthetic-private-response-marker\r\n");
      setImmediate(() => socket.end(`Content-Length: ${body.length}\r\nConnection: close\r\n\r\n${body}`));
    });
  });
  const outgoing = request(f.url.replace("ws:", "http:"), { headers: { ...headers,
    "x-nanocodex-relay-id": "synthetic-private-invalid-id-marker", upgrade: "websocket", connection: "Upgrade",
    "sec-websocket-key": "c3ludGhldGljLWZpeHR1cmU=", "sec-websocket-version": "13",
  } });
  outgoing.end();
  const [response] = await once(outgoing, "response");
  let received = ""; for await (const chunk of response) received += chunk;
  assert.equal(response.statusCode, 403);
  assert.equal(response.headers["x-request-id"], "synthetic-private-response-marker");
  assert.equal(received, body);
  assert.equal(records.length, 1);
  assert.equal(records[0].outcome, "upstream_rejected"); assert.equal(records[0].status, 403);
  assert.equal(records[0].relay_id, undefined);
  assertSafe(records);
});

test("real TLS failure logs one fixed outcome without the exception or request markers", { timeout: 10_000 }, async (t) => {
  const f = await fixture(t, true);
  const records = [];
  t.mock.method(console, "info", (record) => records.push(JSON.parse(record)));
  f.upstream.on("connection", (socket) => socket.destroy());
  const outgoing = request(f.url.replace("ws:", "http:"), { headers: { ...headers,
    upgrade: "websocket", connection: "Upgrade", "sec-websocket-key": "c3ludGhldGljLWZpeHR1cmU=",
  } });
  outgoing.end();
  const [response] = await once(outgoing, "response");
  response.resume(); await once(response, "end");
  assert.equal(response.statusCode, 502);
  assert.equal(records.length, 1); assert.equal(records[0].outcome, "upstream_error");
  assertSafe(records);
});
