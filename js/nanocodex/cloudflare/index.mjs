import module from "../pkg-web/nanocodex_bg.wasm";
import { bindAgent } from "./Agent.mjs";

/** Cloudflare Durable Object Agent backed by the package's compiled WASM. */
export const Agent = bindAgent(module);
export { cloudflareEgress } from "./egress.mjs";
export { createWorkersAiResponses } from "./workers-ai-responses.mjs";
export { createGatewayResponses } from "./gateway-responses.mjs";
export { createSubagentRouting } from "../runtime/subagent-routing.mjs";
