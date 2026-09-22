import { routeObservation, type RouterObservation } from "./router-telemetry";
import { DurableObject } from "cloudflare:workers";
import { z } from "zod";
import { createGatewayResponses } from "nanocodex/cloudflare/gateway-responses";
import { createWorkersAiResponses } from "nanocodex/cloudflare/workers-ai-responses";
import {
  OSS_MODEL, ROUTING_CANDIDATES, resolveThreadRoute, routingPolicySchema, projectThreadRouteDiagnostics,
  type RoutingAi, type RoutingAvailability, type ThreadRoute, type ThreadRoutingPolicy, type ThreadRouteDiagnostics,
} from "./thread-model-routing";
import { gatewayAvailability, gatewayRuntime } from "./gateway-runtime";
import { PROBE_OWNER } from "./provider-probe-schedule";
import { finalizeInferenceResponse, projectInferenceStream } from "./inference-stream";
import type { ProviderObservation } from "./provider-telemetry";
export type InferenceOrigin = { clientIngressColo: string | null };
export const INFERENCE_INGRESS_HEADER = "x-inference-ingress-colo";
const unknownOrigin: InferenceOrigin = { clientIngressColo: null };
export function inferenceOrigin(value: unknown): InferenceOrigin {
  return { clientIngressColo: typeof value === "string" && /^[A-Z]{3}$/.test(value) ? value : null };
}

/** Deployment bindings only. No account credentials or agent runtime belong here. */
export type InferenceSessionEnv = {
  AI: RoutingAi;
  OPENROUTER_API_KEY?: string;
  AI_GATEWAY_API_KEY?: string;
  CLOUDFLARE_AI_API_TOKEN?: string;
  NANOCODEX_CLOUDFLARE_ACCOUNT_ID?: string;
  NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED?: string;
  NANOCODEX_PROVIDER_PROBES?: string;
  /** Deployment-global, content-free probe aggregates; never an account service. */
  NANOCODEX_PROVIDER_PROBE_COORDINATOR?: {
    getByName(name: string): { snapshot(origin?: InferenceOrigin): Promise<unknown>; observe?(observation: ProviderObservation): Promise<unknown>; observeRoute?(observation: RouterObservation): Promise<unknown> };
  };
};
export const INFERENCE_KEY_ID_HEADER = "x-inference-key-id";
export const INFERENCE_MAX_OUTPUT_TOKENS_HEADER = "x-inference-max-output-tokens";
export const INFERENCE_MAX_BODY_BYTES = 256 * 1024;
export const INFERENCE_MAX_INPUT_BYTES = 32 * 1024;
export const INFERENCE_MAX_OUTPUT_TOKENS = 4096;
export const INFERENCE_TIMEOUT_MS = 120_000;
export const INFERENCE_PROBE_TIMEOUT_MS = 250;
const STORAGE_KEY = "inference_session_v1";
const keyId = z.string().regex(/^[A-Za-z0-9_.:-]{1,256}$/);
const name = z.string().min(1).max(128);
const status = z.enum(["in_progress", "completed", "incomplete"]).optional();
const textPart = z.object({
  type: z.enum(["input_text", "output_text", "text"]), text: z.string(),
  annotations: z.array(z.unknown()).max(0).optional(),
}).strict();
const content = z.union([z.string(), z.array(textPart).max(1024)]);
const historyItem = z.union([
  z.object({ type: z.literal("message").optional(), id: name.optional(), status,
    role: z.enum(["user", "assistant", "system", "developer"]), content }).strict(),
  z.object({ type: z.literal("function_call"), id: name.optional(), status,
    call_id: name, name, arguments: z.string() }).strict(),
  z.object({ type: z.literal("custom_tool_call"), id: name.optional(), status,
    call_id: name, name, input: z.string() }).strict(),
  z.object({ type: z.enum(["function_call_output", "custom_tool_call_output"]),
    id: name.optional(), status, call_id: name, output: content }).strict(),
  z.object({ type: z.literal("reasoning"), id: name.optional(), status,
    summary: z.array(z.object({ type: z.literal("summary_text"), text: z.string() }).strict()).optional(),
    content: z.array(z.object({ type: z.literal("reasoning_text"), text: z.string() }).strict()).optional(),
  }).strict(),
]);
const tool = z.union([
  z.object({ type: z.literal("function"), name, description: z.string().max(8192).optional(),
    parameters: z.record(z.string(), z.unknown()).optional(),
    // The pure adapters do not enforce strict JSON schemas.
    strict: z.union([z.literal(false), z.null()]).optional(),
  }).strict(),
  z.object({ type: z.literal("custom"), name, description: z.string().max(8192).optional(),
    format: z.union([
      z.object({ type: z.literal("text") }).strict(),
      z.object({ type: z.literal("grammar"), syntax: z.enum(["lark", "regex"]), definition: z.string().max(32768) }).strict(),
    ]).optional(),
  }).strict(),
]);
const requestSchema = z.object({
  model: z.string().min(1).max(256).default("auto"), input: z.union([z.string().min(1), z.array(historyItem).min(1).max(1024)]),
  instructions: z.string().optional(), stream: z.boolean().default(false),
  max_output_tokens: z.number().int().min(1).max(INFERENCE_MAX_OUTPUT_TOKENS).optional(),
  reasoning: z.object({ effort: z.enum(["low", "medium", "high"]) }).strict().optional(),
  tools: z.array(tool).max(128).optional(),
  tool_choice: z.union([z.enum(["auto", "none", "required"]),
    z.object({ type: z.enum(["function", "custom"]), name }).strict()]).optional(),
  parallel_tool_calls: z.boolean().optional(),
  temperature: z.number().min(0).max(2).optional(), top_p: z.number().min(0).max(1).optional(),
  text: z.object({ format: z.object({ type: z.literal("text") }).strict() }).strict().optional(),
  store: z.literal(false).optional(),
}).strict();
export type InferenceRequest = z.infer<typeof requestSchema>;
export type InferenceRoute = Pick<ThreadRoute, "version" | "policy_version" | "backend" | "provider_model" |
  "model" | "thinking" | "reasoning_mode" | "fast_mode" | "family" | "confidence" | "objective" |
  "selection" | "created_at" | "router_duration_ms"> & { diagnostics?: ThreadRouteDiagnostics };
