"use client";

import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useSyncExternalStore,
} from "react";
import { useMutation } from "@tanstack/react-query";

export { createConnectAgentSource } from "./connectAgentSource.mjs";

const NanocodexConnectContext = createContext(null);
const DISCONNECTED = connectionSnapshot("disconnected");

/** Creates a caller-owned React config around one decorated Connect client. */
export function createConfig(parameters) {
  if (!parameters || typeof parameters !== "object") {
    throw new TypeError("createConfig requires parameters");
  }
  const client = parameters.client;
  if (!client || typeof client !== "object") {
    throw new TypeError("createConfig requires a client");
  }

  const listeners = new Set();
  let state = client._hasSession?.()
    ? connectionSnapshot("connecting")
    : DISCONNECTED;
  let reconnecting;

  function setState(nextState) {
    if (Object.is(state, nextState)) return;
    state = nextState;
    for (const listener of [...listeners]) listener();
  }

  return Object.freeze({
    client,
    getState() {
      return state;
    },
    subscribe(listener) {
      if (typeof listener !== "function") {
        throw new TypeError("config subscription requires a listener");
      }
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    async _reconnectAgent(agentOptions, reconnectOptions) {
      if (state.status === "connected") {
        return Object.freeze({ connection: state.connection, agent: state.agent });
      }
      if (reconnecting) return reconnecting;
      if (!client._hasSession?.()) {
        setState(DISCONNECTED);
        return undefined;
      }
      if (state.status !== "connecting") setState(connectionSnapshot("connecting"));
      reconnecting = (async () => {
        let agent;
        try {
          const resumed = client._resumeConnection?.(reconnectOptions);
          const refreshing = client.connection.reconnect(reconnectOptions);
          void refreshing.catch(() => {});
          if (resumed) {
            try {
              agent = await client.agent.create({ ...agentOptions, connection: resumed });
              setState(connectionSnapshot("connected", resumed, agent));
            } catch {
              agent = undefined;
            }
          }
          const connection = await refreshing;
          if (!connection) {
            await agent?.session.shutdown().catch(() => {});
            setState(DISCONNECTED);
            return undefined;
          }
          if (!agent
            || resumed.agentId !== connection.agentId
            || resumed.grant.id.toLowerCase() !== connection.grant.id.toLowerCase()) {
            await agent?.session.shutdown().catch(() => {});
            agent = await client.agent.create({ ...agentOptions, connection });
          }
          setState(connectionSnapshot("connected", connection, agent));
          refreshMppBalance(client, connection, (refreshed) => {
            if (state.status === "connected"
              && state.connection.grant.id.toLowerCase() === refreshed.grant.id.toLowerCase()) {
              setState(connectionSnapshot("connected", refreshed, state.agent));
            }
          });
          return Object.freeze({ connection, agent });
        } catch (error) {
          await agent?.session.shutdown().catch(() => {});
          setState(DISCONNECTED);
          throw error;
        }
      })().finally(() => { reconnecting = undefined; });
      return reconnecting;
    },
    _setConnection(status, connection, agent) {
      setState(connectionSnapshot(status, connection, agent));
    },
  });
}

/** Supplies one already-created Nanocodex Connect config to Connect hooks. */
export function NanocodexProvider({ children, config }) {
  if (!config) throw new TypeError("NanocodexProvider requires a config");
  return createElement(NanocodexConnectContext.Provider, { value: config }, children);
}

/** Resolves an explicit config first and otherwise uses the nearest provider. */
export function useConfig(parameters = {}) {
  const context = useContext(NanocodexConnectContext);
  const config = parameters.config ?? context;
  if (!config) {
    throw new Error("Nanocodex Connect hooks must be used inside NanocodexProvider");
  }
  return config;
}

/** Returns the current discriminated Nanocodex Connect connection snapshot. */
export function useConnection(parameters = {}) {
  const config = useConfig(parameters);
  const subscribe = useCallback(
    (listener) => config.subscribe(listener),
    [config],
  );
  const getSnapshot = useCallback(() => config.getState(), [config]);
  return useSyncExternalStore(subscribe, getSnapshot, () => DISCONNECTED);
}

/** Returns the capability-bound agent injected by the active Connect grant. */
export function useAgent(parameters = {}) {
  return useConnection(parameters).agent;
}

/** Connects the configured Nanocodex Connect client. */
export function useConnect(parameters = {}) {
  const config = useConfig(parameters);
  const snapshot = useConnection({ config });
  useCloseCommittedDialog(config, snapshot);
  return useMutation({
    ...parameters.mutation,
    mutationKey: ["nanocodex", "connect"],
    mutationFn: async (variables) => {
      config._setConnection("connecting");
      try {
        const connection = await config.client.connection.connect(manualDialogClose(variables));
        config._setConnection("connected", connection);
        refreshConfigMppBalance(config, connection);
        return connection;
      } catch (error) {
        config._setConnection("disconnected");
        throw error;
      }
    },
  });
}

/** Connects once, then instantiates the real app-owned Nanocodex agent with the user's ChatGPT account. */
export function useConnectAgent(parameters = {}) {
  const config = useConfig(parameters);
  const snapshot = useConnection({ config });
  useCloseCommittedDialog(config, snapshot);
  useEffect(() => {
    if (parameters.reconnectOnMount === false) return;
    void config._reconnectAgent(parameters.agent, parameters.reconnect).catch((error) => {
      console.error("Nanocodex Connect session restore failed", error);
    });
  }, [config, parameters.agent, parameters.reconnect, parameters.reconnectOnMount]);
  const mutation = useMutation({
    ...parameters.mutation,
    mutationKey: ["nanocodex", "connectAgent"],
    mutationFn: async (variables) => {
      config._setConnection("connecting");
      let agent;
      try {
        const connection = await config.client.connection.connect(manualDialogClose(variables));
        agent = await config.client.agent.create({
          ...parameters.agent,
          connection,
        });
        config._setConnection("connected", connection, agent);
        refreshConfigMppBalance(config, connection);
        return Object.freeze({ agent, connection });
      } catch (error) {
        await agent?.session.shutdown().catch(() => {});
        config._setConnection("disconnected");
        throw error;
      }
    },
  });
  return Object.assign(mutation, {
    agent: snapshot.agent,
    connection: snapshot.connection,
    connectionStatus: snapshot.status,
    connect: mutation.mutate,
    connectAsync: mutation.mutateAsync,
  });
}

/** Funds the current connection with MACH through the decorated client. */
export function useFund(parameters = {}) {
  const config = useConfig(parameters);
  return useConnectMutation(
    config,
    "fund",
    (variables) => config.client.machineUsd.fund(variables),
    parameters.mutation,
  );
}

/** Charges the current connection through its MPP grant. */
export function useCharge(parameters = {}) {
  const config = useConfig(parameters);
  return useConnectMutation(
    config,
    "charge",
    (variables) => config.client.mpp.charge(variables),
    parameters.mutation,
  );
}

/** Signs the Nanocodex account out without revoking its app grant or access key. */
export function useLogoutAccount(parameters = {}) {
  const config = useConfig(parameters);
  return useMutation({
    ...parameters.mutation,
    mutationKey: ["nanocodex", "logoutAccount"],
    mutationFn: async () => {
      const agent = config.getState().agent;
      config._setConnection("disconnected");
      const settled = await Promise.allSettled([
        agent?.session.shutdown(),
        config.client.account.logout(),
      ]);
      const failure = settled.find((result) => result.status === "rejected");
      if (failure?.status === "rejected") throw failure.reason;
    },
  });
}

/** Revokes the current connection's grant. */
export function useRevokeGrant(parameters = {}) {
  const config = useConfig(parameters);
  return useMutation({
    ...parameters.mutation,
    mutationKey: ["nanocodex", "revokeGrant"],
    mutationFn: async (variables) => {
      const connection = config.getState().connection;
      if (!connection) throw new Error("No active Nanocodex Connect grant to revoke");
      const agent = config.getState().agent;
      if (agent) {
        await agent.session.shutdown();
        config._setConnection("connected", connection);
      }
      config.client.dialog.showWallet?.();
      try {
        await config.client.provider.request({
          method: "wallet_revokeAccessKey",
          params: [{
            address: connection.accountAddress,
            accessKeyAddress: connection.accessKey.keyId,
          }],
        });
      } finally {
        config.client.dialog.hideWallet?.();
      }
      const result = await config.client.grant.revoke(variables);
      config._setConnection("disconnected");
      return result;
    },
  });
}

/**
 * Renders requests produced by Dialog.memory and resolves them through the
 * dialog instance. The component renders nothing while there is no request.
 */
export function NanocodexDialog({ dialog }) {
  if (!dialog || typeof dialog !== "object") {
    throw new TypeError("NanocodexDialog requires a dialog");
  }
  if (typeof dialog.getRequest !== "function" || typeof dialog.subscribe !== "function") {
    throw new TypeError("NanocodexDialog requires a memory dialog instance");
  }
  const subscribe = useCallback((listener) => dialog.subscribe(listener), [dialog]);
  const getSnapshot = useCallback(() => dialog.getRequest(), [dialog]);
  const request = useSyncExternalStore(subscribe, getSnapshot, () => undefined);
  if (!request) return null;

  const id = `nanocodex-connect-${request.id}`;
  if (request.type === "connect") {
    return createElement(
      "section",
      { "aria-labelledby": id, role: "dialog" },
      createElement("h2", { id }, "Nanocodex Connect"),
      createElement(
        "p",
        null,
        `${request.app.name} wants to ${request.permission.title.toLowerCase()}.`,
      ),
      createElement("p", null, request.permission.description),
      createElement(
        "p",
        null,
        "One passkey approval signs a TIP-1053 witness-bound access-key authorization. The witness commits to the exact SIWE message and its Resources.",
      ),
      createElement(
        "p",
        null,
        `MPP permission: ${request.mpp.limit.toString()} ${request.mpp.symbol} atomics every ${request.mpp.period} seconds, up to ${request.mpp.maxPerRequest.toString()} atomics per request.`,
      ),
      request.permission.connectors.length === 0
        ? null
        : createElement(
          "ul",
          null,
          ...request.permission.connectors.map((connector) => createElement(
            "li",
            { key: connector.id },
            createElement("strong", null, connector.name),
            ` — ${connector.detail}`,
          )),
        ),
      request.auth.resources.length === 0
        ? null
        : createElement(
          "ul",
          { "aria-label": "SIWE resources" },
          ...request.auth.resources.map((resource) => createElement(
            "li",
            { key: resource },
            resource,
          )),
        ),
      dialogActions(dialog, "Approve with passkey", connectApproval(request)),
    );
  }

  if (request.type === "machineUsdFund") {
    const dollars = (request.usdAmountCents / 100).toLocaleString(undefined, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    });
    return createElement(
      "section",
      { "aria-labelledby": id, role: "dialog" },
      createElement("h2", { id }, "Nanocodex Connect"),
      createElement("p", null, `Add $${dollars} of MACH to this connection?`),
      dialogActions(dialog, "Add MACH", { approved: true }),
    );
  }

  return null;
}

