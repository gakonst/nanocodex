import assert from "node:assert/strict";
import test from "node:test";
import { listProjects, mainThread } from "./mainThreadApi.ts";

const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });

test("ensure uses authenticated PUT and reuses the server identity without sending migration data", async () => {
  const calls: RequestInit[] = [];
  const fetcher: typeof fetch = async (url, init) => {
    assert.equal(url, "/v1/main-thread");
    calls.push(init!);
    return json({ agent_id: "durable-global" });
  };
  assert.equal(await mainThread("PUT", fetcher), "durable-global");
  assert.equal(await mainThread("PUT", fetcher), "durable-global");
  for (const call of calls) {
    assert.equal(call.method, "PUT");
    assert.equal(call.credentials, "same-origin");
    assert.equal(call.body, undefined);
    assert.equal(call.cache, "no-store");
  }
});

test("lookup is read-only and accepts an unassigned Main Thread", async () => {
  assert.equal(await mainThread("GET", async () => json({ error: "not_found" }, 404)), null);
  await assert.rejects(mainThread("PUT", async () => json({ error: "not_found" }, 404)), /not found/);
  const signal = new AbortController().signal;
  assert.equal(await mainThread("GET", async (_url, init) => {
    assert.equal(init?.method, "GET");
    assert.equal(init?.signal, signal);
    return json({ agent_id: null });
  }, signal), null);
});

test("ensure rejects missing identities and surfaces service errors", async () => {
  for (const body of [{ agent_id: null }, { agent_id: "" }, {}, { agent_id: 42 }]) {
    await assert.rejects(mainThread("PUT", async () => json(body)), /Invalid Main Thread/);
  }
  await assert.rejects(mainThread("PUT", async () => json({ error: "reauthentication_required" }, 401)), /reauthentication required/);
});

test("projects retain server coordinator assignments, including unassigned projects", async () => {
  const data = [{ id: "one", name: "Research", coordinator_agent_id: "coordinator" }, { id: "two", name: "Writing", coordinator_agent_id: null }];
  assert.deepEqual(await listProjects(async (url, init) => {
    assert.equal(url, "/v1/projects");
    assert.equal(init?.credentials, "same-origin");
    assert.equal(init?.method, "GET");
    return json({ data });
  }), data);
  await assert.rejects(listProjects(async () => json({ data: [{ id: "one", name: "Research", coordinator_agent_id: {} }] })), /Invalid project/);
});
