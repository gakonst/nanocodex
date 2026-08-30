import {
  connectionRequestFromGrant,
  connectionFromWire,
  connectionMatchesRequest,
  reconnectRequestFromConnection,
} from "../internal.mjs";
import { InvalidResponseError } from "../Errors.mjs";

const CLOUD_ACCOUNT_PROVIDERS = Object.freeze(["github", "gmail", "gdrive", "x", "whoop", "chatgpt"]);
const CONNECTOR_RESOURCE_PREFIX = "urn:nanocodex:connector:";
const CONNECTORS_RESOURCE_PREFIX = "urn:nanocodex:connectors:";
const APP_RESOURCE_PREFIX = "urn:nanocodex:app:";
const APP_ORIGIN_RESOURCE_PREFIX = "urn:nanocodex:origin:";
const MCP_CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const MCP_CONNECTION_RESOURCE_PREFIX = "urn:nanocodex:mcp:";
const MCP_FOCUS_RESOURCE_PREFIX = "urn:nanocodex:mcp-focus:";
const AGENT_VISIBILITY_RESOURCES = Object.freeze({
  finalMessages: "urn:nanocodex:agent:output:final",
  actionSummaries: "urn:nanocodex:agent:output:actions",
  conversationHistory: "urn:nanocodex:agent:history:read",
  rawTraces: "urn:nanocodex:agent:trace:read",
});
const AGENT_VISIBILITY_RESOURCE_PREFIX = "urn:nanocodex:agent:visibility:";
const AGENT_VISIBILITY_NAMES = Object.freeze({
  finalMessages: "reply",
  actionSummaries: "actions",
  conversationHistory: "history",
  rawTraces: "traces",
});

