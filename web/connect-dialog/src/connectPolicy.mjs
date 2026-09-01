export const productionConnectApiOrigin = "https://nanocodex-connect-api.gakonst.workers.dev";

const appResourcePrefix = "urn:nanocodex:app:";
const appOriginResourcePrefix = "urn:nanocodex:origin:";
const connectorFocusResourcePrefix = "urn:nanocodex:connector-focus:";
const credentialImportResourcePrefix = "urn:nanocodex:credential-import:";
const agentConversationResourcePrefix = "urn:nanocodex:agent:conversation:";
const appToolCatalogResource = /^urn:nanocodex:app-tool-catalog:sha256:[0-9a-f]{64}$/;
const agentConversationId = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const chatGptCredentialImportResource = /^urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:[A-Za-z0-9_-]{43}$/;
const connectorIds = new Set(["chatgpt", "github", "gmail", "gdrive", "x", "slack"]);
const mcpConnectionId = /^[A-Za-z0-9_-]{43}$/;
const mcpConnectionStatuses = new Set([
  "authorization_required",
  "reauthorization_required",
  "connected",
  "disabled",
  "revoked",
]);
const mcpCallbackContinuationVersion = 1;
const mcpCallbackContinuationLifetimeMs = 10 * 60 * 1000;

const signedAppVisibility = Object.freeze([
  Object.freeze({
    resource: "urn:nanocodex:agent:output:final",
    name: "reply",
    label: "Reply",
    detail: "Final agent reply",
  }),
  Object.freeze({
    resource: "urn:nanocodex:agent:output:actions",
    name: "actions",
    label: "Actions",
    detail: "Agent actions and tool calls",
  }),
  Object.freeze({
    resource: "urn:nanocodex:agent:history:read",
    name: "history",
    label: "History",
    detail: "Conversation history",
  }),
  Object.freeze({
    resource: "urn:nanocodex:agent:trace:read",
    name: "traces",
    label: "Thinking & traces",
    detail: "Reasoning, thinking, and full tool traffic",
  }),
  Object.freeze({
    resource: "urn:nanocodex:history:read",
    name: "hosted-history",
    label: "Hosted history",
    detail: "Account and team conversation history",
  }),
  Object.freeze({
    resource: "urn:nanocodex:memory:read",
    name: "memory-read",
    label: "Memory read",
    detail: "Recall hosted team memory",
  }),
  Object.freeze({
    resource: "urn:nanocodex:memory:write",
    name: "memory-write",
    label: "Memory write",
    detail: "Save hosted team memory",
  }),
]);

const productionApps = new Map([
  ["https://nanocodex-connect-playground.gakonst.workers.dev", Object.freeze({
    id: "atlas-workspace",
    name: "Atlas Workspace",
    origin: "https://nanocodex-connect-playground.gakonst.workers.dev",
  })],
  ["chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle", Object.freeze({
    id: "nanocodex-chrome",
    name: "Nanocodex for Chrome",
    origin: "chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle",
  })],
  ["https://cli.nanocodex.xyz", Object.freeze({
    id: "nanocodex-cli",
    name: "Nanocodex CLI",
    origin: "https://cli.nanocodex.xyz",
  })],
]);

export function registeredApp(embeddingOrigin, appId, dialogUrl, isTopLevel, allowDynamicPopup = true) {
  if (!isAppId(appId)) throw new Error("Nanocodex Connect received an invalid app ID.");
  const dialogOrigin = originFromUrl(dialogUrl, "Nanocodex Connect received an invalid dialog URL.");
  const registered = productionApps.get(embeddingOrigin);
  if (registered) {
    if (registered.id !== appId) throw new Error("This application ID does not match its registered origin.");
    return registered;
  }
  if (isLocalDevelopmentOrigin(dialogOrigin) && isLocalDevelopmentOrigin(embeddingOrigin)) {
    return Object.freeze({ id: appId, name: "Atlas Workspace", origin: embeddingOrigin });
  }
  if (allowDynamicPopup && isPopupPresentation(dialogUrl, isTopLevel) && isSecurePopupOrigin(embeddingOrigin)) {
    const url = new URL(embeddingOrigin);
    return Object.freeze({ id: appId, name: url.hostname, origin: embeddingOrigin });
  }
  throw new Error("This application is not registered with Nanocodex Connect.");
}

export function isPopupPresentation(dialogUrl, isTopLevel) {
  try {
    const url = new URL(dialogUrl);
    return isTopLevel === true
      && url.searchParams.getAll("mode").length === 1
      && url.searchParams.get("mode") === "popup";
  } catch {
    return false;
  }
}

