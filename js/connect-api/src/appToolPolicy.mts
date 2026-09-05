export const CHROME_EXTENSION_APP_ID = "nanocodex-chrome";
export const APP_TOOL_CATALOG_RESOURCE_PREFIX = "urn:nanocodex:app-tool-catalog:sha256:";
export const BROWSER_COOKIE_SYNC_RESOURCE = "urn:nanocodex:browser-cookies:sync";
export const CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX =
  "urn:nanocodex:browser-cookies:local-sync:";
const CHROME_EXTENSION_ORIGIN = /^chrome-extension:\/\/[a-p]{32}$/;
const AGENT_CONVERSATION_RESOURCE = /^urn:nanocodex:agent:conversation:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export function isAllowedAppToolCatalogResource(resource: unknown): boolean {
  return typeof resource === "string"
    && new RegExp(`^${APP_TOOL_CATALOG_RESOURCE_PREFIX}[0-9a-f]{64}$`).test(resource);
}

export function appToolCatalogDigestFromResources(
  resources: readonly string[],
): `0x${string}` | undefined {
  const values = resources.filter((resource) => resource.startsWith(APP_TOOL_CATALOG_RESOURCE_PREFIX));
  if (values.length === 0) return undefined;
  if (values.length !== 1 || !isAllowedAppToolCatalogResource(values[0])) {
    throw new Error("The signed app tool catalog resource is invalid.");
  }
  return `0x${values[0].slice(APP_TOOL_CATALOG_RESOURCE_PREFIX.length)}`;
}

export function formatCliBrowserCookieSyncResource(origin: unknown): string {
  const canonical = canonicalBrowserCookieOrigin(origin);
  if (!canonical) throw new TypeError("CLI browser cookie sync requires one canonical origin.");
  return `${CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX}${encodeURIComponent(canonical)}`;
}

export function parseCliBrowserCookieSyncResource(resource: unknown): string | undefined {
  if (typeof resource !== "string"
    || !resource.startsWith(CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX)) return undefined;
  let decoded: string;
  try {
    decoded = decodeURIComponent(resource.slice(CLI_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX.length));
  } catch {
    return undefined;
  }
  const canonical = canonicalBrowserCookieOrigin(decoded);
  return canonical && resource === formatCliBrowserCookieSyncResource(canonical)
    ? canonical
    : undefined;
}

function canonicalBrowserCookieOrigin(value: unknown): string | undefined {
  if (typeof value !== "string" || value.length === 0 || value.length > 2_048) return undefined;
  let url: URL;
  try { url = new URL(value); } catch { return undefined; }
  if (url.origin !== value || url.username || url.password) return undefined;
  if (url.protocol === "https:") return url.origin;
  if (url.protocol !== "http:") return undefined;
  const hostname = url.hostname.toLowerCase();
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]"
    ? url.origin
    : undefined;
}

export function isChromeExtensionGrantResources(
  resources: readonly string[],
  appId: string,
  origin: string,
): boolean {
  if (appId !== CHROME_EXTENSION_APP_ID
    || typeof origin !== "string" || !CHROME_EXTENSION_ORIGIN.test(origin)
    || !Array.isArray(resources) || new Set(resources).size !== resources.length
    || resources.some((resource) => typeof resource !== "string")) return false;
  const catalogs = resources.filter((resource) => resource.startsWith(APP_TOOL_CATALOG_RESOURCE_PREFIX));
  const conversations = resources.filter((resource) => resource.startsWith("urn:nanocodex:agent:conversation:"));
  if (catalogs.length !== 1 || !isAllowedAppToolCatalogResource(catalogs[0])
    || conversations.length > 1
    || conversations.some((resource) => !AGENT_CONVERSATION_RESOURCE.test(resource))) return false;
  const allowed = new Set([
    "urn:nanocodex:agent:run",
    `urn:nanocodex:app:${encodeURIComponent(appId)}`,
    `urn:nanocodex:origin:${encodeURIComponent(origin)}`,
    "urn:nanocodex:connectors:chatgpt",
    BROWSER_COOKIE_SYNC_RESOURCE,
    "urn:nanocodex:agent:visibility:reply,actions,history,traces",
    "urn:nanocodex:authorization:hosted",
    catalogs[0],
    ...conversations,
  ]);
  return resources.length === allowed.size && resources.every((resource) => allowed.has(resource));
}
