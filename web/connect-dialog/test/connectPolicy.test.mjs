import assert from "node:assert/strict";
import test from "node:test";

import {
  accountLoginCapabilities,
  appVisibilityPermissions,
  chatGptConnectorDisposition,
  connectorApprovalDisposition,
  connectApiOrigin,
  createMcpCallbackContinuation,
  deviceMcpReturnPath,
  focusedConnectorFromResources,
  isLocalDevelopmentOrigin,
  isPopupPresentation,
  focusedMcpConnection,
  mcpConnectionApprovalDisposition,
  mcpConnectionsFromWire,
  parseConnectPolicy,
  productionConnectApiOrigin,
  registeredApp,
  restoreMcpCallbackContinuation,
  sanitizeCliWalletResult,
  sanitizeWalletResult,
  signedAppResources,
  usesBrowserLocalWebAuthn,
} from "../src/connectPolicy.mjs";

const LINEAR_MCP = "linear_abcdefghijklmnopqrstuvwxyz0123456789";
const CLOUDFLARE_MCP = "cloudf_abcdefghijklmnopqrstuvwxyz0123456789";

test("local development origins use the canonical localhost family", () => {
  assert.equal(isLocalDevelopmentOrigin("http://nanocodex.localhost:5173"), true);
  assert.equal(isLocalDevelopmentOrigin("http://passkey-a.nanocodex.localhost:5173"), true);
  assert.equal(isLocalDevelopmentOrigin("http://playground-passkey-a.nanocodex.localhost:5173"), true);
  assert.equal(isLocalDevelopmentOrigin("http://nested.evil.nanocodex.localhost:5173"), false);
  assert.equal(isLocalDevelopmentOrigin("https://nanocodex.local"), false);
  assert.equal(isLocalDevelopmentOrigin("http://nanocodex.local"), false);
  assert.equal(isLocalDevelopmentOrigin("https://notnanocodex.local"), false);
  assert.equal(isLocalDevelopmentOrigin("http://nanocodex.example:5173"), false);
  assert.equal(isLocalDevelopmentOrigin("https://connect.example"), false);
});

test("canonical Nanocodex localhost dialogs use the portable server ceremony", () => {
  assert.equal(usesBrowserLocalWebAuthn("http://127.0.0.1:4177"), true);
  assert.equal(usesBrowserLocalWebAuthn("https://localhost:4177"), true);
  assert.equal(usesBrowserLocalWebAuthn("http://nanocodex.localhost:4177"), false);
  assert.equal(usesBrowserLocalWebAuthn("http://passkey-a.nanocodex.localhost:4177"), false);
  assert.equal(usesBrowserLocalWebAuthn("https://nanocodex.local"), false);
  assert.equal(usesBrowserLocalWebAuthn("https://nanocodex.example"), false);
  assert.equal(usesBrowserLocalWebAuthn("https://nanocodex.gakonst.workers.dev"), false);
});

const playground = "https://nanocodex-connect-playground.gakonst.workers.dev";
const chromeExtension = "chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle";
const productionDialog = "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=iframe";
const cli = "https://cli.nanocodex.xyz";
const chatGptCredentialImport = `urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:${"a".repeat(43)}`;

test("an exact ChatGPT Codex auth resource defers the signed ChatGPT connector", () => {
  assert.deepEqual(parseConnectPolicy([
    "urn:nanocodex:connector:chatgpt",
    chatGptCredentialImport,
  ]), { chatGptCredentialImport: true });
  assert.deepEqual(parseConnectPolicy([
    "urn:nanocodex:connectors:github,chatgpt",
    chatGptCredentialImport,
  ]), { chatGptCredentialImport: true });
  assert.deepEqual(parseConnectPolicy([
    "urn:nanocodex:connector:chatgpt",
  ]), { chatGptCredentialImport: false });
});

