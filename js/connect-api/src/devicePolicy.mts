import { Address, PublicKey } from "ox";
import {
  credentialImportDigestFromResources,
  isAllowedChatGptCredentialImportResource,
} from "./chatGptCredentialImport.mts";
import { isAllowedMcpResource, validateMcpResources } from "./mcpPolicy.mts";
import {
  CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX,
  isAllowedAppToolCatalogResource,
  parseCliBrowserCookieSyncResource,
} from "./appToolPolicy.mts";

type UnknownRecord = Record<string, unknown>;
type ParsedCliWalletRequest = Readonly<{
  id: string | number;
  method: "wallet_connect";
  params: readonly unknown[];
  resources: readonly string[];
}>;
type InstallationAccessKey = UnknownRecord & {
  address: string;
  publicKey: string;
  keyType: "secp256k1";
  chainId: string;
  expiry: number;
  limits: NormalizedLimit[];
  scopes: NormalizedScope[];
};
type NormalizedLimit = { token: string; limit: string; period: number };
type NormalizedScope = { address: string; selector: string; recipients?: string[] };
type SanitizedCliWalletResult = Readonly<{
  accounts: readonly Readonly<{
    address: `0x${string}`;
    capabilities: Readonly<{
      keyAuthorization: Readonly<UnknownRecord>;
      personalSign: Readonly<{ keyAuthorization: `0x${string}` }>;
      auth: Readonly<{ approval_id: string }>;
    }> | Readonly<{
      auth: Readonly<{ approval_id: string; mode: "hosted" }>;
    }>;
  }>[];
}>;

export const cliApp = Object.freeze({
  id: "nanocodex-cli",
  name: "Nanocodex CLI",
  origin: "https://cli.nanocodex.xyz",
});

export const cliAppResource = `urn:nanocodex:app:${encodeURIComponent(cliApp.id)}`;
export const cliOriginResource = `urn:nanocodex:origin:${encodeURIComponent(cliApp.origin)}`;
export const agentPortabilityResource = "urn:nanocodex:agent:durability:portability";

const requiredResources = new Set([
  "urn:nanocodex:agent:run",
  cliAppResource,
  cliOriginResource,
]);
const optionalResources = new Set([
  "urn:nanocodex:agent:output:final",
  "urn:nanocodex:agent:output:actions",
  "urn:nanocodex:agent:history:read",
  "urn:nanocodex:agent:trace:read",
  agentPortabilityResource,
  "urn:nanocodex:history:read",
  "urn:nanocodex:memory:read",
  "urn:nanocodex:memory:write",
  "urn:nanocodex:capability:mercator:boost",
  "urn:nanocodex:mpp:machusd:spend",
  "urn:nanocodex:authorization:hosted",
]);
const connectors = new Set([
  "github", "gmail", "gdrive", "gcalendar", "gtasks", "gdocs",
  "gsheets", "gslides", "gcontacts", "slack", "x", "chatgpt",
]);
const connectorFocusPrefix = "urn:nanocodex:connector-focus:";
const visibility = new Set(["reply", "actions", "history", "traces"]);
const chainId = "0x1079";
const accessKeyLifetime = 30 * 86_400;
const accessKeyClockSkew = 5 * 60;
const machineUsd = "0x20c000000000000000000000f37de3740adec032";
const usdcE = "0x20c000000000000000000000b9537d11c60e8b50";
const tip20ChannelEscrow = "0x33b901018174ddabe4841042ab76ba85d4e24f25";
const mercatorSettlement = "0xa295c42fbcc026a62304a7701f25b4c91799b0da";
const mppLimit = "0x989680";
const mppPeriod = 86_400;