export type InferenceSessionMetadata = {
  id: string; key_id: string; routing: ThreadRoutingPolicy; route: InferenceRoute | null;
  counters: { requests: number; completed: number; failed: number };
  deleted?: true;
};
export class InferenceRequestError extends Error {
  constructor(readonly code: string, readonly status = 400) { super(code); }
}
const bytes = (value: unknown) => new TextEncoder().encode(JSON.stringify(value)).byteLength;

/** Validate before consuming a routing decision or making any provider call. */
export function validateInferenceRequest(value: unknown, tokenLimit = INFERENCE_MAX_OUTPUT_TOKENS): InferenceRequest {
  if (!Number.isInteger(tokenLimit) || tokenLimit < 1 || tokenLimit > INFERENCE_MAX_OUTPUT_TOKENS)
    throw new InferenceRequestError("invalid_key_token_limit", 403);
  if (bytes(value) > INFERENCE_MAX_BODY_BYTES) throw new InferenceRequestError("request_too_large", 413);
  const parsed = requestSchema.safeParse(value);
  if (!parsed.success) throw new InferenceRequestError("invalid_inference_request");
  const body = parsed.data;
  if (body.model !== "auto" && !ROUTING_CANDIDATES.some(c => c.backend !== "chatgpt"
    && (c.model === body.model || c.id === body.model))) throw new InferenceRequestError("unknown_model");
  if (bytes(body.input) + bytes(body.instructions ?? "") > INFERENCE_MAX_INPUT_BYTES)
    throw new InferenceRequestError("input_too_large", 413);
  body.max_output_tokens ??= tokenLimit;
  if (body.max_output_tokens > tokenLimit) throw new InferenceRequestError("max_output_tokens_exceeds_key_limit");
  const definitions = new Map((body.tools ?? []).map(t => [t.name, t.type]));
  if (definitions.size !== (body.tools?.length ?? 0)) throw new InferenceRequestError("duplicate_tool_name");
  if (typeof body.tool_choice === "object" && definitions.get(body.tool_choice.name) !== body.tool_choice.type)
    throw new InferenceRequestError("invalid_tool_choice");
  if (body.tool_choice === "required" && definitions.size === 0) throw new InferenceRequestError("invalid_tool_choice");
  // Validate full-history pairing locally, before the adapters' equivalent check.
  const pending = new Set<string>(), seen = new Set<string>();
  for (const item of typeof body.input === "string" ? [] : body.input) {
    if (item.type === "function_call" || item.type === "custom_tool_call") {
      if (seen.has(item.call_id)) throw new InferenceRequestError("invalid_tool_history");
      if (item.type === "function_call") {
        let args: unknown;
        try { args = JSON.parse(item.arguments); } catch { throw new InferenceRequestError("invalid_tool_history"); }
        if (!args || typeof args !== "object" || Array.isArray(args)) throw new InferenceRequestError("invalid_tool_history");
      }
      seen.add(item.call_id); pending.add(item.call_id);
    } else if (item.type === "function_call_output" || item.type === "custom_tool_call_output") {
      if (!pending.delete(item.call_id)) throw new InferenceRequestError("invalid_tool_history");
    } else if ((item.type === "message" || item.type === undefined) && pending.size) {
      throw new InferenceRequestError("invalid_tool_history");
    }
  }
  if (pending.size) throw new InferenceRequestError("invalid_tool_history");
  return body;
}

