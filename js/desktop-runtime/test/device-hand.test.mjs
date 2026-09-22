import assert from "node:assert/strict";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";
import { describeDeviceHand, connectDeviceHand } from "../src/device-hand.mjs";

// Synthetic desktop fixtures must never discover or install a host provider.
process.env.NANOCODEX_COMPUTER = "off";

test("desktop shares the CLI identity and owns only its client lease", { timeout: 5000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), "device-hand-test-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const binary = join(dir, "hand");
  const machine = { id: "11111111-1111-4111-8111-111111111111", name: "My Mac", workspace: dir };
  await writeFile(binary, `#!${process.execPath}\nconst machine = ${JSON.stringify(machine)};\nif (process.argv.includes('--describe')) console.log(JSON.stringify(machine));\nelse { console.log(JSON.stringify({machine,status:'connected',factory:{name:'mac-test',status:'connected'}})); process.stdin.resume(); process.stdin.on('end',()=>process.exit(0)); }\n`, { mode: 0o700 });
  assert.deepEqual(await describeDeviceHand(binary, {}), { ...machine, kind: "local" });
  const states = [], controller = new AbortController();
  const connection = connectDeviceHand({ binary, env: {}, signal: controller.signal, onState: value => states.push(value) });
  await connection.ready;
  assert.equal(states[0].machine.id, machine.id);
  assert.equal(states[0].factory.status, "connected");
  await connection.close();
  assert.equal(states.length, 1, "Stopping this client's lease must not claim the shared host failed");
});

test("failed helper startup rejects readiness and can be closed", { timeout: 5000 }, async () => {
  const controller = new AbortController();
  const connection = connectDeviceHand({ binary: "/missing/hand", env: {}, signal: controller.signal, onState() {} });
  await assert.rejects(connection.ready);
  await connection.close();
});

test("app adopts the shared computer without replacing project Hands or rediscovering on every prompt", { timeout: 5000 }, async t => {
  const { createServer } = await import("node:http");
  const { once } = await import("node:events");
  const { readFile } = await import("node:fs/promises");
  const { DesktopRuntime } = await import("../src/runtime.mjs");
  const dir = await mkdtemp(join(tmpdir(), "device-hand-app-"));
  const binary = join(dir, "hand"), calls = join(dir, "describes");
  const machine = { id: "22222222-2222-4222-8222-222222222222", name: "My Mac", workspace: dir };
  await writeFile(binary, `#!${process.execPath}\nconst fs=require('node:fs'); const machine=${JSON.stringify(machine)}; if(process.argv.includes('--describe')) {fs.appendFileSync(${JSON.stringify(calls)},'describe\\n'); console.log(JSON.stringify(machine));} else {console.log(JSON.stringify({machine,status:'connected',factory:{name:'mac-test',status:'connected'}}));process.stdin.resume();process.stdin.on('end',()=>process.exit(0));}\n`, { mode: 0o700 });
  const server = createServer((_req, res) => { res.setHeader("content-type", "application/json"); res.end('{"data":[]}'); });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  let saved;
  const runtime = new DesktopRuntime({ baseUrl: `http://127.0.0.1:${server.address().port}`, apiKey: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`,
    defaults: { deviceBinary: binary }, dataDirectory: dir, persist: async value => { saved = value; },
    saved: { hands: [{ id: "project", kind: "local", name: "Project", workspace: dir }] } });
  t.after(async () => { await runtime.close(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); await rm(dir, { recursive: true, force: true }); });
  await runtime.refresh();
  const first = await runtime.prepareDefaultHand();
  const second = await runtime.prepareDefaultHand();
  assert.equal(first.id, machine.id); assert.equal(second.id, machine.id);
  assert.equal(first.factory.status, "connected");
  assert.equal(runtime.state().hands.length, 2);
  assert.equal(runtime.state().hands[0].id, machine.id);
  assert(saved.hands.some(hand => hand.id === "project"));
  assert.equal((await readFile(calls, "utf8")).trim(), "describe");
});
