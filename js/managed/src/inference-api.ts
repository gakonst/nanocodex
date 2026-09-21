import { authenticate, requireSameOriginMutation, type AccountAuthEnv, type Principal } from "./account-auth";
import { authorizeInferenceKey, routeInferenceKeys, type InferenceKeysEnv } from "./inference-keys";
import { executeStatelessInferenceResponse, type InferenceSessionEnv } from "./inference-session";
import { ROUTING_CANDIDATES } from "./thread-model-routing";
import { gatewayAvailability } from "./gateway-runtime";

export interface InferenceApiEnv extends AccountAuthEnv, InferenceKeysEnv, Omit<InferenceSessionEnv, "AI"> {
  AI?: InferenceSessionEnv["AI"];
  NANOCODEX_INFERENCE_ENABLED?: string;
  /** Only the deployment operator may issue keys backed by shared provider credits. */
  NANOCODEX_ADMIN_USER_ID?: string;
  NANOCODEX_INFERENCE_SESSIONS: DurableObjectNamespace;
}
const BASE = "/v1/inference";
const SESSION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const MAX_BODY = 262_144;
export function isInferenceCredential(request: Request): boolean {
  return /\bnci_/i.test(request.headers.get("authorization") ?? "");
}
function json(value: unknown, status = 200): Response {
  return Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
}
async function readBody(request: Request): Promise<Record<string, unknown>> {
  const reader = request.body?.getReader();
  if (!reader) return {};
  const chunks: Uint8Array[] = []; let size = 0;
  try {
    while (true) {
      const next = await reader.read(); if (next.done) break;
      size += next.value.byteLength;
      if (size > MAX_BODY) { await reader.cancel(); throw new Error("body_too_large"); }
      chunks.push(next.value);
    }
  } finally { reader.releaseLock(); }
  const bytes = new Uint8Array(size); let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  let body: unknown;
  try {
    const text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes);
    body = text.trim() ? JSON.parse(text) : {};
  } catch { throw new Error("invalid_request"); }
  if (!body || typeof body !== "object" || Array.isArray(body)) throw new Error("invalid_request");
  return body as Record<string, unknown>;
}
/** Standard SDK entry points use the familiar structured error envelope. */
export async function routeInferenceApi(request: Request, env: InferenceApiEnv, url: URL,
  trustedPrincipal?: Principal): Promise<Response | undefined> {
  const response = await routeInferenceApiInternal(request, env, url, trustedPrincipal);
  if (!response || response.status < 400 || (url.pathname !== "/v1/responses" && url.pathname !== "/v1/models")) return response;
  const body = await response.json<{error?: string | {code?: string}}>();
  const code = typeof body.error === "string" ? body.error : body.error?.code ?? "inference_unavailable";
  const type = response.status === 401 ? "authentication_error" : response.status === 429 ? "rate_limit_error"
    : response.status >= 500 ? "server_error" : "invalid_request_error";
  return Response.json({error:{message:code.replaceAll("_", " "),type,param:null,code}},
    {status:response.status,headers:response.headers});
}

