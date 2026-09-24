import assert from "node:assert/strict";
import test from "node:test";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createElement } from "react";
import { act, create } from "react-test-renderer";

import { createConfig, useConnectAgent, useLogoutAccount } from "../cloud/index.mjs";

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

async function waitFor(predicate) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (predicate()) return;
    await act(async () => new Promise((resolve) => setTimeout(resolve, 0)));
  }
  throw new Error("condition was not met");
}