export function normalizeInferencePolicy(value: unknown = {}): ThreadRoutingPolicy {
  const parsed = routingPolicySchema.safeParse(value);
  if (!parsed.success) throw new InferenceRequestError("invalid_routing_policy");
  const policy = parsed.data;
  if (policy.strategy !== "direct") throw new InferenceRequestError("unsupported_routing_strategy");
  const candidates = ROUTING_CANDIDATES.filter(c => c.backend !== "chatgpt"
    && (!policy.candidates || policy.candidates.includes(c.id))).map(c => c.id);
  if (!candidates.length) throw new InferenceRequestError("no_inference_candidates");
  return { ...policy, strategy: "direct", candidates, estimates: policy.estimates.filter(e => e.backend !== "chatgpt") };
}

/** Read trusted live cohorts and enabled deployment probes only before the first pin. Missing, slow or invalid
 * telemetry remains unknown; it never blocks routing or becomes retained session data.
 * The shared resolver validates freshness, sample counts and candidate/effort matching.
 */
export async function inferenceRoutingAvailability(env: InferenceSessionEnv, signal: AbortSignal, origin: InferenceOrigin = unknownOrigin): Promise<RoutingAvailability> {
  const availability: RoutingAvailability = {
    ...gatewayAvailability(env), signal,
    workerColo: null, clientIngressColo: origin.clientIngressColo, provider_performance: [],
  };
  const coordinator = env.NANOCODEX_PROVIDER_PROBE_COORDINATOR;
  if (!coordinator || signal.aborted) return availability;
  availability.observeRoute = async route => {
    const observation = routeObservation(route, origin.clientIngressColo);
    const target = coordinator.getByName(PROBE_OWNER);
    if (!observation || !target.observeRoute) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try { await Promise.race([target.observeRoute(observation), new Promise(resolve => { timer = setTimeout(resolve, INFERENCE_PROBE_TIMEOUT_MS); })]); }
    finally { clearTimeout(timer); }
  };
  let timer: ReturnType<typeof setTimeout> | undefined;
  let abort: (() => void) | undefined;
  try {
    const snapshot = await Promise.race([
      coordinator.getByName(PROBE_OWNER).snapshot(origin),
      new Promise<unknown>(resolve => { timer = setTimeout(() => resolve([]), INFERENCE_PROBE_TIMEOUT_MS); }),
      new Promise<unknown>(resolve => { abort = () => resolve([]); signal.addEventListener("abort", abort, { once: true }); }),
    ]);
    if (Array.isArray(snapshot)) availability.provider_performance = snapshot.slice(0, 512).filter(metric =>
      metric && typeof metric === "object"
      && (((metric.source === "live" || (metric.source === "probe" && env.NANOCODEX_PROVIDER_PROBES === "true")) && metric.scope === "deployment_global" && metric.workerColo === null)
        || (metric.source === "live" && metric.scope === "client_ingress" && origin.clientIngressColo !== null
          && metric.clientIngressColo === origin.clientIngressColo))
      && ["workers_ai", "openrouter", "vercel", "cloudflare"].includes(metric.backend));
  } catch { /* No retry or provider fallback: unavailable telemetry is unknown. */ }
  finally {
    clearTimeout(timer);
    if (abort) signal.removeEventListener("abort", abort);
  }
  return availability;
}

