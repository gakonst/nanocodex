import { createManagedAccessClaims, readManagedAccess, signManagedAccessClaims, handBrokerRequest, type ManagedAccessPrincipal } from 'nanocodex/cloudflare/managed-access';
const request = new Request('https://account.test/v1/account/hands/screens');
const principal = {} as ManagedAccessPrincipal & { credentialId: string };
const env = { NANOCODEX_ACCESS_SECRET: 'fixture' };
const claims = await createManagedAccessClaims(request, principal);
const credential: string = claims.principal.credentialId;
const token: string = await signManagedAccessClaims(claims, env);
const cached = await readManagedAccess<typeof principal>(request, env);
if (cached) { const id: string = cached.credentialId; handBrokerRequest(request, cached); }
void credential; void token;
