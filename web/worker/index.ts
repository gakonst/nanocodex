const json = (body: unknown, init?: ResponseInit) =>
  Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      ...init?.headers,
    },
  });

const RESPONSES_UPGRADE_URL = "https://api.openai.com/v1/responses";
const RESPONSES_WEBSOCKETS_BETA = "responses_websockets=2026-02-06";
const WEB_SEARCH_URL = "https://api.openai.com/v1/alpha/search";
const IMAGE_GENERATION_URL = "https://api.openai.com/v1/images/generations";
const IMAGE_EDIT_URL = "https://api.openai.com/v1/images/edits";
const MODEL = "gpt-5.6-sol";
const IMAGE_MODEL = "gpt-image-2";
const MAX_JSON_BODY_CHARS = 32 * 1024 * 1024;
const MAX_SEARCH_OUTPUT_CHARS = 1024 * 1024;
const MAX_API_KEY_CHARS = 1_024;
const BYOK_SESSION_TTL_MS = 60 * 60 * 1_000;
const BYOK_COOKIE = "nanocodex_byok";

type WorkerEnv = {
  ENVIRONMENT: string;
  OPENAI_API_KEY?: string;
  BYOK_SESSIONS?: DurableObjectNamespace;
};

type Credential = { apiKey: string; source: "user" | "deployment" };
type StoredCredential = { apiKey: string; expiresAt: number };

export default {
  async fetch(request: Request, env: WorkerEnv): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === "/api/health" && request.method === "GET") {
      const credential = await resolveCredential(request, env);
      return json({
        agent_configured: Boolean(credential),
        credential_source: credential?.source ?? null,
        service: "nanocodex",
        runtime: "cloudflare-workers",
        status: "ok",
      });
    }

    if (url.pathname === "/api/auth/openai" && request.method === "PUT") {
      return createByokSession(request, env, url);
    }

    if (url.pathname === "/api/auth/openai" && request.method === "DELETE") {
      return clearByokSession(request, env, url);
    }

    if (url.pathname === "/api/responses") {
      return upgradeResponsesWebSocket(request, env, url);
    }

    if (url.pathname === "/api/tools/web-search" && request.method === "POST") {
      return proxyWebSearch(request, env, url);
    }

    if (url.pathname === "/api/tools/image-generation" && request.method === "POST") {
      return proxyImageGeneration(request, env, url);
    }

    if (url.pathname === "/api/proposals" && request.method === "POST") {
      return json(
        {
          status: "payment_required",
          mode: "testnet_preview",
          amount: "0.20",
          currency: "USD",
          message: "A live MPP challenge will replace this preview response.",
        },
        { status: 402 },
      );
    }

    return json({ error: "not_found" }, { status: 404 });
  },
};

async function proxyWebSearch(request: Request, env: WorkerEnv, url: URL): Promise<Response> {
  const credential = await validateToolRequest(request, env, url);
  if (credential instanceof Response) return credential;
  const decoded = await readJsonBody(request);
  if (decoded instanceof Response) return decoded;
  const sessionId = typeof decoded.session_id === "string" ? decoded.session_id : "";
  if (!/^[A-Za-z0-9._:-]{1,200}$/.test(sessionId)) return json({ error: "invalid session" }, { status: 400 });
  const commands = asObject(decoded.commands);
  if (!commands || !hasWebOperation(commands)) {
    return json({ error: "web__run requires at least one operation" }, { status: 400 });
  }
  const queries = Array.isArray(commands.search_query) ? commands.search_query.length : 0;
  if (queries > 4) return json({ error: "web__run accepts at most 4 search queries" }, { status: 400 });
  if (queries === 4 && !["medium", "long"].includes(String(commands.response_length))) {
    return json({ error: "four search queries require medium or long response_length" }, { status: 400 });
  }
  const upstream = await fetch(WEB_SEARCH_URL, {
    method: "POST",
    headers: openAiHeaders(credential.apiKey),
    body: JSON.stringify({
      id: sessionId,
      model: MODEL,
      commands,
      settings: { allowed_callers: ["direct"], external_web_access: true },
      max_output_tokens: 10_000,
    }),
  });
  const body = await upstream.text();
  if (body.length > MAX_SEARCH_OUTPUT_CHARS) {
    return json({ error: "web search response exceeded 1 MiB" }, { status: 502 });
  }
  if (!upstream.ok) return upstreamError("web search", upstream.status, body);
  let payload: unknown;
  try { payload = JSON.parse(body); } catch { return json({ error: "web search returned invalid JSON" }, { status: 502 }); }
  const output = asObject(payload)?.output;
  if (typeof output !== "string") return json({ error: "web search response omitted output" }, { status: 502 });
  return json({ output });
}

