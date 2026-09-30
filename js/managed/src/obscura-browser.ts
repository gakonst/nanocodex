import type { BrowserBinding } from "agents/browser";

/** Build-owned modules, never code or bindings supplied by a browser tool call. */
export type ObscuraBrowserBundle = Readonly<{
  compatibilityDate: string;
  mainModule: string;
  modules: WorkerLoaderWorkerCode["modules"];
}>;

export type ObscuraBrowserOptions = Readonly<{
  bundle: ObscuraBrowserBundle;
  /**
   * A native Fetcher already bound to the account/session public-network policy.
   * Must reject credentials, Vault headers and unauthorized connector access,
   * and recheck every redirect. Never pass the raw NANOCODEX/egress binding.
   * Omission gives the engine no page network capability.
   */
  network?: Fetcher;
}>;

/**
 * A structural Agents SDK BrowserBinding. The loaded Worker owns the session
 * HTTP endpoints and CDP WebSocket; the account owns assets and network policy.
 * No env.BROWSER dependency, remote browser, or global fetch fallback exists.
 */
export function createObscuraBrowserBinding(
  loader: WorkerLoader,
  options: ObscuraBrowserOptions,
): BrowserBinding {
  if (!options.bundle.modules[options.bundle.mainModule]) {
    throw new TypeError("Obscura bundle is missing its main module");
  }
  const workerName = `obscura:${crypto.randomUUID()}`;
  const code = {
    compatibilityDate: options.bundle.compatibilityDate,
    mainModule: options.bundle.mainModule,
    modules: options.bundle.modules,
    globalOutbound: null,
    env: options.network ? { NETWORK: options.network } : {},
  };
  return {
    fetch(input, init) {
      const request = new Request(input, init);
      const url = new URL(request.url);
      if (
        url.origin !== "https://localhost" ||
        !/^\/v1\/devtools\/browser(?:\/[^/]+(?:\/json\/(?:list|protocol))?)?$/.test(
          url.pathname,
        )
      ) {
        return Promise.resolve(new Response(null, { status: 404 }));
      }
      return loader
        .get(workerName, () => code)
        .getEntrypoint()
        .fetch(request);
    },
  };
}