function useConnectMutation(config, action, mutationFn, mutation) {
  return useMutation({
    ...mutation,
    mutationKey: ["nanocodex", action],
    mutationFn: async (variables) => {
      const result = await mutationFn(variables);
      if (result?.connection) {
        config._setConnection("connected", result.connection, config.getState().agent);
        refreshConfigMppBalance(config, result.connection);
      }
      return result;
    },
  });
}

function refreshConfigMppBalance(config, connection) {
  refreshMppBalance(config.client, connection, (refreshed) => {
    const current = config.getState();
    if (current.status === "connected"
      && current.connection.grant.id.toLowerCase() === refreshed.grant.id.toLowerCase()) {
      config._setConnection("connected", refreshed, current.agent);
    }
  });
}

function useCloseCommittedDialog(config, snapshot) {
  useLayoutEffect(() => {
    if (snapshot.status === "connected") config.client.dialog?.hideWallet?.();
  }, [config, snapshot.status, snapshot.connection?.grant.id]);
}

function manualDialogClose(options) {
  return {
    ...options,
    dialog: { ...options?.dialog, close: "manual" },
  };
}

function refreshMppBalance(client, connection, publish) {
  if (connection.mpp.balanceStatus === "ready") return;
  void client.mpp.getBalance({ grantId: connection.grant.id }).then(publish).catch((error) => {
    console.error("Nanocodex Connect balance refresh failed", error);
  });
}

