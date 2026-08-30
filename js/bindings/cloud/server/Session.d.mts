export type Client = Readonly<{
  create(options: Readonly<{
    subject: string;
    organization?: string | undefined;
    expiresIn?: number | undefined;
    signal?: AbortSignal | undefined;
  }>): Promise<Readonly<{ token: string; expires_at: number }>>;
  handler(options: Readonly<{
    authenticate(request: Request): Promise<Readonly<{
      subject: string;
      organization?: string | undefined;
      expiresIn?: number | undefined;
    }> | undefined> | Readonly<{
      subject: string;
      organization?: string | undefined;
      expiresIn?: number | undefined;
    }> | undefined;
  }>): (request: Request) => Promise<Response>;
}>;

export function create(parameters: Readonly<{
  appId: string;
  appOrigin: string;
  secret: string;
  baseUrl?: string | URL | undefined;
  fetch?: typeof globalThis.fetch | undefined;
}>): Client;
