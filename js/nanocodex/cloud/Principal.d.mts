export type Exchange = Readonly<{ token: string; expiresAt: number }>;

export type Instance = Readonly<{
  create(options: Readonly<{
    resources: readonly string[];
    signal?: AbortSignal | undefined;
  }>): Promise<Exchange>;
}>;

export type Principal<type extends string = string> = Readonly<{
  key: string;
  name: string;
  type: type;
  setup(context: Readonly<{
    appId: string;
    appOrigin: string | undefined;
  }>): Instance;
}>;

export function host(options?: Readonly<{
  /** Same-origin application route that verifies the current host login. */
  url?: string | URL | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  key?: string | undefined;
  name?: string | undefined;
}>): Principal<"host">;