async function proxyImageGeneration(request: Request, env: WorkerEnv, url: URL): Promise<Response> {
  const credential = await validateToolRequest(request, env, url);
  if (credential instanceof Response) return credential;
  const decoded = await readJsonBody(request);
  if (decoded instanceof Response) return decoded;
  const prompt = typeof decoded.prompt === "string" ? decoded.prompt.trim() : "";
  if (!prompt) return json({ error: "image prompt must not be empty" }, { status: 400 });
  const images = Array.isArray(decoded.images)
    ? decoded.images.filter((image): image is string => typeof image === "string")
    : [];
  if (images.length > 5 || images.some((image) => !image.startsWith("data:image/"))) {
    return json({ error: "image edits require at most five data-image inputs" }, { status: 400 });
  }
  const upstream = await fetch(images.length ? IMAGE_EDIT_URL : IMAGE_GENERATION_URL, {
    method: "POST",
    headers: openAiHeaders(credential.apiKey),
    body: JSON.stringify({
      ...(images.length ? { images: images.map((image_url) => ({ image_url })) } : {}),
      prompt,
      background: "auto",
      model: IMAGE_MODEL,
      quality: "auto",
      size: "auto",
    }),
  });
  const payload = await upstream.json().catch(() => undefined) as {
    data?: Array<{ b64_json?: unknown }>;
    error?: { message?: unknown };
  } | undefined;
  if (!upstream.ok) {
    const message = typeof payload?.error?.message === "string" ? payload.error.message : `HTTP ${upstream.status}`;
    return json({ error: `image generation failed: ${message}` }, { status: 502 });
  }
  const encoded = payload?.data?.[0]?.b64_json;
  if (typeof encoded !== "string" || !encoded) {
    return json({ error: "image generation returned no image" }, { status: 502 });
  }
  return json({ image_url: `data:image/png;base64,${encoded}` });
}

async function validateToolRequest(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Credential | Response> {
  if (!sameOrigin(request, url)) return json({ error: "forbidden" }, { status: 403 });
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "expected JSON" }, { status: 415 });
  }
  return await resolveCredential(request, env)
    ?? json({ error: "OpenAI credentials are not configured" }, { status: 503 });
}

async function readJsonBody(request: Request): Promise<Record<string, unknown> | Response> {
  const body = await request.text();
  if (body.length > MAX_JSON_BODY_CHARS) return json({ error: "request body is too large" }, { status: 413 });
  try {
    const decoded = JSON.parse(body);
    return asObject(decoded) ?? json({ error: "expected a JSON object" }, { status: 400 });
  } catch {
    return json({ error: "invalid JSON" }, { status: 400 });
  }
}

function hasWebOperation(commands: Record<string, unknown>): boolean {
  return ["search_query", "image_query", "open", "click", "find", "finance", "weather", "sports", "time"]
    .some((key) => Array.isArray(commands[key]) && commands[key].length > 0);
}

function openAiHeaders(apiKey: string): Record<string, string> {
  return {
    Authorization: `Bearer ${apiKey}`,
    "content-type": "application/json",
    "User-Agent": "nanocodex-web/0.1.0",
  };
}

function upstreamError(operation: string, status: number, body: string): Response {
  let message = body.slice(0, 4_096);
  try {
    const parsed = asObject(JSON.parse(body));
    const error = asObject(parsed?.error);
    if (typeof error?.message === "string") message = error.message;
  } catch { /* Use the bounded response body. */ }
  return json({ error: `${operation} failed: ${message || `HTTP ${status}`}` }, { status: 502 });
}

