import type { BrowserHttpRequest } from "../browser/host.mjs";

export type ResponsesCanonicalModel = "@cf/zai-org/glm-5.3" | "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna";

/** Structural Workers AI binding; provider credentials stay with Cloudflare. */
export type WorkersAiBinding<Model extends ResponsesCanonicalModel = "@cf/zai-org/glm-5.3"> = {
  run(model: Model, input: Record<string, unknown>): Promise<unknown>;
};
export type WorkersAiResponsesOptions<Model extends ResponsesCanonicalModel = "@cf/zai-org/glm-5.3"> = Readonly<{
  /** Canonical response identity; defaults to GLM-5.3. */
  model?: Model;
  /** Local transport identity, never fetched. Defaults to https://workers-ai.invalid/v1. */
  apiBaseUrl?: string;
}>;
export type WorkersAiResponsesTransport = Readonly<{
  apiBaseUrl: string;
  createResponse(endpoint: string, sessionId: string, request: BrowserHttpRequest): Promise<Response>;
}>;
/**
 * Buffered, stateless Responses SSE over the GLM-5.3 Workers AI binding.
 * Requires full text history; opaque compaction and unsupported modalities fail explicitly.
 * Custom grammars are supplied as instructions, not enforced by the provider.
 * Cancellation stops waiting; the binding does not expose cancellation of inference.
 */
export function createWorkersAiResponses<Model extends ResponsesCanonicalModel = "@cf/zai-org/glm-5.3">(ai: WorkersAiBinding<Model>, options?: WorkersAiResponsesOptions<Model>): WorkersAiResponsesTransport;
