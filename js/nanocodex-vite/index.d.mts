export type NanocodexChatGptViteOptions = Readonly<{
  /** Defaults to NANOCODEX_AUTH_FILE, NANOCODEX_CODEX_AUTH_FILE, CODEX_HOME/auth.json, then ~/.codex/auth.json. */
  authFile?: string | URL | undefined;
  /** Same-origin host-managed WebSocket path. Defaults to /api/responses. */
  responsesPath?: `/${string}` | undefined;
}>;

export type NanocodexDevApplication = Readonly<{
  /** Development response headers applied to every request handled by this mount. */
  headers?: Readonly<Record<string, string>> | undefined;
  /** Non-root URL path where the application is served during development. */
  path: `/${string}`;
  /** Application root. Its Vite config and index.html remain owned by the application. */
  root: string | URL;
}>;

export type NanocodexViteOptions = Readonly<{
  /** Local ChatGPT subscription support is on by default; pass false to disable it. */
  chatGpt?: NanocodexChatGptViteOptions | false | undefined;
  /** Sibling Vite applications mounted into this development server. */
  devApplications?: readonly NanocodexDevApplication[] | undefined;
  /** Start the fixed local OAuth callback relay while serving. */
  oauthRelay?: boolean | undefined;
}>;

export type NanocodexVitePlugin = Readonly<{
  name: "nanocodex";
  enforce: "pre";
  resolveId(source: string, importer?: string): string | null;
  config(config: unknown, environment: Readonly<{ command: "build" | "serve" }>): unknown | Promise<unknown>;
  configureServer(server: unknown): void | Promise<void>;
  closeBundle(): void | Promise<void>;
}>;

/** Complete local integration for an ordinary Vite browser application. */
export function nanocodex(options?: NanocodexViteOptions): NanocodexVitePlugin;
