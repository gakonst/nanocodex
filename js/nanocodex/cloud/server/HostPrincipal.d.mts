export type Claims = Readonly<{
  /** Stable opaque identity-provider namespace. Limited to 512 non-control characters. */
  issuer: string;
  /** Stable opaque tenant namespace within the issuer. Limited to 512 non-control characters. */
  tenant: string;
  /** Stable opaque user subject; limited to 512 non-control characters. */
  subject: string;
  /** Opaque host-login session identifier; limited to 512 non-control characters. */
  sessionId: string;
}>;

export type Exchange = Readonly<{ token: string; expiresAt: number }>;

export type Client = Readonly<{
  create(options: Claims & Readonly<{
    resources: readonly string[];
    expiresIn?: number | undefined;
    signal?: AbortSignal | undefined;
  }>): Promise<Exchange>;
  handler(options: Readonly<{
    authenticate(request: Request): Promise<Claims | undefined> | Claims | undefined;
  }>): (request: Request) => Promise<Response>;
  revoke(options: Claims & Readonly<{
    signal?: AbortSignal | undefined;
  }>): Promise<void>;
}>;

export function create(parameters: Readonly<{
  appId: string;
  appOrigin: string;
  secret: string;
  baseUrl?: string | URL | undefined;
  fetch?: typeof globalThis.fetch | undefined;
}>): Client;
