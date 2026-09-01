import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Secp256k1 } from "ox";
import { KeyAuthorization } from "ox/tempo";

import {
  managedAgentPortabilityGranted,
  managedGrantHeaders,
  managedGrantUpstreamMethod,
  managedGrantWebSocketHeaders,
} from "../src/managedGrant.mjs";

const source = await readFile(new URL("../src/index.ts", import.meta.url), "utf8");

test("signed zero-spend policy retains an explicit empty call-scope list", () => {
  const policy = {
    address: "0x1111111111111111111111111111111111111111",
    chainId: 4217n,
    expiry: 2_000_000_000,
    type: "secp256k1",
    limits: [
      { token: "0x20c0000000000000000000006637932dE5413804", limit: 0n, period: 0 },
      { token: "0x20C000000000000000000000b9537d11c60E8b50", limit: 0n, period: 0 },
    ],
  };
  const signature = Secp256k1.sign({
    payload: "0xdeadbeef",
    privateKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
  });
  const explicit = KeyAuthorization.deserialize(KeyAuthorization.serialize(
    KeyAuthorization.from({ ...policy, scopes: [] }, { signature }),
  ));
  assert.equal(explicit.limits?.length, 2);
  assert.ok(explicit.limits?.every(({ limit }) => limit === 0n));
  assert.deepEqual(explicit.scopes, []);

  const omittedScopes = KeyAuthorization.deserialize(KeyAuthorization.serialize(
    KeyAuthorization.from(policy, { signature }),
  ));
  assert.equal(omittedScopes.scopes, undefined);

  const emptyLimits = KeyAuthorization.deserialize(KeyAuthorization.serialize(
    KeyAuthorization.from({ ...policy, limits: [], scopes: [] }, { signature }),
  ));
  assert.equal(emptyLimits.limits, undefined);
});

test("managed grant headers serialize only the exact delegated slice", () => {
  const headers = managedGrantHeaders({
    brokerUserId: "account-1",
    capabilities: [
      "nanocodex.agent",
      "agent.trace.read",
      "history:read",
      "memory:write",
      "github",
      "mcp:not-a-header-capability",
    ],
    connectors: ["github", "gmail"],
    connectorConnections: { gmail: ["g".repeat(43)] },
    grantId: `0x${"a".repeat(64)}`,
    mcpIds: ["mcp-1"],
    appToolCatalogDigest: `0x${"c".repeat(64)}`,
  });

  assert.deepEqual(JSON.parse(headers["x-nanocodex-connect-capabilities"]), [
    "agents:read",
    "agents:write",
    "tools:use",
    "history:read",
    "memory:write",
  ]);
  assert.deepEqual(JSON.parse(headers["x-nanocodex-connect-connectors"]), ["github", "gmail"]);
  assert.deepEqual(JSON.parse(headers["x-nanocodex-connect-connector-connections"]), {
    gmail: ["g".repeat(43)],
  });
  assert.deepEqual(JSON.parse(headers["x-nanocodex-connect-mcp-ids"]), ["mcp-1"]);
  assert.equal(
    headers["x-nanocodex-connect-app-tool-catalog-digest"],
    `0x${"c".repeat(64)}`,
  );
  assert.equal(headers["x-nanocodex-connect-user"], "account-1");
  assert.equal(headers["x-nanocodex-connect-grant-id"], `0x${"a".repeat(64)}`);
});

test("managed grant WebSocket headers assert the internal service origin", () => {
  const headers = managedGrantWebSocketHeaders({
    brokerUserId: "account-1",
    capabilities: ["nanocodex.agent"],
    connectors: ["chatgpt"],
    grantId: `0x${"a".repeat(64)}`,
    mcpIds: [],
  }, "https://nanocodex.internal");
  assert.equal(headers.origin, "https://nanocodex.internal");
  assert.equal(headers.upgrade, "websocket");
  assert.equal(headers["x-nanocodex-connect-user"], "account-1");
});

test("managed grant POST reads become internal GETs without broadening mutations", () => {
  for (const resource of ["", "/events", "/events/history", "/turns/turn-1"]) {
    assert.equal(managedGrantUpstreamMethod("POST", resource), "GET", resource);
  }
  assert.equal(managedGrantUpstreamMethod("POST", "/turns"), "POST");
  assert.equal(managedGrantUpstreamMethod("POST", "/turns/turn-1/cancel"), "POST");
  assert.equal(managedGrantUpstreamMethod("GET", "/events"), "GET");
});

