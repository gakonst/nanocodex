import assert from "node:assert/strict";
import test from "node:test";
import { liveAgentFailure, liveAgentRequest, liveAgentSettings, nativeLiveRequest } from "nanocodex/cloudflare/managed-live";
const owner = { kind: "api_key", userId: "owner", organizationId: "org", teamId: "team", authorizationEpoch: 3,
  capabilities: ["agents:read", "agents:write", "tools:use"] };
const request = headers => new Request("https://account.test/v1/agents/live", { headers: { upgrade: "websocket", ...headers } });

test("shared admission keeps browser origin and Connect connector restrictions", () => {
  assert.equal(liveAgentFailure(request(), owner), undefined);
  assert.equal(liveAgentFailure(request(), undefined).status, 401);
  assert.equal(liveAgentFailure(request(), { ...owner, kind: "account_session" }).status, 403);
  assert.equal(liveAgentFailure(request({ origin: "https://account.test" }), { ...owner, kind: "account_session" }), undefined);
  assert.equal(liveAgentFailure(request({ origin: "https://evil.test" }), { ...owner, kind: "account_session" }).status, 403);
  const grant = { ...owner, kind: "connect_grant", connectGrant: { grantId: "grant", connectors: ["github"], mcpIds: [] } };
  assert.equal(liveAgentFailure(request({ origin: "https://account.test" }), grant).status, 403);
  grant.connectGrant.connectors.push("chatgpt");
  assert.equal(liveAgentFailure(request({ origin: "https://account.test" }), grant), undefined);
});

test("shared assertion builder clears every caller Connect assertion then writes only verified scope", () => {
  const headers = Object.fromEntries(["user", "grant-id", "capabilities", "connectors", "connector-connections", "mcp-ids", "app-tool-catalog-digest"].map(k => ["x-nanocodex-connect-" + k, "forged"]));
  const req = request(headers), settings = liveAgentSettings(req);
  const native = liveAgentRequest(req, owner, settings, "fresh", null);
  assert.equal([...native.headers.keys()].some(k => k.startsWith("x-nanocodex-connect-")), false);
  const scoped = { ...owner, kind: "connect_grant", connectGrant: { grantId: "grant", connectors: ["chatgpt"], mcpIds: ["mcp"], connectorConnections: { github: ["connection"] }, appToolCatalogDigest: "digest" } };
  const connect = liveAgentRequest(req, scoped, settings, "fresh", null);
  assert.equal(connect.headers.has("x-nanocodex-connect-user"), false);
  assert.equal(connect.headers.has("x-nanocodex-connect-capabilities"), false);
  assert.equal(connect.headers.get("x-nanocodex-connect-grant-id"), "grant");
  assert.deepEqual(JSON.parse(connect.headers.get("x-nanocodex-connect-connector-connections")), { github: ["connection"] });
  assert.equal(connect.headers.get("x-nanocodex-connect-app-tool-catalog-digest"), "digest");
});

test("direct profile declines every Cookie header including the same-deployment comparison cookie", () => {
  const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
  assert.equal(nativeLiveRequest(request({ authorization: `Bearer ${token}` })), true);
  for (const cookie of ["", "nc_perf_legacy_path=1", "nanocodex_account=fixture"]) {
    assert.equal(nativeLiveRequest(request({ authorization: `Bearer ${token}`, cookie })), false);
  }
});