export function parseCliWalletRequest(value: unknown): ParsedCliWalletRequest {
  if (!isRecord(value) || value.jsonrpc !== "2.0"
    || (typeof value.id !== "string" && typeof value.id !== "number")
    || value.method !== "wallet_connect"
    || !Array.isArray(value.params) || value.params.length !== 1
    || !isRecord(value.params[0])) {
    throw new Error("Device authorization requires one wallet_connect JSON-RPC request.");
  }
  const capabilities = value.params[0].capabilities;
  if (!isRecord(capabilities) || !isRecord(capabilities.auth)
    || !Array.isArray(capabilities.auth.resources)) {
    throw new Error("The CLI wallet_connect request must include signed auth resources.");
  }
  const resources = capabilities.auth.resources;
  if (!resources.every((resource) => typeof resource === "string")
    || new Set(resources).size !== resources.length
    || [...requiredResources].some((resource) => !resources.includes(resource))
    || resources.some((resource) => !isAllowedResource(resource))) {
    throw new Error("The CLI wallet_connect resources are invalid.");
  }
  const signedResources = resources as string[];
  const browserCookieResources = signedResources.filter((resource) => (
    resource.startsWith("urn:nanocodex:browser-cookies")
  ));
  if (browserCookieResources.length > 1
    || (browserCookieResources.length === 1
      && parseCliBrowserCookieSyncResource(browserCookieResources[0]) === undefined)) {
    throw new Error("The CLI browser cookie sync resource must select one canonical origin.");
  }
  const requestedConnectors = connectorResources(signedResources);
  const credentialImport = credentialImportDigestFromResources(signedResources);
  if (credentialImport !== undefined && !requestedConnectors.has("chatgpt")) {
    throw new Error("A ChatGPT credential import requires the signed ChatGPT connector.");
  }
  const focused = signedResources
    .filter((resource) => resource.startsWith(connectorFocusPrefix))
    .map((resource) => resource.slice(connectorFocusPrefix.length));
  if (focused.length > 1
    || (focused[0] !== undefined && !requestedConnectors.has(focused[0]))) {
    throw new Error("The CLI connector focus must name one requested connector.");
  }
  validateMcpResources(signedResources);
  if (!isRecord(capabilities.authorizeAccessKey)) {
    throw new Error("CLI requests must include one prepared installation access-key policy.");
  }
  const accessKey = capabilities.authorizeAccessKey;
  const mpp = signedResources.includes("urn:nanocodex:mpp:machusd:spend");
  validateInstallationAccessKey(accessKey, mpp);
  return Object.freeze({
    id: value.id,
    method: value.method,
    params: value.params,
    resources: Object.freeze([...signedResources]),
  });
}

export function approvedCliAccessKeyMatches(pending: unknown, approved: unknown): boolean {
  if (!isRecord(pending) || !Array.isArray(pending.params) || pending.params.length !== 1
    || !isRecord(pending.params[0]) || !isRecord(pending.params[0].capabilities)
    || !isRecord(pending.params[0].capabilities.authorizeAccessKey)
    || !isRecord(approved)) return false;
  const requested = pending.params[0].capabilities.authorizeAccessKey;
  const auth = pending.params[0].capabilities.auth;
  const resources = isRecord(auth) ? auth.resources : undefined;
  const mpp = Array.isArray(resources)
    && resources.includes("urn:nanocodex:mpp:machusd:spend");
  try {
    validateInstallationAccessKey(requested, mpp);
  } catch {
    return false;
  }
  const expectedLimits = requested.limits.map((limit) => ({
    token: limit.token.toLowerCase(),
    limit: BigInt(limit.limit).toString(),
    period: limit.period,
  }));
  const actualLimits = Array.isArray(approved.limits)
    ? approved.limits.map((limit) => isRecord(limit) ? ({
      token: String(limit.token).toLowerCase(),
      limit: String(limit.limit),
      period: limit.period,
    }) : undefined)
    : [];
  const expectedScopes = requested.scopes.map(normalizeScope).map(scopeKey).sort();
  let actualScopes: string[];
  try {
    actualScopes = Array.isArray(approved.scopes)
      ? approved.scopes.map(normalizeScope).map(scopeKey).sort()
      : [];
  } catch {
    return false;
  }
  return String(approved.address).toLowerCase() === requested.address.toLowerCase()
    && String(approved.key_id).toLowerCase() === requested.address.toLowerCase()
    && approved.chain_id === "4217"
    && approved.key_type === "secp256k1"
    && approved.expiry === requested.expiry
    && JSON.stringify(actualLimits) === JSON.stringify(expectedLimits)
    && JSON.stringify(actualScopes) === JSON.stringify(expectedScopes);
}

