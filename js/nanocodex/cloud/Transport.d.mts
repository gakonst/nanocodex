export const DEFAULT_API_URL: "https://api.nanocodex.xyz";
export const MOCK_ACCOUNT_ADDRESS: `0x${string}`;
export const MOCK_MACHINE_USD_ADDRESS: `0x${string}`;

export type Request = Readonly<{
  path: string;
  method?: "GET" | "POST" | "DELETE" | undefined;
  headers?: HeadersInit | undefined;
  body?: unknown;
  signal?: AbortSignal | undefined;
}>;

export type Instance = Readonly<{
  baseUrl: string;
  fetch?(input: RequestInfo | URL, init?: RequestInit | undefined): Promise<Response>;
  request(request: Request): Promise<unknown>;
}>;

export type Transport<type extends string = string> = Readonly<{
  key: string;
  name: string;
  type: type;
  setup(options: Readonly<{ appId: string }>): Instance;
}>;

export function from<const type extends string>(parameters: Readonly<{
  key: string;
  name: string;
  type: type;
  setup(options: Readonly<{ appId: string }>): Instance;
}>): Transport<type>;

export function http(url?: string | undefined, options?: Readonly<{
  credentials?: RequestCredentials | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  key?: string | undefined;
  name?: string | undefined;
}>): Transport<"http">;

export function mock(options?: Readonly<{
  accountAddress?: `0x${string}` | undefined;
  appName?: string | undefined;
  appOrigin?: string | undefined;
  key?: string | undefined;
  machineUsdAddress?: `0x${string}` | undefined;
  name?: string | undefined;
}>): Transport<"mock">;
