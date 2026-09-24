import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("../src/index.ts", import.meta.url), "utf8");
const managedGrant = await readFile(new URL("../src/managedGrant.mts", import.meta.url), "utf8");

test("signed, hosted, and host-principal approvals persist exact connection snapshots", () => {
  const captures = source.match(/connectorConnectionSnapshot\(status\.connectors,/g) ?? [];
  assert.equal(captures.length, 3);
  assert.match(source, /connectedConnectors,\s*connectorConnections,\s*mcpConnections,/);
});

test("grant exchange completes the approval snapshot with exact live identities", () => {
  assert.match(source, /completeConnectorConnectionSnapshot\(\s*approval\.connectorConnections,\s*approval\.connectedConnectors,\s*liveConnectorStatuses,\s*requested,/);
  assert.match(source, /legacyConnectorCapabilities\.includes\(connector\)/);
  assert.match(source, /legacyConnectorCapabilities\.length === 0/);
  assert.match(source, /connector_connections: grant\.connectorConnections/);
  assert.match(managedGrant, /x-nanocodex-connect-connector-connections/);
});

test("signed browser cookie sync consent enters the retained grant capability set", () => {
  assert.match(
    source,
    /approved\.has\(BROWSER_COOKIE_SYNC_RESOURCE\)\s*\?\s*\[BROWSER_COOKIE_SYNC_RESOURCE\]/,
  );
  assert.match(
    source,
    /grant\.capabilities\.includes\(BROWSER_COOKIE_SYNC_RESOURCE\)/,
  );
  assert.match(
    source,
    /parseCliBrowserCookieSyncResource\(resource\)\s*!==\s*undefined/,
  );
  assert.match(
    source,
    /parseCliBrowserCookieSyncResource\(capability\)/,
  );
});