export function signedAppResources(resources, app) {
  if (!Array.isArray(resources) || !app || typeof app !== "object") {
    throw new Error("Nanocodex Connect received invalid signed application resources.");
  }
  const expectedApp = `${appResourcePrefix}${encodeURIComponent(app.id)}`;
  const expectedOrigin = `${appOriginResourcePrefix}${encodeURIComponent(app.origin)}`;
  const applicationResources = resources.filter((resource) =>
    typeof resource === "string" && resource.startsWith(appResourcePrefix));
  const originResources = resources.filter((resource) =>
    typeof resource === "string" && resource.startsWith(appOriginResourcePrefix));
  if (applicationResources.length !== 1 || applicationResources[0] !== expectedApp
    || originResources.length !== 1 || originResources[0] !== expectedOrigin) {
    throw new Error("The signed application resources do not match this Connect dialog.");
  }
  const conversations = resources.filter((resource) =>
    typeof resource === "string" && resource.startsWith(agentConversationResourcePrefix));
  if (conversations.length > 1
    || (conversations.length === 1
      && !agentConversationId.test(conversations[0].slice(agentConversationResourcePrefix.length)))) {
    throw new Error("The signed durable conversation request is invalid.");
  }
  return resources;
}

export function parseConnectPolicy(resources) {
  if (!Array.isArray(resources)) {
    throw new Error("Nanocodex Connect received invalid signed resources.");
  }
  const signedResources = resources.filter((resource) => typeof resource === "string");
  const credentialImports = signedResources.filter((resource) =>
    resource.startsWith(credentialImportResourcePrefix));
  if (credentialImports.length === 0) {
    return Object.freeze({ chatGptCredentialImport: false });
  }
  if (credentialImports.length !== 1
    || !chatGptCredentialImportResource.test(credentialImports[0])) {
    throw new Error("The signed credential import resource is invalid.");
  }
  const requestedConnectors = signedResources.flatMap((resource) => {
    if (resource.startsWith("urn:nanocodex:connector:")) {
      return [resource.slice("urn:nanocodex:connector:".length)];
    }
    if (resource.startsWith("urn:nanocodex:connectors:")) {
      return resource.slice("urn:nanocodex:connectors:".length).split(",");
    }
    return [];
  });
  if (!requestedConnectors.includes("chatgpt")) {
    throw new Error("The signed ChatGPT credential import has no ChatGPT connector request.");
  }
  return Object.freeze({ chatGptCredentialImport: true });
}

export function connectApiOrigin(auth, dialogOrigin) {
  const configured = authEndpoints(auth);
  if (configured.length === 0) {
    throw new Error("Nanocodex Connect has no account broker URL.");
  }
  const origins = configured.map(endpointOrigin);
  if (origins.every((origin) => origin === productionConnectApiOrigin)) {
    return productionConnectApiOrigin;
  }
  if (isLocalDevelopmentOrigin(dialogOrigin)) {
    const expected = origins[0];
    if (!isLocalDevelopmentOrigin(expected) || origins.some((origin) => origin !== expected)) {
      throw new Error("Local Nanocodex Connect auth endpoints must share one development origin.");
    }
    return expected;
  }
  throw new Error("Nanocodex Connect auth endpoints must use the production Connect API.");
}

export function sanitizeWalletResult(result) {
  if (!isRecord(result) || !Array.isArray(result.accounts)) {
    throw new Error("Accounts did not return a connected account.");
  }
  return {
    ...result,
    accounts: result.accounts.map((value) => {
      if (!isRecord(value)) throw new Error("Accounts returned an invalid connected account.");
      const capabilities = isRecord(value.capabilities) ? value.capabilities : {};
      const auth = isRecord(capabilities.auth) ? capabilities.auth : {};
      if (typeof auth.approval_id !== "string" || auth.approval_id.length === 0) {
        throw new Error("Accounts did not return a signed approval identifier.");
      }
      return {
        ...value,
        capabilities: {
          ...capabilities,
          auth: { approval_id: auth.approval_id },
        },
      };
    }),
  };
}

