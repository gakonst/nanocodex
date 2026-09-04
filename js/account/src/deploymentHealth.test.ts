import assert from "node:assert/strict";
import test from "node:test";

import { createDeploymentHealthResource } from "./deploymentHealth.ts";

test("projects sponsored homepage access without enabling voice", async () => {
  const health = await createDeploymentHealthResource(async () => Response.json({
    agent_configured: true,
    credential_source: "sponsored",
    free_prompts_remaining: 3,
    voice_enabled: true,
  })).read();

  assert.deepEqual(health, {
    agentConfigured: true,
    astraEntitled: false,
    credentialSource: "sponsored",
    deploymentSha: undefined,
    freePromptsRemaining: 3,
    voiceEnabled: false,
  });
});

test("retains user-owned and brokered credential projections", async () => {
  for (const [wire, expected] of [
    ["user", "brokered"],
    ["subscription", "brokered"],
    ["brokered", "brokered"],
  ] as const) {
    const health = await createDeploymentHealthResource(async () => Response.json({
      agent_configured: true,
      credential_source: wire,
      voice_enabled: true,
    })).read();
    assert.equal(health.credentialSource, expected);
    assert.equal(health.astraEntitled, false);
    assert.equal(health.freePromptsRemaining, null);
    assert.equal(health.voiceEnabled, true);
  }
});

test("projects Astra only for an entitled brokered account", async () => {
  const entitled = await createDeploymentHealthResource(async () => Response.json({
    agent_configured: true,
    astra_entitled: true,
    credential_source: "brokered",
  })).read();
  const sponsored = await createDeploymentHealthResource(async () => Response.json({
    agent_configured: true,
    astra_entitled: true,
    credential_source: "sponsored",
    free_prompts_remaining: 1,
  })).read();

  assert.equal(entitled.astraEntitled, true);
  assert.equal(sponsored.astraEntitled, false);
});

test("fails closed for sponsored access without a valid remaining prompt count", async () => {
  const health = await createDeploymentHealthResource(async () => Response.json({
    agent_configured: true,
    credential_source: "sponsored",
  })).read();
  assert.equal(health.agentConfigured, false);
  assert.equal(health.astraEntitled, false);
  assert.equal(health.credentialSource, null);
});
