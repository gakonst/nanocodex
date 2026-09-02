import assert from "node:assert/strict";
import { test } from "node:test";

import {
  connectionFromWire,
  connectionMatchesRequest,
  reconnectRequestFromConnection,
} from "../cloud/internal.mjs";

const A = "a".repeat(43);
const B = "b".repeat(43);

test("cloud grants preserve exact service capability connection selections", () => {
  const connection = connectionFromWire(wire({
    capabilities: [
      "nanocodex.agent",
      "gmail",
      "gdrive",
      "gcalendar",
      "gtasks",
      "gdocs",
      "gsheets",
      "gslides",
      "gcontacts",
      "slack",
    ],
    connectorConnections: {
      gmail: [A, B],
      gdrive: [A],
      gcalendar: [A],
      gtasks: [A],
      gdocs: [A],
      gsheets: [A],
      gslides: [A],
      gcontacts: [A],
      slack: [B],
    },
  }));

  assert.deepEqual(connection.grant.connectors, [
    "gmail",
    "gdrive",
    "gcalendar",
    "gtasks",
    "gdocs",
    "gsheets",
    "gslides",
    "gcontacts",
    "slack",
  ]);
  assert.deepEqual(connection.grant.connectorConnections, {
    gmail: [A, B],
    gdrive: [A],
    gcalendar: [A],
    gtasks: [A],
    gdocs: [A],
    gsheets: [A],
    gslides: [A],
    gcontacts: [A],
    slack: [B],
  });
  assert.equal(Object.isFrozen(connection.grant.connectorConnections), true);
  assert.equal(Object.isFrozen(connection.grant.connectorConnections.gmail), true);
  const retained = reconnectRequestFromConnection(connection);
  assert.deepEqual(retained.connectorConnections, connection.grant.connectorConnections);
  assert.equal(connectionMatchesRequest(connection, retained), true);
  assert.equal(connectionMatchesRequest(connection, {
    ...retained,
    connectorConnections: { ...retained.connectorConnections, slack: [A] },
  }), false);
});

test("cloud connection readers accept legacy grants and reject widened selections", () => {
  const legacy = connectionFromWire(wire({ capabilities: ["nanocodex.agent", "slack"] }));
  assert.equal(legacy.grant.connectorConnections, undefined);

  assert.throws(() => connectionFromWire(wire({
    capabilities: ["nanocodex.agent", "slack"],
    connectorConnections: { github: [A] },
  })), /ungranted connector capability/);
  assert.throws(() => connectionFromWire(wire({
    capabilities: ["nanocodex.agent", "slack"],
    connectorConnections: { slack: ["short"] },
  })), /opaque connection ID/);
  assert.throws(() => connectionFromWire(wire({
    capabilities: ["nanocodex.agent", "slack"],
    connectorConnections: { slack: [A, A] },
  })), /duplicate connections/);
});

function wire({ capabilities, connectorConnections }) {
  return {
    account_address: `0x${"11".repeat(20)}`,
    agent_id: "agent_connector_contract",
    authorization_mode: "hosted",
    grant: {
      id: `0x${"22".repeat(32)}`,
      permission: "agent.run",
      status: "active",
      expires_at: Math.floor(Date.now() / 1_000) + 3_600,
      capabilities,
      mcp_connections: [],
      ...(connectorConnections === undefined ? {} : {
        connector_connections: connectorConnections,
      }),
    },
  };
}