test("malformed ChatGPT credential import resources are rejected", () => {
  for (const resource of [
    "urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:short",
    `urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:${"a".repeat(43)}=`,
    `urn:nanocodex:credential-import:chatgpt:other:sha256:${"a".repeat(43)}`,
    `urn:nanocodex:credential-import:github:codex-auth-v1:sha256:${"a".repeat(43)}`,
  ]) {
    assert.throws(() => parseConnectPolicy([
      "urn:nanocodex:connector:chatgpt",
      resource,
    ]), /credential import resource is invalid/);
  }
});

test("duplicate and orphan ChatGPT credential import resources are rejected", () => {
  assert.throws(() => parseConnectPolicy([
    "urn:nanocodex:connector:chatgpt",
    chatGptCredentialImport,
    chatGptCredentialImport,
  ]), /credential import resource is invalid/);
  assert.throws(() => parseConnectPolicy([
    "urn:nanocodex:connector:github",
    chatGptCredentialImport,
  ]), /no ChatGPT connector request/);
});

test("existing-account login targets only credentials retained by this dialog", () => {
  assert.deepEqual(accountLoginCapabilities([
    { credential: { id: "known-passkey" } },
    { credential: { id: "known-passkey" } },
    { credential: { id: "second-passkey" } },
  ]), {
    method: "login",
    credentialId: ["known-passkey", "second-passkey"],
  });
  assert.deepEqual(accountLoginCapabilities([]), {
    method: "login",
  });
});

test("signed agent visibility resources map to compact consent labels", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:visibility:reply,actions,history,traces",
  ]), [
    {
      resource: "urn:nanocodex:agent:output:final",
      label: "Reply",
      detail: "Final agent reply",
    },
    {
      resource: "urn:nanocodex:agent:output:actions",
      label: "Actions",
      detail: "Agent actions and tool calls",
    },
    {
      resource: "urn:nanocodex:agent:history:read",
      label: "History",
      detail: "Conversation history",
    },
    {
      resource: "urn:nanocodex:agent:trace:read",
      label: "Thinking & traces",
      detail: "Reasoning, thinking, and full tool traffic",
    },
  ]);
});

test("unsigned and malformed resources do not produce visibility claims", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:output",
    "urn:nanocodex:agent:trace:write",
    null,
  ]), []);
  assert.deepEqual(appVisibilityPermissions(undefined), []);
});

test("legacy visibility resources remain readable", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:output:final",
    "urn:nanocodex:agent:trace:read",
  ]).map(({ label }) => label), ["Reply", "Thinking & traces"]);
});

test("a signed durable conversation is visible as a separate approval", () => {
  const resource = "urn:nanocodex:agent:conversation:0f5f2ab8-2585-4d7c-9403-0de76f55ad18";
  assert.deepEqual(appVisibilityPermissions([resource]), [{
    resource,
    label: "Conversation",
    detail: "Create and use one new durable conversation",
  }]);
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:agent:conversation:not-a-uuid",
  ]), []);
});

test("hosted history and memory remain separate signed permissions", () => {
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:history:read",
    "urn:nanocodex:memory:read",
    "urn:nanocodex:memory:write",
  ]).map(({ label }) => label), ["Hosted history", "Memory read", "Memory write"]);
});

test("an exact signed browser tool catalog is visible without implying broad tool access", () => {
  const resource = `urn:nanocodex:app-tool-catalog:sha256:${"c".repeat(64)}`;
  assert.deepEqual(appVisibilityPermissions([resource]), [{
    resource,
    label: "Browser tab tool",
    detail: "Use only the exact local browser tool catalog approved here",
  }]);
  assert.deepEqual(appVisibilityPermissions([
    "urn:nanocodex:app-tool-catalog:sha256:not-a-digest",
  ]), []);
});