/** Deliberately omit router usage, free-form reasons and audit payloads: these may echo input. */
function retainRoute(route: ThreadRoute): InferenceRoute {
  const diagnostics = projectThreadRouteDiagnostics(route);
  return { version: route.version, policy_version: route.policy_version, backend: route.backend,
    provider_model: route.provider_model, model: route.model, thinking: route.thinking,
    reasoning_mode: route.reasoning_mode, fast_mode: route.fast_mode, family: route.family,
    confidence: route.confidence, objective: route.objective, selection: route.selection,
    created_at: route.created_at, router_duration_ms: route.router_duration_ms,
    ...(diagnostics ? { diagnostics } : {}) };
}
function admittedRoute(route: InferenceRoute, policy?: ThreadRoutingPolicy) {
  return route.backend !== "chatgpt" && ROUTING_CANDIDATES.some(c => c.backend === route.backend
    && c.model === route.model && c.provider_model === route.provider_model && c.thinking === route.thinking
    && (!policy || policy.candidates?.includes(c.id)));
}

function matchesModel(route: InferenceRoute, model: string) {
  return model === "auto" || model === route.model || ROUTING_CANDIDATES.some(c => c.id === model
    && c.backend === route.backend && c.model === route.model && c.provider_model === route.provider_model
    && c.thinking === route.thinking);
}
function assertPinnedRequest(route: InferenceRoute, input: InferenceRequest) {
  if (!matchesModel(route, input.model) || (input.reasoning && input.reasoning.effort !== route.thinking))
    throw new InferenceRequestError("route_is_pinned", 409);
}
function requestPolicy(policy: ThreadRoutingPolicy, input: InferenceRequest): ThreadRoutingPolicy {
  const candidates = policy.candidates!.filter(id => {
    const candidate = ROUTING_CANDIDATES.find(c => c.id === id)!;
    return (input.model === "auto" || input.model === candidate.model || input.model === candidate.id)
      && (!input.reasoning || candidate.thinking === input.reasoning.effort);
  });
  if (!candidates.length) throw new InferenceRequestError("no_inference_candidates");
  return { ...policy, candidates };
}

/** Stop waiting even when a routing binding does not expose cancellation. */
async function abortable<T>(pending: Promise<T>, signal: AbortSignal): Promise<T> {
  signal.throwIfAborted();
  let abort: (() => void) | undefined;
  try {
    return await Promise.race([pending, new Promise<never>((_resolve, reject) => {
      abort = () => reject(signal.reason);
      signal.addEventListener("abort", abort, { once: true });
    })]);
  } finally { if (abort) signal.removeEventListener("abort", abort); }
}
async function resolveInferenceRoute(env: InferenceSessionEnv, input: InferenceRequest,
  policy: ThreadRoutingPolicy, signal: AbortSignal, origin: InferenceOrigin = unknownOrigin): Promise<InferenceRoute> {
  const constrained = requestPolicy(policy, input);
  const availability = await inferenceRoutingAvailability(env, signal, origin);
  signal.throwIfAborted();
  if (!ROUTING_CANDIDATES.some(c => constrained.candidates!.includes(c.id)
    && (c.backend === "workers_ai" || c.backend !== "chatgpt" && availability[c.backend])))
    throw new InferenceRequestError("no_inference_candidates");
  availability.bypassSingleCandidate = input.model !== "auto";
  const route = await abortable(resolveThreadRoute(env.AI, input.input, constrained, availability), signal);
  signal.throwIfAborted();
  if (!admittedRoute(route, constrained)) throw new InferenceRequestError("invalid_pinned_route", 503);
  return retainRoute(route);
}