/** Separate credential and runtime boundary. No managed agent is constructed here. */
async function routeInferenceApiInternal(request: Request, env: InferenceApiEnv, url: URL,
  trustedPrincipal?: Principal): Promise<Response | undefined> {
  const standardPath = url.pathname === "/v1/responses" ? "/responses" : url.pathname === "/v1/models" ? "/models" : undefined;
  const inNamespace = standardPath !== undefined || url.pathname === BASE || url.pathname.startsWith(BASE + "/");
  // Run before all account/connector/hand routers, including cached authorization.
  if (isInferenceCredential(request) && !inNamespace) return json({ error: "inference_key_scope" }, 403);
  if (!inNamespace) return undefined;
  if (url.search) return json({ error: "invalid_request" }, 400);
  const suffix = standardPath ?? url.pathname.slice(BASE.length);
  if (suffix === "/keys" || suffix.startsWith("/keys/")) {
    if (isInferenceCredential(request)) return json({ error: "inference_key_scope" }, 403);
    const principal = trustedPrincipal ?? await authenticate(request, env, url);
    if (!principal) return json({ error: "unauthorized" }, 401);
    if (!env.NANOCODEX_ADMIN_USER_ID || principal.userId !== env.NANOCODEX_ADMIN_USER_ID)
      return json({ error: "inference_key_admin_required" }, 403);
    if (request.method !== "GET") {
      const failure = requireSameOriginMutation(request, url, principal);
      if (failure) return failure;
    }
    return routeInferenceKeys(request, env, url, principal);
  }
  if (env.NANOCODEX_INFERENCE_ENABLED !== "true") return json({ error: "inference_unavailable" }, 503);
  if (!env.NANOCODEX_INFERENCE_SESSIONS || !env.AI) return json({ error: "inference_unavailable" }, 503);
  const sessionPath = suffix.match(/^\/sessions\/([^/]+)$/);
  const known = suffix === "/models" || suffix === "/sessions" || suffix === "/responses" || sessionPath;
  if (!known) return json({ error: "not_found" }, 404);
  const expected = suffix === "/models" ? ["GET"] : sessionPath ? ["GET", "DELETE"] : ["POST"];
  if (!expected.includes(request.method)) return json({ error: "method_not_allowed" }, 405);
  // Only the new inference key format authenticates generation; account keys are administrative.
  const key = await authorizeInferenceKey(request, env, { reserve: request.method === "POST" });
  if (key instanceof Response) return key;
  if (suffix === "/models") {
    const available = gatewayAvailability(env);
    const candidates = ROUTING_CANDIDATES.filter(candidate => candidate.backend !== "chatgpt"
      && (candidate.backend === "workers_ai" || available[candidate.backend]));
    return json({ object: "list", data: candidates.map(({ id, model, provider_model, backend, thinking }) => ({ id, object: "model", created: 0, owned_by: backend, model, provider_model, provider: backend, thinking })) });
  }
  try {
    const headers = new Headers({ "content-type": "application/json", "x-inference-key-id": key.id,
      "x-inference-max-output-tokens": String(key.limits.maxOutputTokens) });
    let id: string; let method = request.method; let path = "/session"; let body: Record<string, unknown> | undefined;
    if (suffix === "/sessions") {
      body = await readBody(request);
      if (Object.keys(body).some(name => name !== "routing")) return json({ error: "invalid_request" }, 400);
      id = crypto.randomUUID(); method = "PUT";
      body = { ...body, session_id: id, key_id: key.id };
    } else if (suffix === "/responses") {
      body = await readBody(request);
      if (body.session_id === undefined) {
        return executeStatelessInferenceResponse({ ...env, AI: env.AI }, body, key.limits.maxOutputTokens, request.signal);
      }
      if (typeof body.session_id !== "string" || !SESSION_ID.test(body.session_id)) return json({ error: "session_id_required" }, 400);
      id = body.session_id; delete body.session_id; path = "/responses";
    } else {
      id = sessionPath![1]!;
      if (!SESSION_ID.test(id)) return json({ error: "not_found" }, 404);
    }
    headers.set("x-inference-session-id", id);
    // The caller cannot choose a DO identity, credential, account context, or internal header.
    return await env.NANOCODEX_INFERENCE_SESSIONS.getByName(id).fetch(new Request("https://inference.internal" + path,
      { method, headers, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: request.signal }));
  } catch (error) {
    if (request.signal.aborted) return json({ error: "request_cancelled" }, 499);
    if (error instanceof Error && error.message === "body_too_large") return json({ error: "body_too_large" }, 413);
    if (error instanceof Error && error.message === "invalid_request") return json({ error: "invalid_request" }, 400);
    return json({ error: "inference_unavailable" }, 502);
  }
}
