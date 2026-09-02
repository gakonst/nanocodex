import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("../src/index.ts", import.meta.url), "utf8");
const managedGrant = await readFile(new URL("../src/managedGrant.mjs", import.meta.url), "utf8");

test("signed and hosted approvals persist exact connection snapshots", () => {
  const captures = source.match(/connectorConnectionSnapshot\(status\.connectors,/g) ?? [];
  assert.equal(captures.length, 2);
  assert.match(source, /connectedConnectors,\s*connectorConnections,\s*mcpConnections,/);
});

test("grant exchange completes the approval snapshot with exact live identities", () => {
  assert.match(source, /completeConnectorConnectionSnapshot\(\s*approval\.connectorConnections,\s*approval\.connectedConnectors,\s*liveConnectorStatuses,\s*requested,/);
  assert.match(source, /legacyConnectorCapabilities\.includes\(connector\)/);
  assert.match(source, /legacyConnectorCapabilities\.length === 0/);
  assert.match(source, /connector_connections: grant\.connectorConnections/);
  assert.match(managedGrant, /x-nanocodex-connect-connector-connections/);
});

test("connector execution uses only the provider-neutral selector header", () => {
  assert.match(source, /applyConnectorConnectionSelector\(\s*headers,\s*grant\.legacyConnectorCapabilities\?\.includes\(connector\)[\s\S]*?: grant\.connectorConnections,\s*connector,/);
  assert.match(source, /"x-nanocodex-connector-connection"/);
  assert.doesNotMatch(`${source}\n${managedGrant}`, /x-nanocodex-connector-instance/i);
});