test("managed portability requires the exact grant plus full history and trace visibility", () => {
  const assertion = {
    brokerUserId: "account-1",
    connectors: [],
    grantId: `0x${"b".repeat(64)}`,
    mcpIds: [],
  };
  const capabilities = (granted) => JSON.parse(managedGrantHeaders({
    ...assertion,
    capabilities: granted,
  })["x-nanocodex-connect-capabilities"]);

  const full = [
    "agent.durability.portability",
    "agent.history.read",
    "agent.trace.read",
  ];
  assert.equal(managedAgentPortabilityGranted(full), true);
  assert.ok(capabilities(full).includes("agents:portability"));
  assert.equal(managedAgentPortabilityGranted([
    "agent.durability.portability",
    "agent.history.read",
  ]), false);
  assert.equal(managedAgentPortabilityGranted([
    "agent.history.read",
    "agent.trace.read",
  ]), false);

  const projection = section("function approvedAgentCapabilities(", "function approvedHostedCapabilities(");
  assert.match(
    projection,
    /approved\.has\(agentPortabilityResource\)[\s\S]*?\["agent\.durability\.portability"\]/,
  );
});

test("managed proxy denies durability unless the exact export route has full signed portability", () => {
  const proxy = section("async function proxyManagedAgent(", "async function projectManagedResponse(");
  assert.match(proxy, /\/\^\\\/durability\(\?:\\\/\|\$\)\//);
  assert.match(proxy, /suffix !== "\/durability" \|\| request\.method !== "POST" \|\| new URL\(request\.url\)\.search !== ""/);
  assert.match(proxy, /managedAgentPortabilityGranted\(grant\.capabilities\)/);
  assert.match(proxy, /agent_portability_not_granted/);
  assert.ok(proxy.indexOf("agent_portability_not_granted") < proxy.indexOf("env.ACCOUNTS.fetch"));
  assert.match(proxy, /const upstreamMethod = managedGrantUpstreamMethod\(request\.method, suffix\)/);
  assert.match(proxy, /method: upstreamMethod/);
  assert.match(proxy, /upstreamMethod === "GET" \|\| upstreamMethod === "HEAD" \? undefined : request\.body/);
});

test("managed grant headers omit app tools unless the stored grant carries an exact catalog digest", () => {
  const headers = managedGrantHeaders({
    brokerUserId: "account-1",
    capabilities: ["nanocodex.agent"],
    connectors: [],
    grantId: `0x${"a".repeat(64)}`,
    mcpIds: [],
  });
  assert.equal(headers["x-nanocodex-connect-app-tool-catalog-digest"], undefined);
});

test("every Connect managed request uses the complete grant assertion", () => {
  assert.doesNotMatch(source, /"x-nanocodex-connect-user"/);
  assert.match(source, /managedGrantHeaders\(managedGrantAssertion\(grant\)\)/);
  assert.match(source, /managedGrantHeaders\(assertion\)/);
  assert.match(source, /connectManagedAgent\(env, store, appScope, grantAssertion, conversationId\)/);
  assert.doesNotMatch(
    section("async function createHostedAuthorization(", "async function readHostedBrowserSession("),
    /connectManagedAgent/,
  );
  assert.doesNotMatch(
    section("function createAuth(", "async function measured<value>("),
    /connectManagedAgent/,
  );
});

test("Chrome grants provision a real managed UUID with hosted no-key authorization", () => {
  const creation = section("const [durableAgent, egressSubject]", "mark(\"capabilities\")");
  assert.doesNotMatch(creation, /CHROME_EXTENSION_APP_ID[\s\S]*?agentId\(accountAddress\)/);
  assert.match(creation, /isConnectAgentId\(approval\.durableAgentId\)[\s\S]*?resolveManagedAgentIdentity[\s\S]*?connectManagedAgent\(env, store, appScope, grantAssertion, conversationId\)/);
  const storedGrant = section("const grant: GrantRecord", "try {");
  assert.match(source, /approvedAppToolCatalogDigest = appToolCatalogDigestFromResources\(approval\.resources\)/);
  assert.match(storedGrant, /approvedAppToolCatalogDigest \? \{ appToolCatalogDigest: approvedAppToolCatalogDigest \} : \{\}/);
  assert.match(storedGrant, /agentId: durableAgent\.agentId[\s\S]*?sessionId: durableAgent\.sessionId/);

  const provision = section("async function createManagedAgent(", "async function deleteManagedAgent(");
  assert.match(provision, /!isConnectAgentId\(body\.agent_id\)/);
  assert.match(provision, /!isConnectAgentId\(body\.session_id\)/);
  assert.match(provision, /agentId: body\.agent_id, sessionId: body\.session_id/);
  assert.match(source, /function isConnectAgentId\(value: unknown\)[\s\S]*?\^\[0-9a-f\]/);

  const hosted = section("async function connectionCredential(", "function serverTiming(");
  assert.match(hosted, /approval\.authorization === "hosted"[\s\S]*?cannot carry an access key or MPP authority/);
  assert.match(source, /hostedAuthorization \? "hosted" : "signed"/);

  const signedPolicy = section("function accessKeyWire(", "async function tokenBalance(");
  assert.match(signedPolicy, /authorization\.limits === undefined[\s\S]*?explicitly constrain spending/);
  assert.match(signedPolicy, /authorization\.scopes === undefined[\s\S]*?explicitly constrain contract calls/);
  assert.doesNotMatch(signedPolicy, /authorization\.scopes\?\.|\?\? \[\]/);
});

test("each signed conversation provisions one isolated exact managed agent", () => {
  const selection = section("function approvedAgentConversationId(", "function approvedHostedCapabilities(");
  assert.match(selection, /values\.length !== 1/);
  assert.match(selection, /AGENT_CONVERSATION_ID\.test\(value\)/);
  const provision = section("async function connectManagedAgent(", "function managedGrantAssertion(");
  assert.match(provision, /conversationId\?: string/);
  assert.match(provision, /connect-agent:\$\{appId\}:\$\{assertion\.brokerUserId\}\$\{conversationId/);
  assert.match(provision, /connectAgentIdentity\(retained\)/);
  const creation = section("const conversationId = approvedAgentConversationId", "mark(\"capabilities\")");
  assert.match(creation, /conversationId && isConnectAgentId\(approval\.durableAgentId\)/);
  assert.match(creation, /A new durable conversation cannot reuse an existing agent approval/);
  assert.match(section("const grant: GrantRecord", "try {"), /conversationId \? \{ conversationId \} : \{\}/);
  assert.match(section("function grantWire(", "function accessKeyWire("), /conversation_id: grant\.conversationId/);
  assert.match(section("function connectionWire(", "function grantWire("), /session_id: grant\.sessionId \?\? grant\.agentId/);
});

test("standard tools bind to the authenticated grant origin and no-history state loses its prompt", () => {
  const tools = section("async function handleAgentToolRoute(", "async function connectAccountInfo(");
  assert.match(tools, /authenticatedGrant\(request, env\.CONNECT_STATE\)[\s\S]*?requireGrantAppOrigin\(request, grant\)/);
  assert.doesNotMatch(tools, /requirePlaygroundOrigin/);

  const projection = section("function projectManagedJson(", "function projectManagedEvent(");
  assert.match(projection, /if \("first_prompt" in projected\) projected\.first_prompt = ""/);
});

test("tool-host upgrade uses a one-time exact-origin ticket bound to MCP and app tools", () => {
  const issue = section("async function issueToolHostTicket(", "async function openGrantToolHostWebSocket(");
  assert.match(issue, /tool-host-ticket:\$\{ticket\}/);
  assert.match(issue, /toolFingerprint: await grantToolHostFingerprint\(grant\)/);
  assert.match(issue, /ttl: TOOL_HOST_TICKET_TTL/);

  const open = section("async function openGrantToolHostWebSocket(", "async function grantToolHostFingerprint(");
  assert.match(open, /store\.take<ToolHostTicket>/);
  assert.match(open, /requireGrantAppOrigin\(request, grant, ticket\)/);
  assert.match(open, /grant\.id\.toLowerCase\(\) !== grantId\.toLowerCase\(\)/);
  assert.match(open, /ticket\.grantId\.toLowerCase\(\) !== grantId\.toLowerCase\(\)/);
  assert.match(open, /ticket\.agentId !== agentId/);
  assert.match(open, /ticket\.toolFingerprint\.toLowerCase\(\) !== fingerprint\.toLowerCase\(\)/);
  assert.match(open, /managedGrantWebSocketHeaders\(managedGrantAssertion\(grant\), target\.origin\)/);
  assert.match(open, /superviseGrantSocket\([\s\S]*?\), true\);/);
  assert.doesNotMatch(open, /authenticatedGrant|authorization/);
  assert.match(open, /current\.id\.toLowerCase\(\) === grantId\.toLowerCase\(\)[\s\S]*?current\.agentId === agentId[\s\S]*?current\.appId === grant\.appId[\s\S]*?grantToolHostFingerprint\(current\)/);

  const fingerprint = section("async function grantToolHostFingerprint(", "async function openGrantRealtimeWebSocket(");
  assert.match(fingerprint, /appToolCatalogDigest: grant\.appToolCatalogDigest \?\? null/);
  assert.match(fingerprint, /mcpConnections: grant\.mcpConnections \?\? \[\]/);

  const supervision = section("function superviseGrantSocket(", "function closeSocket(");
  assert.match(supervision, /preserveUpstreamPolicyClose = false/);
  assert.match(supervision, /preserveUpstreamPolicyClose && event\.code === 1008[\s\S]*?close\(1008, event\.reason/);
});

function section(start, end) {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from + start.length);
  assert.notEqual(from, -1, `missing ${start}`);
  assert.notEqual(to, -1, `missing ${end}`);
  return source.slice(from, to);
}
