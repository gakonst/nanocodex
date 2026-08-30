import { InvalidResponseError } from "./Errors.mjs";

const CLOUD_ACCOUNT_PROVIDERS = Object.freeze(["github", "gmail", "gdrive", "x", "whoop", "chatgpt"]);
const MCP_CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;

export function connectionFromWire(value) {
  const wire = object(value, "connection");
  const grant = object(wire.grant, "connection.grant");
  const accessKey = object(wire.access_key, "connection.access_key");
  const mpp = object(wire.mpp, "connection.mpp");
  const capabilities = strings(grant.capabilities, "connection.grant.capabilities");
  const grantMcpConnections = mcpConnections(
    grant.mcp_connections,
    "connection.grant.mcp_connections",
  );
  requireExactMcpProjection(
    capabilities,
    grantMcpConnections,
    "connection.grant",
  );
  return Object.freeze({
    accountAddress: hex(wire.account_address, "connection.account_address"),
    agentId: string(wire.agent_id, "connection.agent_id"),
    grant: Object.freeze({
      id: hex(grant.id, "connection.grant.id"),
      permission: string(grant.permission, "connection.grant.permission"),
      status: status(grant.status),
      expiresAt: integer(grant.expires_at, "connection.grant.expires_at"),
      capabilities,
      connectors: connectors(capabilities, "connection.grant.capabilities"),
      mcpConnections: grantMcpConnections,
      visibility: agentVisibility(capabilities),
    }),
    accessKey: accessKeyFromWire(accessKey),
    mpp: Object.freeze({
      token: hex(mpp.token, "connection.mpp.token"),
      symbol: string(mpp.symbol, "connection.mpp.symbol"),
      balance: bigint(mpp.balance_atomics, "connection.mpp.balance_atomics"),
      balanceStatus: mpp.balance_status === "ready" ? "ready" : "pending",
      settlementToken: hex(mpp.settlement_token, "connection.mpp.settlement_token"),
      settlementSymbol: string(mpp.settlement_symbol, "connection.mpp.settlement_symbol"),
      settlementBalance: bigint(mpp.settlement_balance_atomics, "connection.mpp.settlement_balance_atomics"),
      spent: bigint(mpp.spent_atomics, "connection.mpp.spent_atomics"),
      limit: bigint(mpp.limit_atomics, "connection.mpp.limit_atomics"),
      period: integer(mpp.period, "connection.mpp.period"),
      maxPerRequest: bigint(mpp.max_per_request_atomics, "connection.mpp.max_per_request_atomics"),
    }),
  });
}

export function connectionMatchesRequest(connection, options = {}) {
  if (options.permission !== undefined && connection.grant.permission !== options.permission) {
    return false;
  }
  const requestedCloudAccounts = options.capabilities?.cloudAccounts;
  if (requestedCloudAccounts !== undefined) {
    const requested = CLOUD_ACCOUNT_PROVIDERS.filter(
      (provider) => requestedCloudAccounts?.[provider] === true,
    );
    if (requested.length !== connection.grant.connectors.length
      || requested.some((provider) => !connection.grant.connectors.includes(provider))) {
      return false;
    }
  }
  const requestedAgent = options.capabilities?.agent;
  if (requestedAgent !== undefined) {
    const rawTraces = requestedAgent?.rawTraces === true;
    const expected = {
      finalMessages: rawTraces || requestedAgent?.finalMessages !== false,
      actionSummaries: rawTraces || requestedAgent?.actionSummaries !== false,
      conversationHistory: rawTraces || requestedAgent?.conversationHistory === true,
      rawTraces,
    };
    for (const name of Object.keys(expected)) {
      if (connection.grant.visibility[name] !== expected[name]) return false;
    }
  }
  if (options.mcpConnectionIds !== undefined) {
    if (!Array.isArray(options.mcpConnectionIds)
      || options.mcpConnectionIds.some((id) => typeof id !== "string" || !MCP_CONNECTION_ID.test(id))
      || new Set(options.mcpConnectionIds).size !== options.mcpConnectionIds.length) {
      return false;
    }
    const actual = connection.grant.mcpConnections.map(({ id }) => id);
    if (actual.length !== options.mcpConnectionIds.length
      || actual.some((id) => !options.mcpConnectionIds.includes(id))) {
      return false;
    }
  }
  return true;
}

