import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { once } from "node:events";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";
import { WebSocketServer } from "ws";
import { describeDeviceHand, connectDeviceHand } from "../src/device-hand.mjs";

const binary = process.env.NANOCODEX_DEVICE_TEST_BINARY;
test("real CLI and desktop leases share one authenticated host across account keys", { skip: !binary, timeout: 45_000 }, async t => {
  // node:test's timeout cancels the test, but cannot interrupt an after hook
  // awaiting a child "close" event (which also waits for inherited stdio).
  // Keep this timer referenced and independent of the test's AbortSignal.
  let phase = "creating temporary home";
  let settled = false;
  const children = new Set();
  const watchdog = setTimeout(() => {
    process.stderr.write(`Device Hand integration exceeded 60s during: ${phase}\n`);
    for (const child of children) {
      try { child.kill("SIGKILL"); } catch { /* Still fail if OS cleanup fails. */ }
    }
    // This file runs in its own node:test process. Force a failing exit even
    // when OS handles or a stuck teardown keep the event loop alive.
    process.exit(1);
  }, 60_000);
  const spawnProcess = (...args) => {
    const child = spawn(...args);
    children.add(child);
    child.once("close", () => children.delete(child));
    return child;
  };
  const step = value => {
    phase = value;
    process.stderr.write(`Device Hand integration: ${phase}\n`);
  };
  t.signal.addEventListener("abort", () => {
    if (!settled) process.stderr.write(`Device Hand integration cancelled during: ${phase}\n`);
  }, { once: true });
  const home = await mkdtemp(join(tmpdir(), "ncx-device-"));
  const server = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    if (request.url === "/v1/me") response.end(JSON.stringify({ authentication: "api_key", user: { id: "test-owner" } }));
    else { response.statusCode = 404; response.end('{}'); }
  });
  const sockets = new WebSocketServer({ noServer: true });
  server.on("upgrade", (request, socket, head) => {
    if (request.url !== "/v1/account/tool-host") { socket.destroy(); return; }
    sockets.handleUpgrade(request, socket, head, ws => sockets.emit("connection", ws));
  });
  let catalogs = 0, current, result;
  sockets.on("connection", socket => {
    current = socket;
    socket.on("message", data => {
      const frame = JSON.parse(String(data));
      if (frame.type === "catalog") { catalogs++; socket.send('{"type":"ready"}'); }
      if (frame.type === "ping") socket.send(JSON.stringify({ type: "pong", nonce: frame.nonce }));
      if (frame.type === "drain") socket.send('{"type":"draining"}');
      if (frame.type === "result") { result = frame; socket.send(JSON.stringify({ type: "ack", call_id: frame.call_id })); }
    });
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  const base = { ...process.env, HOME: home, USERPROFILE: home, NANOCODEX_COMPUTER: "off", NANOCODEX_MANAGED_URL: `http://127.0.0.1:${server.address().port}`, NANOCODEX_DESKTOP_DATA: home };
  const env = char => ({ ...base, NANOCODEX_API_KEY: `ncx_live_${char.repeat(12)}_${char.repeat(43)}` });
  const connections = [];
  t.after(async () => {
    step("closing client leases in teardown");
    for (const connection of connections) await connection.close();
    await delay(5000);
    for (const socket of sockets.clients) socket.terminate();
    sockets.close(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    step("removing temporary home");
    await rm(home, { recursive: true, force: true });
    // A timed-out describe may still own a child even with no client leases.
    if (children.size === 0) { clearTimeout(watchdog); settled = true; }
  });
  step("describing first identity");
  const firstId = await describeDeviceHand(binary, env("a"), { spawnProcess });
  step("describing rotated identity");
  const secondId = await describeDeviceHand(binary, env("b"), { spawnProcess });
  assert.equal(firstId.id, secondId.id, "API key rotation must not create another computer");
  for (const key of ["a", "b"]) {
    step(`connecting client ${key}`);
    const connection = connectDeviceHand({ spawnProcess, binary, env: env(key), signal: new AbortController().signal, onState() {} });
    connections.push(connection); await connection.ready;
  }
  assert.equal(catalogs, 1);
  step("closing first client");
  await connections[0].close();
  await delay(2500);
  assert.equal(catalogs, 1, "Closing the first client must preserve the existing publisher");
  step("executing shared host command");
  current.send(JSON.stringify({ type: "call", session_id: "hand-test", call_id: "host-shell", model: "gpt-6-astra", name: "exec_command",
    input: { cmd: process.platform === "win32" ? "echo hand_shared_ok" : "printf hand_shared_ok" }, output_token_budget: 1024, output_byte_budget: 131072, deadline_at: Date.now() + 10_000 }));
  const deadline = Date.now() + 10_000;
  while (!result && Date.now() < deadline) await delay(20);
  assert.equal(result?.outcome.status, "completed");
  assert.match(result.outcome.output.output, /hand_shared_ok/);
  const closed = once(current, "close");
  step("closing final client and waiting for publisher");
  await connections[1].close();
  await Promise.race([closed, delay(10_000, undefined, { ref: false }).then(() => { throw new Error("Publisher outlived its last client"); })]);
  step("describing identity after disconnect");
  assert.equal((await describeDeviceHand(binary, env("a"), { spawnProcess })).id, firstId.id);
});