export function sanitizeCliWalletResult(result) {
  if (!isRecord(result) || !Array.isArray(result.accounts) || result.accounts.length !== 1) {
    throw new Error("Accounts did not return exactly one connected account.");
  }
  const account = result.accounts[0];
  if (!isRecord(account) || typeof account.address !== "string"
    || !/^0x[0-9a-fA-F]{40}$/.test(account.address)) {
    throw new Error("Accounts returned an invalid connected account.");
  }
  const capabilities = isRecord(account.capabilities) ? account.capabilities : {};
  const auth = isRecord(capabilities.auth) ? capabilities.auth : {};
  if (auth.mode === "hosted") {
    if (Object.keys(capabilities).some((key) => key !== "auth")
      || Object.keys(auth).some((key) => key !== "approval_id" && key !== "mode")
      || typeof auth.approval_id !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(auth.approval_id)) {
      throw new Error("Accounts did not return a hosted CLI approval.");
    }
    return {
      accounts: [{
        address: account.address,
        capabilities: { auth: { approval_id: auth.approval_id, mode: "hosted" } },
      }],
    };
  }
  const personalSign = isRecord(capabilities.personalSign) ? capabilities.personalSign : {};
  if (typeof auth.approval_id !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(auth.approval_id)
    || !isRecord(capabilities.keyAuthorization)
    || typeof personalSign.keyAuthorization !== "string"
    || !/^0x[0-9a-fA-F]+$/.test(personalSign.keyAuthorization)) {
    throw new Error("Accounts did not return a signed CLI approval.");
  }
  return {
    accounts: [{
      address: account.address,
      capabilities: {
        keyAuthorization: capabilities.keyAuthorization,
        personalSign: { keyAuthorization: personalSign.keyAuthorization },
        auth: { approval_id: auth.approval_id },
      },
    }],
  };
}

export function appVisibilityPermissions(resources) {
  if (!Array.isArray(resources)) return [];
  const requested = new Set(resources.filter((resource) => typeof resource === "string"));
  const compact = new Set(resources
    .filter((resource) => typeof resource === "string" && resource.startsWith("urn:nanocodex:agent:visibility:"))
    .flatMap((resource) => resource.slice("urn:nanocodex:agent:visibility:".length).split(",")));
  const visibility = signedAppVisibility
    .filter(({ resource, name }) => requested.has(resource) || compact.has(name))
    .map(({ name: _name, ...permission }) => permission);
  const conversations = [...requested].filter((resource) => resource.startsWith(agentConversationResourcePrefix));
  if (conversations.length === 1
    && agentConversationId.test(conversations[0].slice(agentConversationResourcePrefix.length))) {
    visibility.push(Object.freeze({
      resource: conversations[0],
      label: "Conversation",
      detail: "Create and use one new durable conversation",
    }));
  }
  const appToolCatalogs = [...requested].filter((resource) => appToolCatalogResource.test(resource));
  if (appToolCatalogs.length === 1) {
    visibility.push(Object.freeze({
      resource: appToolCatalogs[0],
      label: "Browser tab tool",
      detail: "Use only the exact local browser tool catalog approved here",
    }));
  }
  return visibility;
}

export function accountLoginCapabilities(accounts) {
  const credentialIds = Array.isArray(accounts)
    ? [...new Set(accounts.flatMap((account) => {
      const id = isRecord(account) && isRecord(account.credential)
        ? account.credential.id
        : undefined;
      return typeof id === "string" && id.length > 0 ? [id] : [];
    }))]
    : [];
  return credentialIds.length > 0
    ? Object.freeze({ method: "login", credentialId: Object.freeze(credentialIds) })
    : Object.freeze({ method: "login" });
}

export function connectorApprovalDisposition(requestedConnectors, statuses) {
  if (!Array.isArray(requestedConnectors) || !isRecord(statuses)) return "wait";
  const ready = requestedConnectors.every((connector) =>
    typeof connector === "string"
    && isRecord(statuses[connector])
    && statuses[connector].connected === true);
  if (!ready) return "wait";
  return "respond";
}

export function chatGptConnectorDisposition(value) {
  if (!isRecord(value)) return "invalid";
  if (value.state === "authenticated" && value.connected === true) return "connected";
  if (value.state === "pending"
    && typeof value.verification_url === "string"
    && typeof value.user_code === "string"
    && Number.isSafeInteger(value.expires_at)) return "device";
  return "invalid";
}

export function focusedConnectorFromResources(resources, requestedConnectors) {
  if (!Array.isArray(resources) || !Array.isArray(requestedConnectors)) {
    throw new Error("The signed connector focus is invalid.");
  }
  const focused = resources
    .filter((resource) => typeof resource === "string" && resource.startsWith(connectorFocusResourcePrefix))
    .map((resource) => resource.slice(connectorFocusResourcePrefix.length));
  if (focused.length === 0) return undefined;
  if (focused.length !== 1
    || !connectorIds.has(focused[0])
    || !requestedConnectors.includes(focused[0])) {
    throw new Error("The signed connector focus is invalid.");
  }
  return focused[0];
}