/** Executes one generation using a resolved route; returned tools remain caller-owned. */
async function executeRoutedResponse(env: InferenceSessionEnv, route: InferenceRoute,
  input: InferenceRequest, signal: AbortSignal, fetchImpl: typeof fetch, sessionId?: string, origin: InferenceOrigin = unknownOrigin): Promise<Response> {
  if (!admittedRoute(route)) throw new InferenceRequestError("invalid_pinned_route", 503);
  assertPinnedRequest(route, input);
  signal.throwIfAborted();
  let transport: ReturnType<typeof createWorkersAiResponses>;
  if (route.backend === "workers_ai") {
    transport = createWorkersAiResponses({ run: (model, body) => env.AI.run(model, body) }, { model: OSS_MODEL });
  } else if (route.backend === "cloudflare") {
    if (route.model === OSS_MODEL) throw new InferenceRequestError("invalid_pinned_route", 503);
    if (!gatewayAvailability(env).cloudflare) throw new InferenceRequestError("inference_unavailable", 503);
    const gateway = gatewayRuntime(env, route, () => signal.throwIfAborted(), fetchImpl);
    if (!gateway) throw new InferenceRequestError("inference_unavailable", 503);
    transport = createGatewayResponses(gateway);
  } else {
    transport = createGatewayResponses({ provider: route.backend as "openrouter" | "vercel", model: route.model,
      reasoningEffort: route.thinking, apiKey: (route.backend === "openrouter" ? env.OPENROUTER_API_KEY : env.AI_GATEWAY_API_KEY) ?? "",
      fetch: fetchImpl });
  }
  const began = performance.now(), timestamp = Date.now();
  let ttft: number | null = null, observed = false;
  const observe = async (success: boolean) => {
    if (observed) return;
    observed = true;
    const elapsedMs = performance.now() - began;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      const coordinator = env.NANOCODEX_PROVIDER_PROBE_COORDINATOR?.getByName(PROBE_OWNER);
      if (!coordinator?.observe) return;
      await Promise.race([coordinator.observe({ timestamp, source: "live", workerColo: null,
        clientIngressColo: origin.clientIngressColo, backend: route.backend, model: route.model, effort: route.thinking,
        outcome: success ? "success" : signal.aborted ? (signal.reason?.name === "TimeoutError" ? "timeout" : "cancelled") : "protocol_error",
        status: success ? 200 : null, headersMs: null, fullResponseMs: success ? elapsedMs : null,
        generationTtftMs: success ? ttft : null, clientDeliveryMs: null, elapsedMs }),
        new Promise(resolve => { timeout = setTimeout(resolve, INFERENCE_PROBE_TIMEOUT_MS); })]);
    } catch { /* telemetry must never fail generation */ }
    finally { clearTimeout(timeout); }
  };
  try {
    const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, sessionId ?? "", {
      authorization: "host_managed", signal,
      body: JSON.stringify({ ...input, model: route.model, reasoning: { effort: route.thinking }, store: false }),
    });
    if (!response.ok || !response.body) throw new Error("invalid_provider_protocol");
    const buffering = input.stream && response.headers.get("x-nanocodex-inference-buffering") === "streaming" ? "streaming" : "buffered";
    const headers = { "cache-control": "no-store", "x-nanocodex-inference-buffering": buffering,
      ...(sessionId ? { "x-nanocodex-inference-session-id": sessionId, "x-nanocodex-session-id": sessionId } : {}),
      ...(origin.clientIngressColo ? { "x-nanocodex-ingress-colo": origin.clientIngressColo } : {}),
      "server-timing": `router;dur=${route.router_duration_ms ?? 0}`,
      "x-nanocodex-provider": route.backend, "x-nanocodex-model": route.model, "x-nanocodex-thinking": route.thinking };
    let completed: Record<string, unknown> | undefined;
    const stream = projectInferenceStream(response.body, event => {
      if (event.response && typeof event.response === "object") {
        event.response = { ...event.response, model: route.model, ...(sessionId ? { session_id: sessionId } : {}), route, buffering };
        if (event.type === "response.completed" || event.type === "response.incomplete") completed = event.response;
      }
      return event;
    }, () => { if (buffering === "streaming" && ttft === null) ttft = performance.now() - began; });
    if (input.stream) return finalizeInferenceResponse(new Response(stream, {
      headers: { ...headers, "content-type": "text/event-stream; charset=utf-8" },
    }), signal, observe);
    await new Response(stream).arrayBuffer();
    if (!completed) throw new Error("invalid_provider_protocol");
    signal.throwIfAborted();
    await observe(true);
    return Response.json(completed, { headers });
  } catch (error) { await observe(false); throw error; }
}

