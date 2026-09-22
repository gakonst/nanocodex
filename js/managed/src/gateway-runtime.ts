import type { ThreadRoute } from "./thread-model-routing";

/** Worker secrets stay in the transport closure, never in thread configuration. */
export type GatewaySecrets = { OPENROUTER_API_KEY?: string; AI_GATEWAY_API_KEY?: string };
export function gatewayAvailability(env: GatewaySecrets) {
  return { openrouter: Boolean(env.OPENROUTER_API_KEY?.trim()), vercel: Boolean(env.AI_GATEWAY_API_KEY?.trim()) };
}
export function gatewayRuntime(env: GatewaySecrets, route: ThreadRoute | undefined, assertActive: () => void,
  send: typeof fetch = fetch) {
  if (!route || route.backend !== "openrouter" && route.backend !== "vercel") return undefined;
  const apiKey = route.backend === "openrouter" ? env.OPENROUTER_API_KEY : env.AI_GATEWAY_API_KEY;
  if (!apiKey?.trim()) throw new Error(`Pinned ${route.backend} route requires its configured Worker secret`);
  return { provider: route.backend, model: route.model, reasoningEffort: route.thinking, apiKey,
    fetch: ((input: RequestInfo | URL, init?: RequestInit) => {
      assertActive();
      return send(input, init);
    }) as typeof fetch,
  };
}
