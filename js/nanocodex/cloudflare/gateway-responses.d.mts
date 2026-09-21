import type { ResponsesCanonicalModel, WorkersAiResponsesTransport } from "./workers-ai-responses.mjs";
/** Content-free, attempt-level telemetry. No requests, URLs, errors or response bodies are exposed. */
export type GatewayRequestOutcome = "success" | "http_error" | "network_error" | "protocol_error" | "timeout" | "cancelled";
export type GatewayRequestObserver = Readonly<{
  headers(status: number): void;
  /** Success follows body consumption and protocol validation; HTTP headers are not TTFT. */
  finish(outcome: GatewayRequestOutcome): void | boolean | Promise<void | boolean>;
}>;
export type GatewayResponsesOptions = Readonly<{
  provider: "openrouter" | "vercel";
  model: ResponsesCanonicalModel;
  reasoningEffort: "low" | "medium" | "high";
  /** Server-side credential. Never include in browser state or persisted routing metadata. */
  apiKey: string;
  /** Injectable HTTP transport, defaulting to global fetch. */
  fetch?: typeof globalThis.fetch;
  /** Called once immediately before each outbound attempt. Failures cannot break generation. */
  onRequest?: () => GatewayRequestObserver;
}>;
/** Buffered Responses SSE with complete replay, text/tool history, fixed origin and pinned model/effort. */
export function createGatewayResponses(options: GatewayResponsesOptions): WorkersAiResponsesTransport & Readonly<{ stateless: true }>;
