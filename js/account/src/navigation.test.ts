import assert from "node:assert/strict";
import test from "node:test";

import { agentIdFromPath, pathForAgent, surfaceFromUrl } from "./navigation.ts";

test("durable agent paths carry the managed agent identity", () => {
  const agentId = "77777777-7777-4777-8777-777777777777";
  const path = pathForAgent(agentId);

  assert.equal(path, `/agent/${agentId}`);
  assert.equal(agentIdFromPath(path), agentId);
  assert.equal(surfaceFromUrl(new URL(`https://nanocodex.test${path}`)), "agent");
});

test("the agent collection path has no selected managed agent", () => {
  assert.equal(agentIdFromPath("/agent"), undefined);
  assert.equal(surfaceFromUrl(new URL("https://nanocodex.test/agent")), "agent");
});
