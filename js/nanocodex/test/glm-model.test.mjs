import test from "node:test";
import assert from "node:assert/strict";
import { Agent } from "../managed/index.mjs";
const model = "@cf/zai-org/glm-5.3";
const agentId = "0198d3f0-8844-7000-8000-000000000001";
test("managed GLM creation preserves identity and supported effort", async () => {
  for (const thinking of ["low", "medium", "high"]) {
    let body;
    await Agent.create({ baseUrl: "https://managed.example", idempotencyKey: `glm:${thinking}`,
      settings: { model, thinking, reasoningMode: "standard", fastMode: false },
      fetch: async (_url, init) => { body = JSON.parse(init.body); return Response.json({ agent_id: agentId }); },
    });
    assert.equal(body.settings.model, model);
    assert.equal(body.settings.thinking, thinking);
  }
});
test("managed GLM rejects incompatible effort and reasoning before network", async () => {
  for (const settings of [
    ...["none", "xhigh", "max"].map(thinking => ({ thinking, reasoningMode: "standard" })),
    { thinking: "low", reasoningMode: "pro" },
  ]) {
    await assert.rejects(Agent.create({ baseUrl: "https://managed.example", idempotencyKey: "glm:invalid",
      settings: { model, fastMode: false, ...settings },
      fetch: async () => { assert.fail("invalid GLM policy reached network"); },
    }), /GLM-5.3 requires/);
  }
});