export async function connect(client, options) {
  options ??= {};
  const permission = options.permission ?? "agent.run";
  if (typeof permission !== "string" || permission.length === 0) throw new TypeError("connect permission must be a non-empty string");
  const requestedConnectors = normalizeCloudAccounts(options.capabilities?.cloudAccounts);
  const agentVisibility = normalizeAgentVisibility(options.capabilities?.agent);
  const mcpConnections = normalizeMcpConnections(options.mcpConnections ?? []);
  const focusMcpConnectionId = normalizeMcpFocus(options.focusMcpConnectionId, mcpConnections);
  const exactRequest = connectionRequestFromGrant({
    connectors: requestedConnectors,
    mcpConnections,
    permission,
    visibility: agentVisibility,
  });
  const auth = withConnectionResources(
    options.capabilities?.auth ?? client.auth,
    client.appId,
    client.appOrigin,
    requestedConnectors,
    agentVisibility,
    mcpConnections,
    focusMcpConnectionId,
  );
  const walletAuth = delegateAuthVerification(auth);
  client.dialog.showWallet?.();
  let connected = false;
  try {
    await client.dialog.waitForWallet?.();
    const activeAccount = activeAccountAddress(client.provider);
    const reusable = activeAccount
      ? await registeredAccessKey(client, activeAccount, options.signal)
      : undefined;
    // Reuse only keys already registered with the Connect control plane. Older
    // browser-only keys are replaced in this same passkey ceremony, after which
    // both the private signer and public grant record remain durable.
    const authorizeAccessKey = options.capabilities?.authorizeAccessKey
      ?? (reusable
        ? undefined
        : freshAccessKeyAuthorization(client.accessKey?.authorize));
    const result = await client.provider.request({
      method: "wallet_connect",
      params: [{
        chainId: "0x1079",
        capabilities: {
          ...(walletAuth ? { auth: walletAuth } : {}),
          ...(authorizeAccessKey ? { authorizeAccessKey: serializeAuthorizeAccessKey(authorizeAccessKey) } : {}),
        },
      }],
      ...(mcpConnections.length === 0 ? {} : {
        context: {
          requestedMcpConnections: mcpConnections.map(({ id, name }) => ({
            id,
            name,
            status: "authorization_required",
          })),
          ...(focusMcpConnectionId ? { focusMcpConnection: focusMcpConnectionId } : {}),
        },
      }),
    });
    const account = result.accounts?.[0];
    if (!account) throw new Error("Nanocodex Connect returned no account");
    const approvalId = account.capabilities?.auth?.approval_id;
    if (typeof approvalId !== "string" || approvalId.length === 0) {
      throw new Error("Nanocodex Connect returned no signed approval identifier");
    }
    const keyAuthorization = account.capabilities?.keyAuthorization;
    const preflightKeyMatchesAccount = reusable
      && typeof activeAccount === "string"
      && activeAccount.toLowerCase() === account.address.toLowerCase();
    const reusedAccessKey = keyAuthorization
      ? undefined
      : preflightKeyMatchesAccount
        ? reusable
        : await registeredAccessKey(client, account.address, options.signal);
    if (!keyAuthorization && !reusedAccessKey) {
      throw new Error("Nanocodex Connect returned no new or reusable access key");
    }
    const wire = await client.request({
      method: "POST",
      path: "/v1/connections",
      body: {
        app_id: client.appId,
        account_address: account.address,
        approval_id: approvalId,
        ...(keyAuthorization ? {
          key_authorization: keyAuthorization,
          signed_key_authorization: account.capabilities?.personalSign?.keyAuthorization,
        } : {
          reuse_access_key: reusedAccessKey,
        }),
        permission,
        ...(requestedConnectors.length === 0 ? {} : { requested_connectors: requestedConnectors }),
        ...(mcpConnections.length === 0
          ? {}
          : { requested_mcp_connections: mcpConnections.map(({ id }) => id) }),
      },
      signal: options.signal,
    });
    const grantToken = wire?.grant_token;
    if (typeof grantToken !== "string" || grantToken.length === 0) {
      throw new Error("Nanocodex Connect returned no grant-scoped session");
    }
    const connection = connectionFromWire(wire);
    if (!connectionMatchesRequest(connection, exactRequest)) {
      throw new InvalidResponseError("Nanocodex Connect returned a grant outside the exact approved request");
    }
    client._setSession({
      grantId: connection.grant.id,
      token: grantToken,
      connection: sessionConnectionWire(wire),
    });
    connected = true;
    return connection;
  } finally {
    // The host stays covered until the grant session is committed. React owns
    // manual closure from a layout effect so its connected tree is committed
    // before the modal disappears; imperative callers retain automatic close.
    if (!connected || options.dialog?.close !== "manual") {
      client.dialog.hideWallet?.();
    }
  }
}

// The Nanocodex wallet host owns the complete SIWE ceremony so it can keep
// the authenticated session in the iframe while the user resolves requested
// connectors. Omitting `verify` here also prevents the forwarding Provider
// from replaying the wallet host's one-time challenge after approval.
function delegateAuthVerification(auth) {
  if (!auth || typeof auth === "string") return auth;
  const { verify: _verify, ...forwarded } = auth;
  return forwarded;
}

function normalizeCloudAccounts(cloudAccounts) {
  if (!cloudAccounts || typeof cloudAccounts !== "object" || Array.isArray(cloudAccounts)) return [];
  return CLOUD_ACCOUNT_PROVIDERS.filter((provider) => cloudAccounts[provider] === true);
}

function normalizeAgentVisibility(agent) {
  const rawTraces = agent?.rawTraces === true;
  return Object.freeze({
    finalMessages: rawTraces || agent?.finalMessages !== false,
    actionSummaries: rawTraces || agent?.actionSummaries !== false,
    conversationHistory: rawTraces || agent?.conversationHistory === true,
    rawTraces,
  });
}

function normalizeMcpConnections(value) {
  if (!Array.isArray(value) || value.length > 16) {
    throw new TypeError("mcpConnections must be an array of at most 16 pre-registered connections");
  }
  const ids = new Set();
  return Object.freeze(value.map((connection) => {
    if (!connection || typeof connection !== "object"
      || typeof connection.id !== "string" || !MCP_CONNECTION_ID.test(connection.id)
      || typeof connection.name !== "string" || connection.name.length < 1
      || connection.name.length > 256 || connection.name.trim() !== connection.name
      || ids.has(connection.id)) {
      throw new TypeError("mcpConnections must contain unique opaque 43-character IDs and bounded display names");
    }
    ids.add(connection.id);
    return Object.freeze({ id: connection.id, name: connection.name });
  }));
}

