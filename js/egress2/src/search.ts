import type { ActiveCredential } from "./handler";
import { tracing } from "cloudflare:workers";

const SEARCH_ROUTE = "https://nanocodex.internal/v1/search";
const API_URL = "https://api.openai.com/v1/alpha/search";
const SUBSCRIPTION_URL = "https://chatgpt.com/backend-api/codex/alpha/search";
const MAX_REQUEST_BYTES = 64 * 1024;
const MAX_RESPONSE_BYTES = 1024 * 1024;
const PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";

/** Search has a separate, fixed egress policy; no arbitrary URL or caller headers reach the provider. */
export function createSearchHandler<Env>({ readCredential, upstreamFetch, clock = () => performance.now(),
  log = (event: Record<string, string | number | null>) => console.info(event),
}: {
  readCredential: (owner: string, env: Env) => Promise<ActiveCredential | null>;
  upstreamFetch: (request: Request, owner: string, env: Env, region: string | null) => Promise<Response>;
  clock?: () => number;
  log?: (event: Record<string, string | number | null>) => void;
}) {
  return async (request: Request, env: Env): Promise<Response> => {
    if (request.url !== SEARCH_ROUTE) return error(403, "route_not_allowed");
    if (request.method !== "POST") return error(405, "method_not_allowed");
    const owner = request.headers.get("x-managed2-owner")?.trim();
    if (!owner || request.headers.get("authorization") !== PLACEHOLDER) return error(403, "forbidden");
    const started = clock();
    const supplied = request.headers.get("x-managed2-trace-id");
    const traceId = supplied && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(supplied)
      ? supplied : null;
    let prepareMs = 0, credentialMs = 0, upstreamMs = 0, parseMs = 0;
    let route: "openai_api" | "chatgpt_subscription" | "unknown" = "unknown";
    let upstreamStatus: number | null = null;
    const finish = (response: Response): Response => {
      const duration = (value: number) => Math.max(0, value).toFixed(1);
      const timing = `search_prepare;dur=${duration(prepareMs)}, search_credential;dur=${duration(credentialMs)}, search_upstream;dur=${duration(upstreamMs)}, search_parse;dur=${duration(parseMs)}, search_total;dur=${duration(clock() - started)}, search_route;desc="${route}"`;
      // Only fixed labels and durations: never emit query, owner, URLs, credentials, or provider body.
      try { log({ event: "search_egress", route, status: response.status, upstream_status: upstreamStatus,
        prepare_ms: +duration(prepareMs), credential_ms: +duration(credentialMs), upstream_ms: +duration(upstreamMs),
        parse_ms: +duration(parseMs) }); } catch { /* telemetry is best effort */ }
      response.headers.set("server-timing", timing);
      return response;
    };
    try {
      const body = await tracing.enterSpan("egress2.search.prepare", async span => {
        if (traceId) span.setAttribute("managed2.trace_id", traceId);
        const raw = await boundedText(request, MAX_REQUEST_BYTES);
        return searchBody(JSON.parse(raw) as unknown);
      });
      prepareMs = clock() - started;
      const credentialStart = clock();
      let credential: ActiveCredential | null;
      try { credential = await tracing.enterSpan("egress2.credential", async span => {
        if (traceId) span.setAttribute("managed2.trace_id", traceId);
        return readCredential(owner, env);
      }); }
      catch { return finish(error(502, "credential_unavailable")); }
      credentialMs = clock() - credentialStart;
      if (!credential || !credential.secret) return finish(error(403, "credential_unavailable"));
      route = credential.kind === "chatgpt" ? "chatgpt_subscription" : "openai_api";
      const headers = new Headers({ authorization: `Bearer ${credential.secret}`, "content-type": "application/json",
        "user-agent": credential.kind === "chatgpt" ? "codex_cli_rs" : "nanocodex-web/0.1.0" });
      if (credential.kind === "chatgpt") {
        headers.set("chatgpt-account-id", credential.accountId);
        if (credential.fedramp) headers.set("x-openai-fedramp", "true");
        headers.set("originator", "codex_cli_rs");
      }
      const target = credential.kind === "chatgpt" ? SUBSCRIPTION_URL : API_URL;
      const upstreamStart = clock();
      let upstream: Response;
      try { upstream = await tracing.enterSpan("egress2.upstream", async span => {
        if (traceId) span.setAttribute("managed2.trace_id", traceId);
        span.setAttribute("egress2.route", route);
        const response = await upstreamFetch(new Request(target, {
          method: "POST", headers, body: JSON.stringify(body), redirect: "manual",
        }), owner, env, request.headers.get("x-managed2-relay-region"));
        span.setAttribute("http.response.status_code", response.status);
        return response;
      }); }
      catch { upstreamMs = clock() - upstreamStart; return finish(error(502, "search_unavailable")); }
      upstreamMs = clock() - upstreamStart;
      upstreamStatus = upstream.status;
      if (!upstream.ok) { await upstream.body?.cancel().catch(() => {}); return finish(error(502, "search_unavailable")); }
      const parseStart = clock();
      try {
        const output = await tracing.enterSpan("egress2.search.parse", async span => {
          if (traceId) span.setAttribute("managed2.trace_id", traceId);
          const data: unknown = JSON.parse(await boundedText(upstream, MAX_RESPONSE_BYTES));
          if (!data || typeof data !== "object" || typeof (data as { output?: unknown }).output !== "string") throw new Error("invalid search output");
          return (data as { output: string }).output;
        });
        // Return only the bounded, model-facing output, not arbitrary provider metadata.
        const result = Response.json({ output }, { headers: { "cache-control": "no-store" } });
        parseMs = clock() - parseStart;
        return finish(result);
      } catch { parseMs = clock() - parseStart; return finish(error(502, "invalid_search_response")); }
    } catch { prepareMs = clock() - started; return finish(error(400, "invalid_search_request")); }
  };
}

function error(status: number, code: string): Response {
  return Response.json({ error: code }, { status, headers: { "cache-control": "no-store" } });
}

/** Preserve the standalone Codex search payload, without caller-selected upstream settings. */
function searchBody(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid request");
  const input = value as Record<string, unknown>;
  if (typeof input.session_id !== "string" || !/^[A-Za-z0-9._:-]{1,200}$/.test(input.session_id)
    || !input.commands || typeof input.commands !== "object" || Array.isArray(input.commands)) throw new Error("invalid request");
  const model = input.model ?? "gpt-6-sol";
  if (typeof model !== "string" || !/^[A-Za-z0-9._:-]{1,200}$/.test(model)) throw new Error("invalid model");
  return { id: input.session_id, model, commands: input.commands,
    settings: { allowed_callers: ["direct"], external_web_access: true }, max_output_tokens: 10_000 };
}

async function boundedText(response: Request | Response, limit: number): Promise<string> {
  const reader = response.body?.getReader();
  if (!reader) throw new Error("missing body");
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  let bytes = 0, result = "";
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) return result + decoder.decode();
      bytes += value.byteLength;
      if (bytes > limit) throw new Error("body too large");
      result += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); await response.body?.cancel().catch(() => {}); }
}
