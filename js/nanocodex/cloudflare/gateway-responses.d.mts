import type { ResponsesCanonicalModel, WorkersAiResponsesTransport } from "./workers-ai-responses.mjs";
export type GatewayResponsesOptions = Readonly<{
  provider: "openrouter" | "vercel";
  model: ResponsesCanonicalModel;
  reasoningEffort: "low" | "medium" | "high";
  /** Server-side credential. Never include in browser state or persisted routing metadata. */
  apiKey: string;
  /** Injectable HTTP transport, defaulting to global fetch. */
  fetch?: typeof globalThis.fetch;
}>;
/** Buffered Responses SSE with complete replay, text/tool history, fixed origin and pinned model/effort. */
export function createGatewayResponses(options: GatewayResponsesOptions): WorkersAiResponsesTransport & Readonly<{ stateless: true }>;
