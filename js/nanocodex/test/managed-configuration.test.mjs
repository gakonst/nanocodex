import test from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { Agent } from "../managed/index.mjs";
const id = "0198d3f0-8844-7000-8000-000000000001";
test("configuration, template and operational calls use the existing authenticated client", async () => {
  const requests = [];
  const options = { baseUrl: "https://managed.example", fetch: async (url, init) => {
    const req = new Request(url, init); requests.push(req);
    assert.equal(req.credentials, "include");
    if (new URL(url).pathname === "/v1/agents") return Response.json({ agent_id: id });
    if (req.method === "DELETE" || new URL(url).pathname.endsWith("/result")) return new Response(null, { status: 204 });
    return Response.json({ data: [] });
  } };
  await Agent.definitions.put("reviewer", { tools: [] }, options);
  const agent = await Agent.create({ ...options, definitionId: "reviewer", environmentTemplateId: "offline", configuration: { instructions: "fixture", multi_agent: { enabled: false } } });
  assert.deepEqual(await requests[1].json(), { definition_id: "reviewer", environment_template_id: "offline", configuration: { instructions: "fixture", multi_agent: { enabled: false } } });
  await agent.configuration(); await agent.environment(); await agent.usage({ after: "7" });
  await agent.artifacts.list({ turnId: "turn/with space" }); await agent.webhook.delete();
  await agent.requiredActions.submit("call:1", { status: "cancelled", message: "cancelled" });
  assert.equal(new URL(requests[4].url).searchParams.get("after"), "7");
  assert.equal(new URL(requests[5].url).searchParams.get("turn_id"), "turn/with space");
  assert.throws(() => Agent.definitions.get("../escape", options), /invalid template/);
});
test("webhook verification accepts authentic deliveries and rejects tampering, stale timestamps and wrong secrets", async () => {
  const secret = "fixture-secret-".repeat(4);
  const body = JSON.stringify({ id: "agent:7", type: "turn_completed", agent_id: "agent", turn_id: "turn", cursor: "7", created_at: Date.now() });
  const timestamp = String(Math.floor(Date.now() / 1000));
  const signed = (content = body, time = timestamp) => new Request("https://hooks.example/", { method: "POST", body: content, headers: {
    "webhook-id": "agent:7", "webhook-timestamp": time,
    "webhook-signature": `v1,${createHmac("sha256", secret).update(`agent:7.${time}.${body}`).digest("hex")}`,
  } });
  assert.equal((await Agent.verifyWebhook(signed(), secret)).cursor, "7");
  await assert.rejects(Agent.verifyWebhook(signed(body.replace("turn_completed", "turn_failed")), secret));
  await assert.rejects(Agent.verifyWebhook(signed(), "wrong-secret-".repeat(4)));
  await assert.rejects(Agent.verifyWebhook(signed(body, "1"), secret));
});
