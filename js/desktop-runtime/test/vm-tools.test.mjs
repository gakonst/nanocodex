import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { WebSocketServer } from "ws";
import { DesktopRuntime } from "../src/runtime.mjs";
import { createVmTools, supportsLocalVms } from "../src/vm-tools.mjs";
import { desktopDefaults } from "../src/configuration.mjs";

test("VM callers cannot supply paths or override a host recipe; unsupported platforms do not advertise hosting", () => {
  const tools = createVmTools({ hostName: "Mac", list() {}, start() {}, stop() {} });
  for (const input of [{ name: "../other" }, { name: "fine", binary: "/tmp/evil" }, { name: "" }, { name: "a".repeat(41) }]) {
    assert.throws(() => tools[1].handler(input));
  }
  assert.equal(supportsLocalVms("win32", "x64"), false);
  assert.equal(supportsLocalVms("darwin", "x64"), false);
  assert.equal(supportsLocalVms("darwin", "arm64"), true);
});

test("remote Hand calls create, reuse, stop and restart a retained private VM", { timeout: 20_000, skip: !supportsLocalVms() }, async t => {
  const path = await mkdtemp(join(tmpdir(), "nanocodex-vm-tools-"));
  const rootfs = join(path, "template.ext4"), guestRuntime = join(path, "guest"), binary = join(path, "helper");
  await writeFile(rootfs, "immutable-template"); await writeFile(guestRuntime, "guest");
  await writeFile(binary, `#!${process.execPath}\nif(process.argv.includes('__hand-screen')) console.error('Hand screen is ready'); else console.log(JSON.stringify({fields:{stage:'vm.hand.ready'}})); setInterval(() => {},1000);\n`, { mode: 0o700 });
  await writeFile(join(path, "vm.json"), JSON.stringify({ rootfs, guestRuntime, binary }));
  const defaults = await desktopDefaults({ NANOCODEX_DESKTOP_DATA: path });
  const server = createServer((_req, response) => { response.setHeader("content-type", "application/json"); response.end('{"data":[]}'); });
  const sockets = new WebSocketServer({ server });
  const results = new Map(); let socket, catalog;
  sockets.on("connection", current => {
    socket = current;
    current.on("message", data => {
      const frame = JSON.parse(String(data));
      if (frame.type === "catalog") { catalog = frame; current.send('{"type":"ready"}'); }
      if (frame.type === "ping") current.send(JSON.stringify({ type: "pong", nonce: frame.nonce }));
      if (frame.type === "drain") current.send('{"type":"draining"}');
      if (frame.type === "result") { results.get(frame.call_id)?.(frame); current.send(JSON.stringify({ type: "ack", call_id: frame.call_id })); }
    });
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  let saved;
  const runtime = new DesktopRuntime({ baseUrl: `http://127.0.0.1:${server.address().port}`, apiKey: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`, dataDirectory: path, defaults: { ...defaults, workspace: path, name: "Test Mac" }, persist: async value => { saved = value; } });
  t.after(async () => { await runtime.close(); for (const client of sockets.clients) client.terminate(); sockets.close(); await new Promise(resolve => server.close(resolve)); await rm(path, { recursive: true, force: true }); });
  await runtime.refresh(); const host = await runtime.prepareDefaultHand();
  assert(catalog.machines[0].capabilities.includes("vm_host"));
  assert(catalog.tools.some(tool => tool.remote_name === "start_vm"));
  let sequence = 0;
  async function call(name, input = {}) {
    const call_id = `vm-call-${++sequence}`;
    const result = new Promise(resolve => results.set(call_id, resolve));
    socket.send(JSON.stringify({ type: "call", session_id: "phone", call_id, model: "gpt-5.6-sol", name, input, output_token_budget: 2048, output_byte_budget: 65536, deadline_at: Date.now() + 15_000 }));
    const frame = await result;
    assert.equal(frame.outcome.status, "completed", JSON.stringify(frame));
    assert.equal(frame.outcome.output.success, true, JSON.stringify(frame));
    return frame.outcome.output.structured_result;
  }
  const first = await call("start_vm", { name: "phone-demo" });
  assert.equal(first.status, "connected", JSON.stringify(first));
  assert.equal((await call("start_vm", { name: "phone-demo" })).machine_id, first.machine_id);
  const vm = saved.hands.find(hand => hand.id === first.machine_id);
  assert.notEqual(vm.rootfs, rootfs); assert.equal(vm.vmHost, host.id);
  await writeFile(vm.rootfs, "retained-guest-data");
  assert.equal((await call("stop_vm", { name: "phone-demo" })).status, "stopped");
  const { vmHost, vmName, ...visibleSettings } = vm;
  await runtime.saveHand(visibleSettings);
  assert.equal(saved.hands.find(hand => hand.id === vm.id).vmHost, vmHost, "Native form saves retain remote ownership");
  assert.equal(saved.hands.find(hand => hand.id === vm.id).vmName, vmName);
  assert.equal((await call("start_vm", { name: "phone-demo" })).machine_id, first.machine_id);
  assert.equal(await readFile(vm.rootfs, "utf8"), "retained-guest-data");
  assert.equal(await readFile(rootfs, "utf8"), "immutable-template");
  assert.equal((await call("list_vms")).vms.length, 1);
  await runtime.stopHand(host.id);
  assert.equal(runtime.state().hands.find(hand => hand.id === first.machine_id).status, "stopped");
});
