import assert from "node:assert/strict";
import test from "node:test";
import { Agent, ManagedError } from "../managed/index.mjs";

const id = "0198d3f0-8844-7000-8000-000000000001";
const apiKey = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
test("cloud browser uses authenticated private requests, separate from turn events", async () => {
  const requests = [];
  const agent = Agent.open(id, { baseUrl: "https://managed.example", apiKey, fetch: async (url, init) => {
    const request = new Request(url, init);
    assert.equal(request.headers.get("authorization"), `Bearer ${apiKey}`);
    requests.push([new URL(request.url).pathname, request.method, init.body]);
    return Response.json({ mode: "human", generation: "lease", reason: "Sign in", available: true, tabs: [] });
  } });
  await agent.browser.state();
  await agent.browser.takeover();
  await agent.browser.action("type", { target: "page", generation: "lease", pageUrl: "https://example.com/", text: "test input" });
  await agent.browser.release("lease");
  assert.deepEqual(requests.map(([path, method]) => [path, method]), [
    [`/v1/agents/${id}/browser`, "GET"],
    [`/v1/agents/${id}/browser/takeover`, "POST"],
    [`/v1/agents/${id}/browser/type`, "POST"],
    [`/v1/agents/${id}/browser/release`, "POST"],
  ]);
  assert.throws(() => agent.browser.action("Runtime.evaluate", {}), TypeError);
  assert.equal(requests.length, 4);
});
test("cloud browser preserves authorization and generation errors", async () => {
  for (const [status, error] of [[403, "forbidden"], [409, "control_changed"]]) {
    const agent = Agent.open(id, { baseUrl: "https://managed.example", fetch: async () => Response.json({ error }, { status }) });
    await assert.rejects(agent.browser.release("stale"), (e) => e instanceof ManagedError && e.status === status);
  }
});
