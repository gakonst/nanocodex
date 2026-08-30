import type { ToolContext } from "../types.mjs";

export type WebMcpActionRequest = Readonly<{
  kind: "webmcp" | "semantic" | "published";
  name: string;
  title?: string | undefined;
  origin?: string | undefined;
  input: unknown;
  readOnly: false;
  element?: Readonly<Record<string, unknown>> | undefined;
}>;

export type WebMcpProviderOptions = Readonly<{
  document?: Document | undefined;
  native?: true | false | "require" | undefined;
  fallback?: true | false | "always" | "when-empty" | "never" | undefined;
  fromOrigins?: readonly string[] | undefined;
  sourceId?: string | undefined;
  maxElements?: number | undefined;
  maxTextChars?: number | undefined;
  confirm?(request: WebMcpActionRequest): boolean | Promise<boolean>;
}>;

export type WebMcpToolProvider = Readonly<{
  sourceId: string;
  kind: "webmcp";
  mode: "union";
  deferred: true;
  definitions(): readonly Record<string, unknown>[];
  resolve(name: string): Readonly<{
    name: string;
    parallelSafe: boolean;
    handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
  }> | undefined;
  settled(): Promise<void>;
  refresh(): Promise<void>;
  subscribe(listener: (definitions: readonly Record<string, unknown>[]) => void): () => void;
  close(): void;
}>;

export type GeneratedWebMcpTool = Readonly<{
  name: string;
  title?: string | undefined;
  description: string;
  approved: boolean;
  inputSchema?: Record<string, unknown> | undefined;
  annotations?: Readonly<{
    readOnlyHint?: boolean | undefined;
    untrustedContentHint?: boolean | undefined;
  }> | undefined;
  implementation?: Readonly<{
    kind: "fetch" | "form" | "custom";
    method?: string | undefined;
    path?: string | undefined;
    selector?: string | undefined;
  }> | undefined;
  evidence?: readonly Readonly<Record<string, unknown>>[] | undefined;
}>;

export type WebMcpManifest = Readonly<{
  version: number;
  generatedAt?: string | undefined;
  tools: readonly GeneratedWebMcpTool[];
}>;

export function createProvider(options?: WebMcpProviderOptions): Promise<WebMcpToolProvider>;
export function isProvider(value: unknown): value is WebMcpToolProvider;

export function publish(manifest: WebMcpManifest, options?: {
  document?: Document | undefined;
  exposedTo?: readonly string[] | undefined;
  baseUrl?: string | URL | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  handlers?: Readonly<Record<string, (input: unknown, context: { signal: AbortSignal }) => unknown | Promise<unknown>>> | undefined;
  confirm?(request: WebMcpActionRequest): boolean | Promise<boolean>;
}): Promise<Readonly<{
  tools: readonly string[];
  close(): void;
}>>;
