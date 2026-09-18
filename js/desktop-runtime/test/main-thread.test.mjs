import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { test } from "node:test";
import { DesktopRuntime } from "../src/runtime.mjs";

const key = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
const secondKey = `ncx_live_${"c".repeat(12)}_${"d".repeat(43)}`;
const mainId = "019a65fe-a456-7000-8000-000000000003";
const project = { id: "project-one", name: "Research", coordinator_agent_id: mainId };
async function service(t, handler) {
  const server = createServer(async (request, response) => {
    response.setHeader("content-type", "application/json");
    if (request.url === "/v1/agents") { response.end(JSON.stringify({ data: [] })); return; }
    await handler(request, response);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  return `http://127.0.0.1:${server.address().port}`;
}
async function connected(t, baseUrl) {
  const runtime = new DesktopRuntime({ baseUrl, apiKey: key });
  t.after(() => runtime.close());
  await runtime.refresh();
  return runtime;
}

test("Main Thread reuses the backend identity across concurrent calls and desktop instances", async t => {
  let calls = 0;
  const baseUrl = await service(t, async (request, response) => {
    assert.equal(request.url, "/v1/main-thread");
    assert.equal(request.method, "PUT");
    assert.equal(request.headers.authorization, `Bearer ${key}`);
    let body = "";
    for await (const chunk of request) body += chunk;
    const value = JSON.parse(body);
    assert.deepEqual(value, {}, "The public API owns creation and rejects client-supplied identities");
    calls++;
    response.end(JSON.stringify({ agent_id: mainId }));
  });
  const first = await connected(t, baseUrl);
  const second = await connected(t, baseUrl);
  const results = await Promise.all([first.openMainThread(), first.openMainThread(), second.openMainThread()]);
  for (const result of results) assert.deepEqual(result, { id: mainId, title: "Main Thread", updatedAt: 0, turnCount: 0 });
  assert.equal(first.state().threads.length, 1);
  assert.equal(second.state().threads.length, 1);
  assert.equal(calls, 3);
});

test("project navigation returns the account's project and coordinator identities", async t => {
  const baseUrl = await service(t, (request, response) => {
    assert.equal(request.url, "/v1/projects");
    assert.equal(request.method, "GET");
    assert.equal(request.headers.authorization, `Bearer ${key}`);
    response.end(JSON.stringify({ data: [{ ...project, backend_metadata: true }] }));
  });
  const runtime = await connected(t, baseUrl);
  assert.deepEqual(await runtime.listProjects(), { data: [project] });
});

test("Main Thread and project failures propagate without local substitutes", async t => {
  let status = 503, body = { message: "Service unavailable" };
  const baseUrl = await service(t, (_request, response) => {
    response.statusCode = status;
    response.end(JSON.stringify(body));
  });
  const runtime = await connected(t, baseUrl);
  await assert.rejects(runtime.openMainThread(), /Service unavailable/);
  await assert.rejects(runtime.listProjects(), /Service unavailable/);
  status = 200; body = {};
  await assert.rejects(runtime.openMainThread(), /Invalid Main Thread response/);
  await assert.rejects(runtime.listProjects(), /Invalid projects response/);
  assert.deepEqual(runtime.state().threads, []);
});

for (const method of ["openMainThread", "listProjects"]) {
  test(`${method} rejects a late response after switching accounts`, async t => {
    const requested = Promise.withResolvers();
    let stalled;
    const baseUrl = await service(t, (request, response) => {
      assert.equal(request.headers.authorization, `Bearer ${key}`);
      stalled = response;
      requested.resolve();
    });
    const runtime = await connected(t, baseUrl);
    const pending = runtime[method]();
    const rejection = assert.rejects(pending, /account changed/);
    await requested.promise;
    await runtime.connect({ baseUrl, apiKey: secondKey });
    stalled.end(JSON.stringify(method === "openMainThread" ? { agent_id: mainId } : { data: [project] }));
    await rejection;
    assert.deepEqual(runtime.state().threads, []);
  });
}