function asObject(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

async function upgradeResponsesWebSocket(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  if (!sameOrigin(request, url)) {
    return new Response("Forbidden", { status: 403 });
  }
  const sessionId = url.searchParams.get("session_id");
  if (!sessionId || !/^[A-Za-z0-9._:-]{1,200}$/.test(sessionId)) {
    return new Response("Invalid session", { status: 400 });
  }
  const credential = await resolveCredential(request, env);
  if (!credential) {
    return new Response("OpenAI credentials are not configured", { status: 503 });
  }

  const upstreamResponse = await fetch(RESPONSES_UPGRADE_URL, {
    headers: {
      Upgrade: "websocket",
      Authorization: `Bearer ${credential.apiKey}`,
      "OpenAI-Beta": RESPONSES_WEBSOCKETS_BETA,
      "x-openai-internal-codex-responses-lite": "true",
      "session-id": sessionId,
      "thread-id": sessionId,
      "x-client-request-id": sessionId,
      "x-responsesapi-include-timing-metrics": "true",
      "User-Agent": "nanocodex-web/0.1.0",
    },
  });
  const upstream = upstreamResponse.webSocket;
  if (!upstream) {
    await upstreamResponse.body?.cancel();
    return new Response("OpenAI WebSocket upgrade failed", { status: 502 });
  }

  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  upstream.accept();
  server.accept();
  bridge(server, upstream);
  return new Response(null, { status: 101, webSocket: client });
}

async function createByokSession(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (!sameOrigin(request, url)) return json({ error: "forbidden" }, { status: 403 });
  if (!env.BYOK_SESSIONS) return json({ error: "BYOK sessions are not configured" }, { status: 503 });
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "expected JSON" }, { status: 415 });
  }
  const body = await request.text();
  if (body.length > 4_096) return json({ error: "request body is too large" }, { status: 413 });
  let apiKey: unknown;
  try {
    apiKey = asObject(JSON.parse(body))?.api_key;
  } catch {
    return json({ error: "invalid JSON" }, { status: 400 });
  }
  const normalizedApiKey = typeof apiKey === "string" ? apiKey.trim() : "";
  if (!normalizedApiKey || normalizedApiKey.length > MAX_API_KEY_CHARS) {
    return json({ error: "api_key must be a non-empty string of at most 1024 characters" }, { status: 400 });
  }

  const sessionId = randomSessionId();
  const stub = env.BYOK_SESSIONS.get(env.BYOK_SESSIONS.idFromName(sessionId));
  const stored = await stub.fetch("https://byok.internal/credential", {
    method: "PUT",
    body: normalizedApiKey,
  });
  if (!stored.ok) return json({ error: "failed to create BYOK session" }, { status: 503 });
  await deleteSession(request, env);
  return json(
    { agent_configured: true, credential_source: "user", expires_in: BYOK_SESSION_TTL_MS / 1_000 },
    { headers: { "set-cookie": sessionCookie(sessionId, url) } },
  );
}

async function clearByokSession(
  request: Request,
  env: WorkerEnv,
  url: URL,
): Promise<Response> {
  if (!sameOrigin(request, url)) return json({ error: "forbidden" }, { status: 403 });
  await deleteSession(request, env);
  const credential = env.OPENAI_API_KEY
    ? { agent_configured: true, credential_source: "deployment" }
    : { agent_configured: false, credential_source: null };
  return json(credential, { headers: { "set-cookie": clearSessionCookie(url) } });
}

async function resolveCredential(request: Request, env: WorkerEnv): Promise<Credential | undefined> {
  const sessionId = sessionIdFromRequest(request);
  if (sessionId && env.BYOK_SESSIONS) {
    try {
      const stub = env.BYOK_SESSIONS.get(env.BYOK_SESSIONS.idFromName(sessionId));
      const response = await stub.fetch("https://byok.internal/credential");
      if (response.ok) {
        const apiKey = await response.text();
        if (apiKey) return { apiKey, source: "user" };
      }
    } catch { /* A deployment credential remains a valid fallback. */ }
  }
  return env.OPENAI_API_KEY ? { apiKey: env.OPENAI_API_KEY, source: "deployment" } : undefined;
}

