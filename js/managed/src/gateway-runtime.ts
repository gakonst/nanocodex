import type { GatewayResponsesOptions } from "nanocodex/cloudflare/gateway-responses";
import { beginLiveProviderObservation, type ProviderTelemetryStore } from "./provider-telemetry";
import type { ThreadRoute } from "./thread-model-routing";

/** Worker secrets stay in the transport closure, never in thread configuration. */
export type GatewaySecrets = { OPENROUTER_API_KEY?: string; AI_GATEWAY_API_KEY?: string };
export function gatewayAvailability(env: GatewaySecrets) {
  return { openrouter: Boolean(env.OPENROUTER_API_KEY?.trim()), vercel: Boolean(env.AI_GATEWAY_API_KEY?.trim()) };
}
export type GatewayTelemetry = {
  store: Pick<ProviderTelemetryStore, "append">;
  /** Actual execution location; ingress alone cannot establish it. */
  workerColo: string | null;
  clientIngressColo: string | null;
};
export function gatewayRuntime(env: GatewaySecrets, route: ThreadRoute | undefined, assertActive: () => void,
  send: typeof fetch = fetch, telemetry?: GatewayTelemetry): (GatewayResponsesOptions & { fetch: typeof fetch }) | undefined {
  if (!route || route.backend !== "openrouter" && route.backend !== "vercel") return undefined;
  const apiKey = route.backend === "openrouter" ? env.OPENROUTER_API_KEY : env.AI_GATEWAY_API_KEY;
  if (!apiKey?.trim()) throw new Error(`Pinned ${route.backend} route requires its configured Worker secret`);
  const onRequest = telemetry ? () => beginLiveProviderObservation({
    workerColo: telemetry.workerColo, clientIngressColo: telemetry.clientIngressColo,
    backend: route.backend, model: route.model, effort: route.thinking,
  }, telemetry.store) : undefined;
  return { ...(onRequest ? { onRequest } : {}), provider: route.backend, model: route.model, reasoningEffort: route.thinking, apiKey,
    fetch: ((input: RequestInfo | URL, init?: RequestInit) => {
      assertActive();
      return send(input, init);
    }) as typeof fetch,
  };
}
