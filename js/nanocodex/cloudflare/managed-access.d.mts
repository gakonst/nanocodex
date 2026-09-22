/** Credential-bound authority reused for at most 120 seconds; live renewal is separate. */
export type ManagedAccessPrincipal = Readonly<{
  kind: "account_session" | "api_key" | "connect_grant" | "service";
  userId: string;
  organizationId: string;
  teamId: string;
  authorizationEpoch: number;
  capabilities: readonly string[];
  connectGrant?: unknown;
}>;
export type ManagedAccessEnv = { NANOCODEX_ACCESS_SECRET?: string; DEPLOYMENT_SHA?: string };
export type ManagedAccessClaims<P extends ManagedAccessPrincipal = ManagedAccessPrincipal> = {
  version: 1; audience: string; binding: string; issuedAt: number; expiresAt: number; principal: P;
};
export const MANAGED_ACCESS_HEADER: "x-nanocodex-access";
export const MANAGED_ACCESS_TTL_MS: 120000;
export function managedAccessRequest(request: Request): boolean;
export function readManagedAccess<P extends ManagedAccessPrincipal = ManagedAccessPrincipal>(request: Request, env: ManagedAccessEnv, now?: number): Promise<P | undefined>;
export function createManagedAccessClaims<P extends ManagedAccessPrincipal>(request: Request, principal: P, now?: number): Promise<ManagedAccessClaims<P>>;
export function signManagedAccessClaims(claims: ManagedAccessClaims, env: ManagedAccessEnv): Promise<string>;
export function isHandViewerUpgrade(request: Request): boolean;
export function handRequestFailure(request: Request, principal: ManagedAccessPrincipal): "forbidden" | "forbidden_origin" | undefined;
export function handBrokerRequest(request: Request, principal: ManagedAccessPrincipal): Request;