test("production Connect policy pins the API and registered embedding app", () => {
  assert.equal(connectApiOrigin({
    challenge: `${productionConnectApiOrigin}/v1/connect/auth/challenge`,
    url: `${productionConnectApiOrigin}/v1/connect/auth`,
  }, "https://nanocodex-connect.gakonst.workers.dev"), productionConnectApiOrigin);
  assert.deepEqual(registeredApp(playground, "atlas-workspace", productionDialog, false), {
    id: "atlas-workspace",
    name: "Atlas Workspace",
    origin: playground,
  });
  assert.deepEqual(registeredApp(chromeExtension, "nanocodex-chrome", productionDialog, false), {
    id: "nanocodex-chrome",
    name: "Nanocodex for Chrome",
    origin: chromeExtension,
  });
  assert.deepEqual(registeredApp(cli, "nanocodex-cli", productionDialog, true), {
    id: "nanocodex-cli",
    name: "Nanocodex CLI",
    origin: cli,
  });
});

test("only top-level popup dialogs admit unknown secure app origins", () => {
  assert.throws(() => connectApiOrigin({
    challenge: `${productionConnectApiOrigin}/v1/connect/auth/challenge`,
    verify: `${productionConnectApiOrigin}/v1/connect/auth`,
    logout: "https://attacker.example/collect",
  }, "https://nanocodex-connect.gakonst.workers.dev"), /production Connect API/);
  assert.deepEqual(registeredApp(
    "https://consumer.example",
    "consumer-example",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  ), {
    id: "consumer-example",
    name: "consumer.example",
    origin: "https://consumer.example",
  });
  assert.deepEqual(registeredApp(
    "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
    "consumer-extension",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  ).origin, "chrome-extension://abcdefghijklmnopabcdefghijklmnop");
  assert.throws(() => registeredApp(
    "https://consumer.example",
    "consumer-example",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    false,
  ), /not registered/);
  assert.throws(() => registeredApp(
    "http://attacker.example",
    "attacker",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  ), /not registered/);
  assert.equal(isPopupPresentation("https://dialog.example/?mode=popup", true), true);
  assert.equal(isPopupPresentation("https://dialog.example/?mode=popup", false), false);
  assert.equal(isPopupPresentation("https://dialog.example/?mode=iframe", true), false);
});

test("loopback auth and apps are accepted only by a loopback dialog", () => {
  assert.equal(connectApiOrigin({
    challenge: `${productionConnectApiOrigin}/v1/connect/auth/challenge`,
    verify: `${productionConnectApiOrigin}/v1/connect/auth`,
  }, "http://127.0.0.1:4177"), productionConnectApiOrigin);
  assert.equal(connectApiOrigin({
    challenge: "http://127.0.0.1:8787/v1/connect/auth/challenge",
    verify: "http://127.0.0.1:8787/v1/connect/auth",
  }, "http://127.0.0.1:4177"), "http://127.0.0.1:8787");
  assert.equal(registeredApp(
    "http://localhost:4173",
    "atlas-workspace",
    "http://127.0.0.1:4177/connect-dialog/?mode=iframe",
    false,
  ).id, "atlas-workspace");
  assert.throws(() => connectApiOrigin({ url: "http://127.0.0.1:8787/v1/connect/auth" }, "https://dialog.example"), /production Connect API/);
  assert.throws(() => connectApiOrigin({
    challenge: "http://127.0.0.1:8787/v1/connect/auth/challenge",
    verify: "http://localhost:8787/v1/connect/auth",
  }, "http://127.0.0.1:4177"), /share one development origin/);
});

test("localhost development keeps Connect auth on one exact instance origin", () => {
  assert.equal(connectApiOrigin({
    challenge: "http://passkey-a.nanocodex.localhost:20735/v1/connect/auth/challenge",
    verify: "http://passkey-a.nanocodex.localhost:20735/v1/connect/auth",
  }, "http://passkey-a.nanocodex.localhost:20735"), "http://passkey-a.nanocodex.localhost:20735");
  assert.throws(() => connectApiOrigin({
    challenge: "http://passkey-a.nanocodex.localhost:20735/v1/connect/auth/challenge",
    verify: "http://127.0.0.1:8787/v1/connect/auth",
  }, "http://passkey-a.nanocodex.localhost:20735"), /share one development origin/);
});

