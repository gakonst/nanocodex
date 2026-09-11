import test from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { Agent } from "../managed/index.mjs";
const id = "0198d3f0-8844-7000-8000-000000000001";
test("caller-owned creation keys survive retries and separate SDK invocations", async () => {
  const requests = [];
  const options = { baseUrl: "https://managed.example", idempotencyKey: "create:job-42", fetch: async (url, init) => {
    const request = new Request(url, init);
    requests.push({ key: request.headers.get("idempotency-key"), body: await request.text() });
    if (requests.length === 1) throw new Error("lost creation receipt");
    return Response.json({ agent_id: id });
  } };
  const configuration = { tools: [], multi_agent: { enabled: false } };
  const first = await Agent.create({ ...options, configuration });
  const recovered = await Agent.create({ ...options, configuration });
  assert.equal(first.id, recovered.id);
  assert.equal(requests.length, 3);
  assert.deepEqual(requests, Array(3).fill({ key: "create:job-42", body: JSON.stringify({ configuration }) }));
  await Agent.create({ ...options, idempotencyKey: "create:job-43", settings: { model: "gpt-5.6-luna", thinking: "low", reasoningMode: "standard", fastMode: false } });
  assert.equal(requests[3].key, "create:job-43");
  assert.equal(JSON.parse(requests[3].body).settings.reasoning_mode, "standard");
  assert.equal(Object.hasOwn(JSON.parse(requests[3].body), "idempotencyKey"), false);
  await Agent.create({ ...options, idempotencyKey: "~".repeat(256) });
  assert.equal(requests[4].body, "");
  assert.equal(requests[4].key.length, 256);
  await first.state();
  assert.equal(requests[5].key, null, "creation keys must not become handle-wide headers");
});
test("creation keys reject invalid values before sending a request", async () => {
  let requests = 0;
  const options = { baseUrl: "https://managed.example", fetch: async () => { requests += 1; return Response.json({ agent_id: id }); } };
  for (const idempotencyKey of [null, 42, "", "has space", "line\nbreak", "é", "x".repeat(257)]) {
    await assert.rejects(Agent.create({ ...options, idempotencyKey }), /invalid managed creation idempotency key/);
  }
  assert.equal(requests, 0);
});
test("creation conflicts are surfaced without generating a replacement key", async () => {
  let requests = 0;
  await assert.rejects(Agent.create({ baseUrl: "https://managed.example", idempotencyKey: "create:job-42", fetch: async (url, init) => {
    requests += 1;
    assert.equal(new Request(url, init).headers.get("idempotency-key"), "create:job-42");
    return Response.json({ error: "agent_initialization_conflict" }, { status: 409 });
  } }), error => error.status === 409 && error.code === "agent_initialization_conflict");
  assert.equal(requests, 1);
});
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
