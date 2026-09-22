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
