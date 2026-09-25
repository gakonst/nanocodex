const USER_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
export const API_KEY = /^ncx_live_([A-Za-z0-9_-]{12})_([A-Za-z0-9_-]{43})$/;
const CONNECT_USER_HEADER = "x-nanocodex-connect-user";
const CONNECT_GRANT_ID_HEADER = "x-nanocodex-connect-grant-id";
const CONNECT_CAPABILITIES_HEADER = "x-nanocodex-connect-capabilities";
const CONNECT_CONNECTORS_HEADER = "x-nanocodex-connect-connectors";
const CONNECT_CONNECTOR_CONNECTIONS_HEADER = "x-nanocodex-connect-connector-connections";
const CONNECT_MCP_IDS_HEADER = "x-nanocodex-connect-mcp-ids";
const CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER = "x-nanocodex-connect-app-tool-catalog-digest";
const SESSION_OWNER_ASSERTION = "x-nanocodex-owner-id";
const SESSION_ORGANIZATION_ASSERTION = "x-nanocodex-session-organization-id";
const SESSION_TEAM_ASSERTION = "x-nanocodex-session-team-id";
const SESSION_AUTHORIZATION_EPOCH_ASSERTION = "x-nanocodex-authorization-epoch";
const SESSION_CAPABILITIES_ASSERTION = "x-nanocodex-capabilities";
export function isUserId(value) {
    return typeof value === "string" && USER_ID.test(value);
}
function isUuid(value) {
    return typeof value === "string" && UUID.test(value);
}
function isOrganizationRole(value) {
    return value === "owner" || value === "writer" || value === "reader";
}
export function isOrganizationCapabilities(value) {
    if (!Array.isArray(value) || new Set(value).size !== value.length)
        return false;
    return value.every((capability) => capability === "agents:read"
        || capability === "agents:portability"
        || capability === "agents:write"
        || capability === "api_keys:read"
        || capability === "api_keys:write"
        || capability === "history:read"
        || capability === "memory:read"
        || capability === "memory:write"
        || capability === "tools:use"
        || capability === "organization:read"
        || capability === "organization:write");
}
export function isApiKeyBase(value) {
    if (typeof value !== "object" || value === null)
        return false;
    const record = value;
    return typeof record.id === "string"
        && /^[A-Za-z0-9_-]{12}$/.test(record.id)
        && typeof record.label === "string"
        && record.label.length <= 120
        && record.prefix === `ncx_live_${record.id}`
        && Number.isFinite(record.createdAt)
        && typeof record.digest === "string"
        && /^[A-Za-z0-9_-]{43}$/.test(record.digest)
        && isUserId(record.userId);
}
export function isStoredApiKey(value) {
    if (!isApiKeyBase(value))
        return false;
    const record = value;
    return isUuid(record.organizationId)
        && isUuid(record.teamId)
        && isOrganizationRole(record.role)
        && isOrganizationCapabilities(record.capabilities)
        && Number.isSafeInteger(record.authorizationEpoch)
        && Number(record.authorizationEpoch) >= 1;
}
export function forwardPrincipalAssertions(headers, principal) {
    headers.set("x-nanocodex-request-principal", JSON.stringify({ kind: principal.kind, user_id: principal.userId }));
    headers.set(SESSION_OWNER_ASSERTION, principal.userId);
    headers.set(SESSION_ORGANIZATION_ASSERTION, principal.organizationId);
    headers.set(SESSION_TEAM_ASSERTION, principal.teamId);
    headers.set(SESSION_AUTHORIZATION_EPOCH_ASSERTION, String(principal.authorizationEpoch));
    headers.set(SESSION_CAPABILITIES_ASSERTION, JSON.stringify(principal.capabilities));
    for (const name of [
        CONNECT_USER_HEADER,
        CONNECT_GRANT_ID_HEADER,
        CONNECT_CAPABILITIES_HEADER,
        CONNECT_CONNECTORS_HEADER,
        CONNECT_CONNECTOR_CONNECTIONS_HEADER,
        CONNECT_MCP_IDS_HEADER,
        CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER,
        "x-nanocodex-connect-sandbox-execution",
    ]) {
        headers.delete(name);
    }
    if (principal.connectGrant) {
        if (principal.connectGrant.sandboxExecution === true) {
            headers.set("x-nanocodex-connect-sandbox-execution", "true");
        }
        headers.set(CONNECT_GRANT_ID_HEADER, principal.connectGrant.grantId);
        headers.set(CONNECT_CONNECTORS_HEADER, JSON.stringify(principal.connectGrant.connectors));
        if (principal.connectGrant.connectorConnections !== undefined) {
            headers.set(CONNECT_CONNECTOR_CONNECTIONS_HEADER, JSON.stringify(principal.connectGrant.connectorConnections));
        }
        headers.set(CONNECT_MCP_IDS_HEADER, JSON.stringify(principal.connectGrant.mcpIds));
        if (principal.connectGrant.appToolCatalogDigest !== undefined) {
            headers.set(CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER, principal.connectGrant.appToolCatalogDigest);
        }
    }
}
async function sha256(value) {
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
    let binary = "";
    for (const byte of digest)
        binary += String.fromCharCode(byte);
    return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
export async function apiKeyDigest(request) {
    const authorization = request.headers.get("authorization");
    if (!authorization?.startsWith("Bearer "))
        return;
    const token = authorization.slice("Bearer ".length);
    return API_KEY.test(token) ? sha256(token) : undefined;
}
export function apiKeyPrincipal(record, digest) {
    if (!isStoredApiKey(record) || record.digest !== digest)
        return;
    return { kind: "api_key", userId: record.userId, organizationId: record.organizationId, teamId: record.teamId, role: record.role, subjectId: `api_key:${record.id}`, credentialId: record.id, authorizationEpoch: record.authorizationEpoch, capabilities: record.capabilities };
}
