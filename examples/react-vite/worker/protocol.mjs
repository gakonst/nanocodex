export const DEFAULT_RESPONSES_UPGRADE_URL = "https://api.openai.com/v1/responses";
export const CHATGPT_RESPONSES_PATH = "/backend-api/codex/responses";

const RESPONSES_WEBSOCKETS_BETA = "responses_websockets=2026-02-06";

export function validateWebSocketRequest(request) {
  const url = new URL(request.url);
  if (url.pathname !== "/api/responses") return new Response("Not found", { status: 404 });
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  if (!sameOrigin(request, url)) return new Response("Forbidden", { status: 403 });
  if (!validSessionId(url.searchParams.get("session_id"))) {
    return new Response("Invalid session", { status: 400 });
  }
  return undefined;
}

export function upstreamHeaders(credential, sessionId) {
  const bearer = credential.kind === "chatgpt" ? credential.accessToken : credential.apiKey;
  return {
    Upgrade: "websocket",
    Authorization: `Bearer ${bearer}`,
    "OpenAI-Beta": RESPONSES_WEBSOCKETS_BETA,
    originator: "codex_cli_rs",
    "x-openai-internal-codex-responses-lite": "true",
    "session-id": sessionId,
    "thread-id": sessionId,
    "x-client-request-id": sessionId,
    "x-responsesapi-include-timing-metrics": "true",
    "User-Agent": "codex_cli_rs/0.0.0",
    ...(credential.kind === "chatgpt" ? {
      "ChatGPT-Account-ID": credential.accountId,
      ...(credential.fedramp ? { "X-OpenAI-Fedramp": "true" } : {}),
    } : {}),
  };
}

export function modelConnection(env) {
  const local = localChatGptCredential(env);
  if (local instanceof Response) return local;
  if (local) {
    const endpoint = localEgressUrl(env.NANOCODEX_DEV_CHATGPT_EGRESS_URL);
    if (endpoint instanceof Response) return endpoint;
    return {
      credential: local,
      url: new URL(`.${CHATGPT_RESPONSES_PATH}`, endpoint).href,
    };
  }
  if (typeof env.OPENAI_API_KEY !== "string" || !env.OPENAI_API_KEY.trim()) {
    return new Response("Worker credential is not configured", { status: 500 });
  }
  return {
    credential: { kind: "api_key", apiKey: env.OPENAI_API_KEY.trim() },
    url: DEFAULT_RESPONSES_UPGRADE_URL,
  };
}

function localChatGptCredential(env) {
  if (env.ENVIRONMENT !== "development") return undefined;
  const values = [
    env.NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN,
    env.NANOCODEX_DEV_CHATGPT_ACCOUNT_ID,
    env.NANOCODEX_DEV_CHATGPT_FEDRAMP,
    env.NANOCODEX_DEV_CHATGPT_EXPIRES_AT,
    env.NANOCODEX_DEV_CHATGPT_EGRESS_URL,
    env.NANOCODEX_DEV_CHATGPT_SESSION_ID,
  ];
  if (values.every((value) => typeof value !== "string" || !value.trim())) return undefined;
  const accessToken = env.NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN?.trim();
  const accountId = env.NANOCODEX_DEV_CHATGPT_ACCOUNT_ID?.trim();
  const fedramp = env.NANOCODEX_DEV_CHATGPT_FEDRAMP?.trim().toLowerCase();
  const expiresAt = Number(env.NANOCODEX_DEV_CHATGPT_EXPIRES_AT);
  if (!accessToken || !accountId || !["true", "false"].includes(fedramp) || !Number.isSafeInteger(expiresAt)) {
    return new Response("Local ChatGPT credential is incomplete", { status: 500 });
  }
  if (expiresAt <= Date.now() + 5 * 60_000) {
    return new Response("Local ChatGPT login expires too soon; run codex login", { status: 503 });
  }
  return { kind: "chatgpt", accessToken, accountId, fedramp: fedramp === "true" };
}

function localEgressUrl(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    return new Response("Local ChatGPT egress is unavailable", { status: 500 });
  }
  if (
    url.protocol !== "http:"
    || url.hostname !== "127.0.0.1"
    || url.username
    || url.password
    || !/^\/[A-Za-z0-9_-]{43}\/$/.test(url.pathname)
    || url.search
    || url.hash
  ) {
    return new Response("Local ChatGPT egress is invalid", { status: 500 });
  }
  return url;
}

function sameOrigin(request, url) {
  const origin = request.headers.get("Origin");
  if (!origin) return false;
  try {
    return new URL(origin).host === url.host;
  } catch {
    return false;
  }
}

function validSessionId(sessionId) {
  return typeof sessionId === "string" && /^[A-Za-z0-9._:-]{1,200}$/.test(sessionId);
}