function normalizeMcpFocus(value, connections) {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || !MCP_CONNECTION_ID.test(value)
    || !connections.some(({ id }) => id === value)) {
    throw new TypeError("focusMcpConnectionId must identify one requested MCP connection");
  }
  return value;
}

function withConnectionResources(
  auth,
  appId,
  appOrigin,
  requestedConnectors,
  agentVisibility,
  mcpConnections,
  focusMcpConnectionId,
) {
  const configured = typeof auth === "object" && auth !== null
    ? (auth.resources ?? []).filter((resource) =>
      !Object.values(AGENT_VISIBILITY_RESOURCES).includes(resource)
      && !resource.startsWith(AGENT_VISIBILITY_RESOURCE_PREFIX)
      && !resource.startsWith(CONNECTOR_RESOURCE_PREFIX)
      && !resource.startsWith(CONNECTORS_RESOURCE_PREFIX)
      && !resource.startsWith(MCP_CONNECTION_RESOURCE_PREFIX)
      && !resource.startsWith(MCP_FOCUS_RESOURCE_PREFIX)
      && !resource.startsWith(APP_RESOURCE_PREFIX)
      && !resource.startsWith(APP_ORIGIN_RESOURCE_PREFIX))
    : [];
  const visibility = Object.entries(AGENT_VISIBILITY_NAMES)
    .filter(([name]) => agentVisibility[name])
    .map(([, value]) => value);
  const resources = [...new Set([
    ...configured,
    `${APP_RESOURCE_PREFIX}${encodeURIComponent(appId)}`,
    ...(appOrigin ? [`${APP_ORIGIN_RESOURCE_PREFIX}${encodeURIComponent(appOrigin)}`] : []),
    ...(requestedConnectors.length === 0
      ? []
      : [`${CONNECTORS_RESOURCE_PREFIX}${requestedConnectors.join(",")}`]),
    ...(visibility.length === 0
      ? []
      : [`${AGENT_VISIBILITY_RESOURCE_PREFIX}${visibility.join(",")}`]),
    ...mcpConnections.map(({ id }) => `${MCP_CONNECTION_RESOURCE_PREFIX}${id}`),
    ...(focusMcpConnectionId ? [`${MCP_FOCUS_RESOURCE_PREFIX}${focusMcpConnectionId}`] : []),
  ])];
  if (typeof auth === "string") return { url: auth, resources };
  return { ...auth, resources };
}

function reusableAccessKeys(provider, accountAddress) {
  const records = provider?.store?.getState?.().accessKeys;
  if (!Array.isArray(records)) return undefined;
  const now = Math.floor(Date.now() / 1000);
  const matching = records.filter((record) =>
    record
    && typeof record === "object"
    && typeof record.address === "string"
    && typeof record.expiry === "number"
    && record.expiry > now
    && typeof record.access === "string"
    && record.access.toLowerCase() === accountAddress.toLowerCase()
    && Number(record.chainId) === 4217
  );
  const channelAuthorities = persistedChannelAuthorities(accountAddress);
  matching.sort((left, right) => {
    const leftOwnsChannel = channelAuthorities.has(left.address.toLowerCase()) ? 1 : 0;
    const rightOwnsChannel = channelAuthorities.has(right.address.toLowerCase()) ? 1 : 0;
    return rightOwnsChannel - leftOwnsChannel || right.expiry - left.expiry;
  });
  return matching.map((selected) => ({ key_id: selected.address, expiry: selected.expiry }));
}

function activeAccountAddress(provider) {
  const state = provider?.store?.getState?.();
  const account = state?.accounts?.[state.activeAccount ?? 0];
  return typeof account?.address === "string" ? account.address : undefined;
}