test("signed app and origin resources bind the dialog app exactly once", () => {
  const app = registeredApp(
    "https://consumer.example",
    "consumer-example",
    "https://nanocodex.gakonst.workers.dev/connect-dialog/?mode=popup",
    true,
  );
  const resources = [
    "urn:nanocodex:agent:run",
    "urn:nanocodex:app:consumer-example",
    "urn:nanocodex:origin:https%3A%2F%2Fconsumer.example",
  ];
  assert.strictEqual(signedAppResources(resources, app), resources);
  assert.throws(() => signedAppResources([
    "urn:nanocodex:app:other-app",
    "urn:nanocodex:origin:https%3A%2F%2Fconsumer.example",
  ], app), /do not match/);
  assert.throws(() => signedAppResources([
    "urn:nanocodex:app:consumer-example",
    "urn:nanocodex:origin:https%3A%2F%2Fother.example",
  ], app), /do not match/);
  assert.throws(() => signedAppResources([
    ...resources,
    "urn:nanocodex:agent:conversation:not-a-uuid",
  ], app), /durable conversation request is invalid/);
  assert.throws(() => signedAppResources([
    ...resources,
    "urn:nanocodex:agent:conversation:0f5f2ab8-2585-4d7c-9403-0de76f55ad18",
    "urn:nanocodex:agent:conversation:8dd9ec4e-5bd8-46d2-8749-40456742e9e5",
  ], app), /durable conversation request is invalid/);
});

test("wallet result sanitization retains signatures without exposing the account bearer", () => {
  const keyAuthorization = { address: "0xkey", witness: "0xwitness" };
  const personalSign = { keyAuthorization: "0xsigned", message: "0xmessage" };
  const result = sanitizeWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        auth: { approval_id: "approval-1", token: "account-wide-secret", agent_id: "agent-1" },
        keyAuthorization,
        personalSign,
      },
    }],
  });
  assert.deepEqual(result.accounts[0].capabilities.auth, { approval_id: "approval-1" });
  assert.strictEqual(result.accounts[0].capabilities.keyAuthorization, keyAuthorization);
  assert.strictEqual(result.accounts[0].capabilities.personalSign, personalSign);
  assert.equal(JSON.stringify(result).includes("account-wide-secret"), false);
});

test("CLI wallet result retains only grant bootstrap material", () => {
  const result = sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      label: "private label",
      capabilities: {
        auth: { approval_id: "a".repeat(43), token: "account-wide-secret" },
        keyAuthorization: { keyId: "0x0000000000000000000000000000000000000002" },
        personalSign: { keyAuthorization: "0x1234", message: "signed message" },
        identity: { idToken: "identity-secret" },
      },
    }],
  });
  assert.deepEqual(result, {
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        keyAuthorization: { keyId: "0x0000000000000000000000000000000000000002" },
        personalSign: { keyAuthorization: "0x1234" },
        auth: { approval_id: "a".repeat(43) },
      },
    }],
  });
  assert.equal(JSON.stringify(result).includes("secret"), false);
});

test("CLI hosted wallet result retains only its one-use approval", () => {
  assert.deepEqual(sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: { auth: { approval_id: "h".repeat(43), mode: "hosted" } },
    }],
  }), {
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: { auth: { approval_id: "h".repeat(43), mode: "hosted" } },
    }],
  });
  assert.throws(() => sanitizeCliWalletResult({
    accounts: [{
      address: "0x0000000000000000000000000000000000000001",
      capabilities: {
        auth: { approval_id: "h".repeat(43), mode: "hosted" },
        keyAuthorization: {},
      },
    }],
  }), /hosted CLI approval/);
});

