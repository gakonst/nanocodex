import assert from "node:assert/strict";
import test from "node:test";

import { MERCATOR_OAUTH_MCP_URL, needsMercatorOnboarding } from "./mercatorOnboarding.ts";
import { canonicalRemoteMcpTarget } from "../../mcp-target.mts";

test("Mercator onboarding uses the hosted OAuth endpoint and canonical account name", () => {
  assert.deepEqual(canonicalRemoteMcpTarget(MERCATOR_OAUTH_MCP_URL), {
    endpoint: MERCATOR_OAUTH_MCP_URL,
    name: "Mercator",
  });
  assert.equal(needsMercatorOnboarding([]), true);
  assert.equal(needsMercatorOnboarding([{ name: "Mercator" }]), false);
  assert.equal(needsMercatorOnboarding([{ name: "Other MCP" }]), true);
});