function validateInstallationAccessKey(
  value: UnknownRecord,
  mpp: boolean,
): asserts value is InstallationAccessKey {
  const allowed = new Set([
    "address", "publicKey", "keyType", "chainId", "expiry", "limits", "scopes",
  ]);
  const now = Math.floor(Date.now() / 1_000);
  if (Object.keys(value).some((key) => !allowed.has(key))
    || typeof value.address !== "string" || !/^0x[0-9a-fA-F]{40}$/.test(value.address)
    || typeof value.publicKey !== "string" || !/^0x04[0-9a-fA-F]{128}$/.test(value.publicKey)
    || value.keyType !== "secp256k1"
    || value.chainId !== chainId
    || !isSafeInteger(value.expiry)
    || value.expiry <= now - accessKeyClockSkew
    || value.expiry > now + accessKeyLifetime + accessKeyClockSkew
    || !Array.isArray(value.limits)
    || !Array.isArray(value.scopes)) {
    throw new Error("The CLI installation access-key policy is invalid.");
  }
  let derivedAddress;
  try {
    derivedAddress = Address.fromPublicKey(
      PublicKey.fromHex(value.publicKey as `0x${string}`),
    );
  } catch {
    throw new Error("The CLI installation public key is invalid.");
  }
  if (derivedAddress.toLowerCase() !== value.address.toLowerCase()) {
    throw new Error("The CLI installation public key does not match its address.");
  }
  const limits = value.limits;
  const normalizedLimits = limits.map(normalizeLimit);
  if (!mpp) {
    const expectedLimits = [
      { token: machineUsd, limit: "0x0", period: 0 },
      { token: usdcE, limit: "0x0", period: 0 },
    ];
    if (JSON.stringify(normalizedLimits) !== JSON.stringify(expectedLimits)
      || value.scopes.length !== 0) {
      throw new Error("A CLI installation key without MPP must explicitly disable spending and contract calls.");
    }
    return;
  }
  const expectedLimits = [
    { token: machineUsd, limit: mppLimit, period: mppPeriod },
    { token: usdcE, limit: mppLimit, period: mppPeriod },
  ];
  const scopes = value.scopes.map(normalizeScope);
  const expectedScopes = [
    { address: usdcE, selector: "0xa9059cbb", recipients: [mercatorSettlement] },
    { address: usdcE, selector: "0x95777d59", recipients: [mercatorSettlement] },
    { address: machineUsd, selector: "0xa9059cbb", recipients: [mercatorSettlement] },
    { address: machineUsd, selector: "0x95777d59", recipients: [mercatorSettlement] },
    { address: tip20ChannelEscrow, selector: "0xedc53b00" },
    { address: tip20ChannelEscrow, selector: "0xdc48471e" },
  ];
  if (JSON.stringify(normalizedLimits) !== JSON.stringify(expectedLimits)
    || JSON.stringify(scopes) !== JSON.stringify(expectedScopes)) {
    throw new Error("The CLI MPP access-key policy is invalid.");
  }
}

function normalizeLimit(value: unknown): NormalizedLimit {
  if (!isRecord(value)
    || Object.keys(value).some((key) => !["token", "limit", "period"].includes(key))
    || typeof value.token !== "string" || !/^0x[0-9a-fA-F]{40}$/.test(value.token)
    || typeof value.limit !== "string" || !/^0x[0-9a-fA-F]+$/.test(value.limit)
    || !isSafeInteger(value.period) || value.period < 0) {
    throw new Error("The CLI token limit is invalid.");
  }
  return {
    token: value.token.toLowerCase(),
    limit: value.limit.toLowerCase(),
    period: value.period,
  };
}

function normalizeScope(value: unknown): NormalizedScope {
  if (!isRecord(value)
    || Object.keys(value).some((key) => !["address", "selector", "recipients"].includes(key))
    || typeof value.address !== "string" || !/^0x[0-9a-fA-F]{40}$/.test(value.address)
    || typeof value.selector !== "string" || !/^0x[0-9a-fA-F]{8}$/.test(value.selector)
    || (value.recipients !== undefined && (!Array.isArray(value.recipients)
      || value.recipients.length === 0
      || value.recipients.some((item) => typeof item !== "string"
        || !/^0x[0-9a-fA-F]{40}$/.test(item))))) {
    throw new Error("The CLI MPP call scope is invalid.");
  }
  return {
    address: value.address.toLowerCase(),
    selector: value.selector.toLowerCase(),
    ...(value.recipients === undefined
      ? {}
      : { recipients: (value.recipients as string[]).map((item) => item.toLowerCase()) }),
  };
}

function scopeKey(value: NormalizedScope): string {
  return `${value.address}:${value.selector}:${value.recipients?.join(",") ?? ""}`;
}

export function parseCliRegisterBody(value: unknown): ParsedCliWalletRequest {
  if (!isRecord(value) || !isRecord(value.message)
    || value.message.type !== "rpc-requests"
    || !Array.isArray(value.message.payload)
    || value.message.payload.length !== 1) {
    throw new Error("Device registration requires one RPC request envelope.");
  }
  return parseCliWalletRequest(value.message.payload[0]);
}