/** Builds the exact non-secret request projection retained by one minted grant. */
export function reconnectRequestFromConnection(connection) {
  return connectionRequestFromGrant(connection.grant);
}

/** Builds the exact non-secret request projection for normalized grant fields. */
export function connectionRequestFromGrant(grant) {
  return Object.freeze({
    capabilities: Object.freeze({
      agent: grant.visibility,
      cloudAccounts: Object.freeze(Object.fromEntries(
        grant.connectors.map((provider) => [provider, true]),
      )),
    }),
    mcpConnectionIds: Object.freeze(grant.mcpConnections.map(({ id }) => id)),
    permission: grant.permission,
  });
}

export function preparedConnectionFromWire(value) {
  const wire = object(value, "prepared connection");
  const app = object(wire.app, "prepared connection.app");
  const auth = object(wire.auth, "prepared connection.auth");
  const permission = object(wire.permission, "prepared connection.permission");
  const mpp = object(wire.mpp, "prepared connection.mpp");
  const prepared = Object.freeze({
    requestId: string(wire.request_id, "prepared connection.request_id"),
    app: Object.freeze({
      id: string(app.id, "prepared connection.app.id"),
      name: string(app.name, "prepared connection.app.name"),
      origin: string(app.origin, "prepared connection.app.origin"),
    }),
    accountAddress: hex(wire.account_address, "prepared connection.account_address"),
    auth: Object.freeze({
      message: string(auth.message, "prepared connection.auth.message"),
      resources: strings(auth.resources, "prepared connection.auth.resources"),
    }),
    permission: Object.freeze({
      id: string(permission.id, "prepared connection.permission.id"),
      title: string(permission.title, "prepared connection.permission.title"),
      description: string(permission.description, "prepared connection.permission.description"),
      connectors: array(permission.connectors, "prepared connection.permission.connectors").map((item, index) => {
        const connector = object(item, `prepared connection.permission.connectors[${index}]`);
        return Object.freeze({
          id: string(connector.id, `prepared connection.permission.connectors[${index}].id`),
          name: string(connector.name, `prepared connection.permission.connectors[${index}].name`),
          detail: string(connector.detail, `prepared connection.permission.connectors[${index}].detail`),
        });
      }),
    }),
    accessKey: accessKeyFromWire(object(wire.access_key, "prepared connection.access_key")),
    mpp: Object.freeze({
      token: hex(mpp.token, "prepared connection.mpp.token"),
      symbol: string(mpp.symbol, "prepared connection.mpp.symbol"),
      limit: bigint(mpp.limit_atomics, "prepared connection.mpp.limit_atomics"),
      period: integer(mpp.period, "prepared connection.mpp.period"),
      maxPerRequest: bigint(mpp.max_per_request_atomics, "prepared connection.mpp.max_per_request_atomics"),
    }),
  });
  return { prepared, wire };
}

export function accessKeyFromWire(wire) {
  return Object.freeze({
    address: hex(wire.address, "access key.address"),
    chainId: bigint(wire.chain_id, "access key.chain_id"),
    keyId: hex(wire.key_id, "access key.key_id"),
    ...(wire.public_key === undefined ? {} : { publicKey: hex(wire.public_key, "access key.public_key") }),
    keyType: keyType(wire.key_type),
    limits: Object.freeze(array(wire.limits, "access key.limits").map((item, index) => {
      const limit = object(item, `access key.limits[${index}]`);
      return Object.freeze({
        token: hex(limit.token, `access key.limits[${index}].token`),
        limit: bigint(limit.limit, `access key.limits[${index}].limit`),
        ...(limit.period === undefined ? {} : { period: integer(limit.period, `access key.limits[${index}].period`) }),
      });
    })),
    scopes: Object.freeze(array(wire.scopes, "access key.scopes").map((item, index) => {
      const scope = object(item, `access key.scopes[${index}]`);
      return Object.freeze({
        address: hex(scope.address, `access key.scopes[${index}].address`),
        ...(scope.selector === undefined ? {} : { selector: string(scope.selector, `access key.scopes[${index}].selector`) }),
        ...(scope.recipients === undefined ? {} : { recipients: strings(scope.recipients, `access key.scopes[${index}].recipients`) }),
      });
    })),
    witness: hex(wire.witness, "access key.witness"),
    expiry: integer(wire.expiry, "access key.expiry"),
    ...(wire.authorization === undefined ? {} : { authorization: hex(wire.authorization, "access key.authorization") }),
  });
}