export function mcpConnectionsFromWire(value) {
  if (!Array.isArray(value) || value.length > 32) {
    throw new Error("Nanocodex Connect received invalid MCP connections.");
  }
  const ids = new Set();
  return Object.freeze(value.map((candidate) => {
    if (!isRecord(candidate)
      || Object.keys(candidate).some((key) => key !== "id" && key !== "name" && key !== "status")
      || typeof candidate.id !== "string" || !mcpConnectionId.test(candidate.id)
      || typeof candidate.name !== "string" || candidate.name.length < 1 || candidate.name.length > 256
      || candidate.name.trim() !== candidate.name
      || typeof candidate.status !== "string" || !mcpConnectionStatuses.has(candidate.status)
      || ids.has(candidate.id)) {
      throw new Error("Nanocodex Connect received invalid MCP connections.");
    }
    ids.add(candidate.id);
    return Object.freeze({
      id: candidate.id,
      name: candidate.name,
      status: candidate.status,
    });
  }));
}

export function focusedMcpConnection(value, connections) {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || !mcpConnectionId.test(value)
    || !Array.isArray(connections)
    || !connections.some((connection) => isRecord(connection) && connection.id === value)) {
    throw new Error("The focused MCP connection is invalid.");
  }
  return value;
}

export function mcpConnectionApprovalDisposition(requestedConnections, connections) {
  if (!Array.isArray(requestedConnections) || !Array.isArray(connections)) return "wait";
  const statuses = new Map(connections.flatMap((connection) => isRecord(connection)
    && typeof connection.id === "string" && typeof connection.status === "string"
      ? [[connection.id, connection.status]]
      : []));
  return requestedConnections.every((connection) => isRecord(connection)
    && typeof connection.id === "string"
    && statuses.get(connection.id) === "connected")
    ? "respond"
    : "wait";
}

export function createMcpCallbackContinuation(value, now = Date.now()) {
  if (!isRecord(value)
    || typeof value.requestId !== "string" || value.requestId.length < 1 || value.requestId.length > 512
    || typeof value.apiUrl !== "string"
    || (value.apiUrl !== productionConnectApiOrigin && !isLocalDevelopmentOrigin(value.apiUrl))
    || typeof value.accountAddress !== "string" || !/^0x[0-9a-fA-F]{40}$/.test(value.accountAddress)
    || typeof value.token !== "string" || value.token.length < 1 || value.token.length > 4096
    || !Array.isArray(value.requestedConnectors)
    || value.requestedConnectors.some((id) => typeof id !== "string" || !connectorIds.has(id))
    || new Set(value.requestedConnectors).size !== value.requestedConnectors.length
    || !isRecord(value.connectorStatuses)
    || !Number.isSafeInteger(now) || now < 0) {
    throw new Error("Nanocodex Connect could not retain the MCP callback.");
  }
  const requestedMcpConnections = mcpConnectionsFromWire(value.requestedMcpConnections);
  const connectorStatuses = sanitizeConnectorStatuses(value.connectorStatuses);
  const result = sanitizeCliWalletResult(value.result);
  return Object.freeze({
    version: mcpCallbackContinuationVersion,
    expiresAt: now + mcpCallbackContinuationLifetimeMs,
    requestId: value.requestId,
    apiUrl: value.apiUrl,
    accountAddress: value.accountAddress,
    token: value.token,
    requestedConnectors: Object.freeze([...value.requestedConnectors]),
    requestedMcpConnections,
    connectorStatuses,
    result,
  });
}

export function restoreMcpCallbackContinuation(value, expected, now = Date.now()) {
  const returnedConnector = isRecord(expected) && typeof expected.returnedConnector === "string"
    ? expected.returnedConnector
    : undefined;
  const returnedMcpConnection = isRecord(expected) && typeof expected.returnedMcpConnection === "string"
    ? expected.returnedMcpConnection
    : undefined;
  if (!isRecord(value) || value.version !== mcpCallbackContinuationVersion
    || !Number.isSafeInteger(value.expiresAt) || value.expiresAt < now
    || value.expiresAt > now + mcpCallbackContinuationLifetimeMs
    || !isRecord(expected)
    || value.requestId !== expected.requestId
    || value.apiUrl !== expected.apiUrl
    || (returnedConnector === undefined) === (returnedMcpConnection === undefined)) {
    throw new Error("The retained MCP callback is invalid or expired.");
  }
  const restored = createMcpCallbackContinuation({
    requestId: value.requestId,
    apiUrl: value.apiUrl,
    accountAddress: value.accountAddress,
    token: value.token,
    requestedConnectors: value.requestedConnectors,
    requestedMcpConnections: value.requestedMcpConnections,
    connectorStatuses: value.connectorStatuses,
    result: value.result,
  }, value.expiresAt - mcpCallbackContinuationLifetimeMs);
  const expectedConnectors = Array.isArray(expected.requestedConnectors)
    ? expected.requestedConnectors
    : [];
  const expectedMcpConnections = mcpConnectionsFromWire(expected.requestedMcpConnections);
  if (!sameStrings(restored.requestedConnectors, expectedConnectors)
    || !sameMcpConnections(restored.requestedMcpConnections, expectedMcpConnections)
    || (returnedConnector !== undefined && !expectedConnectors.includes(returnedConnector))
    || (returnedMcpConnection !== undefined
      && !expectedMcpConnections.some(({ id }) => id === returnedMcpConnection))
    || restored.result.accounts[0]?.address.toLowerCase() !== restored.accountAddress.toLowerCase()) {
    throw new Error("The retained MCP callback does not match this request.");
  }
  return restored;
}

