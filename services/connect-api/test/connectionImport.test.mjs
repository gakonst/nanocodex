import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("../src/index.ts", import.meta.url), "utf8");

function section(start, end) {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from + start.length);
  assert.notEqual(from, -1, `missing ${start}`);
  assert.notEqual(to, -1, `missing ${end}`);
  return source.slice(from, to);
}

test("connection import is bounded and precedes live recheck, approval take, and grant", () => {
  const route = section("async function createConnection(", "async function connectionRequestBody(");
  const installPolicy = route.indexOf("connectionCredential(");
  const brokerImport = route.indexOf("importChatGptCredential(");
  const connectorRecheck = route.indexOf("connectorStatuses(");
  const mcpRecheck = route.indexOf("connectedRequestedMcpConnections(");
  const approvalTake = route.indexOf("takeConnectApproval(");
  const grant = route.indexOf("const grant: GrantRecord");
  assert.ok(installPolicy > 0);
  assert.ok(installPolicy < brokerImport);
  assert.ok(brokerImport < connectorRecheck);
  assert.ok(connectorRecheck < mcpRecheck);
  assert.ok(mcpRecheck < approvalTake);
  assert.ok(approvalTake < grant);

  const parser = section("async function connectionRequestBody(", "async function approvedChatGptCredentialImport(");
  assert.match(parser, /boundedJson\(request, MAX_CONNECTION_REQUEST_BYTES/);
  assert.match(parser, /chatgpt_credential_import/);
  assert.match(parser, /contains an unknown field/);

  const approval = section("async function approvedChatGptCredentialImport(", "async function importChatGptCredential(");
  assert.match(approval, /\(value === undefined\) !== \(approvedDigest === undefined\)/);
  assert.match(approval, /chatGptCredentialImportDigest\(credential\) !== approvedDigest/);
  assert.match(approval, /app\.appId !== CLI_APP_ID \|\| app\.origin !== CLI_APP_ORIGIN/);
  assert.match(approval, /approvedConnectors\(approval\.resources\)\.has\("chatgpt"\)/);
});

test("generic Slack approval expands only its captured workspace snapshot", () => {
  assert.match(source, /connectedConnectorSelection\(\s*liveConnectorStatuses,\s*requested,\s*approval\.connectedConnectors,\s*\)/);
  assert.match(source, /\.filter\(\(reference\) => approved\.has\(reference\)\)/);
});

test("credential bytes cross only the private EGRESS import call and never enter grant storage", () => {
  const importer = section("async function importChatGptCredential(", "async function connectionCredential(");
  assert.match(importer, /env\.EGRESS\.fetch/);
  assert.match(importer, /method: "PUT"/);
  assert.match(importer, /\/users\/\$\{encodeURIComponent\(brokerUserId\)\}\/credentials\/chatgpt/);
  assert.doesNotMatch(importer, /console\./);
  assert.doesNotMatch(importer, /store\./);
  assert.doesNotMatch(importer, /response\.(?:json|text)\(/);
  assert.doesNotMatch(importer, /x-nanocodex-connect-user/);

  const route = section("async function createConnection(", "async function connectionRequestBody(");
  const grant = route.slice(route.indexOf("const grant: GrantRecord"), route.indexOf("try {", route.indexOf("const grant: GrantRecord")));
  assert.doesNotMatch(grant, /access_token|refresh_token|chatgpt_credential_import/);
});

test("Connect realtime admission follows the exact grant app origin", () => {
  const ticket = section("async function issueRealtimeTicket(", "async function openGrantRealtimeWebSocket(");
  assert.match(ticket, /appId: grant\.appId/);
  assert.match(ticket, /appOrigin: grant\.appOrigin/);

  const websocket = section("async function openGrantRealtimeWebSocket(", "async function openGrantModelWebSocket(");
  assert.doesNotMatch(websocket, /requirePlaygroundOrigin\(request\)/);
  assert.match(websocket, /ticket\.appId !== grant\.appId/);
  assert.match(websocket, /ticket\.appOrigin !== grant\.appOrigin/);
  assert.match(websocket, /grant\.capabilities\.includes\("chatgpt"\)/);
  assert.match(websocket, /grant\.capabilities\.includes\("agent\.output\.final"\)/);
  assert.match(websocket, /requireGrantAppOrigin\(request, grant, ticket\)/);
  assert.match(websocket, /managedGrantWebSocketHeaders\(managedGrantAssertion\(grant\), target\.origin\)/);

  const grantRoute = section("async function handleGrantRoute(", "async function connectManagedAgent(");
  const grantAuthentication = grantRoute.indexOf("await authenticatedGrant(request, env.CONNECT_STATE, grantId)");
  const reconnect = grantRoute.indexOf('action === "reconnect" && request.method === "POST"');
  assert.ok(grantAuthentication > 0);
  assert.ok(reconnect > grantAuthentication);
  assert.match(grantRoute, /requireGrantAppOrigin\(request, grant\)/g);
  assert.doesNotMatch(grantRoute, /requirePlaygroundOrigin\(request\)/);
  assert.match(grantRoute, /action === "reconnect" && request\.method === "POST"/);

  const originGuard = section("function requireGrantAppOrigin(", "function requireCallerApp(");
  assert.match(originGuard, /origin !== grant\.appOrigin/);
  assert.match(originGuard, /ticket\.appId !== grant\.appId/);
  assert.match(originGuard, /ticket\.appOrigin !== grant\.appOrigin/);
});
