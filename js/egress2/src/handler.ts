/** The owner header is asserted by the trusted host, never by an untrusted client. */
export const OWNER_HEADER = "x-managed2-owner";
export const CREDENTIAL_PLACEHOLDER = "NANOCODEX_PROVIDER_CREDENTIAL";
const OPENAI_URL = "https://api.openai.com/v1/responses";
const CHATGPT_URL = "https://chatgpt.com/backend-api/codex/responses";
const CACHE_TTL_MS = 60_000;
const MAX_CACHE_ENTRIES = 256;
// Only a canonical UUIDv4 from the trusted host may appear in diagnostics.
const TRACE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export type ActiveCredential =
  | { kind: "openai"; secret: string }
  | { kind: "chatgpt"; secret: string; accountId: string; fedramp: boolean; expiresAt: number; revision?: string };
type CredentialReader<Env> = (ownerId: string, env: Env) => Promise<string | ActiveCredential | null>;
type UpstreamFetch<Env> = (request: Request, ownerId: string, env: Env) => Promise<Response>;
type CredentialRecovery<Env> = (ownerId: string, rejectedRevision: string, env: Env) => Promise<ActiveCredential | null>;

/** No Cloudflare runtime dependency: inject the credential reader and upstream fetch in tests. */
export function createEgressHandler<Env>({
  readCredential,
  recoverCredential,
  upstreamFetch = (request: Request) => fetch(request),
  now = Date.now,
  clock = () => performance.now(),
  log = (event: Record<string, string | number | null>) => console.info(event),
}: {
  readCredential: CredentialReader<Env>;
  recoverCredential?: CredentialRecovery<Env>;
  upstreamFetch?: UpstreamFetch<Env>;
  now?: () => number;
  clock?: () => number;
  log?: (event: Record<string, string | number | null>) => void;
}) {
  const cache = new Map<string, { value: ActiveCredential; expiresAt: number }>();

  return {
    invalidate(ownerId: string) {
      cache.delete(ownerId);
    },

    async fetch(request: Request, env: Env): Promise<Response> {
      const url = new URL(request.url);
      if (url.href !== OPENAI_URL && url.href !== CHATGPT_URL) {
        return new Response("Upstream not allowed", { status: 403 });
      }
      if (request.method !== "POST" && !(request.method === "GET" && request.headers.get("upgrade")?.toLowerCase() === "websocket")) {
        return new Response("Method not allowed", { status: 405 });
      }

      const ownerId = request.headers.get(OWNER_HEADER)?.trim();
      if (!ownerId) return new Response("Missing owner", { status: 400 });
      if (request.headers.get("authorization") !== `Bearer ${CREDENTIAL_PLACEHOLDER}`) {
        return new Response("Missing credential placeholder", { status: 400 });
      }

      // Only fixed, allowlisted fields leave this worker as diagnostics. Never log URLs,
      // headers, error messages, owner identifiers, credentials or request bodies.
      const started = clock();
      const suppliedTraceId = request.headers.get("x-managed2-trace-id");
      const traceId = suppliedTraceId && TRACE_ID.test(suppliedTraceId) ? suppliedTraceId : null;
      let egressRequestId: string | null = null;
      let retryAttempt = 0;
      let recoveryMs: number | null = null;
      let recoveryOutcome = "not_attempted";
      let credentialMs = 0;
      let upstreamMs = 0;
      let dispatchMs: number | null = null;
      let upstreamStatus: number | null = null;
      let cacheResult = "hit";
      let routeKind: "openai_api" | "chatgpt_subscription" = url.href === CHATGPT_URL
        ? "chatgpt_subscription" : "openai_api";
      const finish = (response: Response): Response => {
        const totalMs = Math.max(0, clock() - started);
        const duration = (value: number) => Math.max(0, value).toFixed(1);
        // One fixed route label makes provider placement visible at response headers,
        // while durations end when upstream response headers arrive (not stream EOF).
        const timing = `egress_credential;dur=${duration(credentialMs)}, ${dispatchMs === null ? "" : `egress_dispatch;dur=${duration(dispatchMs)}, `}egress_upstream_headers;dur=${duration(upstreamMs)}, ${recoveryMs === null ? "" : `egress_recovery;dur=${duration(recoveryMs)}, `}egress_total;dur=${duration(totalMs)}, egress_route;desc="${routeKind}", egress_cache;desc="${cacheResult}"`;
        try { log({ event: "responses_egress", trace_id: traceId, egress_request_id: egressRequestId,
          route_kind: routeKind, response_status: response.status, upstream_status: upstreamStatus,
          credential_cache: cacheResult, credential_ms: Number(duration(credentialMs)),
          upstream_dispatch_ms: dispatchMs === null ? null : Number(duration(dispatchMs)),
          upstream_headers_ms: Number(duration(upstreamMs)), recovery_ms: recoveryMs === null ? null : Number(duration(recoveryMs)),
          recovery_outcome: recoveryOutcome, retry_attempt: retryAttempt, total_ms: Number(duration(totalMs)) }); }
        catch { /* observability must never break a model response */ }
        // Workerd's WebSocket 101 cannot be constructed as a regular Response.
        if (response.status === 101) return response;
        const result = new Response(response.body, response);
        result.headers.set("server-timing", timing); // overwrite untrusted upstream timing
        return result;
      };
      const lookupStarted = clock();
      let entry = cache.get(ownerId);
      if (entry && entry.expiresAt <= now()) {
        cache.delete(ownerId);
        entry = undefined;
      }
      if (!entry) {
        cacheResult = "miss";
        // Do not echo failures from credential storage: they may contain sensitive data.
        let credential: string | ActiveCredential | null;
        try {
          credential = await readCredential(ownerId, env);
        } catch {
          credentialMs = clock() - lookupStarted;
          return finish(new Response("Credential unavailable", { status: 502 }));
        }
        if (!credential) { credentialMs = clock() - lookupStarted; return finish(new Response("Credential unavailable", { status: 403 })); }
        const value = typeof credential === "string" ? { kind: "openai" as const, secret: credential } : credential;
        if (!value.secret || (value.kind === "chatgpt" && (!value.accountId || value.expiresAt <= now()))) {
          credentialMs = clock() - lookupStarted;
          return finish(new Response("Credential unavailable", { status: 403 }));
        }
        entry = { value, expiresAt: Math.min(now() + CACHE_TTL_MS,
          value.kind === "chatgpt" ? value.expiresAt - 5 * 60_000 : Infinity) };
        if (cache.size >= MAX_CACHE_ENTRIES) cache.delete(cache.keys().next().value!);
        cache.set(ownerId, entry);
      }
      credentialMs = clock() - lookupStarted;
      const credentialEnd = clock();
      const credential = entry.value;
      routeKind = credential.kind === "chatgpt" ? "chatgpt_subscription" : "openai_api";
      if (url.href === CHATGPT_URL && credential.kind !== "chatgpt") {
        return finish(new Response("Upstream not allowed", { status: 403 }));
      }

      // Created inside this service, never accepted from the caller. The subscription
      // route is a private relay; API-key egress goes directly to OpenAI.
      if (credential.kind === "chatgpt") egressRequestId = crypto.randomUUID();
      const send = (active: ActiveCredential, retry = false): Promise<Response> => {
        const headers = new Headers(request.headers);
        headers.set("authorization", `Bearer ${active.secret}`);
        // The host-only identity and observability metadata must never reach OpenAI.
        for (const name of [...headers.keys()]) {
          if (name.startsWith("x-managed2-") || name.startsWith("x-nanocodex-egress-")
            || name === "x-nanocodex-subject" || name === "server-timing") headers.delete(name);
        }
        headers.delete("host");
        // Never let the caller spoof account metadata or originator.
        headers.delete("chatgpt-account-id");
        headers.delete("x-openai-fedramp");
        headers.delete("originator");
        if (active.kind === "chatgpt") {
          headers.set("x-nanocodex-egress-request-id", egressRequestId!);
          headers.set("chatgpt-account-id", active.accountId);
          if (active.fedramp) headers.set("x-openai-fedramp", "true");
          if (request.method === "POST") headers.set("originator", "codex_cli_rs");
        }
        return upstreamFetch(new Request(active.kind === "chatgpt" ? CHATGPT_URL : OPENAI_URL, {
          method: request.method, headers,
          // Avoid teeing the common API-key stream; only a recoverable subscription
          // 401 needs a preserved original for its one permitted replay.
          body: request.method === "POST"
            ? (!retry && active.kind === "chatgpt" && active.revision && recoverCredential
              ? request.clone().body : request.body) : null,
          duplex: "half", redirect: "manual",
        } as RequestInit), ownerId, env);
      };
      // Never follow a provider redirect carrying the real credential to another origin.
      dispatchMs = clock() - credentialEnd;
      const fetchAttempt = async (active: ActiveCredential, retry = false): Promise<Response> => {
        const upstreamStarted = clock();
        try { return await send(active, retry); }
        finally { upstreamMs += clock() - upstreamStarted; }
      };
      try {
        let upstream = await fetchAttempt(credential);
        upstreamStatus = upstream.status;
        if (upstream.status === 401 && credential.kind === "chatgpt"
          && credential.revision && recoverCredential) {
          await upstream.body?.cancel().catch(() => {});
          const recoveryStarted = clock();
          let recovered: ActiveCredential | null;
          try { recovered = await recoverCredential(ownerId, credential.revision, env); }
          catch { recovered = null; }
          recoveryMs = clock() - recoveryStarted;
          if (!recovered || recovered.kind !== "chatgpt" || recovered.expiresAt <= now()
            || recovered.accountId !== credential.accountId || recovered.revision === credential.revision) {
            recoveryOutcome = "unavailable";
            cache.delete(ownerId);
            return finish(new Response("Credential unavailable", { status: 502 }));
          }
          recoveryOutcome = "recovered";
          entry = { value: recovered, expiresAt: Math.min(now() + CACHE_TTL_MS, recovered.expiresAt - 5 * 60_000) };
          cache.set(ownerId, entry);
          retryAttempt = 1;
          upstream = await fetchAttempt(recovered, true); // exactly one retry, including a WebSocket upgrade
          upstreamStatus = upstream.status;
        }
        if (upstream.status >= 300 && upstream.status < 400) {
          upstream.body?.cancel().catch(() => {});
          return finish(new Response("Upstream redirect refused", { status: 502 }));
        }
        return finish(upstream); // The upstream response body stays a stream; no buffering.
      } catch {
        return finish(new Response("Upstream unavailable", { status: 502 }));
      }
    },
  };
}