test("the reviewed passkey approval becomes ready only after requested connectors are connected", () => {
  const ready = {
    github: { connected: true },
    gmail: { connected: true },
  };
  assert.equal(connectorApprovalDisposition(["github", "gmail"], ready), "respond");
  assert.equal(connectorApprovalDisposition([], ready), "respond");
  assert.equal(connectorApprovalDisposition(["github", "x"], ready), "wait");
  assert.equal(connectorApprovalDisposition(["github"], undefined), "wait");
});

test("ChatGPT connector responses distinguish an immediate local claim from device login", () => {
  assert.equal(chatGptConnectorDisposition({
    state: "authenticated",
    connected: true,
    account_id: "account",
  }), "connected");
  assert.equal(chatGptConnectorDisposition({
    state: "pending",
    verification_url: "https://auth.openai.com/codex/device",
    user_code: "ABCD-EFGH",
    expires_at: Date.now() + 60_000,
  }), "device");
  assert.equal(chatGptConnectorDisposition({ state: "authenticated" }), "invalid");
  assert.equal(chatGptConnectorDisposition({ state: "pending", connected: true }), "invalid");
});

test("connector focus is singular, known, and included in the signed connector grant", () => {
  assert.equal(focusedConnectorFromResources([
    "urn:nanocodex:connectors:chatgpt,github",
    "urn:nanocodex:connector-focus:github",
  ], ["chatgpt", "github"]), "github");
  assert.equal(focusedConnectorFromResources([
    "urn:nanocodex:connectors:github",
  ], ["github"]), undefined);
  assert.equal(focusedConnectorFromResources([
    "urn:nanocodex:connectors:github,gmail",
  ], ["github", "gmail"]), undefined);
  assert.throws(() => focusedConnectorFromResources([
    "urn:nanocodex:connector-focus:gmail",
  ], ["github"]), /focus is invalid/);
  assert.throws(() => focusedConnectorFromResources([
    "urn:nanocodex:connector-focus:github",
    "urn:nanocodex:connector-focus:gmail",
  ], ["github", "gmail"]), /focus is invalid/);
  assert.throws(() => focusedConnectorFromResources([
    "urn:nanocodex:connector-focus:unknown",
  ], ["unknown"]), /focus is invalid/);
});

test("generic MCP metadata is bounded, secret-free, and separate from connector IDs", () => {
  const connections = mcpConnectionsFromWire([
    { id: LINEAR_MCP, name: "Linear", status: "authorization_required" },
    { id: CLOUDFLARE_MCP, name: "Cloudflare", status: "connected" },
  ]);
  assert.deepEqual(connections, [
    { id: LINEAR_MCP, name: "Linear", status: "authorization_required" },
    { id: CLOUDFLARE_MCP, name: "Cloudflare", status: "connected" },
  ]);
  assert.equal(focusedMcpConnection(LINEAR_MCP, connections), LINEAR_MCP);
  assert.equal(focusedMcpConnection(undefined, connections), undefined);
  for (const unsafe of [
    [{ id: LINEAR_MCP, name: "Linear", status: "connected", endpoint: "https://mcp.linear.app/mcp" }],
    [{ id: LINEAR_MCP, name: "Linear", status: "connected", token: "secret" }],
    [{ id: LINEAR_MCP, name: "Linear", status: "unknown" }],
    [{ id: "linear", name: "Linear", status: "connected" }],
  ]) assert.throws(() => mcpConnectionsFromWire(unsafe), /invalid MCP connections/);
  assert.throws(() => focusedMcpConnection("x".repeat(43), connections), /focused MCP/);
});

test("all requested generic MCP connections must be connected before device settlement", () => {
  const requested = [
    { id: LINEAR_MCP, name: "Linear", status: "authorization_required" },
    { id: CLOUDFLARE_MCP, name: "Cloudflare", status: "authorization_required" },
  ];
  assert.equal(mcpConnectionApprovalDisposition(requested, [
    { ...requested[0], status: "connected" },
    { ...requested[1], status: "connected" },
  ]), "respond");
  assert.equal(mcpConnectionApprovalDisposition(requested, [
    { ...requested[0], status: "connected" },
    requested[1],
  ]), "wait");
  assert.equal(mcpConnectionApprovalDisposition(requested, []), "wait");
});

