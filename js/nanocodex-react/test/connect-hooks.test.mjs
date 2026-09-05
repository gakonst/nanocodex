import assert from "node:assert/strict";
import test from "node:test";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createElement } from "react";
import { act, create } from "react-test-renderer";
import { Client, Dialog, Transport } from "nanocodex/connect";

import { createConfig, useConnect, useConnectAgent, useLogoutAccount } from "../cloud/index.mjs";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("useConnectAgent reopens one persisted durable grant session on mount", async () => {
  const connection = Object.freeze({
    grant: Object.freeze({
      id: "0x01",
      permission: "agent.run",
      capabilities: Object.freeze(["agent.output.final", "agent.output.actions"]),
      connectors: Object.freeze([]),
      visibility: Object.freeze({
        finalMessages: true,
        actionSummaries: true,
        conversationHistory: false,
        rawTraces: false,
      }),
    }),
    mpp: Object.freeze({ balanceStatus: "ready" }),
  });
  const agent = Object.freeze({ id: "agent-durable" });
  let reconnects = 0;
  let reconnectOptions;
  let creates = 0;
  let notifications = 0;
  const config = createConfig({
    client: {
      _hasSession() { return true; },
      connection: {
        async reconnect(options) {
          reconnects += 1;
          reconnectOptions = options;
          return connection;
        },
      },
      agent: {
        async create(options) {
          creates += 1;
          assert.equal(options.connection, connection);
          return agent;
        },
      },
    },
  });
  const unsubscribe = config.subscribe(() => { notifications += 1; });
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  let snapshot;

  function Consumer() {
    snapshot = useConnectAgent({
      config,
      reconnect: {
        capabilities: { agent: { finalMessages: true } },
        permission: "agent.run",
      },
    });
    return null;
  }

  let root;
  await act(async () => {
    root = create(createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(Consumer),
    ));
  });
  await waitFor(() => snapshot.connectionStatus === "connected");

  assert.equal(reconnects, 1);
  assert.deepEqual(reconnectOptions, {
    capabilities: { agent: { finalMessages: true } },
    permission: "agent.run",
  });
  assert.equal(creates, 1);
  assert.equal(notifications, 1);
  assert.equal(snapshot.connection, connection);
  assert.equal(snapshot.agent, agent);
  await act(async () => root.unmount());
  unsubscribe();
  queryClient.clear();
});

test("useConnectAgent validates a retained agent while refreshing its grant projection", async () => {
  const cached = Object.freeze({
    agentId: "agent-durable",
    grant: Object.freeze({ id: "0x01" }),
    mpp: Object.freeze({ balanceStatus: "ready" }),
  });
  const fresh = Object.freeze({
    agentId: "agent-durable",
    grant: Object.freeze({ id: "0x01" }),
    mpp: Object.freeze({ balanceStatus: "ready" }),
  });
  const agent = Object.freeze({ id: "agent-durable" });
  let resolveRefresh;
  const refresh = new Promise((resolve) => { resolveRefresh = resolve; });
  let creates = 0;
  const config = createConfig({
    client: {
      _hasSession() { return true; },
      _resumeConnection() { return cached; },
      connection: { reconnect() { return refresh; } },
      agent: {
        async create(options) {
          creates += 1;
          assert.equal(options.connection, cached);
          return agent;
        },
      },
    },
  });
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  let snapshot;

  function Consumer() {
    snapshot = useConnectAgent({ config });
    return null;
  }

  let root;
  await act(async () => {
    root = create(createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(Consumer),
    ));
  });
  await waitFor(() => snapshot.connection === cached);
  assert.equal(creates, 1);

  await act(async () => resolveRefresh(fresh));
  await waitFor(() => snapshot.connection === fresh);
  assert.equal(snapshot.agent, agent);
  assert.equal(creates, 1);

  await act(async () => root.unmount());
  queryClient.clear();
});

test("useConnect signs exact pre-registered MCP IDs without forwarding host secrets", async () => {
  const mcpId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const walletRequests = [];
  const apiRequests = [];
  const client = Client.create({
    appId: "atlas-workspace",
    appOrigin: "https://consumer.example",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        walletRequests.push(request);
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: {
              auth: { approval_id: "approval-test" },
              keyAuthorization: {
                address: keyId,
                keyId,
                keyType: "p256",
                chainId: 4217n,
                expiry,
                witness: `0x${"22".repeat(32)}`,
              },
              personalSign: { keyAuthorization: "0x1234" },
            },
          }],
        };
      },
    },
    session: false,
    transport: Transport.from({
      key: "capture",
      name: "capture",
      type: "capture",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch() { return Response.json({ ok: true }); },
          async request(request) {
            apiRequests.push(request);
            return testConnectionWire({
              expiry,
              keyId,
              capabilities: [
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "chatgpt",
                `mcp:${mcpId}`,
              ],
              mcpConnections: [{ id: mcpId, name: "Linear workspace" }],
            });
          },
        };
      },
    }),
  });
  const config = createConfig({ client });
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  let connect;

  function Consumer() {
    connect = useConnect({ config });
    return null;
  }

  let root;
  await act(async () => {
    root = create(createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(Consumer),
    ));
  });
  await act(async () => connect.mutateAsync({
    capabilities: { cloudAccounts: { chatgpt: true } },
    focusMcpConnectionId: mcpId,
    mcpConnections: [{
      id: mcpId,
      name: "Linear workspace",
      endpoint: "https://mcp.linear.app/mcp",
      token: "provider-secret",
    }],
    permission: "agent.run",
  }));

  assert.deepEqual(walletRequests[0].context, {
    focusMcpConnection: mcpId,
    requestedMcpConnections: [{
      id: mcpId,
      name: "Linear workspace",
      status: "authorization_required",
    }],
  });
  const resources = walletRequests[0].params[0].capabilities.auth.resources;
  assert.equal(resources.includes(`urn:nanocodex:mcp:${mcpId}`), true);
  assert.equal(resources.includes(`urn:nanocodex:mcp-focus:${mcpId}`), true);
  assert.deepEqual(apiRequests[0].body.requested_mcp_connections, [mcpId]);
  const captured = JSON.stringify(
    { walletRequests, apiRequests },
    (_key, value) => typeof value === "bigint" ? value.toString() : value,
  );
  assert.equal(captured.includes("provider-secret"), false);
  assert.equal(captured.includes("mcp.linear.app"), false);
  let invalidIdError;
  await act(async () => {
    try {
      await connect.mutateAsync({
        mcpConnections: [{ id: `${mcpId}x`, name: "Substituted MCP" }],
      });
    } catch (error) {
      invalidIdError = error;
    }
  });
  assert.match(invalidIdError.message, /opaque 43-character IDs/);
  assert.equal(walletRequests.length, 1);
  await act(async () => root.unmount());
  queryClient.clear();
});