/** Session wrapper preserves the committed provider/model/effort pin. */
export async function executeInferenceResponse(env: InferenceSessionEnv, session: Pick<InferenceSessionMetadata, "id" | "route">,
  input: InferenceRequest, signal: AbortSignal, fetchImpl: typeof fetch = fetch, origin: InferenceOrigin = unknownOrigin): Promise<Response> {
  if (!session.route) throw new InferenceRequestError("invalid_pinned_route", 503);
  return executeRoutedResponse(env, session.route, input, signal, fetchImpl, session.id, origin);
}

function inferenceErrorResponse(error: unknown, signal: AbortSignal): Response {
  if (error instanceof InferenceRequestError) return json({ error: { code: error.code } }, error.status);
  const timeout = signal.aborted && signal.reason?.name === "TimeoutError";
  return json({ error: { code: timeout ? "inference_timeout" : "inference_failed" } }, timeout ? 504 : 502);
}

/** Standard Responses request: no storage, retained transcript, or account context. */
export async function executeStatelessInferenceResponse(env: InferenceSessionEnv, rawBody: unknown,
  maxOutputTokens: number, signal: AbortSignal, origin: InferenceOrigin = unknownOrigin): Promise<Response> {
  const controller = new AbortController();
  const abort = () => controller.abort(signal.reason);
  signal.addEventListener("abort", abort, { once: true });
  if (signal.aborted) abort();
  const timer = setTimeout(() => controller.abort(new DOMException("Inference timeout", "TimeoutError")), INFERENCE_TIMEOUT_MS);
  let streaming = false;
  const cleanup = () => { clearTimeout(timer); signal.removeEventListener("abort", abort); };
  try {
    const input = validateInferenceRequest(rawBody, maxOutputTokens);
    controller.signal.throwIfAborted();
    const route = await resolveInferenceRoute(env, input, normalizeInferencePolicy(), controller.signal, origin);
    const response = await abortable(executeRoutedResponse(env, route, input, controller.signal, fetch, undefined, origin), controller.signal);
    if (input.stream) {
      streaming = true;
      return finalizeInferenceResponse(response, controller.signal, cleanup);
    }
    return response;
  } catch (error) { return inferenceErrorResponse(error, controller.signal); }
  finally { if (!streaming) cleanup(); }
}

async function boundedJson(request: Request, signal: AbortSignal): Promise<unknown> {
  signal.throwIfAborted();
  const declared = request.headers.get("content-length");
  if (declared && (!/^\d+$/.test(declared) || Number(declared) > INFERENCE_MAX_BODY_BYTES))
    throw new InferenceRequestError("request_too_large", 413);
  if (!request.body) throw new InferenceRequestError("invalid_json");
  const reader = request.body.getReader(), chunks: Uint8Array[] = [];
  let length = 0;
  const abort = () => { void reader.cancel(signal.reason).catch(() => {}); };
  signal.addEventListener("abort", abort, { once: true });
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > INFERENCE_MAX_BODY_BYTES) {
        void reader.cancel().catch(() => {});
        throw new InferenceRequestError("request_too_large", 413);
      }
      chunks.push(value);
    }
    signal.throwIfAborted();
  } finally { signal.removeEventListener("abort", abort); reader.releaseLock(); }
  const body = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
  try { return JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(body)); }
  catch { throw new InferenceRequestError("invalid_json"); }
}
const json = (value: unknown, status = 200) => Response.json(value, { status, headers: { "cache-control": "no-store" } });

/** Private DO binding. The public gateway must authenticate and replace trusted headers.
 * The base DurableObject imports no agent or account runtime.
 * All mutations are guarded before their first await, including deletion and pinning.
 */