async function isRegisteredAccessKey(client, accountAddress, keyId, signal) {
  try {
    const value = await client.request({
      method: "GET",
      path: `/v1/access-keys/${accountAddress}/${keyId}?app_id=${encodeURIComponent(client.appId)}`,
      signal,
    });
    return value?.registered === true;
  } catch {
    // Registration discovery is only an optimization. If it is unavailable,
    // create a fresh authorization in the one passkey ceremony and fail closed.
    return false;
  }
}

async function registeredAccessKey(client, accountAddress, signal) {
  for (const candidate of reusableAccessKeys(client.provider, accountAddress)) {
    if (await isRegisteredAccessKey(client, accountAddress, candidate.key_id, signal)) {
      return candidate;
    }
  }
  return undefined;
}

function freshAccessKeyAuthorization(authorization) {
  if (!authorization || typeof authorization !== "object") return authorization;
  const { reuse: _reuse, ...fresh } = authorization;
  return fresh;
}

function persistedChannelAuthorities(accountAddress) {
  const authorities = new Set();
  if (typeof localStorage === "undefined") return authorities;
  const prefix = `nanocodex:connect:mpp:${accountAddress.toLowerCase()}:`;
  for (const name of Object.keys(localStorage)) {
    if (!name.startsWith(prefix) || !name.includes(":chan:")) continue;
    try {
      const snapshot = JSON.parse(localStorage.getItem(name));
      const authority = snapshot?.descriptor?.authorizedSigner;
      if (typeof authority === "string") authorities.add(authority.toLowerCase());
    } catch {
      // Ignore unrelated or corrupt browser storage; the MPP store owns its
      // eventual validation and will fail closed if selected directly.
    }
  }
  return authorities;
}

function serializeAuthorizeAccessKey(value) {
  const { limits, scopes, ...authorization } = value;
  return {
    ...authorization,
    ...(value.chainId === undefined ? {} : { chainId: toHex(value.chainId) }),
    ...(limits?.length ? {
      limits: limits.map((limit) => ({
        ...limit,
        limit: toHex(limit.limit),
      })),
    } : {}),
    ...(scopes?.length ? { scopes } : {}),
  };
}

function toHex(value) {
  return `0x${BigInt(value).toString(16)}`;
}

export async function disconnect(client, options = {}) {
  const session = client._captureSession?.();
  client._clearSession();
  if (!session) return;
  await session.request({
    method: "POST",
    path: "/v1/connections/disconnect",
    signal: options.signal,
  });
}

export async function reconnect(client, options = {}) {
  const session = client._getSession();
  if (!session) return undefined;
  let retainedRequest;
  if (session.connection) {
    try {
      const retained = connectionFromWire(session.connection);
      if (retained.grant.id.toLowerCase() !== session.grantId.toLowerCase()
        || !connectionMatchesRequest(retained, options)) {
        client._clearSession();
        return undefined;
      }
      retainedRequest = reconnectRequestFromConnection(retained);
    } catch {
      // A legacy or corrupt projection can still be validated against the live
      // grant. Current sessions always retain a complete exact projection.
    }
  }
  client._setSessionToken(session.token);
  try {
    const wire = await client.request({
      method: "GET",
      path: `/v1/grants/${session.grantId}`,
      signal: options.signal,
    });
    const connection = connectionFromWire(wire);
    if (connection.grant.status !== "active"
      || connection.grant.expiresAt <= Math.floor(Date.now() / 1_000)) {
      client._clearSession();
      return undefined;
    }
    if (!connectionMatchesRequest(connection, options)
      || (retainedRequest && !connectionMatchesRequest(connection, retainedRequest))) {
      client._clearSession();
      return undefined;
    }
    client._setSession({
      grantId: session.grantId,
      token: session.token,
      connection: sessionConnectionWire(wire),
    });
    return connection;
  } catch (error) {
    client._setSessionToken(undefined);
    if (error?.status === 401 || error?.status === 403) client._clearSession();
    throw error;
  }
}

function sessionConnectionWire(wire) {
  const { grant_token: _grantToken, ...connection } = wire;
  return connection;
}
