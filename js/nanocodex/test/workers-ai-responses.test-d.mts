import { createWorkersAiResponses, type WorkersAiBinding } from "../cloudflare/workers-ai-responses.mjs";
import type { BrowserHttpRequest } from "../browser/host.mjs";
const binding: WorkersAiBinding = { async run(model, input) {
  const exact: "@cf/zai-org/glm-5.3" = model;
  void exact; void input;
  return { choices: [] };
} };
const transport = createWorkersAiResponses(binding);
const handler: (endpoint: string, sessionId: string, request: BrowserHttpRequest) => Promise<Response> = transport.createResponse;
void handler;
// @ts-expect-error a real AI binding is mandatory
createWorkersAiResponses({});
// @ts-expect-error credentials do not belong to the adapter
createWorkersAiResponses(binding, { apiKey: "no" });
