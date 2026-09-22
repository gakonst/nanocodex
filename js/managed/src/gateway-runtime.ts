import type { GatewayResponsesOptions } from "nanocodex/cloudflare/gateway-responses";
import { beginLiveProviderObservation, type ProviderTelemetryStore } from "./provider-telemetry";
import type { ThreadRoute } from "./thread-model-routing";

/** Worker secrets stay in the transport closure, never in thread configuration. */
export type GatewaySecrets = {
  OPENROUTER_API_KEY?: string; AI_GATEWAY_API_KEY?: string;
  AI?: { run(model: string, input: unknown): Promise<unknown> };
  NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED?: string;
  CLOUDFLARE_AI_API_TOKEN?: string;
  NANOCODEX_CLOUDFLARE_ACCOUNT_ID?: string;
};
/** Partial REST configuration is unavailable; never silently switch transports. */
export function cloudflareRestConfig(env: GatewaySecrets) {
  const accountId = env.NANOCODEX_CLOUDFLARE_ACCOUNT_ID;
  const apiKey = env.CLOUDFLARE_AI_API_TOKEN;
  return accountId && /^[a-f0-9]{32}$/i.test(accountId) && typeof apiKey === "string" && /^[\x21-\x7e]+$/.test(apiKey)
    ? { accountId, apiKey } : undefined;
}
export function gatewayAvailability(env: GatewaySecrets) {
  const restSelected = env.CLOUDFLARE_AI_API_TOKEN !== undefined || env.NANOCODEX_CLOUDFLARE_ACCOUNT_ID !== undefined;
  return { openrouter: Boolean(env.OPENROUTER_API_KEY?.trim()), vercel: Boolean(env.AI_GATEWAY_API_KEY?.trim()),
    cloudflare: env.NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED === "true"
      && (restSelected ? Boolean(cloudflareRestConfig(env)) : typeof env.AI?.run === "function") };
}
export type GatewayTelemetry = {
  store: Pick<ProviderTelemetryStore, "append">;
  /** Actual execution location; ingress alone cannot establish it. */
  workerColo: string | null;
  clientIngressColo: string | null;
};
export function gatewayRuntime(env: GatewaySecrets, route: Pick<ThreadRoute, "backend" | "model" | "provider_model" | "thinking"> | undefined, assertActive: () => void,
  send: typeof fetch = fetch, telemetry?: GatewayTelemetry): GatewayResponsesOptions | undefined {
  if (!route || !["openrouter", "vercel", "cloudflare"].includes(route.backend)) return undefined;
  const onRequest = telemetry ? () => beginLiveProviderObservation({
    workerColo: telemetry.workerColo, clientIngressColo: telemetry.clientIngressColo,
    backend: route.backend, model: route.model, effort: route.thinking,
  }, telemetry.store) : undefined;
  if (route.backend === "cloudflare") {
    if (!(route.model === "gpt-6-astra" || route.model === "gpt-5.6-sol" || route.model === "gpt-5.6-terra" || route.model === "gpt-5.6-luna") || !gatewayAvailability(env).cloudflare) throw new Error("Pinned cloudflare route requires its configured transport and frontier gate");
    const rest = cloudflareRestConfig(env);
    if (rest) {
      if (route.provider_model !== `openai/${route.model}`) throw new Error("Cloudflare request does not match pinned model");
      return { ...(onRequest ? { onRequest } : {}), provider: "cloudflare", model: route.model,
        reasoningEffort: route.thinking, ...rest, fetch: ((input, init) => {
          assertActive(); return send(input, init);
        }) as typeof fetch };
    }
    return { ...(onRequest ? { onRequest } : {}), provider: "cloudflare", model: route.model,
      reasoningEffort: route.thinking, ai: { run: (model, input) => {
        assertActive();
        if (model !== route.provider_model || model !== `openai/${route.model}`)
          throw new Error("Cloudflare request does not match pinned model");
        return env.AI!.run(model, input);
      } } };
  }
  if (route.backend !== "openrouter" && route.backend !== "vercel") return undefined;
  const apiKey = route.backend === "openrouter" ? env.OPENROUTER_API_KEY : env.AI_GATEWAY_API_KEY;
  if (!apiKey?.trim()) throw new Error(`Pinned ${route.backend} route requires its configured Worker secret`);
  return { ...(onRequest ? { onRequest } : {}), provider: route.backend, model: route.model, reasoningEffort: route.thinking, apiKey,
    fetch: ((input: RequestInfo | URL, init?: RequestInit) => {
      assertActive();
      return send(input, init);
    }) as typeof fetch,
  };
}