export function grantFromWire(value) {
  const grant = object(value, "grant");
  const capabilities = strings(grant.capabilities, "grant.capabilities");
  const grantMcpConnections = mcpConnections(grant.mcp_connections, "grant.mcp_connections");
  requireExactMcpProjection(capabilities, grantMcpConnections, "grant");
  return Object.freeze({
    id: hex(grant.id, "grant.id"),
    permission: string(grant.permission, "grant.permission"),
    status: status(grant.status),
    expiresAt: integer(grant.expires_at, "grant.expires_at"),
    capabilities,
    connectors: connectors(capabilities, "grant.capabilities"),
    mcpConnections: grantMcpConnections,
    visibility: agentVisibility(capabilities),
  });
}

const AGENT_VISIBILITY_CAPABILITIES = Object.freeze({
  finalMessages: "agent.output.final",
  actionSummaries: "agent.output.actions",
  conversationHistory: "agent.history.read",
  rawTraces: "agent.trace.read",
});

function agentVisibility(capabilities) {
  const recognized = Object.values(AGENT_VISIBILITY_CAPABILITIES)
    .some((capability) => capabilities.includes(capability));
  const rawTraces = capabilities.includes(AGENT_VISIBILITY_CAPABILITIES.rawTraces);
  return Object.freeze({
    finalMessages: rawTraces || !recognized || capabilities.includes(AGENT_VISIBILITY_CAPABILITIES.finalMessages),
    actionSummaries: rawTraces || !recognized || capabilities.includes(AGENT_VISIBILITY_CAPABILITIES.actionSummaries),
    conversationHistory: rawTraces || capabilities.includes(AGENT_VISIBILITY_CAPABILITIES.conversationHistory),
    rawTraces,
  });
}

export function machineUsdConfigFromWire(value) {
  const wire = object(value, "machineUSD config");
  return Object.freeze({
    chainId: integer(wire.chain_id, "machineUSD config.chain_id"),
    minUsdAmountCents: integer(wire.min_usd_amount_cents, "machineUSD config.min_usd_amount_cents"),
    maxUsdAmountCents: integer(wire.max_usd_amount_cents, "machineUSD config.max_usd_amount_cents"),
    onrampEnabled: boolean(wire.onramp_enabled, "machineUSD config.onramp_enabled"),
    stripePublishableKey: string(wire.stripe_publishable_key, "machineUSD config.stripe_publishable_key"),
    tokenAddress: hex(wire.token_address, "machineUSD config.token_address"),
  });
}

export function fundResultFromWire(value, client) {
  const wire = object(value, "machineUSD funding result");
  const order = object(wire.order, "machineUSD funding result.order");
  return Object.freeze({
    order: Object.freeze({
      id: string(order.id, "machineUSD order.id"),
      status: string(order.status, "machineUSD order.status"),
      usdAmountCents: integer(order.usd_amount_cents, "machineUSD order.usd_amount_cents"),
      machineUsdAmount: bigint(order.machine_usd_amount_atomics, "machineUSD order.machine_usd_amount_atomics"),
      issuanceTransactionHash: hex(order.issuance_transaction_hash, "machineUSD order.issuance_transaction_hash"),
    }),
    connection: connectionFromWire(wire.connection),
  });
}