export function sanitizeCliWalletResult(value: unknown): SanitizedCliWalletResult {
  if (!isRecord(value) || Object.keys(value).some((key) => key !== "accounts")
    || !Array.isArray(value.accounts) || value.accounts.length !== 1) {
    throw new Error("Accounts did not return exactly one connected account.");
  }
  const account = value.accounts[0];
  if (!isRecord(account)
    || Object.keys(account).some((key) => key !== "address" && key !== "capabilities")
    || typeof account.address !== "string" || !/^0x[0-9a-fA-F]{40}$/.test(account.address)
    || !isRecord(account.capabilities)) {
    throw new Error("Accounts returned an invalid connected account.");
  }
  const capabilities = account.capabilities;
  const auth = isRecord(capabilities.auth) ? capabilities.auth : undefined;
  if (auth?.mode === "hosted") {
    if (Object.keys(capabilities).some((key) => key !== "auth")
      || Object.keys(auth).some((key) => key !== "approval_id" && key !== "mode")
      || typeof auth.approval_id !== "string"
      || !/^[A-Za-z0-9_-]{43}$/.test(auth.approval_id)) {
      throw new Error("Accounts returned an invalid hosted CLI approval.");
    }
    return {
      accounts: [{
        address: account.address as `0x${string}`,
        capabilities: { auth: { approval_id: auth.approval_id, mode: "hosted" } },
      }],
    };
  }
  if (Object.keys(capabilities).some((key) => (
    key !== "auth" && key !== "keyAuthorization" && key !== "personalSign"
  ))
    || !auth
    || Object.keys(auth).some((key) => key !== "approval_id")
    || typeof auth.approval_id !== "string"
    || !/^[A-Za-z0-9_-]{43}$/.test(auth.approval_id)
    || !isRecord(capabilities.keyAuthorization)
    || !isRecord(capabilities.personalSign)
    || Object.keys(capabilities.personalSign).some((key) => key !== "keyAuthorization")
    || typeof capabilities.personalSign.keyAuthorization !== "string"
    || !/^0x[0-9a-fA-F]+$/.test(capabilities.personalSign.keyAuthorization)) {
    throw new Error("Accounts returned an invalid CLI approval.");
  }
  return {
    accounts: [{
      address: account.address as `0x${string}`,
      capabilities: {
        keyAuthorization: capabilities.keyAuthorization,
        personalSign: {
          keyAuthorization: capabilities.personalSign.keyAuthorization as `0x${string}`,
        },
        auth: { approval_id: auth.approval_id },
      },
    }],
  };
}

export function managedMemoryCapability(
  path: string,
  operation?: unknown,
): "history:read" | "memory:read" | "memory:write" | undefined {
  if (path === "/v1/history/sessions/search"
    || /^\/v1\/history\/sessions\/[^/]+\/read$/.test(path)) return "history:read";
  if (/^\/v1\/memory\/[^/]+$/.test(path) && operation === "delete") return "memory:write";
  if (path !== "/v1/memory") return undefined;
  if (operation === "list") return "memory:read";
  if (operation === "scan" || operation === "read") return "memory:read";
  if (operation === "put" || operation === "delete") return "memory:write";
  return undefined;
}

export function requestedConnectorsSatisfied(
  connected: readonly string[],
  requested: readonly string[],
): boolean {
  if (!Array.isArray(connected) || !Array.isArray(requested)) return false;
  const actual = new Set(connected);
  return actual.size === requested.length && requested.every((connector) => actual.has(connector));
}

function isAllowedResource(resource: string): boolean {
  if (requiredResources.has(resource) || optionalResources.has(resource)) return true;
  if (isAllowedMcpResource(resource)) return true;
  if (isAllowedAppToolCatalogResource(resource)) return true;
  if (isAllowedChatGptCredentialImportResource(resource)) return true;
  if (resource.startsWith(CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX)) {
    return parseCliBrowserCookieSyncResource(resource) !== undefined;
  }
  if (resource.startsWith("urn:nanocodex:connector:")) {
    return connectors.has(resource.slice("urn:nanocodex:connector:".length));
  }
  if (resource.startsWith("urn:nanocodex:connectors:")) {
    const requested = resource.slice("urn:nanocodex:connectors:".length).split(",");
    return requested.length > 0 && new Set(requested).size === requested.length
      && requested.every((connector) => connectors.has(connector));
  }
  if (resource.startsWith(connectorFocusPrefix)) {
    return connectors.has(resource.slice(connectorFocusPrefix.length));
  }
  if (resource.startsWith("urn:nanocodex:agent:visibility:")) {
    const requested = resource.slice("urn:nanocodex:agent:visibility:".length).split(",");
    return requested.length > 0 && new Set(requested).size === requested.length
      && requested.every((permission) => visibility.has(permission));
  }
  return false;
}

function connectorResources(resources: readonly string[]): Set<string> {
  return new Set(resources.flatMap((resource) => {
    if (resource.startsWith("urn:nanocodex:connector:")) {
      return [resource.slice("urn:nanocodex:connector:".length)];
    }
    if (resource.startsWith("urn:nanocodex:connectors:")) {
      return resource.slice("urn:nanocodex:connectors:".length).split(",");
    }
    return [];
  }).filter((connector) => connectors.has(connector)));
}

function isSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value);
}

function isRecord(value: unknown): value is UnknownRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isIpAddress(value: string): boolean {
  if (/^(?:\d{1,3}\.){3}\d{1,3}$/.test(value)) {
    return value.split(".").every((part) => Number(part) <= 255);
  }
  return value.includes(":") && /^[0-9a-f:.]+$/i.test(value);
}
