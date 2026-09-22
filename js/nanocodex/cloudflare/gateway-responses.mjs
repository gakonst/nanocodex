import { createWorkersAiResponses } from "./workers-ai-responses.mjs";

const ENDPOINTS = Object.freeze({
  openrouter: "https://openrouter.ai/api/v1/chat/completions",
  vercel: "https://ai-gateway.vercel.sh/v1/chat/completions",
});
const MODELS = ["@cf/zai-org/glm-5.3", "gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];
const fail = message => { throw new Error(`Gateway Responses: ${message}`); };

/** Server-side, buffered/full-history transport; no WebSocket or opaque compaction. */
export function createGatewayResponses(options) {
  const { provider, model, reasoningEffort, apiKey, fetch: fetchImpl = globalThis.fetch } = options;
  if (!Object.hasOwn(ENDPOINTS, provider)) fail("unsupported provider");
  if (!MODELS.includes(model)) fail("unsupported canonical model");
  if (!["low", "medium", "high"].includes(reasoningEffort)) fail("unsupported reasoning effort");
  if (typeof apiKey !== "string" || !apiKey.trim() || /[\r\n]/.test(apiKey)) fail("a server-side API key is required");
  if (typeof fetchImpl !== "function") fail("fetch is required");
  const endpoint = ENDPOINTS[provider];
  const gatewayModel = model === MODELS[0]
    ? (provider === "openrouter" ? "z-ai/glm-5.3" : "zai/glm-5.3") : `openai/${model}`;
  const apiBaseUrl = `https://${provider}-responses.invalid/v1`;
  const adapter = signal => createWorkersAiResponses({
    async run(_model, input) {
      signal?.throwIfAborted();
      if (input.reasoning_effort !== undefined && input.reasoning_effort !== reasoningEffort) fail("reasoning override does not match pinned effort");
      // Vercel documents reasoning_effort as the Chat Completions alias:
      // https://vercel.com/docs/ai-gateway/sdks-and-apis/openai-chat-completions/reasoning
      const payload = { ...input, model: gatewayModel, reasoning_effort: reasoningEffort };
      if (provider === "openrouter") {
        delete payload.reasoning_effort;
        payload.reasoning = { effort: reasoningEffort };
        payload.provider = { require_parameters: true };
      }
      let response;
      try {
        response = await fetchImpl(endpoint, { method: "POST", redirect: "error", signal,
          headers: { "content-type": "application/json", authorization: `Bearer ${apiKey}` },
          body: JSON.stringify(payload) });
      } catch {
        signal?.throwIfAborted();
        fail("provider request failed");
      }
      if (!response.ok) {
        // Never parse, quote, or retain an upstream error body or status text.
        try { await response.body?.cancel(); } catch { /* best effort release */ }
        fail("provider rejected request");
      }
      try { return await response.json(); }
      catch { signal?.throwIfAborted(); fail("invalid provider response"); }
    },
  }, { model, apiBaseUrl });
  return Object.freeze({ apiBaseUrl, stateless: true,
    async createResponse(endpoint, sessionId, request) {
      try { return await adapter(request.signal).createResponse(endpoint, sessionId, request); }
      catch {
        request.signal?.throwIfAborted();
        // Adapter validation must not echo untrusted request/provider fields either.
        fail("request failed or is incompatible with the pinned model and effort");
      }
    },
  });
}
