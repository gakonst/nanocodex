import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { DesktopRuntime, managedOrigin, validateHand, validateSettings, compareCursor, restoredLayout } from "../src/runtime.mjs";
import { desktopPreferences } from "../src/configuration.mjs";

const key = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
const secondKey = `ncx_live_${"c".repeat(12)}_${"d".repeat(43)}`;
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
async function service(t, handler = (_request, response) => response.end(JSON.stringify({ data: [] }))) {
  const server = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    handler(request, response);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  return `http://127.0.0.1:${server.address().port}`;
}
async function directory(t) {
  const path = await mkdtemp(join(tmpdir(), "nanocodex-runtime-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}

test("service origins cannot redirect credentials or embed paths", () => {
  assert.equal(managedOrigin("https://nanocodex.gakonst.workers.dev"), "https://nanocodex.gakonst.workers.dev");
  assert.equal(managedOrigin("http://127.0.0.1:8787"), "http://127.0.0.1:8787");
  for (const origin of ["http://remote.example", "https://user:password@example.com", "https://example.com/api", "https://example.com?key=secret"]) assert.throws(() => managedOrigin(origin));
});
test("Astra accepts its supported settings and rejects None or Pro", () => {
  const settings = { model: "gpt-6-astra", thinking: "high", reasoning_mode: "standard", fast_mode: false };
  for (const thinking of ["low", "medium", "high", "xhigh", "max"]) assert.equal(validateSettings({ ...settings, thinking }).thinking, thinking);
  for (const thinking of ["none", "ultra"]) assert.throws(() => validateSettings({ ...settings, thinking }), /Astra supports/);
  assert.throws(() => validateSettings({ ...settings, reasoning_mode: "pro" }), /Standard/);
});
test("accepted turns lock model and mode while effort and Fast use a minimal patch", async t => {
  const current = { model: "gpt-6-astra", thinking: "high", reasoning_mode: "standard", fast_mode: false };
  const patches = [];
  const baseUrl = await service(t, async (request, response) => {
    if (request.method === "PATCH") {
      let body = "";
      for await (const chunk of request) body += chunk;
      const patch = JSON.parse(body); patches.push(patch);
      response.end(JSON.stringify({ settings: { ...current, ...patch } }));
    } else if (request.url === "/v1/agents") response.end(JSON.stringify({ data: [] }));
    else response.end(JSON.stringify({ settings: current, accepted_turns: 1, completed_turns: 0 }));
  });
  const runtime = new DesktopRuntime({ baseUrl, apiKey: key });
  t.after(() => runtime.close());
  await runtime.refresh();
  const agentId = "019a65fe-a456-7000-8000-000000000003";
  const updated = await runtime.settings({ agentId, settings: { ...current, thinking: "max", fast_mode: true } });
  assert.equal(updated.thinking, "max");
  assert.deepEqual(patches, [{ thinking: "max", fast_mode: true }]);
  await assert.rejects(runtime.settings({ agentId, settings: { ...current, model: "gpt-5.6-sol" } }), /new tab/);
  assert.equal(patches.length, 1);
});
test("Hand scope and VM resource validation preserve explicit grants", () => {
  const config = { id: "desktop-test", name: "Laptop", workspace: "/tmp/project", kind: "local", agentId: "test-agent" };
  assert.deepEqual(validateHand(config), config);
  assert.throws(() => validateHand({ ...config, workspace: "relative" }));
  assert.throws(() => validateHand({ ...config, id: "brain" }));
  assert.throws(() => validateHand({ ...config, kind: "vm", cpus: 0 }));
  assert.ok(compareCursor("999999999999999999", "1000000000000000000") < 0);
});
test("old or partially corrupt tab preferences restore with safe defaults", () => {
  assert.deepEqual(restoredLayout({ tabs: [null, { id: "one" }, { id: "one" }, { id: "two", draft: 42, folder: "relative" }], activeTabId: "missing", theme: "unknown" }), {
    tabs: [{ id: "one", draft: "", target: "", folder: "" }, { id: "two", draft: "", target: "", folder: "" }], activeTabId: "one", tabPosition: "left", theme: "system",
  });
  assert.equal(restoredLayout({ tabs: [null] }), undefined);
});
test("credential-store failure preserves the verified current account", async t => {
  const baseUrl = await service(t);
  const runtime = new DesktopRuntime({ baseUrl, apiKey: key, saveConnection: async () => { throw new Error("Keychain unavailable"); } });
  t.after(() => runtime.close());
  await runtime.refresh();
  await assert.rejects(runtime.connect({ baseUrl, apiKey: secondKey, remember: true }), /Keychain unavailable/);
  assert.equal(runtime.state().connected, true);
});
test("a late preference failure does not turn a committed sign-in into a revoked credential", async t => {
  const baseUrl = await service(t);
  let saved;
  const runtime = new DesktopRuntime({ baseUrl, saveConnection: async value => { saved = value; }, persist: async () => { throw new Error("disk full"); } });
  t.after(() => runtime.close());
  const state = await runtime.connect({ baseUrl, apiKey: key, remember: true });
  assert.equal(saved.apiKey, key);
  assert.equal(state.connected, true);
  assert.equal(state.hasCredentials, true);
  assert.match(state.error, /preferences could not be saved/);
});
test("a delayed layout from the previous account cannot replace current drafts or saved preferences", async t => {
  const path = await directory(t);
  const baseUrl = await service(t);
  const preferences = await desktopPreferences({ directory: path, baseUrl, apiKey: key });
  const runtime = new DesktopRuntime({ baseUrl, apiKey: key, ...preferences });
  t.after(async () => { await runtime.close(); await preferences.close(); });
  await runtime.refresh();
  const previousScope = runtime.state().accountScope;
  const previousLayout = { accountScope: previousScope, tabs: [{ id: "old", draft: "old account draft" }], activeTabId: "old", tabPosition: "left", theme: "system" };
  await runtime.saveLayout(previousLayout);
  const release = deferred();
  // This message has not reached the host yet when the account changes.
  const delayed = release.promise.then(() => runtime.saveLayout(previousLayout));
  await runtime.connect({ baseUrl, apiKey: secondKey });
  const currentScope = runtime.state().accountScope;
  assert.notEqual(currentScope, previousScope);
  await runtime.saveLayout({ ...previousLayout, accountScope: currentScope, tabs: [{ id: "new", draft: "current account draft" }], activeTabId: "new" });
  const saved = await readFile(join(path, "desktop.json"), "utf8");
  assert.equal(Object.hasOwn(JSON.parse(saved).preferences.layout, "accountScope"), false);
  release.resolve(); await delayed;
  assert.equal(runtime.state().layout.tabs[0].draft, "current account draft");
  assert.equal(await readFile(join(path, "desktop.json"), "utf8"), saved);
  await runtime.disconnect();
  assert.notEqual(runtime.state().accountScope, currentScope);
});
test("an old account response cannot create UI state after disconnect", async t => {
  const accepted = deferred();
  const release = deferred();
  const baseUrl = await service(t, async (request, response) => {
    if (request.method === "POST") {
      accepted.resolve(); await release.promise;
      response.end(JSON.stringify({ agent_id: "019a65fe-a456-7000-8000-000000000001" }));
    } else response.end(JSON.stringify({ data: [] }));
  });
  const runtime = new DesktopRuntime({ baseUrl, apiKey: key });
  t.after(() => runtime.close());
  await runtime.refresh();
  const creation = runtime.createThread();
  const rejection = assert.rejects(creation, /account changed/);
  await accepted.promise;
  await runtime.disconnect();
  release.resolve();
  await rejection;
  assert.deepEqual(runtime.state().threads, []);
  assert.equal(runtime.state().connected, false);
});
test("immutable SDK history can receive subsequent streamed turn events", { timeout: 5_000 }, async t => {
  const agentId = "019a65fe-a456-7000-8000-000000000002";
  const raw = [
    { cursor: "1", created_at: 1, turn_id: "turn-1", type: "turn_accepted", id: "turn-1", input: "hello" },
    { cursor: "2", created_at: 2, turn_id: "turn-1", type: "turn_completed", id: "turn-1", final_message: "world" },
  ];
  const baseUrl = await service(t, (request, response) => {
    if (request.url.includes("/events/history")) response.end(JSON.stringify({ data: [], latest_cursor: "0", has_more: false }));
    else if (request.url.includes("/events?")) {
      response.setHeader("content-type", "text/event-stream");
      response.write(raw.map(event => `id: ${event.cursor}\nevent: message\ndata: ${JSON.stringify(event)}\n\n`).join(""));
    } else if (request.url === `/v1/agents/${agentId}`) response.end(JSON.stringify({ active_turns: [] }));
    else response.end(JSON.stringify({ data: [agentId] }));
  });
  const runtime = new DesktopRuntime({ baseUrl, apiKey: key });
  t.after(() => runtime.close());
  await runtime.refresh();
  const completed = deferred();
  runtime.on("event", event => {
    if (event.type === "thread" && event.thread.events.length === 2) completed.resolve(event.thread);
  });
  const first = runtime.openThread(agentId);
  const second = runtime.openThread(agentId);
  assert.deepEqual(await first, await second);
  const snapshot = await completed.promise;
  assert.equal(snapshot.events[1].data.final_message, "world");
  assert.deepEqual(snapshot.activeTurns, []);
  assert.equal(snapshot.error, undefined);
});
test("saved drafts and Hand grants are private and account scoped", async t => {
  const path = await directory(t);
  const first = await desktopPreferences({ directory: path, apiKey: key });
  const preferences = { layout: { tabs: [{ id: "one", draft: "private draft" }] }, hands: [] };
  await first.persist(preferences);
  const stored = await readFile(join(path, "desktop.json"), "utf8");
  assert.equal(stored.includes(key), false);
  assert.equal((await stat(join(path, "desktop.json"))).mode & 0o777, 0o600);
  assert.deepEqual((await desktopPreferences({ directory: path, apiKey: key })).saved, preferences);
  assert.deepEqual((await desktopPreferences({ directory: path, apiKey: secondKey })).saved, {});
});
test("VM readiness is an observed handshake and stopping cancels pending setup", async t => {
  const path = await directory(t);
  const binary = join(path, "vm-fixture");
  const image = join(path, "root.ext4");
  await writeFile(image, "fixture");
  await writeFile(binary, `#!${process.execPath}\nsetTimeout(() => console.log(JSON.stringify({ fields: { stage: 'vm.hand.ready' } })), 100); setInterval(() => {}, 1000);\n`, { mode: 0o700 });
  const runtime = new DesktopRuntime({ baseUrl: await service(t), apiKey: key, dataDirectory: path });
  t.after(() => runtime.close());
  await runtime.refresh();
  await runtime.saveHand({ id: "vm-test", name: "VM", workspace: path, kind: "vm", binary, rootfs: image, guestRuntime: image, cpus: 2, memoryMiB: 2048 });
  assert.notEqual(runtime.state().hands[0].rootfs, image);
  assert.equal(await readFile(runtime.state().hands[0].rootfs, "utf8"), "fixture");
  const start = runtime.startHand("vm-test");
  assert.equal(runtime.state().hands[0].status, "connecting");
  await start;
  assert.equal(runtime.state().hands[0].status, "connected");
  await runtime.stopHand("vm-test");
  assert.equal(runtime.state().hands[0].status, "stopped");
  const restart = runtime.startHand("vm-test");
  await runtime.stopHand("vm-test");
  await restart;
  assert.equal(runtime.state().hands[0].status, "stopped");
});
test("JSONL host exposes only desktop actions and shuts down on stdin EOF", async t => {
  const path = await directory(t);
  const environment = { ...process.env, NANOCODEX_DESKTOP_DATA: path };
  for (const name of ["NC_API_KEY", "NANOCODEX_API_KEY", "NANOCODEX_ENV_FILE", "NANOCODEX_MANAGED_URL"]) delete environment[name];
  const child = spawn(process.execPath, [new URL("../src/host.mjs", import.meta.url).pathname], { env: environment, stdio: ["pipe", "pipe", "pipe"] });
  t.after(() => { if (child.exitCode === null) child.kill("SIGKILL"); });
  const messages = [];
  const complete = deferred();
  createInterface({ input: child.stdout }).on("line", line => {
    const message = JSON.parse(line); messages.push(message);
    if (message.id === 2) complete.resolve();
  });
  child.stdin.write(`${JSON.stringify({ id: 1, method: "request", args: ["/v1/agents"] })}\n${JSON.stringify({ id: 2, method: "state", args: [] })}\n`);
  await complete.promise;
  assert.match(messages.find(message => message.id === 1).error, /Invalid desktop request/);
  assert.equal(messages.find(message => message.id === 2).result.connected, false);
  assert.equal(JSON.stringify(messages).includes("apiKey"), false);
  const exit = once(child, "exit");
  child.stdin.end();
  assert.equal((await exit)[0], 0);
});