export function chargeResultFromWire(value, client) {
  const wire = object(value, "MPP charge result");
  const receipt = object(wire.receipt, "MPP charge result.receipt");
  return Object.freeze({
    receipt: Object.freeze({
      id: string(receipt.id, "MPP receipt.id"),
      amount: bigint(receipt.amount_atomics, "MPP receipt.amount_atomics"),
      origin: string(receipt.origin, "MPP receipt.origin"),
      transactionHash: hex(receipt.transaction_hash, "MPP receipt.transaction_hash"),
    }),
    connection: connectionFromWire(wire.connection),
  });
}

function array(value, label) {
  if (!Array.isArray(value)) throw new InvalidResponseError(`${label} must be an array`);
  return value;
}

function object(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new InvalidResponseError(`${label} must be an object`);
  }
  return value;
}

function string(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    throw new InvalidResponseError(`${label} must be a non-empty string`);
  }
  return value;
}

function strings(value, label) {
  return Object.freeze(array(value, label).map((item, index) => string(item, `${label}[${index}]`)));
}

function connectors(capabilities, label) {
  const providers = ["github", "gmail", "gdrive", "x", "whoop", "chatgpt"];
  const items = strings(capabilities, label);
  return Object.freeze(providers.filter((provider) =>
    items.includes(provider) || items.includes(`urn:nanocodex:connector:${provider}`)
  ));
}

function mcpConnections(value, label) {
  if (value === undefined) return Object.freeze([]);
  const ids = new Set();
  const connections = array(value, label);
  if (connections.length > 16) throw new InvalidResponseError(`${label} must contain at most 16 connections`);
  return Object.freeze(connections.map((item, index) => {
    const connection = object(item, `${label}[${index}]`);
    if (Object.keys(connection).some((key) => key !== "id" && key !== "name")) {
      throw new InvalidResponseError(`${label}[${index}] contains private or unknown fields`);
    }
    const id = string(connection.id, `${label}[${index}].id`);
    const name = string(connection.name, `${label}[${index}].name`);
    if (!MCP_CONNECTION_ID.test(id) || ids.has(id)
      || name.length < 1 || name.length > 256 || name.trim() !== name) {
      throw new InvalidResponseError(`${label}[${index}] must contain an exact hosted MCP identity`);
    }
    ids.add(id);
    return Object.freeze({ id, name });
  }));
}

function requireExactMcpProjection(capabilities, connections, label) {
  const ids = capabilities.flatMap((capability) => capability.startsWith("mcp:")
    ? [capability.slice("mcp:".length)]
    : []);
  if (ids.length > 16
    || ids.some((id) => !MCP_CONNECTION_ID.test(id))
    || new Set(ids).size !== ids.length) {
    throw new InvalidResponseError(`${label}.capabilities contains invalid hosted MCP identities`);
  }
  const metadataIds = connections.map(({ id }) => id);
  if (ids.length !== metadataIds.length || ids.some((id) => !metadataIds.includes(id))) {
    throw new InvalidResponseError(`${label} MCP capabilities and metadata must match exactly`);
  }
}

function hex(value, label) {
  const result = string(value, label);
  if (!/^0x[0-9a-fA-F]+$/.test(result)) throw new InvalidResponseError(`${label} must be hex`);
  return result;
}

function bigint(value, label) {
  try {
    return BigInt(value);
  } catch {
    throw new InvalidResponseError(`${label} must be an integer string`);
  }
}

function integer(value, label) {
  if (!Number.isSafeInteger(value)) throw new InvalidResponseError(`${label} must be a safe integer`);
  return value;
}

function boolean(value, label) {
  if (typeof value !== "boolean") throw new InvalidResponseError(`${label} must be a boolean`);
  return value;
}

function status(value) {
  if (value !== "active" && value !== "revoked" && value !== "expired") {
    throw new InvalidResponseError("grant.status is invalid");
  }
  return value;
}

function keyType(value) {
  if (value !== "secp256k1" && value !== "p256" && value !== "webAuthn") {
    throw new InvalidResponseError("access key.key_type is invalid");
  }
  return value;
}
