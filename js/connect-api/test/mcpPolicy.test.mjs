import assert from "node:assert/strict";
import test from "node:test";

import {
  canonicalRemoteMcpTarget,
  isAllowedMcpResource,
  validateMcpResources,
} from "../src/mcpPolicy.mts";

const id = "a".repeat(43);
const other = "b".repeat(43);

test("canonicalizes the Linear MCP shorthand without exposing credentials", () => {
  assert.deepEqual(canonicalRemoteMcpTarget("mcp.linear.app"), {
    endpoint: "https://mcp.linear.app/mcp",
    name: "mcp.linear.app",
  });
});

test("accepts public HTTPS MCP URLs and rejects private or credential-bearing targets", () => {
  assert.equal(canonicalRemoteMcpTarget("https://example.com/custom/mcp").endpoint,
    "https://example.com/custom/mcp");
  for (const target of [
    "http://mcp.linear.app/mcp",
    "mcp.example.com",
    "mcp.localhost",
    "https://127.0.0.1/mcp",
    "https://mcp.example.com:8443/mcp",
    "https://user:password@mcp.example.com/mcp",
    "https://mcp.example.com/mcp?access_token=secret",
  ]) {
    assert.throws(() => canonicalRemoteMcpTarget(target));
  }
});

test("MCP resources require opaque IDs and singular contained focus", () => {
  assert.equal(isAllowedMcpResource(`urn:nanocodex:mcp:${id}`), true);
  assert.equal(isAllowedMcpResource("urn:nanocodex:mcp:https://mcp.linear.app/mcp"), false);
  assert.deepEqual(validateMcpResources([
    `urn:nanocodex:mcp:${id}`,
    `urn:nanocodex:mcp:${other}`,
    `urn:nanocodex:mcp-focus:${other}`,
  ]), { requested: [id, other], focus: other });
  assert.throws(() => validateMcpResources([
    `urn:nanocodex:mcp:${id}`,
    `urn:nanocodex:mcp-focus:${other}`,
  ]));
  assert.throws(() => validateMcpResources([
    `urn:nanocodex:mcp:${id}`,
    `urn:nanocodex:mcp-focus:${id}`,
    `urn:nanocodex:mcp-focus:${id}`,
  ]));
});