test("useConnectAgent closes the manual dialog after the connected tree commits", async () => {
  const events = [];
  const connection = Object.freeze({
    grant: Object.freeze({ id: "0x01" }),
    mpp: Object.freeze({ balanceStatus: "ready" }),
  });
  const agent = Object.freeze({ id: "agent-durable" });
  let snapshot;
  const config = createConfig({
    client: {
      _hasSession() { return false; },
      connection: {
        async connect(options) {
          assert.equal(options.dialog.close, "manual");
          events.push("connect");
          return connection;
        },
      },
      agent: {
        async create() {
          events.push("agent");
          return agent;
        },
      },
      dialog: {
        hideWallet() { events.push("hide"); },
      },
    },
  });
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });

  function Consumer() {
    snapshot = useConnectAgent({ config, reconnectOnMount: false });
    events.push(`render:${snapshot.connectionStatus}`);
    return null;
  }

  let root;
  await act(async () => {
    root = create(createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(Consumer),
    ));
  });
  await act(async () => snapshot.connectAsync({}));

  assert.equal(snapshot.connectionStatus, "connected");
  assert.ok(events.indexOf("render:connected") < events.indexOf("hide"));
  await act(async () => root.unmount());
  queryClient.clear();
});

test("useLogoutAccount shuts down the durable agent and clears the connected snapshot", async () => {
  const connection = Object.freeze({ grant: Object.freeze({ id: "0x01" }) });
  const calls = [];
  const agent = Object.freeze({
    session: Object.freeze({ async shutdown() { calls.push("shutdown"); } }),
  });
  const config = createConfig({
    client: {
      _hasSession() { return false; },
      account: {
        async logout() { calls.push("logout"); },
      },
    },
  });
  config._setConnection("connected", connection, agent);
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  let disconnect;

  function Consumer() {
    disconnect = useLogoutAccount({ config });
    return null;
  }

  let root;
  await act(async () => {
    root = create(createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(Consumer),
    ));
  });
  await act(async () => disconnect.mutateAsync());

  assert.deepEqual(calls, ["shutdown", "logout"]);
  assert.equal(config.getState().status, "disconnected");
  await act(async () => root.unmount());
  queryClient.clear();
});

test("useLogoutAccount publishes disconnected before remote cleanup settles", async () => {
  let release;
  let started;
  const remoteStarted = new Promise((resolve) => { started = resolve; });
  const remoteCleanup = new Promise((resolve) => { release = resolve; });
  const config = createConfig({
    client: {
      _hasSession() { return false; },
      account: {
        async logout() {
          started();
          await remoteCleanup;
        },
      },
    },
  });
  config._setConnection("connected", Object.freeze({ grant: Object.freeze({ id: "0x01" }) }));
  const queryClient = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  let disconnect;

  function Consumer() {
    disconnect = useLogoutAccount({ config });
    return null;
  }

  let root;
  await act(async () => {
    root = create(createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(Consumer),
    ));
  });
  let logout;
  await act(async () => { logout = disconnect.mutateAsync(); });
  await remoteStarted;
  assert.equal(config.getState().status, "disconnected");
  release();
  await act(async () => logout);
  await act(async () => root.unmount());
  queryClient.clear();
});

function testConnectionWire({ expiry, keyId, capabilities, mcpConnections = [] }) {
  return {
    grant_token: "grant-session-test",
    account_address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
    agent_id: "agent-connect-react",
    grant: {
      id: `0x${"33".repeat(32)}`,
      permission: "agent.run",
      status: "active",
      expires_at: expiry,
      capabilities,
      mcp_connections: mcpConnections,
    },
    access_key: {
      address: keyId,
      chain_id: "4217",
      key_id: keyId,
      key_type: "p256",
      limits: [],
      scopes: [],
      witness: `0x${"22".repeat(32)}`,
      expiry,
      authorization: "0x1234",
    },
    mpp: {
      token: "0x20c0000000000000000000000000000000000001",
      symbol: "MACH",
      balance_status: "ready",
      settlement_token: "0x20C000000000000000000000b9537d11c60E8b50",
      settlement_symbol: "USDC.e",
      settlement_balance_atomics: "0",
      limit_atomics: "10000000",
      max_per_request_atomics: "250000",
      period: 86_400,
      balance_atomics: "0",
      spent_atomics: "0",
    },
  };
}

async function waitFor(predicate) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (predicate()) return;
    await act(async () => new Promise((resolve) => setTimeout(resolve, 0)));
  }
  throw new Error("condition was not met");
}
