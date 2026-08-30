export type Session = Readonly<{ token: string; expiresAt: number }>;

export type Instance = Readonly<{
  getSession(options?: Readonly<{ signal?: AbortSignal | undefined }>): Promise<Session>;
}>;

export type Identity<type extends string = string> = Readonly<{
  key: string;
  name: string;
  type: type;
  setup(context: Readonly<{
    appId: string;
    appOrigin: string | undefined;
  }>): Instance;
}>;

export function from<const type extends string>(parameters: Readonly<{
  key: string;
  name: string;
  type: type;
  setup(context: Readonly<{
    appId: string;
    appOrigin: string | undefined;
  }>): Instance;
}>): Identity<type>;

export function host(options?: Readonly<{
  /** Same-origin application route that returns a Nanocodex identity session. */
  url?: string | URL | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  key?: string | undefined;
  name?: string | undefined;
}>): Identity<"host">;

export function custom(parameters: Readonly<{
  getSession(context: Readonly<{
    appId: string;
    appOrigin: string | undefined;
    signal?: AbortSignal | undefined;
  }>): Promise<Readonly<{ token: string; expires_at: number }>> | Readonly<{
    token: string;
    expires_at: number;
  }>;
  key?: string | undefined;
  name?: string | undefined;
}>): Identity<"custom">;