async function deleteSession(request: Request, env: WorkerEnv): Promise<void> {
  const sessionId = sessionIdFromRequest(request);
  if (!sessionId || !env.BYOK_SESSIONS) return;
  const stub = env.BYOK_SESSIONS.get(env.BYOK_SESSIONS.idFromName(sessionId));
  await stub.fetch("https://byok.internal/credential", { method: "DELETE" });
}

function sessionIdFromRequest(request: Request): string | undefined {
  const cookie = request.headers.get("cookie");
  if (!cookie) return undefined;
  for (const part of cookie.split(";")) {
    const [name, ...rest] = part.trim().split("=");
    if (name !== BYOK_COOKIE) continue;
    const value = rest.join("=");
    if (/^[A-Za-z0-9_-]{43}$/.test(value)) return value;
  }
  return undefined;
}

function randomSessionId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function sessionCookie(sessionId: string, url: URL): string {
  const secure = url.protocol === "https:" ? "; Secure" : "";
  return `${BYOK_COOKIE}=${sessionId}; Path=/api; HttpOnly; SameSite=Strict; Max-Age=${BYOK_SESSION_TTL_MS / 1_000}${secure}`;
}

function clearSessionCookie(url: URL): string {
  const secure = url.protocol === "https:" ? "; Secure" : "";
  return `${BYOK_COOKIE}=; Path=/api; HttpOnly; SameSite=Strict; Max-Age=0${secure}`;
}

function sameOrigin(request: Request, url: URL): boolean {
  const origin = request.headers.get("Origin");
  if (!origin) return false;
  try {
    return new URL(origin).origin === url.origin;
  } catch {
    return false;
  }
}

function bridge(left: WebSocket, right: WebSocket): void {
  forward(left, right);
  forward(right, left);
}

function forward(source: WebSocket, destination: WebSocket): void {
  source.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      closeSocket(source, 1003, "text frames required");
      closeSocket(destination, 1003, "text frames required");
      return;
    }
    if (destination.readyState === WebSocket.OPEN) destination.send(event.data);
  });
  source.addEventListener("close", (event) => {
    closeSocket(destination, event.code, event.reason || "peer closed");
  });
  source.addEventListener("error", () => {
    closeSocket(destination, 1011, "peer WebSocket failed");
  });
}

function closeSocket(socket: WebSocket, code: number, reason: string): void {
  if (socket.readyState !== WebSocket.CONNECTING && socket.readyState !== WebSocket.OPEN) return;
  const safeCode = code === 1000 || (code >= 3000 && code <= 4999) ? code : 1011;
  socket.close(safeCode, reason.slice(0, 120));
}

export class ByokSession {
  readonly #state: DurableObjectState;

  constructor(state: DurableObjectState) {
    this.#state = state;
  }

  async fetch(request: Request): Promise<Response> {
    if (request.method === "PUT") {
      const apiKey = await request.text();
      if (!apiKey || apiKey.length > MAX_API_KEY_CHARS) return new Response(null, { status: 400 });
      const credential: StoredCredential = {
        apiKey,
        expiresAt: Date.now() + BYOK_SESSION_TTL_MS,
      };
      await this.#state.storage.put("credential", credential);
      await this.#state.storage.setAlarm(credential.expiresAt);
      return new Response(null, { status: 204 });
    }
    if (request.method === "DELETE") {
      await this.#state.storage.deleteAll();
      return new Response(null, { status: 204 });
    }
    const credential = await this.#state.storage.get<StoredCredential>("credential");
    if (!credential || credential.expiresAt <= Date.now()) {
      if (credential) await this.#state.storage.deleteAll();
      return new Response(null, { status: 404 });
    }
    return new Response(credential.apiKey, {
      headers: { "cache-control": "no-store", "content-type": "text/plain" },
    });
  }

  async alarm(): Promise<void> {
    await this.#state.storage.deleteAll();
  }
}
