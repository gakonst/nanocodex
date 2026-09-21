import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";
import { createWorkersAiResponses } from "../cloudflare/workers-ai-responses.mjs";
const transport = createGatewayResponses({ provider: "vercel", model: "gpt-6-astra", reasoningEffort: "high", apiKey: "synthetic", fetch: async () => new Response() });
const base: string = transport.apiBaseUrl;
void base;
const stateless: true = transport.stateless;
void stateless;
createWorkersAiResponses({ async run(model) { const pinned: "gpt-6-astra" = model; return { pinned }; } }, { model: "gpt-6-astra" });
// @ts-expect-error unsupported provider
createGatewayResponses({ provider: "other", model: "gpt-6-astra", reasoningEffort: "high", apiKey: "synthetic" });
// @ts-expect-error noncanonical model
createGatewayResponses({ provider: "vercel", model: "openai/gpt-6-astra", reasoningEffort: "high", apiKey: "synthetic" });
createGatewayResponses({ provider: "vercel", model: "gpt-6-astra", reasoningEffort: "high", apiKey: "synthetic",
  onRequest: () => ({ headers(status) { const code: number = status; void code; }, async finish(outcome) {
    const result: "success" | "http_error" | "network_error" | "protocol_error" | "timeout" | "cancelled" = outcome;
    void result; return true;
  } }),
});
const ai = { async run(model: string, input: Record<string, unknown>): Promise<unknown> { return { model, input }; } };
createGatewayResponses({ provider: "cloudflare", model: "gpt-5.6-sol", reasoningEffort: "high", ai });
createGatewayResponses({ provider: "cloudflare", model: "gpt-6-astra", reasoningEffort: "medium", ai: {
  async run(model, input) {
    const upstream: "openai/gpt-6-astra" | "openai/gpt-5.6-sol" | "openai/gpt-5.6-terra" | "openai/gpt-5.6-luna" = model;
    return { upstream, input };
  },
} });
// @ts-expect-error Cloudflare requires its AI binding
createGatewayResponses({ provider: "cloudflare", model: "gpt-6-astra", reasoningEffort: "high" });
// @ts-expect-error Cloudflare has no API key
createGatewayResponses({ provider: "cloudflare", model: "gpt-6-astra", reasoningEffort: "high", ai, apiKey: "synthetic" });
// @ts-expect-error Cloudflare has no fetch transport
createGatewayResponses({ provider: "cloudflare", model: "gpt-6-astra", reasoningEffort: "high", ai, fetch: globalThis.fetch });
// @ts-expect-error GLM continues to use the Workers AI transport
createGatewayResponses({ provider: "cloudflare", model: "@cf/zai-org/glm-5.3", reasoningEffort: "high", ai });
// @ts-expect-error HTTP gateways still require an API key
createGatewayResponses({ provider: "openrouter", model: "gpt-6-astra", reasoningEffort: "high" });
// @ts-expect-error HTTP gateways do not accept a binding
createGatewayResponses({ provider: "vercel", model: "gpt-6-astra", reasoningEffort: "high", apiKey: "synthetic", ai });
