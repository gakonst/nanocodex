export const identitySessionResourcePrefix: "urn:nanocodex:identity-session:";

export type EmbedProject = Readonly<{
  appId: string;
  appOrigin: string;
  secretSha256: string;
}>;

export type EmbedIdentity = Readonly<{
  appId: string;
  appOrigin: string;
  issuer: string;
  subject: string;
  organization?: string | undefined;
}>;

export function parseEmbedProjects(encoded: string | undefined): readonly EmbedProject[];
export function authenticateEmbedProject(encoded: string | undefined, parameters: Readonly<{
  appId: string;
  appOrigin: string;
  secret: string;
}>): Promise<EmbedProject | undefined>;
export function parseEmbedSessionBody(value: unknown): Readonly<{
  appOrigin: string;
  expiresIn: number;
  subject: string;
  organization?: string | undefined;
}>;
export function identitySessionToken(resources: unknown): string | undefined;
export function embedPrincipalId(identity: EmbedIdentity): Promise<string>;
export function isEmbedIdentity(value: unknown): value is EmbedIdentity;
export function sha256Base64Url(value: string): Promise<string>;
