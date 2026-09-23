export type OrganizationCapability = "agents:read" | "agents:portability" | "agents:write" | "api_keys:read" | "api_keys:write" | "history:read" | "memory:read" | "memory:write" | "tools:use" | "organization:read" | "organization:write";
export type ApiKeyBase = Readonly<{ id: string; label: string; prefix: string; createdAt: number; digest: string; userId: string }>;
export type StoredApiKey = ApiKeyBase & Readonly<{ organizationId: string; teamId: string; role: "owner" | "writer" | "reader"; capabilities: readonly OrganizationCapability[]; authorizationEpoch: number }>;
export type ApiKeyPrincipal = Readonly<{ kind: "api_key"; userId: string; organizationId: string; teamId: string; role: StoredApiKey["role"]; subjectId: `api_key:${string}`; credentialId: string; authorizationEpoch: number; capabilities: readonly OrganizationCapability[] }>;
export type AdmissionPrincipal = Readonly<{ kind: "api_key" | "account_session" | "connect_grant" | "service"; userId: string; organizationId: string; teamId: string; authorizationEpoch: number; capabilities: readonly string[]; connectGrant?: Readonly<{ grantId: string; connectors: readonly string[]; connectorConnections?: unknown; mcpIds: readonly string[]; appToolCatalogDigest?: string }> }>;
export const API_KEY: RegExp;
export function isUserId(value: unknown): value is string;
export function isOrganizationCapabilities(value: unknown): value is readonly OrganizationCapability[];
export function isApiKeyBase(value: unknown): value is ApiKeyBase;
export function isStoredApiKey(value: unknown): value is StoredApiKey;
export function apiKeyDigest(request: Request): Promise<string | undefined>;
/** Only a record returned by live authorized key resolution may be projected. */
export function apiKeyPrincipal(record: unknown, digest: string): ApiKeyPrincipal | undefined;
/** Replace all authority assertions; callers must supply a live authenticated principal. */
export function forwardPrincipalAssertions(headers: Headers, principal: AdmissionPrincipal): void;
