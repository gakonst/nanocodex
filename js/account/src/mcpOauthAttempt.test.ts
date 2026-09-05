import assert from "node:assert/strict";
import test from "node:test";

import { mcpOauthAttemptMode } from "./mcpOauthAttempt.ts";

test("only a live MCP OAuth popup owns the account action fence", () => {
  assert.equal(mcpOauthAttemptMode(undefined), "idle");
  assert.equal(mcpOauthAttemptMode({ popup: {} }), "blocking");
  assert.equal(mcpOauthAttemptMode({}), "recoverable");
});
