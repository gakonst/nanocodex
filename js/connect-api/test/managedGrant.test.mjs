import assert from "node:assert/strict";
import test from "node:test";

import {
  managedAgentExistenceStatus,
  managedGrantHeaders,
  managedGrantUpstreamMethod,
} from "../src/managedGrant.mts";

test("managed grant headers carry the exact connection snapshot without credentials", () => {
  const connectionId = "a".repeat(43);
  const headers = managedGrantHeaders({
    brokerUserId: "user-1",
    capabilities: ["gmail"],
    connectors: ["gmail"],
    connectorConnections: { gmail: [connectionId] },
    grantId: `0x${"b".repeat(64)}`,
    mcpIds: [],
  });
  assert.deepEqual(
    JSON.parse(headers["x-nanocodex-connect-connector-connections"]),
    { gmail: [connectionId] },
  );
  assert.equal(JSON.stringify(headers).includes("token"), false);

  const legacy = managedGrantHeaders({
    brokerUserId: "user-1",
    capabilities: ["gmail"],
    connectors: ["gmail"],
    grantId: `0x${"c".repeat(64)}`,
    mcpIds: [],
  });
  assert.equal(legacy["x-nanocodex-connect-connector-connections"], undefined);
});

test("managed reads use the internal GET boundary while mutations remain POST", () => {
  assert.equal(managedGrantUpstreamMethod("POST", ""), "GET");
  assert.equal(managedGrantUpstreamMethod("POST", "/events"), "GET");
  assert.equal(managedGrantUpstreamMethod("POST", "/events/history"), "GET");
  assert.equal(managedGrantUpstreamMethod("POST", "/turns/turn-1"), "GET");
  assert.equal(managedGrantUpstreamMethod("POST", "/turns"), "POST");
  assert.equal(managedGrantUpstreamMethod("POST", "/turns/turn-1/cancel"), "POST");
});

test("managed existence probes replace only a definitive missing session", () => {
  assert.equal(managedAgentExistenceStatus(new Response(null, { status: 204 })), "available");
  assert.equal(managedAgentExistenceStatus(new Response(null, { status: 404 })), "missing");
  assert.equal(managedAgentExistenceStatus(new Response(null, { status: 409 })), "unavailable");
  assert.equal(managedAgentExistenceStatus(new Response(null, { status: 403 })), "unavailable");
  assert.equal(managedAgentExistenceStatus(new Response(null, { status: 503 })), "unavailable");
});
