import type { ResponsesCanonicalModel, WorkersAiResponsesTransport } from "./workers-ai-responses.mjs";
/** Content-free, attempt-level telemetry. No requests, URLs, errors or response bodies are exposed. */
export type GatewayRequestOutcome = "success" | "http_error" | "network_error" | "protocol_error" | "timeout" | "cancelled";
export type GatewayRequestObserver = Readonly<{
  /** HTTP only; binding attempts leave the status unset. */
  headers(status: number): void;
  /** First emitted nonempty text or validated tool output; excludes reasoning and HTTP headers. */
  firstToken?(): void;
  /** Success follows body consumption and protocol validation; HTTP headers are not TTFT. */
  finish(outcome: GatewayRequestOutcome): void | boolean | Promise<void | boolean>;
}>;
type GatewayResponsesCommonOptions = Readonly<{
  reasoningEffort: "low" | "medium" | "high";
  /** Called once immediately before each outbound attempt. Failures cannot break generation. */
  onRequest?: () => GatewayRequestObserver;
}>;
export type GatewayResponsesOptions = GatewayResponsesCommonOptions & (Readonly<{
  provider: "openrouter" | "vercel";
  model: ResponsesCanonicalModel;
  /** Server-side credential. Never include in browser state or persisted routing metadata. */
  apiKey: string;
  /** Injectable HTTP transport, defaulting to global fetch. */
  fetch?: typeof globalThis.fetch;
  ai?: never;
  accountId?: never;
}> | Readonly<{
  provider: "cloudflare";
  model: Exclude<ResponsesCanonicalModel, "@cf/zai-org/glm-5.3">;
  /** Native Responses binding; credentials stay with Cloudflare. Cancellation stops waiting, not inference. */
  ai: { run(model: `openai/${Exclude<ResponsesCanonicalModel, "@cf/zai-org/glm-5.3">}`, input: Record<string, unknown>): Promise<unknown> };
  apiKey?: never;
  fetch?: never;
  accountId?: never;
}> | Readonly<{
  provider: "cloudflare";
  model: Exclude<ResponsesCanonicalModel, "@cf/zai-org/glm-5.3">;
  /** Account-scoped native Responses REST; no provider API key or connector access. */
  accountId: string;
  /** Deployment-owned Cloudflare API token with inference access. */
  apiKey: string;
  fetch?: typeof globalThis.fetch;
  ai?: never;
}>);
/** Incremental (stream:true) or buffered Responses SSE with complete replay and pinned model/effort.
 * Text/reasoning stream incrementally; tools are emitted only after terminal validation.
 * x-nanocodex-inference-buffering reports streaming or buffered binding fallback.
 */
export function createGatewayResponses(options: GatewayResponsesOptions): WorkersAiResponsesTransport & Readonly<{ stateless: true }>;
