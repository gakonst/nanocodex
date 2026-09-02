export const CHROME_EXTENSION_APP_ID = "nanocodex-chrome";
export const APP_TOOL_CATALOG_RESOURCE_PREFIX = "urn:nanocodex:app-tool-catalog:sha256:";
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
    "urn:nanocodex:agent:visibility:reply,actions,history,traces",
    "urn:nanocodex:authorization:hosted",
    catalogs[0],
    ...conversations,
  ]);
  return resources.length === allowed.size && resources.every((resource) => allowed.has(resource));
}