export function isLocalDevelopmentOrigin(value) {
  try {
    const url = new URL(value);
    const hostname = url.hostname.toLowerCase();
    const localNanocodex = hostname === "nanocodex.localhost"
      || /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/.test(hostname);
    return url.origin === value
      && (url.protocol === "http:" || url.protocol === "https:")
      && (
        hostname === "localhost"
        || hostname === "127.0.0.1"
        || hostname === "[::1]"
        || localNanocodex
      );
  } catch {
    return false;
  }
}

export function usesBrowserLocalWebAuthn(value) {
  try {
    const url = new URL(value);
    const hostname = url.hostname.toLowerCase();
    return url.origin === value
      && (url.protocol === "http:" || url.protocol === "https:")
      && (
        hostname === "localhost"
        || hostname === "127.0.0.1"
        || hostname === "[::1]"
      );
  } catch {
    return false;
  }
}

export function deviceMcpReturnPath(value) {
  const url = new URL(value);
  const result = new URL("/connect", url.origin);
  for (const parameter of ["user_code", "api_origin"]) {
    const parameterValue = url.searchParams.get(parameter);
    if (parameterValue !== null) result.searchParams.set(parameter, parameterValue);
  }
  return `${result.pathname}${result.search}`;
}

function sanitizeConnectorStatuses(value) {
  const result = {};
  for (const [id, status] of Object.entries(value)) {
    if (!connectorIds.has(id) || !isRecord(status)
      || Object.keys(status).some((key) => key !== "connected" && key !== "account_id" && key !== "label")
      || typeof status.connected !== "boolean"
      || (status.account_id !== undefined && typeof status.account_id !== "string")
      || (status.label !== undefined && typeof status.label !== "string")) {
      throw new Error("Nanocodex Connect received invalid connector statuses.");
    }
    result[id] = Object.freeze({
      connected: status.connected,
      ...(status.account_id === undefined ? {} : { account_id: status.account_id }),
      ...(status.label === undefined ? {} : { label: status.label }),
    });
  }
  return Object.freeze(result);
}

function sameStrings(left, right) {
  return Array.isArray(right)
    && left.length === right.length
    && left.every((value, index) => value === right[index]);
}

function sameMcpConnections(left, right) {
  return left.length === right.length
    && left.every((value, index) => value.id === right[index].id && value.name === right[index].name);
}

function authEndpoints(auth) {
  if (typeof auth === "string") return [auth];
  if (!isRecord(auth)) return [];
  const endpoints = [];
  for (const name of ["challenge", "url", "verify", "logout"]) {
    if (!(name in auth)) continue;
    if (typeof auth[name] !== "string") {
      throw new Error(`Nanocodex Connect auth ${name} must be a URL.`);
    }
    endpoints.push(auth[name]);
  }
  return endpoints;
}

function endpointOrigin(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("Nanocodex Connect received an invalid auth endpoint.");
  }
  if ((url.protocol !== "https:" && url.protocol !== "http:") || url.username || url.password) {
    throw new Error("Nanocodex Connect received an unsafe auth endpoint.");
  }
  return url.origin;
}

function isAppId(value) {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
}

function isSecurePopupOrigin(value) {
  try {
    const url = new URL(value);
    if (url.protocol === "chrome-extension:") {
      return url.href === value && /^[a-p]{32}$/.test(url.hostname);
    }
    return url.origin === value && url.protocol === "https:" && url.hostname.length > 0;
  } catch {
    return false;
  }
}

function originFromUrl(value, error) {
  try {
    const url = new URL(value);
    return url.origin;
  } catch {
    throw new Error(error);
  }
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