export class InferenceSessionRuntime {
  #busy = false;
  constructor(private readonly ctx: Pick<DurableObjectState, "id" | "storage">, private readonly env: InferenceSessionEnv) {}
  async fetch(request: Request): Promise<Response> {
    if (this.#busy) return json({ error: { code: "session_busy" } }, 409);
    this.#busy = true;
    const controller = new AbortController();
    const abort = () => controller.abort(request.signal.reason);
    request.signal.addEventListener("abort", abort, { once: true });
    if (request.signal.aborted) abort();
    const timer = setTimeout(() => controller.abort(new DOMException("Inference timeout", "TimeoutError")), INFERENCE_TIMEOUT_MS);
    let streaming = false;
    const cleanup = () => { clearTimeout(timer); request.signal.removeEventListener("abort", abort); this.#busy = false; };
    try {
      const response = await this.#handle(request, controller.signal);
      if (response.headers.get("content-type")?.startsWith("text/event-stream")) {
        streaming = true;
        return finalizeInferenceResponse(response, controller.signal, cleanup);
      }
      return response;
    } catch (error) { return inferenceErrorResponse(error, controller.signal); }
    finally { if (!streaming) cleanup(); }
  }
  async #handle(request: Request, signal: AbortSignal): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (!((path === "/session" && ["PUT", "GET", "DELETE"].includes(request.method))
      || (path === "/responses" && request.method === "POST"))) throw new InferenceRequestError("not_found", 404);
    const retained = await this.ctx.storage.get<InferenceSessionMetadata>(STORAGE_KEY);
    if (path === "/session" && request.method === "PUT") {
      const parsed = z.object({ key_id: keyId, session_id: z.uuid().optional(), routing: z.unknown().optional() }).strict().safeParse(await boundedJson(request, signal));
      if (!parsed.success) throw new InferenceRequestError("invalid_session_request");
      // The immutable tombstone also prevents rebinding a deleted object to another key.
      if (retained) throw new InferenceRequestError(retained.key_id === parsed.data.key_id ? "session_exists" : "session_key_mismatch", 409);
      const routing = normalizeInferencePolicy(parsed.data.routing);
      const metadata: InferenceSessionMetadata = { id: parsed.data.session_id ?? this.ctx.id.toString(), key_id: parsed.data.key_id,
        routing, route: null, counters: { requests: 0, completed: 0, failed: 0 } };
      signal.throwIfAborted();
      await this.ctx.storage.put(STORAGE_KEY, metadata);
      return json(metadata, 201);
    }
    const header = keyId.safeParse(request.headers.get(INFERENCE_KEY_ID_HEADER));
    if (!header.success) throw new InferenceRequestError("inference_key_required", 403);
    if (!retained || retained.key_id !== header.data || retained.deleted) throw new InferenceRequestError("session_not_found", 404);
    if (request.method === "GET") return json(retained);
    if (request.method === "DELETE") {
      await this.ctx.storage.put(STORAGE_KEY, { ...retained, deleted: true });
      return new Response(null, { status: 204 });
    }
    const limitHeader = request.headers.get(INFERENCE_MAX_OUTPUT_TOKENS_HEADER);
    const limit = limitHeader === null ? INFERENCE_MAX_OUTPUT_TOKENS : /^\d+$/.test(limitHeader) ? Number(limitHeader) : NaN;
    const input = validateInferenceRequest(await boundedJson(request, signal), limit);
    if (retained.route) assertPinnedRequest(retained.route, input);
    signal.throwIfAborted();
    const origin = inferenceOrigin(request.headers.get(INFERENCE_INGRESS_HEADER));
    if (!retained.route) {
      retained.route = await resolveInferenceRoute(this.env, input, retained.routing, signal, origin);
      // Await durable commit before issuing the generation request. Failed generation keeps this exact pin.
      await this.ctx.storage.put(STORAGE_KEY, retained);
    }
    retained.counters.requests++;
    await this.ctx.storage.put(STORAGE_KEY, retained);
    try {
      const response = await executeInferenceResponse(this.env, retained, input, signal, fetch, origin);
      if (input.stream) return finalizeInferenceResponse(response, signal, async success => {
        retained.counters[success ? "completed" : "failed"]++;
        await this.ctx.storage.put(STORAGE_KEY, retained);
      });
      retained.counters.completed++;
      await this.ctx.storage.put(STORAGE_KEY, retained);
      return response;
    } catch (error) {
      retained.counters.failed++;
      await this.ctx.storage.put(STORAGE_KEY, retained);
      throw error;
    }
  }
}

/** Durable binding delegates to the same isolated executor used by the focused tests. */
export class InferenceSession extends DurableObject<InferenceSessionEnv> {
  readonly #runtime: InferenceSessionRuntime;
  constructor(ctx: DurableObjectState, env: InferenceSessionEnv) {
    super(ctx, env);
    this.#runtime = new InferenceSessionRuntime(ctx, env);
  }
  fetch(request: Request): Promise<Response> { return this.#runtime.fetch(request); }
}