test("device MCP callbacks preserve only the signed verification route", () => {
  assert.equal(deviceMcpReturnPath(
    "http://demo.nanocodex.localhost:20735/connect?api_origin=http%3A%2F%2Fdemo.nanocodex.localhost%3A20735&user_code=BRCTKLDT&thread=browser-only&mcp_result=retry",
  ), "/connect?user_code=BRCTKLDT&api_origin=http%3A%2F%2Fdemo.nanocodex.localhost%3A20735");
});

test("device callback continuation is short-lived, exact, account-bound, and secret-field free", () => {
  const now = 1_800_000_000_000;
  const input = {
    requestId: "device:rpc:ABCD1234",
    apiUrl: "http://demo.nanocodex.localhost:20735",
    accountAddress: "0x1111111111111111111111111111111111111111",
    token: "connect-session-token",
    requestedConnectors: ["github", "gmail"],
    requestedMcpConnections: [
      { id: LINEAR_MCP, name: "Linear", status: "authorization_required" },
    ],
    connectorStatuses: {
      github: { connected: true, label: "octocat" },
      gmail: { connected: true, connections: [
        { id: "g".repeat(43), account_id: "google-account", label: "mail@example.test" },
      ] },
    },
    result: {
      accounts: [{
        address: "0x1111111111111111111111111111111111111111",
        capabilities: { auth: { approval_id: "a".repeat(43), mode: "hosted" } },
      }],
    },
  };
  const retained = createMcpCallbackContinuation(input, now);
  assert.equal(retained.expiresAt, now + 10 * 60 * 1000);
  assert.equal(JSON.stringify(retained).includes("authorization_url"), false);
  assert.deepEqual(restoreMcpCallbackContinuation(JSON.parse(JSON.stringify(retained)), {
    requestId: input.requestId,
    apiUrl: input.apiUrl,
    returnedMcpConnection: LINEAR_MCP,
    requestedConnectors: input.requestedConnectors,
    requestedMcpConnections: [
      { id: LINEAR_MCP, name: "Linear", status: "connected" },
    ],
  }, now + 1_000), retained);
  assert.deepEqual(restoreMcpCallbackContinuation(JSON.parse(JSON.stringify(retained)), {
    requestId: input.requestId,
    apiUrl: input.apiUrl,
    returnedConnector: "github",
    requestedConnectors: input.requestedConnectors,
    requestedMcpConnections: input.requestedMcpConnections,
  }, now + 1_000), retained);
  assert.throws(() => restoreMcpCallbackContinuation(retained, {
    requestId: "device:other:ABCD1234",
    apiUrl: input.apiUrl,
    returnedMcpConnection: LINEAR_MCP,
    requestedConnectors: input.requestedConnectors,
    requestedMcpConnections: input.requestedMcpConnections,
  }, now), /invalid or expired/);
  assert.throws(() => restoreMcpCallbackContinuation(retained, {
    requestId: input.requestId,
    apiUrl: input.apiUrl,
    returnedMcpConnection: LINEAR_MCP,
    requestedConnectors: input.requestedConnectors,
    requestedMcpConnections: input.requestedMcpConnections,
  }, now + 10 * 60 * 1000 + 1), /invalid or expired/);
  assert.throws(() => restoreMcpCallbackContinuation({
    ...retained,
    accountAddress: "0x2222222222222222222222222222222222222222",
  }, {
    requestId: input.requestId,
    apiUrl: input.apiUrl,
    returnedConnector: "github",
    requestedConnectors: input.requestedConnectors,
    requestedMcpConnections: input.requestedMcpConnections,
  }, now), /does not match/);
});
