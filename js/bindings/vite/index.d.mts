export type NanocodexChatGptViteOptions = Readonly<{
  /** Defaults to NANOCODEX_AUTH_FILE, NANOCODEX_CODEX_AUTH_FILE, CODEX_HOME/auth.json, then ~/.codex/auth.json. */
  authFile?: string | URL | undefined;
  /** Same-origin host-managed WebSocket path. Defaults to /api/responses. */
  responsesPath?: `/${string}` | undefined;
}>;

export type NanocodexViteOptions = Readonly<{
  /** Local ChatGPT subscription support is on by default; pass false to disable it. */
  chatGpt?: NanocodexChatGptViteOptions | false | undefined;
  /** Generate and continuously reconcile a review-first WebMCP manifest. */
  webMcp?: true | import("../webmcp/generator.mjs").GenerateWebMcpManifestOptions | false | undefined;
}>;

export type NanocodexVitePlugin = Readonly<{
  name: "nanocodex";
  enforce: "pre";
  resolveId(source: string, importer?: string): string | null;
  configResolved(config: Readonly<{ root?: string; logger?: unknown }>): void;
  config(config: unknown, environment: Readonly<{ command: "build" | "serve" }>): unknown | Promise<unknown>;
  configureServer(server: unknown): void | Promise<void>;
  buildStart(): void | Promise<void>;
  closeBundle(): void | Promise<void>;
}>;

/** Complete local integration for an ordinary Vite browser application. */
export function nanocodex(options?: NanocodexViteOptions): NanocodexVitePlugin;