function connectionSnapshot(status, connection, agent) {
  return Object.freeze({
    agent,
    connection,
    status,
    isConnected: status === "connected",
    isConnecting: status === "connecting",
    isDisconnected: status === "disconnected",
  });
}

function dialogActions(dialog, confirmLabel, result) {
  return createElement(
    "div",
    null,
    createElement("button", {
      type: "button",
      onClick: () => dialog.reject(),
    }, "Not now"),
    createElement("button", {
      type: "button",
      onClick: () => dialog.respond(result),
    }, confirmLabel),
  );
}

function connectApproval(request) {
  const signature = playgroundHex(`${request.accessKey.witness}:${request.accessKey.keyId}`, 65);
  const serialized = playgroundHex(`${request.auth.message}:${signature}`, 160);
  return Object.freeze({
    approved: true,
    address: request.accountAddress,
    capabilities: Object.freeze({
      auth: Object.freeze({}),
      keyAuthorization: Object.freeze({
        address: request.accessKey.address,
        chainId: `0x${request.accessKey.chainId.toString(16)}`,
        expiry: request.accessKey.expiry,
        keyId: request.accessKey.keyId,
        keyType: "p256",
        limits: Object.freeze([Object.freeze({
          limit: `0x${request.mpp.limit.toString(16)}`,
          period: request.mpp.period,
          token: request.mpp.token,
        })]),
        publicKey: request.accessKey.publicKey,
        signature,
        witness: request.accessKey.witness,
      }),
      personalSign: Object.freeze({
        keyAuthorization: serialized,
        message: request.auth.message,
      }),
      signature,
    }),
  });
}

function playgroundHex(seed, bytes) {
  let state = 2166136261;
  let output = "";
  for (let index = 0; output.length < bytes * 2; index += 1) {
    state ^= seed.charCodeAt(index % seed.length) + index;
    state = Math.imul(state, 16777619) >>> 0;
    output += state.toString(16).padStart(8, "0");
  }
  return `0x${output.slice(0, bytes * 2)}`;
}
