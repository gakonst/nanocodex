import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const app = source("../connect-dialog/src/App.tsx");
const chooser = source("../connect-dialog/src/AccountChooser.tsx");
const accountMenu = source("../src/AccountMenu.tsx");
const profileConnectors = source("../src/ProfileConnectors.tsx");
const connectorCompletion = source("../connect-dialog/src/connectorCompletion.ts");
const playgroundConfig = source("../connect-playground/src/config.ts");

test("Account and embedded Connect render the same identity and connection components", () => {
  assert.match(app, /from "\.\/AccountChooser"/);
  assert.match(app, /from "\.\/AccountConnectionSurface"/);
  assert.match(app, /from "\.\/ConnectionLogo"/);
  assert.match(accountMenu, /from "@nanocodex-connect\/AccountChooser"/);
  assert.match(accountMenu, /from "@nanocodex-connect\/AccountConnectionSurface"/);
  assert.match(accountMenu, /from "@nanocodex-connect\/ConnectionLogo"/);
  assert.match(profileConnectors, /from "@nanocodex-connect\/AccountConnectionSurface"/);
  assert.doesNotMatch(app, />Existing<|>New<|function ConnectorLogo/);
});

test("the shared chooser keeps current, remembered, system, creation, and cancellation actions explicit", () => {
  assert.match(chooser, /account\.current \? " is-current"/);
  assert.match(chooser, /orderedPasskeys\(storedPasskeys\)\.map/);
  assert.match(chooser, />Use another passkey</);
  assert.match(chooser, />Create a new account</);
  assert.match(chooser, />Cancel<\/button>/);
});

test("scoped Connect filters signed request context and retains approval return hooks", () => {
  assert.match(app, /request\.permission\.connectors\.length[\s\S]*?<WizardConnectorList/);
  assert.match(app, /request\.mcpConnections\.length[\s\S]*?<McpConnectionList/);
  assert.match(app, /appVisibilityPermissions\(request\.auth\.resources\)/);
  assert.match(app, /request\.auth\.resources\.map/);
  assert.match(app, /await host\.respond\(result\)/);
  assert.match(app, /host\.reject\(new Error\("The request was not approved\."\)\)/);
});

test("Account and filtered Connect reuse the shared MCP card without exposing opaque IDs", () => {
  assert.match(app, /McpConnectionCard/);
  assert.match(profileConnectors, /McpConnectionCard/);
  const sharedSurface = source("../connect-dialog/src/AccountConnectionSurface.tsx");
  assert.match(sharedSurface, /export function McpConnectionAddCard/);
  assert.match(sharedSurface, /Linear shorthand or a public HTTPS endpoint/);
  assert.doesNotMatch(sharedSurface, /canonicalRemoteMcpTarget|private.*host|localhost/);
  assert.doesNotMatch(sharedSurface, /navigator\.clipboard\.writeText\(connection\.id\)/);
  assert.doesNotMatch(sharedSurface, /shortMcpConnectionIdentifier|Copy identifier/);
  assert.doesNotMatch(profileConnectors, /mcpConnections\?\.map[\s\S]*?className={`connection-card connector-row mcp-connector-row/);
});

test("Account owns MCP creation and in-place authorization without broadening embedded grants", () => {
  assert.match(profileConnectors, /method: "POST"[\s\S]*?body: JSON\.stringify\(\{ target \}\)/);
  assert.match(profileConnectors, /mcp-connections\/\$\{encodeURIComponent\(connection\.id\)\}\/start/);
  assert.match(profileConnectors, /body: JSON\.stringify\(\{ return_to: connectorReturnTo\(\) \}\)/);
  assert.match(profileConnectors, /searchParams\.get\("mcp_connection"\)/);
  assert.match(profileConnectors, /searchParams\.get\("mcp_result"\)/);
  assert.match(profileConnectors, /mcpConnectionAction\(connection\.status\)/);
  assert.doesNotMatch(app, /McpConnectionAddCard/);
});

test("Account supports adding and disconnecting individual Google accounts", () => {
  assert.match(profileConnectors, /multi && status\.connected \? "Add account"/);
  assert.match(profileConnectors, /status\.connections\?\.map/);
  assert.match(profileConnectors, /disconnect\(definition\.id, connection\.id\)/);
  assert.match(profileConnectors, /connectors\/\$\{id\}\$\{connectionId \? `\/\$\{encodeURIComponent\(connectionId\)\}` : ""\}/);
});

test("Account and embedded Connect share strict in-place OAuth completion", () => {
  assert.match(app, /connectorCompletionFor\(event/);
  assert.match(profileConnectors, /from "@nanocodex-connect\/connectorCompletion"/);
  assert.match(profileConnectors, /window\.open\([\s\S]*?"about:blank"[\s\S]*?popup\.location\.href = authorizationUrl\.href/);
  assert.match(profileConnectors, /connectorCompletionFor\(event,[\s\S]*?origin: window\.location\.origin[\s\S]*?source: attempt\.popup/);
  assert.match(profileConnectors, /refreshConnectors\(attempt\.abort\.signal\)[\s\S]*?statuses\[attempt\.connector\]\.connected/);
  assert.match(profileConnectors, /window\.opener\.postMessage\(connectorCompletion\(id as ConnectorId, result\), window\.location\.origin\)/);
  assert.match(profileConnectors, /authorization popup was blocked[\s\S]*?authorization popup was closed before it completed/);
  assert.doesNotMatch(profileConnectors, /window\.location\.assign\(authorizationUrl\.href\)/);
  assert.doesNotMatch(profileConnectors, /localStorage|sessionStorage/);
  assert.match(connectorCompletion, /event\.origin === expected\.origin[\s\S]*?event\.source === expected\.source[\s\S]*?event\.data\.connector === expected\.connector/);
});

test("embedded Connect requires an explicit final approval after requested accounts are ready", () => {
  assert.match(app, /!wizard[\s\S]*?!pendingApproval[\s\S]*?approvalReady/);
  assert.match(app, /wizard && approvalReady\(next/);
  assert.match(app, /connectedAccessReady \? "Approve access" : "Connect requested accounts"/);
  assert.match(app, /onClick=\{approveConnectedAccess\}/);
  const finishAttempt = app.slice(app.indexOf("const finishConnectorAttempt"), app.indexOf("useEffect(() =>", app.indexOf("const finishConnectorAttempt")));
  assert.doesNotMatch(finishAttempt, /setMcpConnections\(undefined\)/);
});

test("the Connect playground grants its managed agent the history and memory tools it presents", () => {
  assert.match(playgroundConfig, /"urn:nanocodex:history:read"/);
  assert.match(playgroundConfig, /"urn:nanocodex:memory:read"/);
  assert.match(playgroundConfig, /"urn:nanocodex:memory:write"/);
});

function source(path: string): string {
  return readFileSync(new URL(path, import.meta.url), "utf8");
}
